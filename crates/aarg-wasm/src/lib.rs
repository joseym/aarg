//! WebAssembly bindings for AARG's domain logic: both the deterministic,
//! model-free core and the model-driven pipeline, run over a host-provided
//! LLM callback.
//!
//! **Deterministic exports** are the no-LLM, no-filesystem half, so a web UI
//! can run them entirely in the browser, offline, on any target: validate a
//! dataset, preview how it covers a job description, project the canonical
//! draft into the ATS payload, check that a variant makes no claim the
//! canonical draft doesn't (the never-fabricate backstop, running
//! client-side), scrub AI-tell dashes from a draft or payload before it
//! ships, mirror JD phrasing a recorded skill already backs, and classify
//! every line of a canonical draft by whether it traces back to the
//! dataset (the free-edit UI's provenance story). Everything
//! crosses the JS boundary as JSON, the same shape the CLI reads and writes
//! on disk. The logic lives in plain `*_impl` functions (`Result<String,
//! String>`, or a plain `String` for the one infallible export) so it is
//! testable as ordinary Rust on the host; the thin `#[wasm_bindgen]`
//! wrappers just turn the error string into a thrown JS error.
//!
//! **Model-driven exports** (`parse_jd_llm`, `analyze_gap_llm`,
//! `tailor_draft`, `review_draft`) run the real domain agents — they need an
//! `LlmClient`, and the only one available in a browser is a JS-provided
//! async callback, so these exports are `wasm32`-only. Each one takes a
//! `js_sys::Function` argument and drives it through the Send-preserving
//! channel bridge in `bridge.rs`: the agent runs against a `BridgeClient`
//! (satisfies `LlmClient`, itself `Send`), while a separate task (the pump)
//! owns the callback and does the actual `!Send` JS work. See `bridge.rs`'s
//! module doc for why that split is necessary.

use wasm_bindgen::prelude::*;

use aarg_domain::dataset::types::ResumeDataset;
use aarg_domain::jd::JobRequirements;
use aarg_domain::tailor::TailoredResume;
use aarg_domain::variant::VariantPayload;

/// The Send-preserving channel bridge (`BridgeClient`, the pump, the model
/// resolver) that lets the model-driven exports below run the real domain
/// agents over a JS-provided LLM callback. See `bridge.rs` for the design.
pub mod bridge;

// The model-driven exports run real agents, so they need the agent context
// types and the bridge. They are wasm-only (they take a `js_sys::Function`),
// so their imports are gated too, keeping the native (test) build clean.
#[cfg(target_arch = "wasm32")]
use aarg_domain::agent::{Agent, AgentContext};
#[cfg(target_arch = "wasm32")]
use aarg_domain::gap::GapReport;
#[cfg(target_arch = "wasm32")]
use aarg_domain::llm::TokenUsage;
#[cfg(target_arch = "wasm32")]
use aarg_domain::metric::{AnchorStyle, MetricTarget};
#[cfg(target_arch = "wasm32")]
use aarg_domain::review::{
    AdversarialReport, AdversarialReviewerAgent, Objection, ObjectionKind, ObjectionTarget,
    ReviewError, ReviewInput,
};
#[cfg(target_arch = "wasm32")]
use aarg_domain::strengthen::{self, StrengthenTarget};
#[cfg(target_arch = "wasm32")]
use aarg_domain::tailor::{BuildId, Evaluation, Evaluator, JdId, LoopLimits, LoopObserver};
#[cfg(target_arch = "wasm32")]
use aarg_domain::trace::Tracer;
#[cfg(target_arch = "wasm32")]
use aarg_domain::variant::{
    HumanVariantAgent, HumanVariantInput, Variant, VariantAdapterAgent, VariantInput,
};
#[cfg(target_arch = "wasm32")]
use bridge::{BridgeClient, BridgeUser, Models};

/// Route a wasm panic's message and source location to the browser's
/// console (`console.error`), instead of the default: a bare
/// `RuntimeError: unreachable executed` with no indication of what failed
/// or where. `#[wasm_bindgen(start)]` marks this to run once, automatically,
/// the moment the wasm module finishes instantiating — before any exported
/// function is callable — so every panic in this crate is covered from the
/// first call onward.
#[wasm_bindgen(start)]
fn start() {
    console_error_panic_hook::set_once();
}

/// Parse a JSON argument into a domain type, naming what was malformed.
fn parse<T: serde::de::DeserializeOwned>(json: &str, what: &str) -> Result<T, String> {
    serde_json::from_str(json).map_err(|e| format!("invalid {what}: {e}"))
}

/// Serialize a result to JSON.
fn dump<T: serde::Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string(value).map_err(|e| format!("could not serialize result: {e}"))
}

/// Render an error and its full cause chain into one line, joined with `: `.
///
/// A `#[source]`/`#[from]` cause never reaches the browser through
/// `to_string()` alone — that prints only the top error's `Display`, which for
/// a wrapper like `LoopError::Evaluate` is the bare "the draft evaluator
/// failed" with the real reason (an LLM transport error, a reply that failed
/// validation) buried underneath. Walking `std::error::Error::source()` and
/// joining every level surfaces the whole story, so a rejected export carries
/// e.g. "the draft evaluator failed: the model call failed: Load failed"
/// instead of just the first clause. Every export that rejects with a Rust
/// error routes through here, so the browser always sees the full chain,
/// never a lone opaque headline.
///
/// One presentation fix-up on the joined chain: `LlmError::Transport`'s fixed
/// headline, "the LLM transport failed", is rewritten to "the model call
/// failed". In this crate that variant carries every failure of the JS `llm`
/// callback — and since the callback rejects with the *server's* message (an
/// unreachable local model server, a missing model, a provider reject), the
/// failure it reports is usually the model call itself, not the transport to
/// it. The chain ends up in the UI toast verbatim, so the headline should not
/// misdirect. The wording can't change at the source: the Display lives in
/// `aarg-core`, whose native clients use the same variant for real transport
/// faults.
///
/// Not gated to wasm alone: the native `cargo test` build exercises it (the
/// wasm exports that call it are wasm-only, but the rendering logic is plain
/// Rust worth testing on the host).
#[cfg(any(target_arch = "wasm32", test))]
fn error_chain<E: std::error::Error>(err: &E) -> String {
    let mut out = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        out.push_str(": ");
        out.push_str(&cause.to_string());
        source = cause.source();
    }
    out.replace("the LLM transport failed:", "the model call failed:")
}

fn validate_impl(dataset_json: &str) -> Result<String, String> {
    let dataset: ResumeDataset = parse(dataset_json, "dataset")?;
    dump(&aarg_domain::dataset::validate::validate(&dataset))
}

fn analyze_gap_impl(jd_json: &str, dataset_json: &str) -> Result<String, String> {
    let jd: JobRequirements = parse(jd_json, "job requirements")?;
    let dataset: ResumeDataset = parse(dataset_json, "dataset")?;
    dump(&aarg_domain::gap::deterministic_gap(&jd, &dataset))
}

fn project_ats_impl(canonical_json: &str) -> Result<String, String> {
    let draft: TailoredResume = parse(canonical_json, "canonical draft")?;
    dump(&aarg_domain::variant::ats_payload(&draft))
}

fn check_claims_impl(canonical_json: &str, payload_json: &str) -> Result<String, String> {
    let canonical: TailoredResume = parse(canonical_json, "canonical draft")?;
    let payload: VariantPayload = parse(payload_json, "variant payload")?;
    let (ok, divergences) = match aarg_domain::variant::check_claims(&canonical, &payload) {
        Ok(()) => (true, Vec::new()),
        Err(divergence) => (false, divergence.divergences),
    };
    dump(&serde_json::json!({ "ok": ok, "divergences": divergences }))
}

// `normalize_dashes` can't fail — it's a character-by-character rewrite of
// whatever string it's given — so its `_impl` returns a plain `String`
// rather than the `Result<String, String>` the other five use.
fn normalize_dashes_impl(text: &str) -> String {
    aarg_domain::tailor::normalize_dashes(text)
}

fn scrub_resume_impl(canonical_json: &str) -> Result<String, String> {
    let mut draft: TailoredResume = parse(canonical_json, "canonical draft")?;
    aarg_domain::tailor::scrub_resume_text(&mut draft);
    dump(&draft)
}

fn scrub_variant_impl(payload_json: &str) -> Result<String, String> {
    let mut payload: VariantPayload = parse(payload_json, "variant payload")?;
    aarg_domain::variant::scrub_variant_text(&mut payload);
    dump(&payload)
}

/// A JSON-serializable mirror of `aarg_domain::mirror::MirrorMatch`. `mirror`
/// is a `no_std`-agnostic domain module owned elsewhere this round, and its
/// `MirrorMatch` doesn't derive `Serialize` — so the binding maps it to this
/// local, serializable shape rather than editing the domain type.
#[derive(serde::Serialize)]
struct MirrorMatchJson {
    phrase: String,
    dataset_skill: String,
}

fn backed_phrases_impl(jd_json: &str, dataset_json: &str) -> Result<String, String> {
    let jd: JobRequirements = parse(jd_json, "job requirements")?;
    let dataset: ResumeDataset = parse(dataset_json, "dataset")?;
    let matches: Vec<MirrorMatchJson> = aarg_domain::mirror::backed_phrases(&jd, &dataset)
        .into_iter()
        .map(|m| MirrorMatchJson {
            phrase: m.phrase,
            dataset_skill: m.dataset_skill,
        })
        .collect();
    dump(&matches)
}

fn keyword_key_impl(name: &str) -> Result<String, String> {
    dump(&aarg_domain::keywords::keyword_key(name))
}

fn check_provenance_impl(canonical_json: &str, dataset_json: &str) -> Result<String, String> {
    let draft: TailoredResume = parse(canonical_json, "canonical draft")?;
    let dataset: ResumeDataset = parse(dataset_json, "dataset")?;
    dump(&aarg_domain::provenance::check_provenance(&draft, &dataset))
}

/// How much a matched requirement counts toward coverage, by how hard the JD
/// asks for it: a make-or-break `critical` skill is worth 3× a `preferred`
/// nice-to-have. This is a *presentation* weighting — how loudly to celebrate
/// a match in the headline — not a domain policy, so it lives in the binding.
fn importance_weight(importance: aarg_domain::jd::Importance) -> u32 {
    use aarg_domain::jd::Importance;
    match importance {
        Importance::Critical => 3,
        Importance::Required => 2,
        Importance::Preferred => 1,
    }
}

/// Per-importance tally: how many requirements at this tier the dataset backs,
/// out of how many the JD asks for.
#[derive(Default, serde::Serialize)]
struct TierCoverage {
    matched: usize,
    total: usize,
}

fn weighted_coverage_impl(gap_json: &str, jd_json: &str) -> Result<String, String> {
    let gap: aarg_domain::gap::GapReport = parse(gap_json, "gap report")?;
    let jd: JobRequirements = parse(jd_json, "job requirements")?;

    // The denominator: every requirement the JD lists (required + preferred),
    // each contributing its importance weight. Only *solidly matched*
    // requirements (`gap.matched`) earn their weight — weak and unknown ones
    // earn nothing, the same "usable evidence only" line the rest of the
    // pipeline holds.
    let mut critical = TierCoverage::default();
    let mut required = TierCoverage::default();
    let mut preferred = TierCoverage::default();
    let mut total_weight: u32 = 0;
    for skill in jd.required_skills.iter().chain(jd.preferred_skills.iter()) {
        total_weight += importance_weight(skill.importance);
        tier_of(
            skill.importance,
            &mut critical,
            &mut required,
            &mut preferred,
        )
        .total += 1;
    }

    let mut matched_weight: u32 = 0;
    for m in &gap.matched {
        matched_weight += importance_weight(m.jd_skill.importance);
        tier_of(
            m.jd_skill.importance,
            &mut critical,
            &mut required,
            &mut preferred,
        )
        .matched += 1;
    }

    // `score` is a fraction in `0.0..=1.0` (the UI multiplies by 100 for a
    // percent). A JD that lists no requirements has nothing to cover, so its
    // coverage is vacuously 0 rather than a misleading 100% — there is no
    // evidence either way. `min(1.0)` guards the theoretical case of a gap
    // report carrying more matches than the JD has requirements.
    let score = if total_weight == 0 {
        0.0
    } else {
        (matched_weight as f32 / total_weight as f32).min(1.0)
    };
    let matched_count = critical.matched + required.matched + preferred.matched;
    let total_count = critical.total + required.total + preferred.total;

    dump(&serde_json::json!({
        "score": score,
        "matched": matched_count,
        "total": total_count,
        "by_importance": {
            "critical": critical,
            "required": required,
            "preferred": preferred,
        },
    }))
}

