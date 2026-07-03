//! The JSON API handlers. Each is a thin adapter over the same library
//! service the matching CLI command calls — so the browser surface adds a new
//! *way in*, never new behavior, and the never-fabricate guards inside the
//! reused code carry over unchanged (a `PUT /api/dataset` is validated by the
//! same [`dataset::validate`] the CLI runs, and refused if it isn't clean).
//!
//! Every handler returns a `Resp`: a success payload through
//! [`super::json_response`] / [`super::bytes_response`], or a failure through
//! [`super::error_response`] with a sensible status. Nothing here panics — the
//! workspace lints deny `unwrap`/`expect`/`panic` in production code — and no
//! error message carries key material.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use hyper::body::Incoming;
use hyper::{Request, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{AppState, Resp, bytes_response, error_response, json_response, log, read_body};
use crate::ats;
use crate::builds::{self, BuildError, BuildMeta};
use crate::commands::configured_client;
use crate::commands::tailor::{resolve_ats_template, resolve_human_template};
use crate::config::Config;
use crate::dataset::store;
use crate::dataset::types::ResumeDataset;
use crate::dataset::validate;
use crate::fetch::{self, FetchError};
use crate::gap::GapReport;
use crate::jd::JobRequirements;
use crate::llm::{CompletionRequest, LlmClient, LlmError, TokenUsage};
use crate::pricing;
use crate::render::{self, RenderError};
use crate::review::AdversarialReport;
use crate::tailor::{TailoredResume, scrub_resume_text};
use crate::templates;
use crate::variant::{self, TemplateId, Variant, VariantPayload};

// ---------------------------------------------------------------------
// POST /api/llm — proxy one completion through the server's credentials
// ---------------------------------------------------------------------

/// Run one non-streaming completion. The body is a `CompletionRequest` (the
/// `aarg-core` wire type the browser's bridge callback already produces); the
/// server builds a client with the *same* credential resolution the CLI uses
/// ([`configured_client`], which reads env / keychain / a CLI-delegated token)
/// and returns the `CompletionResponse`. The key never crosses to the browser
/// — that's the whole reason this route exists. Streaming (SSE) is a later
/// slice.
pub(super) async fn llm(req: Request<Incoming>) -> Resp {
    let body = match read_body(req).await {
        Ok(body) => body,
        Err(resp) => return resp,
    };
    let request: CompletionRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(error) => {
            return error_response(
                400,
                "bad_request",
                format!("invalid CompletionRequest JSON: {error}"),
            );
        }
    };

    // Build the client per request: a keychain read is cheap, and a
    // CLI-delegated token is refreshed each time (mirrors the MCP server).
    // A credential failure (no key configured) is a server-config problem the
    // browser can't fix, so it's a 503 — never a leak of what the key is.
    let client = match configured_client().await {
        Ok((client, _config)) => client,
        Err(error) => {
            return error_response(503, "no_credentials", error.to_string());
        }
    };

    match client.complete(request).await {
        Ok(response) => json_response(200, &response),
        Err(error) => {
            // One stderr line so an operator running `aarg serve` can see an
            // upstream failure (a provider overload, an outage, an auth reject)
            // that the browser only ever surfaces as a chained rejection. The
            // request body carries the resume and JD, so it is never logged —
            // only a bounded snippet of the error itself.
            log(&format!(
                "/api/llm upstream failed: {}",
                clip(&error.to_string(), 200)
            ));
            llm_error_response(error)
        }
    }
}

/// Clip a message to at most `max` characters (on a char boundary) for a log
/// line, appending an ellipsis when it was longer, so one upstream error can't
/// spill a wall of text to stderr.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let head: String = text.chars().take(max).collect();
        format!("{head}…")
    }
}

/// Map an `LlmError` to a status + JSON body without leaking secrets. A
/// missing key is a 503 (server misconfigured); a provider rejection passes
/// its own HTTP status through; anything else is a 502 upstream error.
fn llm_error_response(error: LlmError) -> Resp {
    match error {
        LlmError::MissingApiKey { .. } => error_response(503, "no_credentials", error.to_string()),
        LlmError::Api {
            status,
            ref kind,
            ref message,
        } => {
            // Reuse the provider's own status when it's a valid HTTP code; the
            // message is the provider's, which never echoes the key.
            let code = StatusCode::from_u16(status).map_or(502, |_| status);
            error_response(code, kind, message.clone())
        }
        other => error_response(502, "upstream", other.to_string()),
    }
}

// ---------------------------------------------------------------------
// POST /api/render — stage a payload + template and return the PDF
// ---------------------------------------------------------------------

/// The `POST /api/render` body: which variant, the projected payload, and an
/// optional template name (a built-in like `minimal`/`technical`, or a user
/// human template). The payload is a full [`VariantPayload`] — the same shape
/// the wasm `project_ats` export emits and every build stores on disk.
#[derive(Deserialize)]
struct RenderRequest {
    variant: String,
    payload: VariantPayload,
    #[serde(default)]
    template: Option<String>,
}

/// Render a variant payload to a PDF and return the bytes. Reuses the CLI's
/// render machinery: the payload and template are staged into a scratch dir
/// and `typst` is shelled out to. A missing `typst` binary is a `503` carrying
/// the install instructions; a compile failure is a `500` carrying typst's own
/// stderr (both are `RenderError`'s `Display`).
pub(super) async fn render(req: Request<Incoming>) -> Resp {
    let body = match read_body(req).await {
        Ok(body) => body,
        Err(resp) => return resp,
    };
    let RenderRequest {
        variant,
        mut payload,
        template,
    } = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(error) => {
            return error_response(
                400,
                "bad_request",
                format!("invalid render request: {error}"),
            );
        }
    };

    let variant = match variant.as_str() {
        "ats" => Variant::Ats,
        "human" => Variant::Human,
        other => {
            return error_response(
                400,
                "bad_request",
                format!("unknown variant {other:?}; use \"ats\" or \"human\""),
            );
        }
    };
    // Keep the payload's own variant tag in step with the requested one, so the
    // output filename (`resume.ats.pdf` / `resume.human.pdf`) is consistent.
    payload.variant = variant;

    // Resolve the template: a named one for the variant, else the configured
    // default (falling back to the built-in). ATS never reads a user file.
    let template_name = match template {
        Some(name) => name,
        None => match Config::load() {
            Ok(config) => match variant {
                Variant::Ats => config.templates.ats_name().to_string(),
                Variant::Human => config.templates.human_name().to_string(),
            },
            Err(error) => return error_response(500, "internal", error.to_string()),
        },
    };
    // Accept both the bare template name ("classic") and the prefixed id every
    // stored artifact carries ("ats/classic" in meta.json and the payloads'
    // `template` stamps) — an API that rejects its own artifacts' ids just
    // makes every client re-derive the bare name. A prefix that contradicts
    // the requested variant is a real mistake and stays a 400.
    let template_name = match strip_variant_prefix(&template_name, variant) {
        Ok(name) => name,
        Err(message) => return error_response(400, "bad_template", message),
    };
    let template = match templates::resolve(&template_name, variant) {
        Ok(template) => template,
        Err(error) => return error_response(400, "bad_template", error.to_string()),
    };

    // Fail fast with the install message if typst is absent, before spawning a
    // blocking task to stage files.
    if let Err(error) = render::ensure_available() {
        return render_error_response(error);
    }

    // typst is a blocking subprocess and staging is blocking IO, so run the
    // whole render off the async worker threads.
    let rendered = tokio::task::spawn_blocking(move || render_to_bytes(&payload, &template)).await;
    match rendered {
        Ok(Ok(bytes)) => bytes_response(200, "application/pdf", bytes),
        Ok(Err(error)) => render_error_response(error),
        Err(join) => error_response(
            500,
            "internal",
            format!("the render task did not complete: {join}"),
        ),
    }
}

