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

use hyper::body::Incoming;
use hyper::{Request, StatusCode};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{AppState, Resp, bytes_response, error_response, json_response, read_body};
use crate::commands::configured_client;
use crate::config::Config;
use crate::dataset::store;
use crate::dataset::types::ResumeDataset;
use crate::dataset::validate;
use crate::fetch::{self, FetchError};
use crate::llm::{CompletionRequest, LlmClient, LlmError};
use crate::render::{self, RenderError};
use crate::templates;
use crate::variant::{Variant, VariantPayload};

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
        Err(error) => llm_error_response(error),
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
                "target": b.target,
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
        ("ats_report", "ats_report.json"),
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
// POST /api/fetch-jd — fetch a cross-origin posting server-side
// ---------------------------------------------------------------------

/// The `POST /api/fetch-jd` body: the posting URL to fetch.
#[derive(Deserialize)]
struct FetchJdRequest {
    url: String,
}

/// Fetch a job posting's text from a supported board (Greenhouse/Lever) — the
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
        Err(error) => error_response(502, "fetch_failed", error.to_string()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
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
    async fn get_build_rejects_a_non_numeric_id() {
        let resp = get_build("../etc").await;
        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn get_build_file_rejects_a_bad_filename_before_touching_disk() {
        let resp = get_build_file("041", "../../secret").await;
        assert_eq!(resp.status(), 400);
    }
}