/// Pick the tally bucket for an importance tier, so the two loops above route
/// each requirement to the same place.
fn tier_of<'a>(
    importance: aarg_domain::jd::Importance,
    critical: &'a mut TierCoverage,
    required: &'a mut TierCoverage,
    preferred: &'a mut TierCoverage,
) -> &'a mut TierCoverage {
    use aarg_domain::jd::Importance;
    match importance {
        Importance::Critical => critical,
        Importance::Required => required,
        Importance::Preferred => preferred,
    }
}

/// Validate a dataset, returning a `ValidationReport` (problems + notes). The
/// never-fabricate invariant starts here: a skill with no evidence is a
/// problem the report flags.
#[wasm_bindgen]
pub fn validate(dataset_json: &str) -> Result<String, JsValue> {
    validate_impl(dataset_json).map_err(|e| JsValue::from_str(&e))
}

/// Preview, with no model call, how a dataset covers a job description: which
/// JD skills resolve against recorded experience and which don't. The parsed
/// `JobRequirements` is produced by the host (parsing is a model call); this is
/// the deterministic coverage half.
#[wasm_bindgen]
pub fn analyze_gap(jd_json: &str, dataset_json: &str) -> Result<String, JsValue> {
    analyze_gap_impl(jd_json, dataset_json).map_err(|e| JsValue::from_str(&e))
}

/// Project a canonical `TailoredResume` into the ATS variant payload — the
/// deterministic, same-facts projection (the human variant's reword needs the
/// model and is not bound here).
#[wasm_bindgen]
pub fn project_ats(canonical_json: &str) -> Result<String, JsValue> {
    project_ats_impl(canonical_json).map_err(|e| JsValue::from_str(&e))
}

/// Check that a variant payload makes no claim the canonical draft doesn't —
/// the never-fabricate backstop, running client-side. Returns
/// `{ "ok": true, "divergences": [] }` when the variant is faithful, or
/// `{ "ok": false, "divergences": [...] }` listing each claim that diverged.
#[wasm_bindgen]
pub fn check_claims(canonical_json: &str, payload_json: &str) -> Result<String, JsValue> {
    check_claims_impl(canonical_json, payload_json).map_err(|e| JsValue::from_str(&e))
}

/// Strip the AI-tell em/en dashes ("led the team — shipped it") out of a
/// string, the way every build's scrub pass does before a draft ships.
/// Punctuation only — it never touches a number, name, or claim. Infallible,
/// so unlike the others this returns a plain string rather than a `Result`.
#[wasm_bindgen]
pub fn normalize_dashes(text: &str) -> String {
    normalize_dashes_impl(text)
}

/// Scrub AI-tell dashes from every free-text field of a canonical
/// `TailoredResume` (headline, summary, bullets, project text, achievements,
/// skills) and return the scrubbed draft as JSON.
#[wasm_bindgen]
pub fn scrub_resume(canonical_json: &str) -> Result<String, JsValue> {
    scrub_resume_impl(canonical_json).map_err(|e| JsValue::from_str(&e))
}

/// Scrub AI-tell dashes from a variant payload's free-text fields and return
/// it as JSON. The human variant is reworded by an LLM, which can
/// reintroduce a dash the canonical draft was already scrubbed of — this is
/// the payload-side pass that runs before it renders.
#[wasm_bindgen]
pub fn scrub_variant(payload_json: &str) -> Result<String, JsValue> {
    scrub_variant_impl(payload_json).map_err(|e| JsValue::from_str(&e))
}

/// JD phrases a recorded, evidence-backed skill already covers in different
/// words (e.g. "AI-powered products" backed by a recorded "AI-Powered
/// Product Development"), so a literal ATS scan can be credited without
/// inserting any claim the dataset doesn't back. Returns a JSON array of
/// `{ "phrase": ..., "dataset_skill": ... }`.
#[wasm_bindgen]
pub fn backed_phrases(jd_json: &str, dataset_json: &str) -> Result<String, JsValue> {
    backed_phrases_impl(jd_json, dataset_json).map_err(|e| JsValue::from_str(&e))
}

/// Reduce a keyword or phrase to its comparison key: lowercased, noise words
/// (seniority, filler) dropped, each remaining word lightly stemmed, then
/// sorted — so "Sr Engineering Manager" and "engineering manager" reduce to
/// the same key. Returns a JSON array of tokens.
#[wasm_bindgen]
pub fn keyword_key(name: &str) -> Result<String, JsValue> {
    keyword_key_impl(name).map_err(|e| JsValue::from_str(&e))
}

/// Classify every line of a canonical `TailoredResume` (each role bullet, the
/// summary, and each skill) by whether it traces back to the dataset:
/// verbatim, grounded (a defensible rewrite of a specific recorded text), or
/// unrecorded (no dataset text plausibly backs it — informational for the
/// UI's free-edit story, not a fabrication verdict; the never-fabricate
/// guards that actually gate a build live in the tailoring pipeline, not
/// here). Returns the `ProvenanceReport` as JSON.
#[wasm_bindgen]
pub fn check_provenance(canonical_json: &str, dataset_json: &str) -> Result<String, JsValue> {
    check_provenance_impl(canonical_json, dataset_json).map_err(|e| JsValue::from_str(&e))
}

/// The headline coverage number, weighted by how hard the JD asks for each
/// skill: a matched `critical` requirement counts 3×, `required` 2×,
/// `preferred` 1×; weak and unknown requirements earn nothing. `gap_json` is a
/// `GapReport` (deterministic or full), `jd_json` the parsed `JobRequirements`.
/// Returns `{ "score": f32 in 0.0..=1.0, "matched": <count>, "total":
/// <count>, "by_importance": { "critical"|"required"|"preferred": { "matched",
/// "total" } } }` — pure and deterministic, no model call. A JD with no
/// requirements scores 0 (nothing to cover, so no evidence either way).
#[wasm_bindgen]
pub fn weighted_coverage(gap_json: &str, jd_json: &str) -> Result<String, JsValue> {
    weighted_coverage_impl(gap_json, jd_json).map_err(|e| JsValue::from_str(&e))
}

// ---------------------------------------------------------------------
// Model-driven exports (wasm-only): the real domain agents, run over a
// JS-provided LLM callback via the Send-preserving bridge.
// ---------------------------------------------------------------------
//
// Each export builds the channel, spawns the pump with the callback, runs the
// agent against an `AgentContext { llm: &BridgeClient, model: &Models, tracer:
// DISABLED, sink: None }`, and serializes the typed output. Every argument and
// result crosses as a JSON string, the same convention as the deterministic
// exports above. Arguments are owned `String`s (not `&str`): a `#[wasm_bindgen]`
// async export returns a `'static` future, which cannot borrow its arguments.

/// Wrap a plain error string into a thrown JS exception, matching the
/// deterministic wrappers' `map_err(|e| JsValue::from_str(&e))`.
#[cfg(target_arch = "wasm32")]
fn throw(message: String) -> JsValue {
    JsValue::from_str(&message)
}

/// Parse a job description into `JobRequirements` (FR-1.4) by running the
/// real `jd_parser` agent over the JS `llm` callback. `models_json` is a
/// `{"cheap","mid","smart"}` (or single `{"model"}`) map. Returns the
/// `JobRequirements` as JSON.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub async fn parse_jd_llm(
    jd_text: String,
    models_json: String,
    llm: js_sys::Function,
) -> Result<String, JsValue> {
    let models = Models::from_json(&models_json).map_err(throw)?;
    let (client, rx) = BridgeClient::new();
    bridge::spawn_pump(rx, llm);
    let ctx = AgentContext {
        llm: &client,
        model: &models,
        tracer: &Tracer::DISABLED,
        sink: None,
    };
    let jd = aarg_domain::jd::parse_jd(&ctx, &jd_text)
        .await
        .map_err(|e| throw(error_chain(&e)))?;
    dump(&jd).map_err(throw)
}

/// Full gap analysis (FR-1.5): the deterministic pass plus the model's
/// semantic match for whatever the alias map could not resolve. Returns the
/// complete `GapReport` (matched / weak / unknown) as JSON.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub async fn analyze_gap_llm(
    jd_json: String,
    dataset_json: String,
    models_json: String,
    llm: js_sys::Function,
) -> Result<String, JsValue> {
    let jd: JobRequirements = parse(&jd_json, "job requirements").map_err(throw)?;
    let dataset: ResumeDataset = parse(&dataset_json, "dataset").map_err(throw)?;
    let models = Models::from_json(&models_json).map_err(throw)?;
    let (client, rx) = BridgeClient::new();
    bridge::spawn_pump(rx, llm);
    let ctx = AgentContext {
        llm: &client,
        model: &models,
        tracer: &Tracer::DISABLED,
        sink: None,
    };
    let report = aarg_domain::gap::analyze_gap(&ctx, &jd, &dataset)
        .await
        .map_err(|e| throw(error_chain(&e)))?;
    dump(&report).map_err(throw)
}

/// Tailor the dataset to one JD (FR-1.6), producing the canonical
/// `TailoredResume` with the never-fabricate guards applied. `gap_json` is a
/// `GapReport` (run `analyze_gap_llm` first); this is a first draft, so no
/// revision context is passed.
///
/// Before returning, the canonical draft is scrubbed of AI-tell em/en dashes
/// (`scrub_resume_text`, punctuation-only — see the CLI's own finalize step
/// for the convention this mirrors), so nothing crosses the boundary
/// unscrubbed the way `scrub_variant_text`'s doc assumes the ATS payload
/// never will be.
///
/// Returns a JSON object `{ "resume": TailoredResume, "warnings": [...],
/// "dropped_unrecorded": [...] }`, not the bare draft:
/// - `resume` is the scrubbed canonical draft.
/// - `warnings` are lines the never-fabricate guards produced while building
///   it (for example, "model added a number not in the dataset, reverted") —
///   the same warnings the CLI prints after every tailor call, so a web UI
///   needs them to give the user the same visibility.
/// - `dropped_unrecorded` lists cleaned skill names the model proposed that
///   resolved to no recorded, evidence-backed skill, offered back to the
///   user to add inline rather than silently discarded.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub async fn tailor_draft(
    dataset_json: String,
    jd_json: String,
    gap_json: String,
    models_json: String,
    llm: js_sys::Function,
) -> Result<String, JsValue> {
    let dataset: ResumeDataset = parse(&dataset_json, "dataset").map_err(throw)?;
    let jd: JobRequirements = parse(&jd_json, "job requirements").map_err(throw)?;
    let gap: GapReport = parse(&gap_json, "gap report").map_err(throw)?;
    let models = Models::from_json(&models_json).map_err(throw)?;
    let (client, rx) = BridgeClient::new();
    bridge::spawn_pump(rx, llm);
    let ctx = AgentContext {
        llm: &client,
        model: &models,
        tracer: &Tracer::DISABLED,
        sink: None,
    };
    let mut outcome = aarg_domain::tailor::tailor_resume(
        &ctx,
        BuildId("wasm".to_string()),
        JdId("wasm".to_string()),
        &jd,
        &dataset,
        &gap,
        None,
    )
    .await
    .map_err(|e| throw(error_chain(&e)))?;
    // Punctuation-only, no claim changes — the same finalize step the CLI
    // runs on the best draft before it ever hands one out (src/commands/tailor.rs).
    aarg_domain::tailor::scrub_resume_text(&mut outcome.resume);
    dump(&serde_json::json!({
        "resume": outcome.resume,
        "warnings": outcome.warnings,
        "dropped_unrecorded": outcome.dropped_unrecorded,
    }))
    .map_err(throw)
}

