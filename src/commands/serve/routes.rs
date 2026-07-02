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
use crate::llm::{CompletionRequest, LlmClient, LlmError, TokenUsage};
use crate::pricing;
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
}