/// Normalize a template reference to the bare name `templates::resolve` takes.
/// Stored artifacts stamp the prefixed id (`ats/classic`, `human/modern` — see
/// `resolve_ats_template`'s `format!("ats/{name}")`), so the API accepts that
/// form too: a matching prefix is stripped; a prefix contradicting the
/// requested variant is refused with a message that names both sides; a bare
/// name passes through untouched.
fn strip_variant_prefix(name: &str, variant: Variant) -> Result<String, String> {
    let (own, other) = match variant {
        Variant::Ats => ("ats/", "human/"),
        Variant::Human => ("human/", "ats/"),
    };
    if let Some(bare) = name.strip_prefix(own) {
        return Ok(bare.to_string());
    }
    if name.starts_with(other) {
        return Err(format!(
            "template {name:?} belongs to the other variant; this is a {} render",
            match variant {
                Variant::Ats => "ats",
                Variant::Human => "human",
            }
        ));
    }
    Ok(name.to_string())
}

/// The blocking half of [`render`]: stage into a unique scratch dir under the
/// OS temp dir, render, read the PDF back, and remove the scratch dir. Runs on
/// a blocking thread. The scratch dir is always cleaned up, success or not.
fn render_to_bytes(
    payload: &VariantPayload,
    template: &render::Template,
) -> Result<Vec<u8>, RenderError> {
    let dir = scratch_dir();
    std::fs::create_dir_all(&dir).map_err(|source| RenderError::Write {
        path: dir.clone(),
        source,
    })?;
    // Render, read, then clean up regardless of the render outcome.
    let result = (|| {
        let pdf = render::render(&dir, payload, template)?;
        std::fs::read(&pdf).map_err(|source| RenderError::PreviewRead { path: pdf, source })
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// A unique scratch directory under the OS temp dir. The name folds in the
/// process id and a monotonic counter so two concurrent renders never collide.
fn scratch_dir() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("aarg-serve-render-{}-{n}", std::process::id()))
}

/// Map a `RenderError` to a response: a missing `typst` binary is a `503`
/// (install it), everything else a `500` carrying typst's own message.
fn render_error_response(error: RenderError) -> Resp {
    let status = match error {
        RenderError::TypstMissing => 503,
        _ => 500,
    };
    error_response(status, "render_failed", error.to_string())
}

// ---------------------------------------------------------------------
// GET/PUT /api/dataset — the workspace source of truth
// ---------------------------------------------------------------------

/// Return the workspace dataset as JSON. A workspace with no dataset yet is a
/// `404` with the same "run `aarg ingest`" message the CLI gives.
pub(super) async fn get_dataset() -> Resp {
    match store::load() {
        Ok(dataset) => json_response(200, &dataset),
        Err(error @ store::DatasetError::NotFound { .. }) => {
            error_response(404, "no_dataset", error.to_string())
        }
        Err(error) => error_response(500, "dataset_error", error.to_string()),
    }
}

/// Validate then save a dataset. The never-fabricate gate runs first: a
/// dataset with problems (e.g. an unbacked skill) is refused with `422` and the
/// findings, *before* any write, so a bad PUT can't corrupt the source of
/// truth. Writes are serialized through the app's async mutex so two concurrent
/// browser saves queue rather than racing the store's advisory file lock.
pub(super) async fn put_dataset(req: Request<Incoming>, state: &AppState) -> Resp {
    let body = match read_body(req).await {
        Ok(body) => body,
        Err(resp) => return resp,
    };
    let dataset: ResumeDataset = match serde_json::from_slice(&body) {
        Ok(dataset) => dataset,
        Err(error) => {
            return error_response(400, "bad_request", format!("invalid dataset JSON: {error}"));
        }
    };

    // The same validation `aarg dataset validate` runs. A dataset with problems
    // is rejected — the validator must never become a backdoor for unbacked
    // claims — with the findings so the UI can show what to fix.
    let report = validate::validate(&dataset);
    if !report.is_clean() {
        let body = json!({
            "error": {
                "kind": "invalid_dataset",
                "message": format!("the dataset has {} problem(s)", report.problems.len()),
            },
            "problems": report.problems,
            "notes": report.notes,
        });
        return json_response(422, &body);
    }

    // Serialize writes: the store takes an advisory file lock (no corruption on
    // a race), but holding this async mutex across the save turns two racing
    // browser PUTs into an orderly queue instead of a spurious failure.
    let _guard = state.dataset_write.lock().await;
    match store::save(&dataset) {
        Ok(()) => json_response(200, &json!({ "status": "saved" })),
        Err(error) => error_response(500, "dataset_error", error.to_string()),
    }
}

// ---------------------------------------------------------------------
// GET /api/builds[...] — past builds and their artifacts
// ---------------------------------------------------------------------

/// List past builds, newest first. `BuildSummary` isn't `Serialize`, so the
/// JSON is built by hand — the same fields the MCP `list_builds` tool exposes.
pub(super) async fn list_builds() -> Resp {
    let builds = match crate::history::list() {
        Ok(builds) => builds,
        Err(error) => return error_response(500, "history_error", error.to_string()),
    };
    let items: Vec<Value> = builds
        .iter()
        .map(|b| {
            json!({
                "id": b.id,
                "created_at": b.created_at,
                "target": b.target(),
                "title": b.title,
                "company": b.company,
                "template": b.template,
                "model": b.model,
                "score": b.score,
                "review_score": b.review_score,
                "coverage": b.coverage,
                "objections": b.objections,
                "tokens_in": b.tokens_in,
                "tokens_out": b.tokens_out,
                "subscription": b.subscription,
            })
        })
        .collect();
    json_response(200, &json!({ "builds": items }))
}

/// Bundle one build's stored JSON artifacts: `meta`, `jd`, `gap_report`,
/// `adversarial_report`, `canonical`, and `ats_report` — each included only
/// when present, so a partial build still returns what it has. Also lists the
/// build's rendered PDF filenames (fetch each via
/// `GET /api/builds/:id/files/:name`). A non-numeric id, or a build with no
/// artifacts at all, is a `404`.
pub(super) async fn get_build(id: &str) -> Resp {
    if !is_numeric_id(id) {
        return error_response(404, "not_found", format!("no build {id:?}"));
    }
    let Ok(root) = crate::builds::builds_root() else {
        return error_response(500, "internal", "could not locate the builds directory");
    };
    let dir = root.join(id);
    if !dir.is_dir() {
        return error_response(404, "not_found", format!("no build {id:?}"));
    }

    let mut obj = Map::new();
    obj.insert("build_id".into(), json!(id));
    // Each artifact is read best-effort as opaque JSON: a missing or unreadable
    // one is simply left out (the build may be mid-write or older).
    for (key, file) in [
        ("meta", "meta.json"),
        ("jd", "jd.json"),
        ("gap_report", "gap_report.json"),
        ("adversarial_report", "adversarial_report.json"),
        ("canonical", "canonical.json"),
        // The rendered variant payloads: the browser preview shows the human
        // one and re-renders it via POST /api/render, and both variants back
        // the "same facts, different presentation" projection the UI relies on.
        ("human_payload", "human_payload.json"),
        ("ats_payload", "ats_payload.json"),
        ("ats_report", "ats_report.json"),
        // The on-disk edit log (workspace edits saved into this build). Present
        // only once at least one edit has been saved; the browser reads it to
        // render the cross-session undo history near the fidelity bar.
        ("edit_log", "edit_log.json"),
    ] {
        if let Some(value) = read_json_artifact(&dir.join(file)) {
            obj.insert(key.into(), value);
        }
    }
    obj.insert("pdfs".into(), json!(pdf_filenames(&dir)));
    json_response(200, &Value::Object(obj))
}

/// Read a JSON file into an opaque `Value`, or `None` if it's absent or
/// unparseable — a missing artifact isn't an error, just an omitted key.
fn read_json_artifact(path: &std::path::Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// The rendered PDF filenames in a build directory, sorted for a stable order.
fn pdf_filenames(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let is_pdf = path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"));
            is_pdf
                .then(|| {
                    path.file_name()
                        .and_then(|n| n.to_str())
                        .map(str::to_string)
                })
                .flatten()
        })
        .collect();
    names.sort();
    names
}