/// Adversarial review of a canonical draft (FR-3.4): a skeptical hiring
/// manager files structured objections, a score, and a verdict. It only
/// flags — it never edits the draft or adds a claim. Returns the
/// `AdversarialReport` as JSON.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub async fn review_draft(
    canonical_json: String,
    jd_json: String,
    dataset_json: String,
    models_json: String,
    llm: js_sys::Function,
) -> Result<String, JsValue> {
    let draft: TailoredResume = parse(&canonical_json, "canonical draft").map_err(throw)?;
    let jd: JobRequirements = parse(&jd_json, "job requirements").map_err(throw)?;
    let dataset: ResumeDataset = parse(&dataset_json, "dataset").map_err(throw)?;
    let models = Models::from_json(&models_json).map_err(throw)?;
    let (client, rx) = BridgeClient::new();
    bridge::spawn_pump(rx, llm);
    let ctx = AgentContext {
        llm: &client,
        model: &models,
        tracer: &Tracer::DISABLED,
        sink: None,
    };
    let report = aarg_domain::review::review_draft(&ctx, draft, jd, dataset)
        .await
        .map_err(|e| throw(error_chain(&e)))?;
    dump(&report).map_err(throw)
}

/// Project the canonical draft into the HUMAN variant payload (FR-5.1) by
/// running the real `VariantAdapterAgent` over the JS `llm` callback — the
/// model-driven reword the deterministic `project_ats` has no equivalent for.
/// `dataset_json` supplies one bit the projection needs: whether the summary is
/// user-confirmed (`summary_confirmed`), in which case it's kept verbatim in
/// the human PDF too.
///
/// Never-fabricate holds structurally: `project_human` takes all
/// role/company/date structure from the canonical draft, reverts any reworded
/// line that gains a number its source lacks, and admits only canonical skills.
/// On top of that, this binding runs the SAME two-stage backstop every CLI
/// build does (which is why it takes the JD): `vet_human` re-reviews for
/// non-numeric prose that inflates a claim without changing a number — the one
/// thing the structural digit guard can't catch — then the claim-divergence
/// lint (FR-5.3, `check_claims`) refuses (throws) any payload that diverges.
///
/// Before returning, the payload is scrubbed of AI-tell em/en dashes
/// (`scrub_variant_text`, the same finalize the render path applies, since an
/// LLM reword can reintroduce a dash the canonical draft was scrubbed of).
/// Returns the human `VariantPayload` as JSON.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub async fn project_human_llm(
    canonical_json: String,
    dataset_json: String,
    jd_json: String,
    models_json: String,
    llm: js_sys::Function,
) -> Result<String, JsValue> {
    let draft: TailoredResume = parse(&canonical_json, "canonical draft").map_err(throw)?;
    let dataset: ResumeDataset = parse(&dataset_json, "dataset").map_err(throw)?;
    let jd: JobRequirements = parse(&jd_json, "job requirements").map_err(throw)?;
    let models = Models::from_json(&models_json).map_err(throw)?;
    let (client, rx) = BridgeClient::new();
    bridge::spawn_pump(rx, llm);
    let ctx = AgentContext {
        llm: &client,
        model: &models,
        tracer: &Tracer::DISABLED,
        sink: None,
    };
    let run = VariantAdapterAgent
        .run(
            &ctx,
            VariantInput {
                draft: draft.clone(),
                variant: Variant::Human,
                summary_locked: dataset.summary_confirmed,
            },
        )
        .await
        .map_err(|e| throw(error_chain(&e)))?;
    // The reworded human variant runs the same two-stage never-fabricate
    // backstop every CLI build does: `vet_human` reverts prose that inflates a
    // claim without changing a number (which the digit guard alone can't catch),
    // then `check_claims` refuses any payload that asserts more than canonical.
    let (mut payload, _review_usage) =
        aarg_domain::variant::vet_human(&ctx, &draft, run.output, &jd, &dataset)
            .await
            .map_err(|e| throw(error_chain(&e)))?;
    aarg_domain::variant::check_claims(&draft, &payload).map_err(|e| throw(error_chain(&e)))?;
    aarg_domain::variant::scrub_variant_text(&mut payload);
    dump(&payload).map_err(throw)
}

/// Re-project the HUMAN variant to address a *layout* objection (FR-5.4). A
/// layout-scoped objection (`ObjectionScope::VariantOnly`, or an
/// `ObjectionKind::LayoutDense` / `ObjectionTarget::Layout`) is a presentation
/// problem, not a content one: the fix is re-running the human projection with
/// the objection's guidance folded in as a directive, never editing a canonical
/// claim. `objection_json` is one `Objection`; its formatted line
/// (`format_objection`) becomes the layout note the `HumanVariantAgent` folds
/// into its prompt.
///
/// Routed by scope like the CLI: this refuses any objection that isn't
/// variant-scoped, since rewording away a *canonical* content objection would
/// mask a claim problem the draft still has. It adds no content path — the
/// re-projection runs the identical `project_human` assembly as
/// [`project_human_llm`] (same digit guard, skills-subset rule, canonical
/// structure), so a directive can only reshape presentation. Because the
/// directive is free text derived from client-supplied objection JSON, it runs
/// the full two-stage backstop — `vet_human` then `check_claims` — before
/// returning, and the payload is dash-scrubbed. Returns the re-projected human
/// `VariantPayload` as JSON.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub async fn refine_layout_llm(
    canonical_json: String,
    dataset_json: String,
    jd_json: String,
    objection_json: String,
    models_json: String,
    llm: js_sys::Function,
) -> Result<String, JsValue> {
    let draft: TailoredResume = parse(&canonical_json, "canonical draft").map_err(throw)?;
    let dataset: ResumeDataset = parse(&dataset_json, "dataset").map_err(throw)?;
    let jd: JobRequirements = parse(&jd_json, "job requirements").map_err(throw)?;
    let objection: Objection = parse(&objection_json, "layout objection").map_err(throw)?;
    // Route by scope, the way the CLI does: only a variant-scoped (presentation)
    // objection belongs here. A canonical-scoped content objection reworded away
    // would mask a claim problem the draft still has, so refuse it — it must be
    // addressed on the canonical draft, not the variant.
    if !matches!(
        objection.scope,
        aarg_domain::review::ObjectionScope::VariantOnly(_)
    ) {
        return Err(throw(
            "refine_layout_llm expects a layout-scoped (variant-only) objection; \
             a content objection must be addressed on the canonical draft"
                .to_string(),
        ));
    }
    let models = Models::from_json(&models_json).map_err(throw)?;
    let (client, rx) = BridgeClient::new();
    bridge::spawn_pump(rx, llm);
    let ctx = AgentContext {
        llm: &client,
        model: &models,
        tracer: &Tracer::DISABLED,
        sink: None,
    };
    // The objection's own formatted line is the layout guidance — the same
    // "target (kind, severity): message · try: suggestion" every host feeds the
    // model, here as a presentation directive.
    let directive = aarg_domain::review::format_objection(&objection);
    let run = HumanVariantAgent
        .run(
            &ctx,
            HumanVariantInput {
                draft: draft.clone(),
                summary_locked: dataset.summary_confirmed,
                directive: Some(directive),
            },
        )
        .await
        .map_err(|e| throw(error_chain(&e)))?;
    // Same two-stage backstop as `project_human_llm`: the directive is free text
    // derived from client-supplied objection JSON, so run `vet_human` (catches
    // non-numeric prose inflation) before the `check_claims` lint.
    let (mut payload, _review_usage) =
        aarg_domain::variant::vet_human(&ctx, &draft, run.output, &jd, &dataset)
            .await
            .map_err(|e| throw(error_chain(&e)))?;
    aarg_domain::variant::check_claims(&draft, &payload).map_err(|e| throw(error_chain(&e)))?;
    aarg_domain::variant::scrub_variant_text(&mut payload);
    dump(&payload).map_err(throw)
}

// ---------------------------------------------------------------------
// Interactive exports (wasm-only): the copilots, run over TWO JS callbacks
// ---------------------------------------------------------------------
//
// The copilots are the human-in-the-loop half of the pipeline: the model only
// ever *asks* (a leading question, a suggested rewrite), and the person's own
// words are what land. So each export needs both an `LlmClient` and a
// `UserHandle`, and in a browser both are JS callbacks. Each export builds two
// Send-preserving bridges — a `BridgeClient` over the `llm` callback and a
// `BridgeUser` over the `user` callback — spawns a pump for each, and runs the
// domain copilot with `AnchorStyle::PLAIN` (no terminal styling in a browser).
//
// The copilots MUTATE the dataset in place — that's exactly how the CLI
// persists their work (it saves the dataset after each). A browser has no
// filesystem the crate can reach, so every export that touches the dataset
// returns the updated dataset alongside its result, and the host persists it.
//
// Two exceptions further down this section, both because the underlying
// domain call doesn't touch the dataset: `tune_interactive` (still needs
// both callbacks — a person drives it turn by turn — but only edits the
// canonical draft) and `voice_rewrite` (an LLM-only autonomous rewrite,
// so it takes just the `llm` callback and no `UserHandle` at all).

/// Assemble one interactive export's result JSON, aborted or not.
///
/// Every dataset-mutating copilot folds the person's answers into `dataset`
/// *as the interview runs*, so those facts are already recorded when the
/// interview ends — whether it ran to completion or an `AskError` cut it
/// short. Returning the mutated dataset either way is exactly what
/// never-fabricate wants: the recorded facts are the user's OWN answers
/// already folded in, and discarding them on abort would lose real work, not
/// prevent any invention. Only the outcome *counts* are unknown once the error
/// interrupts the run, so the caller passes them zeroed and this stamps
/// `aborted: true` plus the error's `Display` as `message`; a clean finish
/// passes the real counts and stamps `aborted: false`.
#[cfg(target_arch = "wasm32")]
fn interactive_result(
    dataset: &ResumeDataset,
    before_json: &str,
    counts: serde_json::Value,
    aborted: Option<String>,
) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert(
        "dataset".to_string(),
        serde_json::to_value(dataset).unwrap_or(serde_json::Value::Null),
    );
    // Whether the interview actually changed the dataset, decided HERE — one
    // serializer, one process, the same evolving map instance — because the JS
    // host cannot compare reliably: its GET copy and this module's copy
    // serialize HashMaps (e.g. `skills.aliases`) in different orders, so a
    // stringify-compare over there reads "always changed". `before_json` is the
    // export's own serialization captured right after parse; an untouched
    // dataset re-serializes identically.
    let after = serde_json::to_string(dataset).unwrap_or_default();
    obj.insert(
        "mutated".to_string(),
        serde_json::Value::Bool(!before_json.is_empty() && after != before_json),
    );
    if let serde_json::Value::Object(fields) = counts {
        obj.extend(fields);
    }
    obj.insert(
        "aborted".to_string(),
        serde_json::Value::Bool(aborted.is_some()),
    );
    if let Some(message) = aborted {
        obj.insert("message".to_string(), serde_json::Value::String(message));
    }
    serde_json::Value::Object(obj)
}

