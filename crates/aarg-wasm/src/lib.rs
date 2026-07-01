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
//! ships, and mirror JD phrasing a recorded skill already backs. Everything
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
use aarg_domain::agent::AgentContext;
#[cfg(target_arch = "wasm32")]
use aarg_domain::gap::GapReport;
#[cfg(target_arch = "wasm32")]
use aarg_domain::tailor::{BuildId, JdId};
#[cfg(target_arch = "wasm32")]
use aarg_domain::trace::Tracer;
#[cfg(target_arch = "wasm32")]
use bridge::{BridgeClient, Models};

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
        .map_err(|e| throw(e.to_string()))?;
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
        .map_err(|e| throw(e.to_string()))?;
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
    .map_err(|e| throw(e.to_string()))?;
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
        .map_err(|e| throw(e.to_string()))?;
    dump(&report).map_err(throw)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
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
}