/// Serve one stored file from a build directory (a rendered PDF). Guarded
/// exactly like the MCP resources route: a bare numeric build id and a plain
/// filename — no `/`, no `..`, no percent-encoding, no NUL — so nothing can
/// escape the build directory. The content type comes from the file's
/// extension. A missing file is a `404`.
pub(super) async fn get_build_file(id: &str, name: &str) -> Resp {
    if !is_numeric_id(id) {
        return error_response(404, "not_found", format!("no build {id:?}"));
    }
    if !is_safe_filename(name) {
        return error_response(400, "bad_request", format!("invalid filename {name:?}"));
    }
    let Ok(root) = crate::builds::builds_root() else {
        return error_response(500, "internal", "could not locate the builds directory");
    };
    let path = root.join(id).join(name);
    if !path.is_file() {
        return error_response(
            404,
            "not_found",
            format!("no file {name:?} in build {id:?}"),
        );
    }
    match std::fs::read(&path) {
        Ok(bytes) => bytes_response(200, super::statics::content_type_for(&path), bytes),
        Err(error) => error_response(500, "internal", format!("could not read the file: {error}")),
    }
}

/// Whether a build id is a plain non-negative integer, digit by digit — the
/// gate before either build route touches disk. `str::parse::<u32>` looks
/// like the obvious check, but it's more permissive than it looks: it accepts
/// a leading `+` (`"+41".parse::<u32>()` succeeds) and would accept leading
/// zeros or whitespace-adjacent forms depending on the exact input, none of
/// which is a build id this server ever created. Checking every byte is
/// ASCII-digit is unambiguous about what's allowed.
fn is_numeric_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())
}

/// Whether a filename is a plain, in-directory name: non-empty and free of any
/// path separator, parent reference, percent-encoding, or NUL — the same
/// shape the MCP resources guard accepts.
fn is_safe_filename(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
        && !name.contains('%')
        && !name.contains('\0')
}

// ---------------------------------------------------------------------
// POST /api/builds — persist a browser-run build the way the CLI does
// ---------------------------------------------------------------------

/// The `POST /api/builds` body: everything the browser's wasm tailor loop
/// produced that the server needs to persist a numbered build, exactly as
/// `aarg tailor`'s finalize step does.
///
/// Note what is *not* here: the ATS `VariantPayload`. The ATS variant is a
/// deterministic projection of the canonical draft (`variant::ats_payload`),
/// so the server re-derives it itself rather than trusting a client-supplied
/// one — a browser could otherwise smuggle claims into the "upload this" PDF
/// that never survived the never-fabricate guards. The `human_payload` *is*
/// accepted as-is, because it's the LLM's reworded projection (produced in the
/// browser via `POST /api/llm`) and can't be reproduced deterministically; it's
/// optional, since a run may have rendered only the ATS variant.
#[derive(Deserialize)]
struct CreateBuildRequest {
    jd: JobRequirements,
    gap_report: GapReport,
    canonical: TailoredResume,
    adversarial_report: AdversarialReport,
    #[serde(default)]
    human_payload: Option<VariantPayload>,
    model: String,
    usage: TokenUsage,
}