/// Interview the person for a real number on each bullet the reviewer flagged
/// as missing one (FR-3.x). `report_json` is an `AdversarialReport`; the
/// `NoMetric` objections targeting a bullet become the interview's targets
/// (the reviewer's suggestion or message rides along as the question's hint).
/// The model only phrases the question — the figure is the person's own, folded
/// onto the bullet's `metric` field, so nothing here can fabricate a number.
///
/// Returns `{ "dataset": ResumeDataset, "added": <count>, "aborted": <bool> }`:
/// the mutated dataset for the host to persist, and how many bullets gained a
/// metric. If the interview is aborted mid-ask, the dataset still carries the
/// numbers already recorded; `added` is 0, `aborted` is true, and a `message`
/// field carries the reason.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub async fn capture_metrics_interactive(
    dataset_json: String,
    report_json: String,
    models_json: String,
    llm: js_sys::Function,
    user: js_sys::Function,
) -> Result<String, JsValue> {
    let mut dataset: ResumeDataset = parse(&dataset_json, "dataset").map_err(throw)?;
    // This module's own serialization of the just-parsed dataset — the
    // "before" side of the `mutated` compare in `interactive_result`.
    let before = serde_json::to_string(&dataset).unwrap_or_default();
    let report: AdversarialReport = parse(&report_json, "adversarial report").map_err(throw)?;
    let models = Models::from_json(&models_json).map_err(throw)?;
    let (client, llm_rx) = BridgeClient::new();
    bridge::spawn_pump(llm_rx, llm);
    let (user_handle, user_rx) = BridgeUser::new();
    bridge::spawn_user_pump(user_rx, user);
    let ctx = AgentContext {
        llm: &client,
        model: &models,
        tracer: &Tracer::DISABLED,
        sink: None,
    };

    // The reviewer's NoMetric objections, keyed to the bullet they target —
    // the same target-building the CLI's tailor command does.
    let targets: Vec<MetricTarget> = report
        .objections
        .iter()
        .filter(|o| o.kind == ObjectionKind::NoMetric)
        .filter_map(|o| match &o.target {
            ObjectionTarget::Bullet(id) => Some(MetricTarget {
                bullet_id: id.clone(),
                hint: o.suggestion.clone().or_else(|| Some(o.message.clone())),
            }),
            _ => None,
        })
        .collect();

    // An `AskError` mid-interview keeps the metrics already recorded (they're
    // the user's own numbers, folded into `dataset` as answered); only the
    // count is lost, so it zeroes and reports `aborted`.
    let json = match aarg_domain::metric::capture_metrics(
        &mut dataset,
        &targets,
        &user_handle,
        &ctx,
        AnchorStyle::PLAIN,
    )
    .await
    {
        Ok(added) => interactive_result(
            &dataset,
            &before,
            serde_json::json!({ "added": added }),
            None,
        ),
        Err(e) => interactive_result(
            &dataset,
            &before,
            serde_json::json!({ "added": 0 }),
            Some(error_chain(&e)),
        ),
    };
    dump(&json).map_err(throw)
}

/// Interview the person to restate, in their own words, each bullet the
/// reviewer flagged as weakly worded (FR-3.x) — vague verbs, unsupported or
/// generic claims, missed JD emphasis. `report_json` is an `AdversarialReport`;
/// its *strengthenable* objections targeting a bullet become the targets. A
/// second agent formats the person's typed facts into a crisp line they approve,
/// fenced by the shared digit guard so it can rephrase but never inflate.
///
/// Returns `{ "dataset": ResumeDataset, "changed": <count>, "aborted": <bool>
/// }`: the mutated dataset for the host to persist, and how many bullets the
/// person rewrote. An aborted interview still returns the rewrites already
/// recorded; `changed` is 0, `aborted` is true, and `message` carries the
/// reason.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub async fn strengthen_interactive(
    dataset_json: String,
    report_json: String,
    models_json: String,
    llm: js_sys::Function,
    user: js_sys::Function,
) -> Result<String, JsValue> {
    let mut dataset: ResumeDataset = parse(&dataset_json, "dataset").map_err(throw)?;
    // This module's own serialization of the just-parsed dataset — the
    // "before" side of the `mutated` compare in `interactive_result`.
    let before = serde_json::to_string(&dataset).unwrap_or_default();
    let report: AdversarialReport = parse(&report_json, "adversarial report").map_err(throw)?;
    let models = Models::from_json(&models_json).map_err(throw)?;
    let (client, llm_rx) = BridgeClient::new();
    bridge::spawn_pump(llm_rx, llm);
    let (user_handle, user_rx) = BridgeUser::new();
    bridge::spawn_user_pump(user_rx, user);
    let ctx = AgentContext {
        llm: &client,
        model: &models,
        tracer: &Tracer::DISABLED,
        sink: None,
    };

    let targets: Vec<StrengthenTarget> = report
        .objections
        .iter()
        .filter(|o| strengthen::is_strengthenable(o.kind))
        .filter_map(|o| match &o.target {
            ObjectionTarget::Bullet(id) => Some(StrengthenTarget {
                bullet_id: id.clone(),
                kind: o.kind,
                concern: o.message.clone(),
            }),
            _ => None,
        })
        .collect();

    // The PRD's 3/3 interview caps (`InterviewLimits::default`) — a browser has
    // no config file to override them from, so the tuned defaults stand.
    // A cut-short interview keeps the bullets the person already rewrote (their
    // own words, already folded into `dataset`); only the count is lost.
    let json = match strengthen::strengthen_bullets(
        &mut dataset,
        &targets,
        &user_handle,
        &ctx,
        strengthen::InterviewLimits::default(),
        AnchorStyle::PLAIN,
    )
    .await
    {
        Ok(changed) => interactive_result(
            &dataset,
            &before,
            serde_json::json!({ "changed": changed }),
            None,
        ),
        Err(e) => interactive_result(
            &dataset,
            &before,
            serde_json::json!({ "changed": 0 }),
            Some(error_chain(&e)),
        ),
    };
    dump(&json).map_err(throw)
}

/// Refine the resume summary the reviewer flagged (FR-3.x): draft a stronger
/// summary grounded only in the person's recorded history, let them use / tweak
/// / write their own / skip, and on acceptance record it as their confirmed
/// summary so tailoring and the human variant use it verbatim. `concern` is the
/// reviewer's summary-objection message; the digit guard and no-new-facts prompt
/// keep every draft honest.
///
/// Returns `{ "dataset": ResumeDataset, "changed": <bool>, "aborted": <bool>
/// }`: the mutated dataset for the host to persist, and whether the summary
/// changed. An aborted interview keeps any summary confirmed on an earlier
/// turn; `changed` is false, `aborted` is true, and `message` carries the
/// reason.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub async fn refine_summary_interactive(
    dataset_json: String,
    concern: String,
    models_json: String,
    llm: js_sys::Function,
    user: js_sys::Function,
) -> Result<String, JsValue> {
    let mut dataset: ResumeDataset = parse(&dataset_json, "dataset").map_err(throw)?;
    // This module's own serialization of the just-parsed dataset — the
    // "before" side of the `mutated` compare in `interactive_result`.
    let before = serde_json::to_string(&dataset).unwrap_or_default();
    let models = Models::from_json(&models_json).map_err(throw)?;
    let (client, llm_rx) = BridgeClient::new();
    bridge::spawn_pump(llm_rx, llm);
    let (user_handle, user_rx) = BridgeUser::new();
    bridge::spawn_user_pump(user_rx, user);
    let ctx = AgentContext {
        llm: &client,
        model: &models,
        tracer: &Tracer::DISABLED,
        sink: None,
    };

    // The PRD's default revision cap (`InterviewLimits::default().revises`),
    // since a browser has no config file to tune it from.
    let max_revises = strengthen::InterviewLimits::default().revises;
    // If the person aborts before accepting, a summary they'd already confirmed
    // in an earlier turn stays on `dataset`; `changed` is a bool, so its zeroed
    // form is `false`.
    let json = match aarg_domain::summary::refine_summary(
        &mut dataset,
        &concern,
        &user_handle,
        &ctx,
        max_revises,
    )
    .await
    {
        Ok(changed) => interactive_result(
            &dataset,
            &before,
            serde_json::json!({ "changed": changed }),
            None,
        ),
        Err(e) => interactive_result(
            &dataset,
            &before,
            serde_json::json!({ "changed": false }),
            Some(error_chain(&e)),
        ),
    };
    dump(&json).map_err(throw)
}

/// Enrich the person's thin work-history roles (the history copilot): for each
/// role with only a line or two recorded, a small agent asks a few leading
/// questions and each answer the person types becomes a new bullet in their own
/// words. JD-agnostic on purpose — this captures history as it was, not bent to
/// a posting. The targets are the dataset's own thin roles (`enrich::thin_roles`).
///
/// Returns `{ "dataset": ResumeDataset, "bullets_added": <count>,
/// "roles_touched": <count>, "aborted": <bool> }`: the mutated dataset for the
/// host to persist, and what the session accomplished. An aborted interview
/// still returns the bullets already typed; both counts are 0, `aborted` is
/// true, and `message` carries the reason.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub async fn enrich_roles_interactive(
    dataset_json: String,
    models_json: String,
    llm: js_sys::Function,
    user: js_sys::Function,
) -> Result<String, JsValue> {
    let mut dataset: ResumeDataset = parse(&dataset_json, "dataset").map_err(throw)?;
    // This module's own serialization of the just-parsed dataset — the
    // "before" side of the `mutated` compare in `interactive_result`.
    let before = serde_json::to_string(&dataset).unwrap_or_default();
    let models = Models::from_json(&models_json).map_err(throw)?;
    let (client, llm_rx) = BridgeClient::new();
    bridge::spawn_pump(llm_rx, llm);
    let (user_handle, user_rx) = BridgeUser::new();
    bridge::spawn_user_pump(user_rx, user);
    let ctx = AgentContext {
        llm: &client,
        model: &models,
        tracer: &Tracer::DISABLED,
        sink: None,
    };

    let targets = aarg_domain::enrich::thin_roles(&dataset);
    // A cut-short interview keeps the bullets the person already typed (their
    // own words, folded into `dataset` per role); only the tallies are lost.
    let json =
        match aarg_domain::enrich::enrich_roles(&mut dataset, &targets, &user_handle, &ctx).await {
            Ok(outcome) => interactive_result(
                &dataset,
                &before,
                serde_json::json!({
                    "bullets_added": outcome.bullets_added,
                    "roles_touched": outcome.roles_touched,
                }),
                None,
            ),
            Err(e) => interactive_result(
                &dataset,
                &before,
                serde_json::json!({ "bullets_added": 0, "roles_touched": 0 }),
                Some(error_chain(&e)),
            ),
        };
    dump(&json).map_err(throw)
}

/// Run the conversational "tune" session over a finished canonical draft —
/// the plain-language counterpart to the objection menu (FR-3.6's tone half,
/// plus pure bullet removal): "drop the intern bullet", "make the summary
/// read more conversational". Mirrors what the CLI's `aarg tune` does over
/// its terminal `UserHandle` (`src/commands/tune.rs`), minus the filesystem
/// load/save and the re-render, which stay host-side.
///
/// `aarg_domain::tune::run_session` drives the whole back-and-forth itself:
/// it asks the opening "want to change anything?" confirm, then loops
/// asking `Question::Text` for the next free-text request until a blank
/// line or a declined offer ends it. So this export takes **no separate
/// per-request argument** — the JS side answers that same loop by resolving
/// the `user` callback once per turn (one scripted request, or whatever a
/// person types into a chat box next), exactly the way `InteractiveUser`
/// answers it in a terminal. Every request is routed (`tune::classify`)
/// onto one of three grounded operations — pure bullet removal, or the
/// digit-guarded voice rewrite for a tone change; a request outside those
/// (a new fact, number, or skill) is reported unsupported and changes
/// nothing — so nothing here can introduce a claim. What happened is
/// reported back through `user.notify` in the same "✓ removed…" / "ℹ
/// nothing to change" vocabulary the CLI prints (`SessionStyle::PLAIN`,
/// since a browser has no terminal to style for).
///
/// `dataset_json` supplies the person's voice samples
/// (`dataset.voice_samples[].text`) for the tone operation to anchor to.
/// The session never mutates the dataset (it only edits the draft in
/// memory), so — unlike the other interactive exports — the dataset is not
/// part of the return.
///
/// Before returning, the draft is scrubbed of AI-tell em/en dashes
/// (`scrub_resume_text`), the same finalize step every other draft-returning
/// export applies.
///
/// Returns `{ "resume": TailoredResume, "changed": <bool>, "usage":
/// {"input_tokens","output_tokens"} }`: the (possibly edited) scrubbed
/// canonical draft, whether it actually changed (so the host knows to
/// re-render and re-score, matching `TuneOutcome::changed_draft`), and the
/// session's total token cost across every request it handled.
///
/// Unlike the dataset-mutating copilots this export carries **no `aborted`
/// field**: `run_session` returns `(bool, TokenUsage)`, never an `AskError`.
/// It ends a session the ordinary way — a declined opening confirm or a blank
/// request line — which reads as "changed nothing", not as an interrupted run
/// with recorded work to preserve. There is no partial-outcome to salvage, so
/// there is nothing for an abort sentinel to guard here.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub async fn tune_interactive(
    canonical_json: String,
    dataset_json: String,
    models_json: String,
    llm: js_sys::Function,
    user: js_sys::Function,
) -> Result<String, JsValue> {
    let mut canonical: TailoredResume = parse(&canonical_json, "canonical draft").map_err(throw)?;
    let dataset: ResumeDataset = parse(&dataset_json, "dataset").map_err(throw)?;
    let models = Models::from_json(&models_json).map_err(throw)?;
    let (client, llm_rx) = BridgeClient::new();
    bridge::spawn_pump(llm_rx, llm);
    let (user_handle, user_rx) = BridgeUser::new();
    bridge::spawn_user_pump(user_rx, user);
    let ctx = AgentContext {
        llm: &client,
        model: &models,
        tracer: &Tracer::DISABLED,
        sink: None,
    };

    let samples: Vec<String> = dataset
        .voice_samples
        .iter()
        .map(|s| s.text.clone())
        .collect();
    let (changed, usage) = aarg_domain::tune::run_session(
        &ctx,
        &mut canonical,
        &user_handle,
        &samples,
        aarg_domain::tune::SessionStyle::PLAIN,
    )
    .await;

    aarg_domain::tailor::scrub_resume_text(&mut canonical);
    dump(&serde_json::json!({
        "resume": canonical,
        "changed": changed,
        "usage": usage,
    }))
    .map_err(throw)
}

/// Triage the JD keywords the dataset can't yet back — the evidence half of
/// FR-3.1's `verify` flows, the flow `aarg tailor`'s inline pivot runs
/// (`src/commands/tailor.rs`, `unbacked_keywords` + `verify_keywords`): the
/// person checks the job keywords they genuinely have off a multi-select
/// checklist, each checked keyword gets the role-plus-evidence interview
/// (which role shows it, roughly how many years, an optional one-sentence
/// description) and becomes a recorded, verified skill backed by that role;
/// anything left unchecked is remembered in
/// `dataset.metadata.declined_skills` so it isn't offered again next run. A
/// `guide` clarification conversation ("I'm not sure what this is — explain
/// it") is always offered mid-interview, since this binding always has an
/// LLM to run it against — matching the CLI's interactive path, which does
/// the same whenever a client is configured.
///
/// `jd_json`/`gap_json` are the already-parsed `JobRequirements` and
/// `GapReport` (run `parse_jd_llm` and `analyze_gap_llm` first, the way the
/// CLI's tailor command does before offering the checklist); the candidate
/// list itself (`verify::unbacked_keywords`) is computed here from them plus
/// the dataset — the same deterministic gather, so it excludes JD phrases a
/// recorded skill already covers and keywords already declined.
///
/// Adds no content path: every recorded skill traces to a role the user
/// picked, and any typed evidence sentence is either the user's verbatim
/// words or a guide-polished rewrite of them, fact-guarded the same way
/// every other evidence flow is (`strengthen::polish`'s digit guard) —
/// nothing here pre-fills a claim.
///
/// Returns `{ "dataset": ResumeDataset, "verified": <count>, "removed":
/// <count>, "skipped": <count>, "bullets_added": <count>, "declined":
/// <count>, "aborted": <bool> }`: the mutated dataset for the host to persist
/// (both the newly recorded skills and the declined-keyword list live in it),
/// and `VerifyOutcome`'s tallies of what the session accomplished. An aborted
/// interview still returns the skills already verified; every count is 0,
/// `aborted` is true, and `message` carries the reason.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub async fn verify_skills_interactive(
    dataset_json: String,
    jd_json: String,
    gap_json: String,
    models_json: String,
    llm: js_sys::Function,
    user: js_sys::Function,
) -> Result<String, JsValue> {
    let mut dataset: ResumeDataset = parse(&dataset_json, "dataset").map_err(throw)?;
    // This module's own serialization of the just-parsed dataset — the
    // "before" side of the `mutated` compare in `interactive_result`.
    let before = serde_json::to_string(&dataset).unwrap_or_default();
    let jd: JobRequirements = parse(&jd_json, "job requirements").map_err(throw)?;
    let gap: GapReport = parse(&gap_json, "gap report").map_err(throw)?;
    let models = Models::from_json(&models_json).map_err(throw)?;
    let (client, llm_rx) = BridgeClient::new();
    bridge::spawn_pump(llm_rx, llm);
    let (user_handle, user_rx) = BridgeUser::new();
    bridge::spawn_user_pump(user_rx, user);
    let ctx = AgentContext {
        llm: &client,
        model: &models,
        tracer: &Tracer::DISABLED,
        sink: None,
    };

    let candidates = aarg_domain::verify::unbacked_keywords(&dataset, &jd, &gap);
    // A cut-short checklist keeps the skills already verified and the keywords
    // already declined (both live on `dataset`, recorded as answered); only the
    // tallies are lost.
    let json = match aarg_domain::verify::verify_keywords(
        &mut dataset,
        &candidates,
        &user_handle,
        Some(&ctx),
    )
    .await
    {
        Ok(outcome) => interactive_result(&dataset, &before, verify_counts(&outcome), None),
        Err(e) => interactive_result(
            &dataset,
            &before,
            verify_counts_zeroed(),
            Some(error_chain(&e)),
        ),
    };
    dump(&json).map_err(throw)
}

/// The `VerifyOutcome`'s five tallies as a JSON object — shared by the batch
/// and single-keyword verify exports, which return the same shape.
#[cfg(target_arch = "wasm32")]
fn verify_counts(outcome: &aarg_domain::verify::VerifyOutcome) -> serde_json::Value {
    serde_json::json!({
        "verified": outcome.verified,
        "removed": outcome.removed,
        "skipped": outcome.skipped,
        "bullets_added": outcome.bullets_added,
        "declined": outcome.declined,
    })
}

/// The same five tallies zeroed, for an aborted verify interview.
#[cfg(target_arch = "wasm32")]
fn verify_counts_zeroed() -> serde_json::Value {
    serde_json::json!({
        "verified": 0,
        "removed": 0,
        "skipped": 0,
        "bullets_added": 0,
        "declined": 0,
    })
}

/// Verify a **single** JD requirement the user clicked (the coverage map's
/// per-row "Fill the gap" / "Strengthen"), rather than the whole-gap checklist
/// [`verify_skills_interactive`] offers. Builds one `KeywordCandidate` for
/// exactly `keyword` — its category taken from the JD skill of the same name so
/// the evidence interview reads right — and re-offers it even if
/// `unbacked_keywords` would have dropped it (already declined, or collapsed
/// into a broader recorded skill): the user explicitly asked to fill *this*
/// gap. Same `{dataset, verified, …, aborted}` shape as the batch export,
/// abort semantics included.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub async fn verify_skill_interactive(
    dataset_json: String,
    jd_json: String,
    keyword: String,
    models_json: String,
    llm: js_sys::Function,
    user: js_sys::Function,
) -> Result<String, JsValue> {
    let mut dataset: ResumeDataset = parse(&dataset_json, "dataset").map_err(throw)?;
    // This module's own serialization of the just-parsed dataset — the
    // "before" side of the `mutated` compare in `interactive_result`.
    let before = serde_json::to_string(&dataset).unwrap_or_default();
    let jd: JobRequirements = parse(&jd_json, "job requirements").map_err(throw)?;
    let models = Models::from_json(&models_json).map_err(throw)?;
    let (client, llm_rx) = BridgeClient::new();
    bridge::spawn_pump(llm_rx, llm);
    let (user_handle, user_rx) = BridgeUser::new();
    bridge::spawn_user_pump(user_rx, user);
    let ctx = AgentContext {
        llm: &client,
        model: &models,
        tracer: &Tracer::DISABLED,
        sink: None,
    };

    let key = aarg_domain::keywords::keyword_key(&keyword);
    let category = jd
        .required_skills
        .iter()
        .chain(jd.preferred_skills.iter())
        .find(|s| aarg_domain::keywords::keyword_key(&s.name) == key)
        .map(|s| s.category)
        .unwrap_or(aarg_domain::dataset::types::SkillCategory::Domain);
    let candidate = aarg_domain::verify::KeywordCandidate {
        name: keyword,
        category,
    };

    // Same abort semantics as the batch export: a skill verified before an
    // abort stays recorded on `dataset`; only the tallies are lost.
    let json = match aarg_domain::verify::verify_keywords(
        &mut dataset,
        std::slice::from_ref(&candidate),
        &user_handle,
        Some(&ctx),
    )
    .await
    {
        Ok(outcome) => interactive_result(&dataset, &before, verify_counts(&outcome), None),
        Err(e) => interactive_result(
            &dataset,
            &before,
            verify_counts_zeroed(),
            Some(error_chain(&e)),
        ),
    };
    dump(&json).map_err(throw)
}

/// Voice-anchored rewrite of a canonical draft (FR-3.6's autonomous half,
/// `aarg_domain::voice::rewrite_to_voice`): flag every line that reads like
/// generic AI prose (a cliché-deny-list hit, or a raw un-bullet-like line —
/// see `voice::flagged_lines`) and rewrite it toward the person's own
/// writing samples. LLM-only — unlike every other export in this section,
/// nothing here asks the person anything, so it takes just the `llm`
/// callback and no `UserHandle` at all.
///
/// Every rewrite runs through the same digit-guard tailoring uses (`
/// digit_runs`): a candidate rewrite that gains a number the source line
/// didn't have is reverted rather than kept, so voice can change phrasing
/// but never invent a figure. The guard runs inside `rewrite_to_voice`
/// itself — this binding doesn't add or relax it.
///
/// `samples_json` is a plain JSON array of strings (`dataset.voice_samples[
/// ].text`), matching `rewrite_to_voice`'s own `samples: &[String]`
/// parameter directly — the host already holds the dataset and can project
/// that one field out of it, so this binding doesn't need (and doesn't
/// take) the whole dataset just to read it.
///
/// Before returning, the draft is scrubbed of AI-tell em/en dashes
/// (`scrub_resume_text`), the same finalize step every other draft-returning
/// export applies.
///
/// Returns `{ "resume": TailoredResume, "rewritten": <count>, "reverted":
/// <count>, "usage": {"input_tokens","output_tokens"} }`: the scrubbed
/// draft, how many lines actually changed, how many candidate rewrites the
/// digit guard discarded, and the call's token cost. A draft with nothing
/// flagged costs no model call and returns unchanged (`rewritten` and
/// `reverted` both 0).
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub async fn voice_rewrite(
    canonical_json: String,
    samples_json: String,
    models_json: String,
    llm: js_sys::Function,
) -> Result<String, JsValue> {
    let draft: TailoredResume = parse(&canonical_json, "canonical draft").map_err(throw)?;
    let samples: Vec<String> = parse(&samples_json, "voice samples").map_err(throw)?;
    let models = Models::from_json(&models_json).map_err(throw)?;
    let (client, llm_rx) = BridgeClient::new();
    bridge::spawn_pump(llm_rx, llm);
    let ctx = AgentContext {
        llm: &client,
        model: &models,
        tracer: &Tracer::DISABLED,
        sink: None,
    };

    let (mut voiced, stats) = aarg_domain::voice::rewrite_to_voice(&ctx, &draft, &samples)
        .await
        .map_err(|e| throw(error_chain(&e)))?;
    aarg_domain::tailor::scrub_resume_text(&mut voiced);

    dump(&serde_json::json!({
        "resume": voiced,
        "rewritten": stats.rewritten,
        "reverted": stats.reverted,
        "usage": stats.usage,
    }))
    .map_err(throw)
}

// ---------------------------------------------------------------------
// The adversarial revision loop (wasm-only): the Evaluator's 2nd consumer
// ---------------------------------------------------------------------

/// Cooperative cancel flag for the (single) in-flight tailor loop. wasm is
/// single-threaded and the browser runs at most one loop at a time, so one
/// static is sound. Its lifecycle is **JS-owned**: the host arms a fresh run
/// by calling `reset_tailor_loop_cancel` when the run *begins* (before gap
/// analysis), not when `tailor_loop` starts. That is what lets a Stop pressed
/// at any point of the run — even during the pre-loop gap analysis, before
/// `tailor_loop` is even called — survive to the loop's first check instead of
/// being wiped by an entry reset.
#[cfg(target_arch = "wasm32")]
static LOOP_CANCELLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Ask the in-flight `tailor_loop` to stop after its current pass.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn cancel_tailor_loop() {
    LOOP_CANCELLED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Arm a fresh cancellable run: clear any stale stop request. The JS host
/// calls this when the run *begins* (before gap analysis), so a Stop pressed
/// at any point of the run — even before `tailor_loop` starts — survives to
/// the loop's first check instead of being wiped by an entry reset.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn reset_tailor_loop_cancel() {
    LOOP_CANCELLED.store(false, std::sync::atomic::Ordering::Relaxed);
}