/// Everything the persist step can fail with, kept as one typed enum so the
/// blocking worker propagates with `?` and the async half maps each variant to
/// the right HTTP status in one place ([`create_build_error_response`]).
#[derive(Debug, thiserror::Error)]
enum CreateBuildError {
    #[error(transparent)]
    Build(#[from] BuildError),
    #[error(transparent)]
    Render(#[from] RenderError),
    #[error(transparent)]
    Ats(#[from] ats::AtsError),
    #[error(transparent)]
    Dataset(#[from] store::DatasetError),
    #[error(transparent)]
    ClaimDivergence(#[from] variant::ClaimDivergence),
}

/// Persist a build the browser's wasm loop produced, mirroring `aarg tailor`'s
/// finalize step so the saved build is byte-for-byte the kind the CLI writes
/// and the history list scores it identically. The heavy lifting (disk writes,
/// the `typst` subprocess, PDF text extraction) is blocking, so it runs on a
/// blocking thread; only the request parse, credential-free config read, and
/// template resolution happen on the async worker.
pub(super) async fn create_build(req: Request<Incoming>) -> Resp {
    let body = match read_body(req).await {
        Ok(body) => body,
        Err(resp) => return resp,
    };
    let request: CreateBuildRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(error) => {
            return error_response(
                400,
                "bad_request",
                format!("invalid create-build request: {error}"),
            );
        }
    };

    // Fail fast with the install message if typst is absent — before allocating
    // a build directory that would otherwise be left empty.
    if let Err(error) = render::ensure_available() {
        return render_error_response(error);
    }

    // Config drives template resolution and the subscription flag. A config that
    // won't load is a server-config problem the browser can't fix.
    let config = match Config::load() {
        Ok(config) => config,
        Err(error) => return error_response(500, "internal", error.to_string()),
    };
    // The ATS template the CLI would use (default `classic`), and — only when a
    // human payload was sent — the default human template. Resolved here so a
    // bad template name is a clean 400 before any disk work begins.
    let ats_chosen = match resolve_ats_template(&config) {
        Ok(chosen) => chosen,
        Err(error) => return error_response(500, "internal", error.to_string()),
    };
    let human_chosen = if request.human_payload.is_some() {
        match resolve_human_template(&None, &config) {
            Ok(chosen) => Some(chosen),
            Err(error) => return error_response(400, "bad_template", error.to_string()),
        }
    } else {
        None
    };
    // Whether the active credential is a Claude plan — the same flag the CLI
    // stamps into `meta.json`. Read from `Config` alone (see
    // [`is_subscription_configured`]) so persisting a build never touches the
    // keychain: this route saves a build, it does not spend the key.
    let subscription = is_subscription_configured(&config);

    let result = tokio::task::spawn_blocking(move || {
        persist_build(request, ats_chosen, human_chosen, subscription)
    })
    .await;
    match result {
        Ok(Ok(id)) => json_response(200, &json!({ "id": id })),
        Ok(Err(error)) => create_build_error_response(error),
        Err(join) => error_response(
            500,
            "internal",
            format!("the build task did not complete: {join}"),
        ),
    }
}

/// The blocking half of [`create_build`]: allocate the next numbered build and
/// write its artifacts in the same order `aarg tailor` finalizes them —
/// `canonical.json`, `adversarial_report.json`, `jd.json`, `gap_report.json`,
/// then the rendered ATS PDF (with `ats_payload.json` alongside), the
/// `ats_report.json` computed from that PDF, an optional human PDF, and finally
/// `meta.json`. Returns the new build's id.
fn persist_build(
    request: CreateBuildRequest,
    ats_chosen: crate::commands::tailor::ChosenTemplate,
    human_chosen: Option<crate::commands::tailor::ChosenTemplate>,
    subscription: bool,
) -> Result<String, CreateBuildError> {
    // Load the dataset first: the ATS coverage report is scored against it, and
    // a build can't be honestly scored without it — fail before writing anything
    // rather than leaving a half-written build directory behind.
    let dataset = store::load()?;
    let root = builds::builds_root()?;
    persist_build_in(
        &root,
        &dataset,
        request,
        ats_chosen,
        human_chosen,
        subscription,
    )
}

/// The core of [`persist_build`] with the builds root and dataset injected —
/// the same `_in` seam `builds::create_next_in` / `history::list_in` use, so a
/// test can drive the whole write-and-render sequence against a tempdir without
/// touching the real workspace.
fn persist_build_in(
    root: &std::path::Path,
    dataset: &ResumeDataset,
    mut request: CreateBuildRequest,
    ats_chosen: crate::commands::tailor::ChosenTemplate,
    human_chosen: Option<crate::commands::tailor::ChosenTemplate>,
    subscription: bool,
) -> Result<String, CreateBuildError> {
    // Strip AI-tell em/en dashes from the canonical prose, exactly as the CLI
    // does before writing, so the stored JSON and every projection start clean.
    // Punctuation only, never a claim change.
    scrub_resume_text(&mut request.canonical);

    let build = builds::create_next_in(root)?;
    builds::write_json(&build.dir, "canonical.json", &request.canonical)?;
    builds::write_json(
        &build.dir,
        "adversarial_report.json",
        &request.adversarial_report,
    )?;
    builds::write_json(&build.dir, "jd.json", &request.jd)?;
    builds::write_json(&build.dir, "gap_report.json", &request.gap_report)?;

    // The ATS variant is re-projected server-side (never trusted from the
    // client) and rendered. `render::render` writes `ats_payload.json` next to
    // the PDF as a side effect.
    let mut ats = variant::ats_payload(&request.canonical);
    ats.template = TemplateId(ats_chosen.id.clone());
    let ats_pdf = render::render(&build.dir, &ats, &ats_chosen.template)?;

    // Coverage is scored against the *rendered* page text (a template bug that
    // drops a section shows up here), matching the CLI's per-iteration evaluator.
    let page_text = ats::extract_pdf_text(&ats_pdf)?;
    let ats_report = ats::keyword_coverage(&request.jd, &request.gap_report, dataset, &page_text);
    builds::write_json(&build.dir, "ats_report.json", &ats_report)?;

    // The human variant, if the browser sent one, is the LLM's reworded
    // projection — rendered as-is (it was already vetted browser-side), only its
    // template stamp updated so the payload records which template drew it.
    // `render::render` writes `human_payload.json` alongside the PDF.
    if let Some(mut human) = request.human_payload.take()
        && let Some(chosen) = human_chosen
    {
        // Never-fabricate, enforced server-side (not just browser-side): a
        // variant may differ from the canonical in presentation, never in
        // claims. Re-run the deterministic divergence check the CLI runs before
        // rendering, so a tampered or buggy client can't persist a human PDF
        // that says more than the canonical draft. A divergence is a 422.
        variant::check_claims(&request.canonical, &human)?;
        human.template = TemplateId(chosen.id.clone());
        render::render(&build.dir, &human, &chosen.template)?;
    }

    // `meta.json` last, so a build that has one is complete. The template id is
    // the ATS one (the "upload this" PDF), matching the CLI.
    builds::write_json(
        &build.dir,
        "meta.json",
        &BuildMeta {
            created_at: Utc::now(),
            model: request.model,
            template: ats_chosen.id,
            tailor_usage: request.usage,
            subscription,
        },
    )?;

    Ok(build.id.0)
}

/// Map a [`CreateBuildError`] to a response: a render failure keeps the CLI's
/// own status split (503 for a missing `typst`, 500 carrying typst's stderr);
/// everything else is a 500 with the typed error's message. No message carries
/// secret material.
fn create_build_error_response(error: CreateBuildError) -> Resp {
    match error {
        CreateBuildError::Render(error) => render_error_response(error),
        CreateBuildError::Dataset(error) => error_response(500, "dataset_error", error.to_string()),
        CreateBuildError::Ats(error) => error_response(500, "ats_error", error.to_string()),
        CreateBuildError::Build(error) => error_response(500, "build_error", error.to_string()),
        CreateBuildError::ClaimDivergence(error) => {
            error_response(422, "claim_divergence", error.to_string())
        }
    }
}

// ---------------------------------------------------------------------
// POST /api/builds/:id/edits — save workspace edits into a stored build
// ---------------------------------------------------------------------

/// The `POST /api/builds/:id/edits` body: a batch of text edits to bake into a
/// stored build's canonical draft. The client translates its positional preview
/// keys to canonical ids *before* sending (`summary`, or `bullet:<source_id>`
/// via the payload bullet's `source_id`), so there's no positional ambiguity to
/// resolve server-side.
#[derive(Deserialize)]
struct SaveEditsRequest {
    edits: Vec<EditItem>,
}

/// One edit: which line (`summary` or `bullet:<canonical-bullet-id>`) and the
/// user's new text for it.
#[derive(Deserialize)]
struct EditItem {
    target: String,
    text: String,
}

/// The `POST /api/builds/:id/edits` success body.
#[derive(Debug, Serialize)]
struct SaveEditsResponse {
    saved: usize,
    log_len: usize,
}

/// One appended entry in a build's `edit_log.json`: when the edit landed, which
/// line it touched, and the text before/after. The `prev` value is what makes
/// cross-session undo honest — a revert re-posts it as an inverse edit, so the
/// log is a complete audit trail rather than a lossy "current state".
#[derive(Serialize, Deserialize)]
struct EditLogEntry {
    at: DateTime<Utc>,
    target: String,
    prev: String,
    next: String,
}

/// Everything the save-edits step can fail with, kept as one typed enum so the
/// blocking worker propagates with `?` and the async half maps each variant to
/// its HTTP status in one place ([`save_edits_error_response`]).
#[derive(Debug, thiserror::Error)]
enum SaveEditsError {
    #[error("this build is missing {0} and cannot be edited")]
    MissingArtifact(&'static str),
    #[error("unknown edit target {0:?}")]
    UnknownTarget(String),
    #[error("could not read {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Build(#[from] BuildError),
    #[error(transparent)]
    Render(#[from] RenderError),
    #[error(transparent)]
    Template(#[from] crate::templates::TemplateError),
    #[error(transparent)]
    ClaimDivergence(#[from] variant::ClaimDivergence),
}

/// Save a batch of workspace edits into a stored build: apply them to the
/// canonical draft, re-project the ATS variant and replay them onto the human
/// variant (both under the never-fabricate guards), re-render both PDFs, and
/// append the edits to the build's on-disk log for cross-session undo. The disk
/// writes, the `typst` subprocess, and JSON (de)serialization are all blocking,
/// so the work runs on a blocking thread; only the request parse, the id/dir
/// guards, and the config read happen on the async worker.
///
/// Writes are serialized through `AppState.build_write` — the same pattern
/// `put_dataset` uses with `dataset_write`. The whole apply is a
/// read-modify-write of `canonical.json` + `edit_log.json` with no file lock of
/// its own, so two concurrent saves/reverts (two tabs on the same build) would
/// otherwise both answer 200 while the loser's edit and log entries silently
/// vanish (last write wins). The guard is acquired before the blocking task
/// spawns and held across its await, so a second request queues behind the
/// first instead of racing it.
pub(super) async fn save_build_edits(req: Request<Incoming>, id: &str, state: &AppState) -> Resp {
    if !is_numeric_id(id) {
        return error_response(404, "not_found", format!("no build {id:?}"));
    }
    let body = match read_body(req).await {
        Ok(body) => body,
        Err(resp) => return resp,
    };
    let request: SaveEditsRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(error) => {
            return error_response(
                400,
                "bad_request",
                format!("invalid save-edits request: {error}"),
            );
        }
    };
    let Ok(root) = crate::builds::builds_root() else {
        return error_response(500, "internal", "could not locate the builds directory");
    };
    let dir = root.join(id);
    if !dir.is_dir() {
        return error_response(404, "not_found", format!("no build {id:?}"));
    }

    // Fail fast with the install message if typst is absent — the same gate
    // create_build uses, so an edit that could never render is refused before
    // any disk work rather than half-applied.
    if let Err(error) = render::ensure_available() {
        return render_error_response(error);
    }
    // Config drives the fallback template names. A config that won't load is a
    // server-config problem the browser can't fix.
    let config = match Config::load() {
        Ok(config) => config,
        Err(error) => return error_response(500, "internal", error.to_string()),
    };

    // Serialize build writes: acquire before spawning the blocking task and
    // hold the guard across its await, so the entire read-apply-write-render
    // sequence of one request completes before the next begins (mirrors how
    // `put_dataset` holds `dataset_write` across validate-then-save).
    let _guard = state.build_write.lock().await;
    let result =
        tokio::task::spawn_blocking(move || apply_build_edits(&dir, request, &config)).await;
    match result {
        Ok(Ok(response)) => json_response(200, &response),
        Ok(Err(error)) => save_edits_error_response(error),
        Err(join) => error_response(
            500,
            "internal",
            format!("the edit task did not complete: {join}"),
        ),
    }
}

/// The blocking half of [`save_build_edits`], with the build directory injected
/// so a test can drive the whole apply-and-render sequence against a tempdir.
/// Applies the edits to the canonical draft, re-projects/re-checks the variants,
/// persists the edited canonical and the appended log, then re-renders both
/// PDFs. The claim-divergence guard runs *before* any write, so a rejected edit
/// leaves the stored build untouched.
fn apply_build_edits(
    dir: &std::path::Path,
    request: SaveEditsRequest,
    config: &Config,
) -> Result<SaveEditsResponse, SaveEditsError> {
    let mut canonical: TailoredResume = read_build_json(&dir.join("canonical.json"))?
        .ok_or(SaveEditsError::MissingArtifact("canonical.json"))?;
    // A build's JD is required (a real build always has one). Its content isn't
    // needed to apply text edits — re-scoring coverage is not part of a text
    // edit — but a build missing it isn't one we edit.
    if read_build_json::<Value>(&dir.join("jd.json"))?.is_none() {
        return Err(SaveEditsError::MissingArtifact("jd.json"));
    }

    // Apply each edit to the canonical, capturing the prior text for the log
    // (and for cross-session undo). An unknown bullet id is a 400 raised here,
    // before anything is written, so a bad batch never half-lands.
    let mut entries = Vec::with_capacity(request.edits.len());
    for edit in &request.edits {
        let prev = apply_edit_to_canonical(&mut canonical, &edit.target, &edit.text)?;
        entries.push(EditLogEntry {
            at: Utc::now(),
            target: edit.target.clone(),
            prev,
            next: edit.text.clone(),
        });
    }
    // Punctuation-only normalization, exactly as every other write path does
    // before storing the canonical. Never a claim change.
    scrub_resume_text(&mut canonical);

    // Re-project the ATS variant deterministically from the edited canonical,
    // stamped with this build's own template (falling back to the configured
    // default). Never trusted from a client — the same guard create_build uses.
    let (ats_id, ats_template) = resolve_build_template(dir, config, Variant::Ats)?;
    let mut ats = variant::ats_payload(&canonical);
    ats.template = TemplateId(ats_id);

    // If this build has a human variant, replay the *same* user text onto its
    // matching lines (the user's words replace the reword; a human line whose
    // source_id got no edit is left as-is) and re-run the deterministic
    // claim-divergence guard before persisting anything — same facts, different
    // presentation, never a new claim. A divergence is a 422.
    let human = match read_build_json::<VariantPayload>(&dir.join("human_payload.json"))? {
        Some(mut human) => {
            for edit in &request.edits {
                apply_edit_to_payload(&mut human, &edit.target, &edit.text);
            }
            variant::scrub_variant_text(&mut human);
            variant::check_claims(&canonical, &human)?;
            let (human_id, human_template) = resolve_build_template(dir, config, Variant::Human)?;
            human.template = TemplateId(human_id);
            Some((human, human_template))
        }
        None => None,
    };

    // Persist the edited canonical and append to the on-disk log *before*
    // rendering: the text edit is the durable change, the PDFs are a projection
    // of it. In production the handler has already verified `typst` is present,
    // so the renders below succeed; this ordering also lets a test without
    // `typst` still observe the canonical and log update.
    builds::write_json(dir, "canonical.json", &canonical)?;
    let mut log: Vec<EditLogEntry> =
        read_build_json(&dir.join("edit_log.json"))?.unwrap_or_default();
    let saved = entries.len();
    log.extend(entries);
    let log_len = log.len();
    builds::write_json(dir, "edit_log.json", &log)?;

    // Re-render both PDFs. `render::render` writes the payload JSON alongside
    // each PDF, so `ats_payload.json`/`human_payload.json` are refreshed too.
    render::render(dir, &ats, &ats_template)?;
    if let Some((human, human_template)) = human {
        render::render(dir, &human, &human_template)?;
    }

    Ok(SaveEditsResponse { saved, log_len })
}

/// Apply one edit to the canonical draft, returning the prior text (for the
/// log). `summary` targets the summary; `bullet:<id>` targets the canonical
/// bullet whose `source_id` matches. An id that names no bullet is a
/// [`SaveEditsError::UnknownTarget`] (a 400 that names it).
fn apply_edit_to_canonical(
    canonical: &mut TailoredResume,
    target: &str,
    text: &str,
) -> Result<String, SaveEditsError> {
    if target == "summary" {
        return Ok(std::mem::replace(&mut canonical.summary, text.to_string()));
    }
    if let Some(id) = target.strip_prefix("bullet:") {
        for role in &mut canonical.roles {
            for bullet in &mut role.bullets {
                if bullet.source_id.0 == id {
                    return Ok(std::mem::replace(&mut bullet.text, text.to_string()));
                }
            }
        }
    }
    Err(SaveEditsError::UnknownTarget(target.to_string()))
}

/// Replay one edit onto the human payload, best-effort: `summary` and
/// `bullet:<source_id>` mirror [`apply_edit_to_canonical`]. A target the human
/// variant doesn't contain (an omitted bullet) is left untouched — omission is
/// allowed, not a divergence — and never an error, since the canonical is the
/// authority on which ids exist.
fn apply_edit_to_payload(payload: &mut VariantPayload, target: &str, text: &str) {
    if target == "summary" {
        payload.summary = text.to_string();
        return;
    }
    if let Some(id) = target.strip_prefix("bullet:") {
        for role in &mut payload.roles {
            for bullet in &mut role.bullets {
                if bullet.source_id.0 == id {
                    bullet.text = text.to_string();
                    return;
                }
            }
        }
    }
}

/// The template to re-render a variant with: the build's own stored stamp
/// (`ats/classic`, `human/modern`, …) when present, else the configured
/// default name. Returns the stamped id and the resolved template, matching
/// `resolve_ats_template`/`resolve_human_template`'s id shape.
fn resolve_build_template(
    dir: &std::path::Path,
    config: &Config,
    variant: Variant,
) -> Result<(String, render::Template), SaveEditsError> {
    let (prefix, default_name) = match variant {
        Variant::Ats => ("ats/", config.templates.ats_name()),
        Variant::Human => ("human/", config.templates.human_name()),
    };
    let name = stored_template_stamp(&dir.join(variant.payload_name()))
        .and_then(|stamp| stamp.strip_prefix(prefix).map(str::to_string))
        .unwrap_or_else(|| default_name.to_string());
    let template = templates::resolve(&name, variant)?;
    Ok((format!("{prefix}{name}"), template))
}

/// The `template` stamp a stored payload JSON carries (e.g. `"ats/classic"`),
/// or `None` if the file is absent or has no such field.
fn stored_template_stamp(path: &std::path::Path) -> Option<String> {
    read_json_artifact(path)?
        .get("template")?
        .as_str()
        .map(str::to_string)
}

/// Read a build artifact as a typed value: `Ok(None)` when the file is absent,
/// `Ok(Some(_))` when it parses, and an error only when it exists but is
/// unreadable or malformed. Absence of a *required* artifact becomes a 404 at
/// the call site; the optional log simply defaults to empty.
fn read_build_json<T: DeserializeOwned>(
    path: &std::path::Path,
) -> Result<Option<T>, SaveEditsError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(serde_json::from_str(&text)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(SaveEditsError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Map a [`SaveEditsError`] to a response: a missing required artifact is a 404,
/// an unknown target a 400, a claim divergence a 422 (the same guard
/// create_build uses), a render failure keeps the CLI's 503/500 split, and a
/// bad template name is a 400. No message carries secret material.
fn save_edits_error_response(error: SaveEditsError) -> Resp {
    match error {
        SaveEditsError::MissingArtifact(_) => error_response(404, "not_found", error.to_string()),
        SaveEditsError::UnknownTarget(_) => error_response(400, "bad_request", error.to_string()),
        SaveEditsError::ClaimDivergence(error) => {
            error_response(422, "claim_divergence", error.to_string())
        }
        SaveEditsError::Render(error) => render_error_response(error),
        SaveEditsError::Template(error) => error_response(400, "bad_template", error.to_string()),
        SaveEditsError::Build(error) => error_response(500, "build_error", error.to_string()),
        SaveEditsError::Json(error) => error_response(500, "internal", error.to_string()),
        SaveEditsError::Io { .. } => error_response(500, "internal", error.to_string()),
    }
}

// ---------------------------------------------------------------------
// POST /api/fetch-jd — fetch a cross-origin posting server-side
// ---------------------------------------------------------------------

/// The `POST /api/fetch-jd` body: the posting URL to fetch.
#[derive(Deserialize)]
struct FetchJdRequest {
    url: String,
}

/// Fetch a job posting's text from a supported board (Greenhouse, Lever, or LinkedIn) — the
/// thing a browser can't do itself (CORS). An unsupported URL is a `422` with
/// the "paste the text instead" guidance; any other fetch failure is a `502`.
pub(super) async fn fetch_jd(req: Request<Incoming>) -> Resp {
    let body = match read_body(req).await {
        Ok(body) => body,
        Err(resp) => return resp,
    };
    let request: FetchJdRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(error) => {
            return error_response(
                400,
                "bad_request",
                format!("expected {{\"url\": ...}}: {error}"),
            );
        }
    };
    match fetch::fetch_jd(&request.url).await {
        Ok(text) => json_response(200, &json!({ "text": text })),
        Err(error @ FetchError::UnsupportedUrl { .. }) => {
            error_response(422, "unsupported_url", error.to_string())
        }
        Err(error) => {
            // Same operator-visible line as the LLM proxy: an upstream fetch
            // failure (a timeout, a 5xx from the posting host) is worth one
            // stderr line. The URL is the user's own input, not sensitive.
            log(&format!(
                "/api/fetch-jd upstream failed: {}",
                clip(&error.to_string(), 200)
            ));
            error_response(502, "fetch_failed", error.to_string())
        }
    }
}

// ---------------------------------------------------------------------
// GET /api/cost — turn a build's token usage into a dollar estimate
// ---------------------------------------------------------------------

/// The `GET /api/cost` query parameters: which model, and how many input/
/// output tokens to price.
struct CostQuery {
    model: String,
    input: u64,
    output: u64,
}

/// Turn token usage into a dollar estimate — the browser's only way to reach
/// [`pricing::cost_usd`], which lives in this native crate (it needs the
/// config's price-override table) and which the wasm build has no way to
/// call. A plain `GET` with query params rather than a JSON body: this is a
/// pure calculation with no side effect, so there's nothing for the
/// `Content-Type` gate to protect.
///
/// Mirrors `aarg history`'s own cost column
/// ([`crate::commands::history::cost_cell`]) exactly: a model absent from the
/// price table prices as `usd: null` rather than an error, and — the same
/// way a subscription run shows `plan` instead of a dollar figure in that
/// column — an active subscription credential suppresses `usd` in favor of a
/// `subscription_note`, because the run's cost is covered by the flat fee and
/// a dollar figure would mislead.
///
/// Deliberately does **not** call [`configured_client`] (the `/api/llm` route's
/// credential resolver): that function fetches the *actual secret* — a
/// keychain read, possibly an OS permission prompt, or (for a `Cli`-delegated
/// credential) a subprocess spawn — none of which this pure calculation
/// needs or should pay for just to answer "would this be free." Whether the
/// active credential is a subscription is knowable from `Config` alone (see
/// [`is_subscription_configured`]), so this route only ever touches the
/// config file, never the keychain.
/// `GET /api/models` — the model tiers the browser should send with its
/// in-browser LLM work, read from the same `Config` the CLI resolves against.
/// Returning the three *resolved* tiers (not one hardcoded model) lets a
/// browser build spend the cheap tier where the CLI does and reserve the smart
/// model for the reviewer/tailor — so an in-browser build costs and reads like
/// a terminal one instead of running everything on the priciest model. Config
/// only; no keychain, no key spend.
pub(super) async fn models() -> Resp {
    use crate::agent::{ModelResolver, ModelTier};
    let config = match Config::load() {
        Ok(config) => config,
        Err(error) => return error_response(500, "config_error", error.to_string()),
    };
    let anthropic = &config.anthropic;
    json_response(
        200,
        &json!({
            "cheap": anthropic.resolve("", ModelTier::Cheap),
            "mid": anthropic.resolve("", ModelTier::Mid),
            "smart": anthropic.resolve("", ModelTier::Smart),
        }),
    )
}

/// `GET /api/templates` — the template names a browser picker can offer per
/// variant, resolved through the very same [`templates::available`] the CLI's
/// `aarg templates list` uses (and that `POST /api/render` resolves against),
/// so the picker can only ever offer names the render route will actually
/// accept. That list is fully enumerable: the built-ins embedded in the binary
/// (`classic`/`minimal` for ATS; `modern`/`technical`/`editorial` for human)
/// *plus* any user human templates discovered on disk under the active
/// workspace's `templates/human/` directory — `available()` walks that
/// directory itself, so user files show up here without the browser knowing
/// where they live. ATS stays built-in only (a custom ATS layout could break an
/// applicant-tracker parser), so no user name ever appears under `ats`. Config/
/// workspace only — no keychain, no key spend.
pub(super) async fn templates() -> Resp {
    let mut ats: Vec<String> = Vec::new();
    let mut human: Vec<String> = Vec::new();
    for listed in templates::available() {
        match listed.variant {
            Variant::Ats => ats.push(listed.name),
            Variant::Human => human.push(listed.name),
        }
    }
    json_response(200, &json!({ "ats": ats, "human": human }))
}

pub(super) async fn cost(req: Request<Incoming>) -> Resp {
    let query = match parse_cost_query(req.uri().query().unwrap_or("")) {
        Ok(query) => query,
        Err(message) => return error_response(400, "bad_request", message),
    };

    // A config that fails to load isn't this pure calculation's problem to
    // surface as an error — treat it as "no config, no subscription, no
    // price overrides" and still answer off the built-in family rates.
    let config = Config::load().unwrap_or_default();
    let subscription = is_subscription_configured(&config);

    json_response(200, &cost_body(&query, subscription, &config.prices))
}

/// Whether the credential `aarg` would use *right now* is a Claude
/// plan/subscription credential (`AuthKind::Oauth`, or `AuthKind::Cli` —
/// `configured_client` always resolves a `Cli`-delegated label to an OAuth
/// token, so it's a subscription too), without ever fetching the secret
/// itself. Mirrors `configured_client`'s resolution order (env override
/// first, then the configured active label) far enough to answer just this
/// one question.
fn is_subscription_configured(config: &Config) -> bool {
    if env_var_set(config.anthropic.auth_token_env()) {
        return true; // The OAuth/subscription env var is always a plan token.
    }
    if env_var_set(config.anthropic.api_key_env()) {
        return false; // The API-key env var is never a plan token.
    }
    let override_label = std::env::var("AARG_KEY").ok();
    let label = override_label
        .as_deref()
        .unwrap_or_else(|| config.anthropic.active_label());
    matches!(
        config.anthropic.kind_for(label),
        crate::config::AuthKind::Oauth | crate::config::AuthKind::Cli
    )
}

/// Whether an environment variable is set to a non-empty value — the same
/// "unset or blank means absent" rule `configured_client`'s own
/// `env_credential` uses, duplicated here (rather than reached for across a
/// module boundary) because it's a one-line check.
fn env_var_set(var: &str) -> bool {
    std::env::var(var).is_ok_and(|value| !value.is_empty())
}

/// Build the `{"usd": ..., "subscription_note": ...}` body: `usd` is `null`
/// either when the model isn't in the price table (an unpriced model is
/// never guessed at) or when `subscription` is true, in which case
/// `subscription_note` explains why — the run's cost is covered by the flat
/// fee, the same reasoning `aarg history`'s `plan` cost-column marker uses.
/// Split out from [`cost`] so the calculation is unit-testable without a
/// keychain or a config file on disk.
fn cost_body(
    query: &CostQuery,
    subscription: bool,
    prices: &std::collections::BTreeMap<String, pricing::Price>,
) -> Value {
    if subscription {
        return json!({ "usd": Value::Null, "subscription_note": "covered by your Claude plan" });
    }
    let usage = TokenUsage {
        input_tokens: query.input,
        output_tokens: query.output,
    };
    let usd = pricing::cost_usd(&query.model, &usage, prices);
    json!({ "usd": usd, "subscription_note": Value::Null })
}

/// Parse `model`/`input`/`output` out of a raw query string (the part of the
/// URI after `?`, or `""` when there is none). Hand-rolled, matching this
/// module's no-framework style, rather than pulling in a URL-parsing crate
/// for three plain params: model ids and token counts are never
/// percent-encoded, so a `key=value` split on `&` and `=` is exactly as
/// correct here as a general decoder would be.
fn parse_cost_query(query: &str) -> Result<CostQuery, String> {
    let mut model = None;
    let mut input = None;
    let mut output = None;
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        match key {
            "model" => model = Some(value.to_string()),
            "input" => input = value.parse::<u64>().ok(),
            "output" => output = value.parse::<u64>().ok(),
            _ => {}
        }
    }
    let model = model
        .filter(|m| !m.is_empty())
        .ok_or_else(|| "missing required query param `model`".to_string())?;
    let input = input.ok_or_else(|| {
        "missing or invalid query param `input` (expected a non-negative integer)".to_string()
    })?;
    let output = output.ok_or_else(|| {
        "missing or invalid query param `output` (expected a non-negative integer)".to_string()
    })?;
    Ok(CostQuery {
        model,
        input,
        output,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use http_body_util::BodyExt;

    use super::*;

    #[test]
    fn a_render_request_deserializes_variant_payload_and_optional_template() {
        // The payload shape mirrors what wasm `project_ats` emits and every
        // build stores. `template` is optional.
        let body = json!({
            "variant": "ats",
            "payload": {
                "variant": "ats",
                "template": "ats/classic",
                "contact": {
                    "full_name": "Ada Lovelace",
                    "email": "ada@example.com",
                    "phone": null,
                    "location": null,
                    "links": []
                },
                "target_title": null,
                "summary": "Engineering leader.",
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
            }
        });
        let parsed: RenderRequest = serde_json::from_value(body).unwrap();
        assert_eq!(parsed.variant, "ats");
        assert_eq!(parsed.template, None);
        assert_eq!(parsed.payload.summary, "Engineering leader.");
    }

    #[test]
    fn the_build_file_guard_rejects_escapes() {
        // Plain filenames are allowed.
        assert!(is_safe_filename("resume.ats.pdf"));
        assert!(is_safe_filename("ats_report.json"));
        // Anything that could escape the build directory is refused.
        assert!(!is_safe_filename(""));
        assert!(!is_safe_filename("../secret"));
        assert!(!is_safe_filename("a/b.pdf"));
        assert!(!is_safe_filename("a\\b.pdf"));
        assert!(!is_safe_filename("%2e%2e"));
        assert!(!is_safe_filename("bad\0name"));
    }

    #[test]
    fn the_build_id_gate_accepts_only_plain_digits() {
        // Ordinary build ids are allowed.
        assert!(is_numeric_id("0"));
        assert!(is_numeric_id("041"));
        // Empty, a leading `+` (which `str::parse::<u32>` would accept), a
        // sign, and non-digit content are all refused.
        assert!(!is_numeric_id(""));
        assert!(!is_numeric_id("+41"));
        assert!(!is_numeric_id("-41"));
        assert!(!is_numeric_id("../etc"));
        assert!(!is_numeric_id("41 "));
    }

    #[tokio::test]
    async fn templates_lists_the_builtins_grouped_by_variant() {
        // The handler groups `templates::available()` by variant. The built-ins
        // are embedded in the binary, so they're always present regardless of
        // the active workspace (a user human template, if any, would simply be
        // an extra name under `human`).
        let resp = templates().await;
        assert_eq!(resp.status(), 200);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();

        let ats: Vec<&str> = body["ats"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(ats.contains(&"classic"), "ats missing classic: {ats:?}");
        assert!(ats.contains(&"minimal"), "ats missing minimal: {ats:?}");

        let human: Vec<&str> = body["human"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        for name in ["modern", "technical", "editorial"] {
            assert!(human.contains(&name), "human missing {name}: {human:?}");
        }
        // ATS stays built-in only — a user name never leaks into the ATS list.
        assert_eq!(ats.len(), 2, "unexpected ATS templates: {ats:?}");
    }

    #[tokio::test]
    async fn get_build_rejects_a_non_numeric_id() {
        let resp = get_build("../etc").await;
        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn get_build_file_rejects_a_bad_filename_before_touching_disk() {
        let resp = get_build_file("041", "../../secret").await;
        assert_eq!(resp.status(), 400);
    }

    #[test]
    fn parse_cost_query_reads_the_three_params_and_rejects_what_it_must() {
        let query = parse_cost_query("model=claude-opus-4-8&input=1000&output=500").unwrap();
        assert_eq!(query.model, "claude-opus-4-8");
        assert_eq!(query.input, 1000);
        assert_eq!(query.output, 500);

        // Order doesn't matter, and an unrelated param is just ignored.
        let query = parse_cost_query("output=1&extra=x&input=2&model=m").unwrap();
        assert_eq!(query.model, "m");
        assert_eq!(query.input, 2);
        assert_eq!(query.output, 1);

        assert!(parse_cost_query("input=1&output=2").is_err()); // no model
        assert!(parse_cost_query("model=m&output=2").is_err()); // no input
        assert!(parse_cost_query("model=m&input=1").is_err()); // no output
        assert!(parse_cost_query("model=m&input=nope&output=2").is_err()); // unparseable
    }

    #[test]
    fn cost_body_prices_a_known_model_and_nulls_an_unknown_one() {
        let prices = std::collections::BTreeMap::new();

        // A known model family (Opus: $15/$75 per Mtok) prices exactly, the
        // same figure `pricing::cost_usd` itself is tested against.
        let known = CostQuery {
            model: "claude-opus-4-8".to_string(),
            input: 1000,
            output: 500,
        };
        let body = cost_body(&known, false, &prices);
        let usd = body["usd"].as_f64().unwrap();
        assert!((usd - (1000.0 / 1_000_000.0 * 15.0 + 500.0 / 1_000_000.0 * 75.0)).abs() < 1e-9);
        assert!(body["subscription_note"].is_null());

        // A model absent from the price table (built-in or override) prices
        // as null, never a guess.
        let unknown = CostQuery {
            model: "some-local-llama".to_string(),
            input: 1000,
            output: 500,
        };
        let body = cost_body(&unknown, false, &prices);
        assert!(body["usd"].is_null());
        assert!(body["subscription_note"].is_null());
    }

    #[test]
    fn cost_body_on_a_subscription_nulls_usd_and_explains_why() {
        let prices = std::collections::BTreeMap::new();
        // Even a perfectly priceable model is suppressed once `subscription`
        // is true — the run's cost is covered by the flat fee.
        let query = CostQuery {
            model: "claude-opus-4-8".to_string(),
            input: 1000,
            output: 500,
        };
        let body = cost_body(&query, true, &prices);
        assert!(body["usd"].is_null());
        assert_eq!(body["subscription_note"], "covered by your Claude plan");
    }

    /// A minimal `CreateBuildRequest` as the browser would POST it: an empty
    /// JD/gap/adversarial report and a bare canonical draft, no human payload.
    fn minimal_create_build_request() -> CreateBuildRequest {
        serde_json::from_value(json!({
            "jd": {
                "company": "Acme",
                "title": "Engineer",
                "seniority": "senior",
                "location": null,
                "remote": "remote",
                "domain_keywords": [],
                "required_skills": [],
                "preferred_skills": [],
                "responsibilities": [],
                "ats_phrases": [],
                "raw_text": "Build things.",
                "source_url": null
            },
            "gap_report": { "matched": [], "weak": [], "unknown": [] },
            "canonical": {
                "build_id": "000",
                "jd_id": "acme",
                "generated_at": "2026-07-01T00:00:00Z",
                "contact": {
                    "full_name": "Ada Lovelace",
                    "email": "ada@example.com",
                    "phone": null,
                    "location": null,
                    "links": []
                },
                "target_title": "Engineer",
                "summary": "Engineering leader.",
                "roles": [],
                "education": [],
                "skills_section": { "skills": [] },
                "projects": [],
                "achievements": [],
                "certifications": []
            },
            "adversarial_report": {
                "objections": [],
                "overall_score": 0.8,
                "persona_notes": "ok"
            },
            "model": "test-model",
            "usage": { "input_tokens": 100, "output_tokens": 50 }
        }))
        .unwrap()
    }

    /// The render route must accept both the bare template name and the
    /// prefixed id stored artifacts carry — and refuse a cross-variant prefix.
    #[test]
    fn template_prefix_is_stripped_matched_or_refused() {
        // Bare names pass through untouched.
        assert_eq!(
            strip_variant_prefix("classic", Variant::Ats).as_deref(),
            Ok("classic")
        );
        // The stored id form ("ats/classic" in meta.json / payload stamps)
        // strips to the bare name templates::resolve takes.
        assert_eq!(
            strip_variant_prefix("ats/classic", Variant::Ats).as_deref(),
            Ok("classic")
        );
        assert_eq!(
            strip_variant_prefix("human/modern", Variant::Human).as_deref(),
            Ok("modern")
        );
        // A prefix contradicting the requested variant is a real mistake.
        assert!(strip_variant_prefix("ats/classic", Variant::Human).is_err());
        assert!(strip_variant_prefix("human/modern", Variant::Ats).is_err());
    }

    /// The persist step writes the same artifacts `aarg tailor` finalizes, into
    /// the injected root. The four pre-render JSON files always land; the ATS
    /// PDF, its payload, the coverage report, and `meta.json` need a real
    /// `typst`, so those assertions are gated behind its availability (CI may
    /// not have it installed).
    #[test]
    fn create_build_writes_the_cli_artifacts_to_the_injected_root() {
        use crate::dataset::types::{Contact, ResumeDataset};

        let root = tempfile::tempdir().unwrap();
        let dataset = ResumeDataset::new(Contact {
            full_name: "Ada Lovelace".into(),
            email: "ada@example.com".into(),
            phone: None,
            location: None,
            links: Vec::new(),
        });
        let request = minimal_create_build_request();
        // ATS is built-in (`classic`), so this resolves without a config file.
        let ats_chosen = resolve_ats_template(&Config::default()).unwrap();

        let result = persist_build_in(root.path(), &dataset, request, ats_chosen, None, false);

        // The first build is `001`; these four artifacts are written before any
        // render, so they exist whether or not typst does.
        let build_dir = root.path().join("001");
        for artifact in [
            "canonical.json",
            "adversarial_report.json",
            "jd.json",
            "gap_report.json",
        ] {
            assert!(build_dir.join(artifact).is_file(), "missing {artifact}");
        }

        if render::ensure_available().is_ok() {
            // typst present: the whole finalize runs and the id comes back.
            assert_eq!(result.unwrap(), "001");
            for artifact in [
                "ats_payload.json",
                "ats_report.json",
                "meta.json",
                "resume.ats.pdf",
            ] {
                assert!(build_dir.join(artifact).is_file(), "missing {artifact}");
            }
        } else {
            // No typst: the render step fails, but the pre-render JSON (asserted
            // above) is still on disk.
            assert!(result.is_err());
        }
    }

    /// Applying an edit rewrites the canonical draft on disk and appends an
    /// `edit_log.json` entry whose `prev` captures the pre-edit text — the value
    /// a cross-session revert re-posts as its inverse. The canonical and log
    /// writes happen before the re-render, so they're asserted unconditionally;
    /// the re-rendered PDF is gated behind a real `typst`.
    #[test]
    fn save_edits_rewrites_the_canonical_and_appends_the_log() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("001");
        std::fs::create_dir_all(&dir).unwrap();

        // Seed the two required artifacts the way a finalized build has them.
        let request = minimal_create_build_request();
        let original_summary = request.canonical.summary.clone();
        builds::write_json(&dir, "canonical.json", &request.canonical).unwrap();
        builds::write_json(&dir, "jd.json", &request.jd).unwrap();

        // One summary edit; the build has no human payload, so the ATS variant
        // (built-in `classic`) is the only projection re-rendered.
        let edits = SaveEditsRequest {
            edits: vec![EditItem {
                target: "summary".into(),
                text: "A sharper, edited summary.".into(),
            }],
        };
        let result = apply_build_edits(&dir, edits, &Config::default());

        // The canonical draft on disk now carries the edited text, whether or
        // not typst was available to re-render the PDF.
        let stored: TailoredResume =
            serde_json::from_str(&std::fs::read_to_string(dir.join("canonical.json")).unwrap())
                .unwrap();
        assert_eq!(stored.summary, "A sharper, edited summary.");

        // The log gained exactly one entry, and its `prev` is the ORIGINAL
        // summary (what a revert would restore), its `next` the new text.
        let log: Vec<EditLogEntry> =
            serde_json::from_str(&std::fs::read_to_string(dir.join("edit_log.json")).unwrap())
                .unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].target, "summary");
        assert_eq!(log[0].prev, original_summary);
        assert_eq!(log[0].next, "A sharper, edited summary.");

        if render::ensure_available().is_ok() {
            // typst present: the whole apply succeeds and the PDF is rendered.
            let response = result.unwrap();
            assert_eq!(response.saved, 1);
            assert_eq!(response.log_len, 1);
            assert!(dir.join("resume.ats.pdf").is_file());
        } else {
            // No typst: the render step fails, but the canonical/log writes
            // above still landed (asserted unconditionally).
            assert!(result.is_err());
        }
    }

    /// An edit naming a bullet id the canonical draft doesn't have is a 400
    /// (`UnknownTarget`) raised before anything is written, so a bad batch never
    /// half-lands.
    #[test]
    fn save_edits_rejects_an_unknown_bullet_id() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("001");
        std::fs::create_dir_all(&dir).unwrap();
        let request = minimal_create_build_request();
        builds::write_json(&dir, "canonical.json", &request.canonical).unwrap();
        builds::write_json(&dir, "jd.json", &request.jd).unwrap();

        let edits = SaveEditsRequest {
            edits: vec![EditItem {
                target: "bullet:does-not-exist".into(),
                text: "nope".into(),
            }],
        };
        let error = apply_build_edits(&dir, edits, &Config::default()).unwrap_err();
        assert!(matches!(error, SaveEditsError::UnknownTarget(_)));
        // Nothing was written: no log, and the canonical is untouched.
        assert!(!dir.join("edit_log.json").exists());
    }
}