/// The headless evaluator for the browser loop: it scores a draft on the
/// reviewer's verdict ALONE. The native binary's evaluator also renders the
/// draft to a PDF with typst, reads its text back, and runs the deterministic
/// ATS coverage check, then blends that into the score — none of which exists
/// in wasm (no typst, no pdfium). So this is the honest half a browser can run:
/// the returned score is the review score, and it will differ from the CLI's
/// combined content+coverage score. `Extra = ()` (there's no `AtsReport` to
/// carry back).
#[cfg(target_arch = "wasm32")]
struct ReviewOnlyEvaluator;

#[cfg(target_arch = "wasm32")]
#[async_trait::async_trait]
impl Evaluator for ReviewOnlyEvaluator {
    type Extra = ();
    type Error = ReviewError;

    async fn evaluate(
        &self,
        ctx: &AgentContext<'_>,
        _iteration: usize,
        resume: TailoredResume,
        jd: &JobRequirements,
        dataset: &ResumeDataset,
        _gap: &GapReport,
    ) -> Result<Evaluation<()>, ReviewError> {
        // Run the reviewer agent directly (not the `review_draft` wrapper) so
        // its token usage travels back on the `Evaluation`.
        let input = ReviewInput {
            draft: resume.clone(),
            jd: jd.clone(),
            dataset: dataset.clone(),
        };
        let run = AdversarialReviewerAgent.run(ctx, input).await?;
        let report = run.output;
        // Score from the *full* report — accepting an objection stops the
        // churn, it must not inflate the honest assessment (the CLI evaluator
        // scores the full report the same way).
        let score = report.overall_score;
        // Then drop the objections the user already dismissed, exactly as the
        // CLI evaluator does (`without_dismissed`), so the loop's stop
        // conditions (`has_blocking_or_major`) and each revision prompt see the
        // filtered set — a browser build never re-litigates an accepted
        // objection.
        let report = report.without_dismissed(&dataset.metadata.dismissed_objections);
        Ok(Evaluation {
            resume,
            report,
            score,
            review_usage: run.usage,
            extra: (),
        })
    }
}

/// The loop's host for the browser: it formats each objection into one plain
/// revision-prompt line, counts revision passes, AND streams a small JSON
/// progress event on every loop milestone so a UI can show a live iteration
/// list and cost ticker (and react to a Stop button — see the cancellation note
/// on [`tailor_loop`]).
///
/// Progress crosses to JS the same Send-preserving way the LLM bridge does: a
/// `LoopObserver` must be `Send + Sync`, so it can't hold the `!Send`
/// `js_sys::Function`. Instead it holds only a `Send` channel sender; each hook
/// serializes its event and queues it (non-blocking, no await), and a separate
/// `spawn_local` pump (`spawn_progress_pump`) owns the callback and calls it as
/// events arrive — draining them while the loop is parked awaiting the model.
///
/// Each event is `{ "phase", "iteration"?, "score"?, "usage"?, "message" }`;
/// `phase` is one of `revising` / `revision_drafted` / `evaluated` /
/// `no_improvement` (plus `drafting` / `done`, emitted by the export itself for
/// the iteration-0 milestones the loop's hooks don't cover).
#[cfg(target_arch = "wasm32")]
struct WasmProgressObserver {
    passes: std::sync::atomic::AtomicUsize,
    progress: futures_channel::mpsc::UnboundedSender<String>,
}

#[cfg(target_arch = "wasm32")]
impl WasmProgressObserver {
    /// Queue one progress event for the JS callback. A closed channel (the pump
    /// went away) is ignored — progress is best-effort narration, never
    /// load-bearing to the loop's result.
    fn emit(&self, event: serde_json::Value) {
        if let Ok(text) = serde_json::to_string(&event) {
            let _ = self.progress.unbounded_send(text);
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl LoopObserver<()> for WasmProgressObserver {
    fn objection_line(&self, objection: &Objection) -> String {
        // The `target_label` prefix ("bullet-3", "summary", ...) is not
        // decoration — the revision pass doesn't hand the model the prior
        // draft, so it's the *only* way a revision prompt tells the model
        // which line an objection is about. `review::format_objection`
        // is the one place both hosts (this loop and the CLI's
        // `CliLoopObserver`) build that line, so a revision prompt reads
        // identically wherever the loop runs.
        aarg_domain::review::format_objection(objection)
    }

    fn revising(&self, iteration: usize, objections: usize) {
        self.emit(serde_json::json!({
            "phase": "revising",
            "iteration": iteration,
            "message": format!("revising to address {objections} objection(s)"),
        }));
    }

    fn revision_drafted(&self, iteration: usize, usage: &TokenUsage) {
        // Carry the revision draft call's own tokens so the live-cost ticker
        // meters every model call, not only the reviews. The running invariant
        // the UI accumulates: initial draft (`drafting`) + initial review
        // (iteration-0 `evaluated`) + each revision draft (this event) + each
        // revision review (`evaluated`) = the final `usage` total.
        self.emit(serde_json::json!({
            "phase": "revision_drafted",
            "iteration": iteration,
            "usage": {
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
            },
            "message": "revision drafted; scoring it",
        }));
    }

    fn evaluated(&self, iteration: usize, eval: &Evaluation<()>) {
        // Record the highest pass number that produced a scored draft.
        self.passes
            .store(iteration, std::sync::atomic::Ordering::Relaxed);
        self.emit(serde_json::json!({
            "phase": "evaluated",
            "iteration": iteration,
            "score": eval.score,
            "usage": {
                "input_tokens": eval.review_usage.input_tokens,
                "output_tokens": eval.review_usage.output_tokens,
            },
            "message": format!("scored {:.2}", eval.score),
        }));
    }

    fn no_improvement(&self) {
        self.emit(serde_json::json!({
            "phase": "no_improvement",
            "message": "the revision didn't improve; keeping the best draft",
        }));
    }

    fn should_continue(&self) -> bool {
        // The loop checks this at the top of each pass; a Stop button that set
        // the cancel flag ends the loop between passes (the in-flight pass, if
        // any, still finishes and its best draft is returned).
        !LOOP_CANCELLED.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Spawn the progress pump: the task that owns the JS `on_progress` callback and
/// forwards each queued progress event to it. Like the LLM and user pumps this
/// is the only place the `!Send` callback is touched, and it's a `spawn_local`
/// task, so no `Send` bound applies. It runs whenever the loop is parked
/// awaiting the model (single-threaded cooperative scheduling), draining events
/// as they queue. When the observer (and its sender) drop, `rx.next()` yields
/// `None` and the loop ends.
#[cfg(target_arch = "wasm32")]
fn spawn_progress_pump(
    mut rx: futures_channel::mpsc::UnboundedReceiver<String>,
    callback: js_sys::Function,
) {
    use futures_util::StreamExt;
    wasm_bindgen_futures::spawn_local(async move {
        while let Some(event) = rx.next().await {
            let _ = callback.call1(&JsValue::NULL, &JsValue::from_str(&event));
        }
    });
}

/// The lenient wire shape for the loop's bounds: both keys optional, so a host
/// can pass `{}` (or a subset) and take the CLI's defaults.
#[cfg(target_arch = "wasm32")]
#[derive(serde::Deserialize)]
struct RawLoopParams {
    #[serde(default)]
    revisions: Option<usize>,
    #[serde(default)]
    acceptable_score: Option<f32>,
}

/// Run the full honest adversarial revision loop (PRD §6.4) end to end: tailor a
/// first draft, score it, then up to `revisions` times ask the model to address
/// the best draft's objections, re-score, and keep a revision only when it beat
/// the best — returning the *best* draft seen, never merely the last.
///
/// `params_json` sets the loop's bounds `{ "revisions": 2, "acceptable_score":
/// 0.85 }` (both optional; those are the CLI defaults, and `revisions = 2`
/// matches it). The draft is scored by [`ReviewOnlyEvaluator`], so **the
/// returned `score` is the reviewer's verdict alone** — the CLI blends in a
/// typst-rendered ATS coverage term that a browser can't compute, so its
/// combined score will differ.
///
/// Before returning, the best draft is scrubbed of AI-tell em/en dashes
/// (`scrub_resume_text`, punctuation-only), the same finalize step
/// [`tailor_draft`] applies, so nothing crosses the boundary unscrubbed.
///
/// Returns a JSON object:
/// - `resume`: the scrubbed best `TailoredResume`.
/// - `warnings`: the never-fabricate guard warnings from the *initial* draft
///   (the loop's internal revisions don't surface their own through the domain
///   API, so these reflect iteration 0).
/// - `dropped_unrecorded`: cleaned skill names the model proposed on the initial
///   draft that no recorded, evidence-backed skill could support.
/// - `score`: the best draft's review-only score (see the caveat above).
/// - `report`: the best draft's `AdversarialReport`.
/// - `iterations`: how many revision passes were scored (0 if the first draft
///   was already good enough or had nothing major to fix).
/// - `usage`: the flow's total token cost as `{ "input_tokens", "output_tokens"
///   }` (`TokenUsage`'s serde shape) — the initial tailor call, the initial
///   review call, and every revision pass's tailor and review calls, summed.
///   As accurate as the values in scope allow: it's a true total of every
///   model call this export itself made, not an estimate.
///
/// `on_progress` is a JS callback invoked with a small JSON progress event on
/// every loop milestone (`{ "phase", "iteration"?, "score"?, "usage"?,
/// "message" }` — see [`WasmProgressObserver`]), so a UI can render a live
/// iteration list and cost ticker instead of waiting for the single final
/// result. Events are best-effort narration: a throwing callback never affects
/// the loop's outcome. Every model call carries its `usage` on an event — the
/// `drafting` (initial tailor) and `evaluated` (initial + revision reviews)
/// and `revision_drafted` (revision tailors) phases — so a ticker that sums
/// each event's `usage` arrives at exactly the final `usage` total below.
///
/// **Cancellation stops between passes.** [`cancel_tailor_loop`] sets a module
/// cancel flag that `WasmProgressObserver::should_continue` reads; the domain
/// loop checks it at the top of each iteration, so a Stop button ends the loop
/// *before* the next revision spends tokens and still returns the best draft in
/// hand. It cannot interrupt an in-flight model call — the current pass always
/// completes — so the stop takes effect at the next pass boundary.
///
/// The flag's lifecycle is **JS-owned**: the host arms a run by calling
/// [`reset_tailor_loop_cancel`] when the run *begins* (before gap analysis),
/// **not** here on entry. That is deliberate — a Stop pressed during the
/// pre-loop phase (gap analysis, before `tailor_loop` is even called) must
/// survive to the loop's first check rather than be wiped by an entry reset.
/// A stop already latched before the loop begins therefore skips every
/// revision after the initial draft+review: the early-skip check below (after
/// iteration 0, before `run_loop`) handles that path. This export reports the
/// flag's final state as `"cancelled"` in its result.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub async fn tailor_loop(
    dataset_json: String,
    jd_json: String,
    gap_json: String,
    params_json: String,
    models_json: String,
    llm: js_sys::Function,
    on_progress: js_sys::Function,
) -> Result<String, JsValue> {
    // The cancel flag is NOT reset here. The JS host arms the run via
    // `reset_tailor_loop_cancel` when it *begins* (before gap analysis), so a
    // Stop pressed during the pre-loop phase survives to the checks below
    // instead of being wiped by an entry reset. See this export's doc comment.
    let dataset: ResumeDataset = parse(&dataset_json, "dataset").map_err(throw)?;
    let jd: JobRequirements = parse(&jd_json, "job requirements").map_err(throw)?;
    let gap: GapReport = parse(&gap_json, "gap report").map_err(throw)?;
    let models = Models::from_json(&models_json).map_err(throw)?;

    // Lenient params: a blank string is `{}`, taking every default.
    let raw: RawLoopParams = if params_json.trim().is_empty() {
        RawLoopParams {
            revisions: None,
            acceptable_score: None,
        }
    } else {
        parse(&params_json, "loop params").map_err(throw)?
    };
    let limits = LoopLimits {
        revisions: raw.revisions.unwrap_or(2),
        acceptable_score: raw.acceptable_score.unwrap_or(0.85),
    };

    let (client, llm_rx) = BridgeClient::new();
    bridge::spawn_pump(llm_rx, llm);
    let ctx = AgentContext {
        llm: &client,
        model: &models,
        tracer: &Tracer::DISABLED,
        sink: None,
    };

    // Iteration 0: the first draft, and the guard warnings it produced.
    let initial = aarg_domain::tailor::tailor_resume(
        &ctx,
        BuildId("wasm".to_string()),
        JdId("wasm".to_string()),
        &jd,
        &dataset,
        &gap,
        None,
    )
    .await
    .map_err(|e| throw(error_chain(&e)))?;
    // Move each field out separately — the initial draft feeds the evaluator,
    // its warnings and dropped skills are reported alongside the best draft.
    // `usage` is `Copy`, so pulling it out here doesn't disturb the other
    // moves; it's the initial tailor call's token cost, folded into the
    // flow-wide total returned below.
    let warnings = initial.warnings;
    let dropped_unrecorded = initial.dropped_unrecorded;
    let initial_resume = initial.resume;
    let initial_tailor_usage = initial.usage;

    let evaluator = ReviewOnlyEvaluator;
    // The progress pump: a `spawn_local` task owns `on_progress` and forwards
    // each queued event to it. The observer holds only the `Send` sender, so it
    // still satisfies `LoopObserver: Send + Sync`; the pump drains events while
    // the loop is parked awaiting the model.
    let (progress_tx, progress_rx) = futures_channel::mpsc::unbounded();
    spawn_progress_pump(progress_rx, on_progress);
    let observer = WasmProgressObserver {
        passes: std::sync::atomic::AtomicUsize::new(0),
        progress: progress_tx,
    };
    // The initial TAILOR draft's own tokens stream here (the loop's hooks only
    // cover the revision calls), so the ticker's accumulation is complete:
    // initial draft (this event) + initial review (iteration-0 `evaluated`) +
    // each revision draft (`revision_drafted`) + each revision review
    // (`evaluated`) = the flow-wide `usage` total returned below.
    observer.emit(serde_json::json!({
        "phase": "drafting",
        "iteration": 0,
        "usage": {
            "input_tokens": initial_tailor_usage.input_tokens,
            "output_tokens": initial_tailor_usage.output_tokens,
        },
        "message": "first draft ready; scoring it",
    }));

    // Score the first draft, then drive the loop from it.
    let initial_eval = evaluator
        .evaluate(&ctx, 0, initial_resume, &jd, &dataset, &gap)
        .await
        .map_err(|e| throw(error_chain(&e)))?;
    // Also `Copy`: the initial review call's token cost, taken before
    // `initial_eval` moves into `run_loop` below.
    let initial_review_usage = initial_eval.review_usage;
    // The iteration-0 score, so the UI's ticker starts before any revision.
    // `score` is `Copy`, so reading it here doesn't disturb the move into
    // `run_loop`.
    observer.emit(serde_json::json!({
        "phase": "evaluated",
        "iteration": 0,
        "score": initial_eval.score,
        "usage": {
            "input_tokens": initial_review_usage.input_tokens,
            "output_tokens": initial_review_usage.output_tokens,
        },
        "message": format!("scored {:.2}", initial_eval.score),
    }));
    // A Stop pressed during the initial draft+review is honored here, before
    // the loop spends a token: skip revisions and take the first draft as the
    // best. (`run_loop` also checks between passes, so a later Stop lands there.)
    let outcome = if LOOP_CANCELLED.load(std::sync::atomic::Ordering::Relaxed) {
        aarg_domain::tailor::LoopOutcome {
            best: initial_eval,
            usage: aarg_domain::llm::TokenUsage::default(),
        }
    } else {
        aarg_domain::tailor::run_loop(
            &ctx,
            &evaluator,
            &observer,
            limits,
            BuildId("wasm".to_string()),
            JdId("wasm".to_string()),
            &jd,
            &dataset,
            &gap,
            initial_eval,
        )
        .await
        .map_err(|e| throw(error_chain(&e)))?
    };

    let mut best = outcome.best;
    // Punctuation-only finalize, the same one `tailor_draft` applies.
    aarg_domain::tailor::scrub_resume_text(&mut best.resume);
    let iterations = observer.passes.load(std::sync::atomic::Ordering::Relaxed);
    observer.emit(serde_json::json!({
        "phase": "done",
        "iterations": iterations,
        "score": best.score,
        "message": "loop complete; returning the best draft",
    }));

    // The flow's total token cost: the initial tailor call, the initial
    // review call, and every revision pass's tailor+review calls
    // (`outcome.usage`, which `run_loop` already accumulates across
    // iterations — see its own `add_usage` calls). Surfaced so a caller can
    // show the flow's cost the way the CLI's `StreamReporter` does.
    let usage = serde_json::json!({
        "input_tokens": initial_tailor_usage.input_tokens
            + initial_review_usage.input_tokens
            + outcome.usage.input_tokens,
        "output_tokens": initial_tailor_usage.output_tokens
            + initial_review_usage.output_tokens
            + outcome.usage.output_tokens,
    });

    // The flag's final state: `true` if the user hit Stop at any point during
    // this run. It means a stop was *requested*, not that work was necessarily
    // skipped — a Stop pressed after the loop had already settled on its best
    // draft changes nothing about the result. The best draft is still returned;
    // a caller can note that a stop was requested.
    let cancelled = LOOP_CANCELLED.load(std::sync::atomic::Ordering::Relaxed);

    dump(&serde_json::json!({
        "resume": best.resume,
        "warnings": warnings,
        "dropped_unrecorded": dropped_unrecorded,
        "score": best.score,
        "report": best.report,
        "iterations": iterations,
        "usage": usage,
        "cancelled": cancelled,
    }))
    .map_err(throw)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use aarg_domain::dataset::types::{
        Contact, EvidenceRef, Proficiency, RoleId, Skill, SkillCategory, SkillId,
    };
    use aarg_domain::jd::{RemotePolicy, Seniority};

    // Build a well-formed empty dataset via the domain constructor, so the
    // round-trip test doesn't hand-encode (and drift from) the schema.
    fn empty_dataset_json() -> String {
        let dataset = ResumeDataset::new(Contact {
            full_name: "Test Person".into(),
            email: "t@example.com".into(),
            phone: None,
            location: None,
            links: Vec::new(),
        });
        serde_json::to_string(&dataset).unwrap()
    }

    #[test]
    fn validate_round_trips_a_dataset_through_json() {
        let out = validate_impl(&empty_dataset_json())
            .expect("validate should accept a well-formed dataset");
        let report: serde_json::Value = serde_json::from_str(&out).unwrap();
        // An empty dataset has no skills, so no evidence problems.
        assert!(report["problems"].as_array().unwrap().is_empty());
    }

    #[test]
    fn a_malformed_dataset_is_an_error_not_a_panic() {
        let err = validate_impl("{ not json").unwrap_err();
        assert!(err.contains("invalid dataset"), "got {err:?}");
    }

    #[test]
    fn normalize_dashes_turns_a_clause_break_into_a_comma() {
        assert_eq!(
            normalize_dashes_impl("Led the team — shipped it"),
            "Led the team, shipped it"
        );
    }

    // A three-level error whose top `Display` hides the two causes beneath it —
    // exactly the shape (`LoopError::Evaluate` → `ReviewError` → `LlmError`)
    // that used to reach the browser as the lone "the draft evaluator failed".
    // The leaf mimics `LlmError::Transport`'s Display, so the chain also
    // exercises the headline rewrite.
    #[derive(Debug)]
    struct Cause;
    impl std::fmt::Display for Cause {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "the LLM transport failed: Load failed")
        }
    }
    impl std::error::Error for Cause {}

    #[derive(Debug)]
    struct Middle(Cause);
    impl std::fmt::Display for Middle {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "the reviewer failed")
        }
    }
    impl std::error::Error for Middle {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    #[derive(Debug)]
    struct Top(Middle);
    impl std::fmt::Display for Top {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "the draft evaluator failed")
        }
    }
    impl std::error::Error for Top {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    #[test]
    fn error_chain_joins_every_cause_not_just_the_headline() {
        let err = Top(Middle(Cause));
        assert_eq!(
            error_chain(&err),
            "the draft evaluator failed: the reviewer failed: \
             the model call failed: Load failed"
        );
    }

    #[test]
    fn error_chain_of_a_lone_error_is_just_its_message() {
        // No source: the chain is exactly the one Display line (with the
        // transport headline rewritten), no trailing `: `.
        assert_eq!(error_chain(&Cause), "the model call failed: Load failed");
    }

    #[test]
    fn error_chain_leaves_other_messages_untouched() {
        // The rewrite targets only `LlmError::Transport`'s fixed headline; a
        // chain without it renders verbatim.
        let err = Middle(Cause);
        assert!(error_chain(&err).starts_with("the reviewer failed: "));
        assert_eq!(
            error_chain(&aarg_domain::llm::LlmError::Stream(
                "the stream broke".into()
            )),
            "the response stream was malformed: the stream broke"
        );
    }

    // A minimal but well-formed `TailoredResume`, with `summary` as the one
    // field the test varies — hand-built rather than routed through a
    // constructor because `TailoredResume` (owned by the domain crate this
    // round) doesn't have one; every other field is the empty/default shape
    // the schema accepts.
    fn canonical_json_with_summary(summary: &str) -> String {
        serde_json::json!({
            "build_id": "b1",
            "jd_id": "jd1",
            "generated_at": "2024-01-01T00:00:00Z",
            "contact": {
                "full_name": "Test Person",
                "email": "t@example.com",
                "phone": null,
                "location": null,
                "links": []
            },
            "target_title": null,
            "summary": summary,
            "roles": [],
            "education": [],
            "skills_section": { "skills": [] },
            "projects": [],
            "achievements": [],
            "certifications": []
        })
        .to_string()
    }

    #[test]
    fn scrub_resume_normalizes_dashes_in_the_summary() {
        let out = scrub_resume_impl(&canonical_json_with_summary("Led the team — shipped it"))
            .expect("scrub_resume should accept a well-formed canonical draft");
        let scrubbed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(scrubbed["summary"], "Led the team, shipped it");
    }

    #[test]
    fn scrub_resume_rejects_malformed_json_without_panicking() {
        let err = scrub_resume_impl("{ not json").unwrap_err();
        assert!(err.contains("invalid canonical draft"), "got {err:?}");
    }

    // Same shape as `canonical_json_with_summary`, but for the projected
    // payload — see `scrub_resume`'s comment for why it's hand-built.
    fn variant_payload_json_with_summary(summary: &str) -> String {
        serde_json::json!({
            "variant": "ats",
            "template": "ats/classic",
            "contact": {
                "full_name": "Test Person",
                "email": "t@example.com",
                "phone": null,
                "location": null,
                "links": []
            },
            "target_title": null,
            "summary": summary,
            "roles": [],
            "education": [],
            "skills_section": { "skills": [] },
            "skill_groups": [],
            "projects": [],
            "achievements": [],
            "certifications": [],
            "layout_hints": {
                "sidebar": false,
                "accent_color": null,
                "density": "standard",
                "show_summary": true,
                "max_pages": 1
            }
        })
        .to_string()
    }

    #[test]
    fn scrub_variant_normalizes_dashes_in_the_summary() {
        let out = scrub_variant_impl(&variant_payload_json_with_summary("fast – reliable"))
            .expect("scrub_variant should accept a well-formed payload");
        let scrubbed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(scrubbed["summary"], "fast, reliable");
    }

    #[test]
    fn scrub_variant_rejects_malformed_json_without_panicking() {
        let err = scrub_variant_impl("{ not json").unwrap_err();
        assert!(err.contains("invalid variant payload"), "got {err:?}");
    }

    fn dataset_with_ai_skill_json() -> String {
        let mut dataset = ResumeDataset::new(Contact {
            full_name: "Test Person".into(),
            email: "t@example.com".into(),
            phone: None,
            location: None,
            links: Vec::new(),
        });
        dataset.skills.skills.push(Skill {
            id: SkillId("skill-1".into()),
            canonical_name: "AI-Powered Product Development".into(),
            aliases: Vec::new(),
            category: SkillCategory::Domain,
            proficiency: Proficiency::Working,
            years: None,
            last_used: None,
            evidence: vec![EvidenceRef::Role(RoleId("role-1".into()))],
            verified: true,
            verified_at: None,
        });
        serde_json::to_string(&dataset).unwrap()
    }

    fn jd_json_with_phrase(phrase: &str) -> String {
        let jd = JobRequirements {
            company: "amplo".into(),
            title: "Staff Engineer".into(),
            seniority: Seniority::Senior,
            location: None,
            remote: RemotePolicy::Unspecified,
            domain_keywords: Vec::new(),
            required_skills: Vec::new(),
            preferred_skills: Vec::new(),
            responsibilities: Vec::new(),
            ats_phrases: vec![phrase.to_string()],
            raw_text: String::new(),
            source_url: None,
        };
        serde_json::to_string(&jd).unwrap()
    }

    #[test]
    fn backed_phrases_mirrors_a_wording_variant_of_a_recorded_skill() {
        let out = backed_phrases_impl(
            &jd_json_with_phrase("AI-powered products"),
            &dataset_with_ai_skill_json(),
        )
        .expect("backed_phrases should accept well-formed input");
        let matches: serde_json::Value = serde_json::from_str(&out).unwrap();
        let matches = matches.as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["phrase"], "AI-powered products");
        assert_eq!(
            matches[0]["dataset_skill"],
            "AI-Powered Product Development"
        );
    }

    #[test]
    fn backed_phrases_rejects_malformed_jd_json() {
        let err = backed_phrases_impl("{ not json", &empty_dataset_json()).unwrap_err();
        assert!(err.contains("invalid job requirements"), "got {err:?}");
    }

    #[test]
    fn keyword_key_collapses_a_seniority_prefix_variant() {
        let with_prefix: Vec<String> =
            serde_json::from_str(&keyword_key_impl("Sr Engineering Manager").unwrap()).unwrap();
        let without_prefix: Vec<String> =
            serde_json::from_str(&keyword_key_impl("engineering manager").unwrap()).unwrap();
        assert_eq!(with_prefix, without_prefix);
    }

    #[test]
    fn check_provenance_classifies_a_verbatim_summary_line() {
        // The dataset's own summary, echoed unchanged into the draft — the
        // clearest possible `verbatim` case, round-tripped through JSON the
        // same way a browser caller would.
        let mut dataset = ResumeDataset::new(Contact {
            full_name: "Test Person".into(),
            email: "t@example.com".into(),
            phone: None,
            location: None,
            links: Vec::new(),
        });
        dataset.summary = Some("Engineering leader with a delivery focus.".into());
        let dataset_json = serde_json::to_string(&dataset).unwrap();

        let out = check_provenance_impl(
            &canonical_json_with_summary("Engineering leader with a delivery focus."),
            &dataset_json,
        )
        .expect("check_provenance should accept well-formed input");
        let report: serde_json::Value = serde_json::from_str(&out).unwrap();
        let lines = report["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 1); // only the summary line: no roles, no skills
        assert_eq!(lines[0]["status"], "verbatim");
        assert_eq!(lines[0]["best_match"]["source"]["type"], "summary");
        assert_eq!(lines[0]["best_match"]["score"], 1.0);
    }

    // A JD with one critical requirement and one preferred one, plus a gap
    // report matching whichever names are passed — so a test can vary which
    // tier is credited and read the weighting off the score.
    fn jd_two_tiers_json() -> String {
        serde_json::json!({
            "company": "amplo", "title": "Staff Engineer", "seniority": "senior",
            "location": null, "remote": "unspecified", "domain_keywords": [],
            "required_skills": [
                {"name": "Rust", "category": "language", "importance": "critical",
                 "context_phrases": []}
            ],
            "preferred_skills": [
                {"name": "Go", "category": "language", "importance": "preferred",
                 "context_phrases": []}
            ],
            "responsibilities": [], "ats_phrases": [], "raw_text": "", "source_url": null
        })
        .to_string()
    }

    // A gap report whose `matched` credits the one named JD skill (name +
    // importance), the only fields `weighted_coverage` reads off a match.
    fn gap_matching_json(name: &str, importance: &str) -> String {
        serde_json::json!({
            "matched": [{
                "jd_skill": {"name": name, "category": "language",
                             "importance": importance, "context_phrases": []},
                "skill_id": "skill-1", "dataset_name": name, "semantic": false
            }],
            "weak": [], "unknown": []
        })
        .to_string()
    }

    #[test]
    fn weighted_coverage_counts_a_critical_match_three_times_a_preferred() {
        // Denominator is fixed at 3 (critical) + 1 (preferred) = 4.
        let critical =
            weighted_coverage_impl(&gap_matching_json("Rust", "critical"), &jd_two_tiers_json())
                .expect("well-formed input");
        let critical: serde_json::Value = serde_json::from_str(&critical).unwrap();
        // A matched critical earns 3 of the 4 available weight.
        assert_eq!(critical["score"], 0.75);
        assert_eq!(critical["matched"], 1);
        assert_eq!(critical["total"], 2);
        assert_eq!(critical["by_importance"]["critical"]["matched"], 1);
        assert_eq!(critical["by_importance"]["preferred"]["matched"], 0);

        let preferred =
            weighted_coverage_impl(&gap_matching_json("Go", "preferred"), &jd_two_tiers_json())
                .expect("well-formed input");
        let preferred: serde_json::Value = serde_json::from_str(&preferred).unwrap();
        // A matched preferred earns 1 of the same 4 — exactly a third of the
        // critical's contribution.
        assert_eq!(preferred["score"], 0.25);
        assert_eq!(preferred["by_importance"]["preferred"]["matched"], 1);
        assert_eq!(preferred["by_importance"]["critical"]["matched"], 0);
    }

    #[test]
    fn weighted_coverage_of_a_jd_with_no_requirements_is_zero() {
        let empty_jd = serde_json::json!({
            "company": "x", "title": "y", "seniority": "mid", "location": null,
            "remote": "unspecified", "domain_keywords": [], "required_skills": [],
            "preferred_skills": [], "responsibilities": [], "ats_phrases": [],
            "raw_text": "", "source_url": null
        })
        .to_string();
        let empty_gap = serde_json::json!({"matched": [], "weak": [], "unknown": []}).to_string();
        let out = weighted_coverage_impl(&empty_gap, &empty_jd).expect("well-formed input");
        let report: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(report["score"], 0.0);
        assert_eq!(report["total"], 0);
    }

    #[test]
    fn weighted_coverage_rejects_malformed_json_without_panicking() {
        let err = weighted_coverage_impl("{ not json", &jd_two_tiers_json()).unwrap_err();
        assert!(err.contains("invalid gap report"), "got {err:?}");
    }

    #[test]
    fn check_provenance_rejects_malformed_draft_json_without_panicking() {
        let err = check_provenance_impl("{ not json", &empty_dataset_json()).unwrap_err();
        assert!(err.contains("invalid canonical draft"), "got {err:?}");
    }

    #[test]
    fn check_provenance_rejects_malformed_dataset_json_without_panicking() {
        let err =
            check_provenance_impl(&canonical_json_with_summary("s"), "{ not json").unwrap_err();
        assert!(err.contains("invalid dataset"), "got {err:?}");
    }

    // `tune_interactive` itself is wasm32-only (it's a `#[wasm_bindgen]` export
    // taking `js_sys::Function`s), so it can't run natively. This proves the
    // flow it drives — `tune::run_session` over BOTH bridges, with local
    // `tokio::spawn` pumps standing in for the JS callbacks — the same
    // structure as `bridge.rs`'s `a_copilot_runs_over_the_user_bridge`, applied
    // to the newly-bound copilot. A missing/garbled JS envelope is covered by
    // the node smoke script instead, since that mapping lives in the wasm-only
    // half of `bridge.rs`.
    #[tokio::test]
    async fn tune_interactive_flow_removes_a_bullet_over_both_bridges() {
        use aarg_domain::agent::AgentContext;
        use aarg_domain::llm::{CompletionResponse, TokenUsage};
        use aarg_domain::trace::Tracer;
        use aarg_domain::user::{Answer, Question};
        use bridge::{BridgeClient, BridgeUser, Models, UserJob};
        use futures_util::StreamExt;
        use std::collections::VecDeque;

        // A minimal draft with one bullet to remove — the same shape
        // `canonical_json_with_summary` builds, with one populated role.
        let canonical_json = serde_json::json!({
            "build_id": "b1",
            "jd_id": "jd1",
            "generated_at": "2024-01-01T00:00:00Z",
            "contact": {
                "full_name": "Test Person",
                "email": "t@example.com",
                "phone": null,
                "location": null,
                "links": []
            },
            "target_title": null,
            "summary": "Engineering leader.",
            "roles": [{
                "id": "role-1",
                "company": "Globex",
                "title": "Intern",
                "start": "2018-06",
                "end": "2019-08",
                "location": null,
                "bullets": [
                    { "source_id": "bullet-9", "text": "Ran the intern mentoring program" }
                ]
            }],
            "education": [],
            "skills_section": { "skills": [] },
            "projects": [],
            "achievements": [],
            "certifications": []
        })
        .to_string();
        let mut canonical: TailoredResume =
            parse(&canonical_json, "canonical draft").expect("well-formed fixture");

        // The LLM bridge: the tune router's one call, answered with a removal
        // naming the fixture's only bullet.
        let (client, mut llm_rx) = BridgeClient::new();
        let llm_pump = tokio::spawn(async move {
            let mut answered = 0;
            while let Some((request, reply)) = llm_rx.next().await {
                let response = CompletionResponse {
                    text: r#"{"action": "remove", "bullet_id": "bullet-9"}"#.to_string(),
                    tool_calls: Vec::new(),
                    model: request.model.clone(),
                    stop_reason: Some("end_turn".to_string()),
                    usage: TokenUsage {
                        input_tokens: 5,
                        output_tokens: 5,
                    },
                };
                let _ = reply.send(Ok(response));
                answered += 1;
            }
            answered
        });

        // The user bridge: yes to the opening offer, one removal request, yes
        // to the removal confirm, then a blank line to finish — the same
        // per-turn script a JS host would drive by resolving the `user`
        // callback once per question.
        let (user, mut user_rx) = BridgeUser::new();
        let user_pump = tokio::spawn(async move {
            let mut texts: VecDeque<String> =
                VecDeque::from(vec!["drop the intern bullet".to_string(), String::new()]);
            let mut asked = 0;
            while let Some(job) = user_rx.next().await {
                match job {
                    UserJob::Ask { question, reply } => {
                        asked += 1;
                        let answer = match question {
                            Question::Text { .. } => {
                                Answer::Text(texts.pop_front().unwrap_or_default())
                            }
                            other => panic!("unexpected question in this flow: {other:?}"),
                        };
                        let _ = reply.send(Ok(answer));
                    }
                    UserJob::Confirm { reply, .. } => {
                        // Answers both the opening offer and the removal confirm.
                        let _ = reply.send(Ok(true));
                    }
                    UserJob::Notify(_) => {}
                }
            }
            asked
        });

        let models = Models::from_json(r#"{"model": "test-model"}"#).expect("valid models json");
        let ctx = AgentContext {
            llm: &client,
            model: &models,
            tracer: &Tracer::DISABLED,
            sink: None,
        };

        // Exactly what `tune_interactive` runs, minus the JSON (de)serialization
        // at the wasm boundary: this is the flow the export drives.
        let (changed, _usage) = aarg_domain::tune::run_session(
            &ctx,
            &mut canonical,
            &user,
            &[],
            aarg_domain::tune::SessionStyle::PLAIN,
        )
        .await;

        assert!(changed);
        assert!(canonical.roles[0].bullets.is_empty());

        drop(client);
        drop(user);
        let answered = llm_pump.await.expect("the llm pump should not panic");
        assert_eq!(answered, 1, "the router made exactly one model call");
        let asked = user_pump.await.expect("the user pump should not panic");
        assert_eq!(
            asked, 2,
            "two free-text turns: the request, then the blank line that ends the loop"
        );
    }
}
