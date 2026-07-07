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

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::{Request, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{
    AppState, Resp, bytes_response, error_response, event_stream_response, json_response, log,
    read_body,
};
use crate::ats;
use crate::builds::{self, BuildError, BuildMeta};
use crate::commands::configured_client;
use crate::commands::tailor::{resolve_ats_template, resolve_human_template};
use crate::config::Config;
use crate::cover::CoverLetter;
use crate::cover_interview::CoverBrief;
use crate::dataset::store;
use crate::dataset::types::ResumeDataset;
use crate::dataset::validate;
use crate::fetch::{self, FetchError};
use crate::gap::GapReport;
use crate::jd::JobRequirements;
use crate::llm::{CompletionRequest, LlmClient, LlmError, StreamEvent, TokenUsage};
use crate::pricing;
use crate::render::{self, RenderError};
use crate::review::AdversarialReport;
use crate::tailor::{TailoredResume, scrub_resume_text};
use crate::templates;
use crate::variant::{self, TemplateId, Variant, VariantPayload};

// ---------------------------------------------------------------------
// POST /api/llm — proxy one completion through the server's credentials
// ---------------------------------------------------------------------

/// Run one completion. The body is a `CompletionRequest` (the `aarg-core` wire
/// type the browser's bridge callback already produces); the server builds a
/// client with the *same* credential resolution the CLI uses
/// ([`configured_client`], which reads env / keychain / a CLI-delegated token).
/// The key never crosses to the browser — that's the whole reason this route
/// exists.
///
/// Two modes, decided by [`stream_mode`]: a client that sends
/// `Accept: text/event-stream` and whose request carries no tools gets the
/// completion streamed as server-sent events, one frame per token; everything
/// else (no Accept, or a tool-bearing request) gets the whole
/// `CompletionResponse` buffered in one JSON response. A tool-bearing request
/// is never streamed because the SSE parser drops non-text content deltas and
/// would silently lose its tool calls.
pub(super) async fn llm(req: Request<Incoming>) -> Resp {
    // Whether the client asked for SSE — read from the headers before
    // `read_body` consumes the request.
    let accepts_event_stream = wants_event_stream(req.headers());

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
    let (client, config) = match configured_client().await {
        Ok(pair) => pair,
        Err(error) => {
            // Both are server-config problems the browser can't fix, so both
            // are 503s, but they are distinct problems: a local provider with
            // no model named is not a credential issue, and labeling it one
            // would send the operator hunting through keychains.
            let kind = match &error {
                crate::commands::CliError::MissingLocalModel { .. } => "no_model",
                _ => "no_credentials",
            };
            return error_response(503, kind, error_chain(&error));
        }
    };
    // A local provider's base URL, so an unreachable-server error can name it
    // and say how to start it; `None` for Anthropic, whose transport errors
    // are plain network failures.
    let base_url = config.active_base_url().map(str::to_string);

    if stream_mode(accepts_event_stream, &request) {
        llm_stream(client, request, base_url).await
    } else {
        llm_buffered(client, request, base_url).await
    }
}

/// The buffered path: wait for the whole completion and return it as one JSON
/// response. This is the original `/api/llm` behavior, unchanged, for a client
/// that sent no `Accept: text/event-stream` (or a tool-bearing request).
async fn llm_buffered(
    client: Box<dyn LlmClient>,
    request: CompletionRequest,
    base_url: Option<String>,
) -> Resp {
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
                clip(&describe_llm_error(&error, base_url.as_deref()), 200)
            ));
            llm_error_response(error, base_url.as_deref())
        }
    }
}

/// The streaming path: open the provider stream and forward each `StreamEvent`
/// as one SSE frame, flushed the moment the model produces it. The stream ends
/// after the terminal `Done` frame (or an error frame). If the stream can't
/// even be *opened* (a 401, a 429, an unreachable host), that surfaces the same
/// way the buffered path surfaces it — a normal JSON error response the
/// browser's non-2xx path handles — because no `200` has been sent yet.
async fn llm_stream(
    client: Box<dyn LlmClient>,
    request: CompletionRequest,
    base_url: Option<String>,
) -> Resp {
    // The stream's own `Done` event doesn't echo the model, so capture it from
    // the request before `stream` consumes it — the `done` frame needs it.
    let model = request.model.clone();

    let stream = match client.stream(request).await {
        Ok(stream) => stream,
        Err(error) => {
            log(&format!(
                "/api/llm upstream failed: {}",
                clip(&describe_llm_error(&error, base_url.as_deref()), 200)
            ));
            return llm_error_response(error, base_url.as_deref());
        }
    };

    event_stream_response(sse_stream_body(stream, model, base_url))
}

/// Turn a provider `TokenStream` into the SSE response body: each `StreamEvent`
/// becomes exactly one `data:` frame, emitted the moment it's polled off the
/// stream (`StreamBody` polls one item at a time, so hyper flushes per event
/// rather than buffering the whole completion). Split from [`llm_stream`] so a
/// test can drive it with a hand-built stream — the route's own `TokenStream`
/// comes from `configured_client`, which isn't injectable.
///
/// The provider task ends the `TokenStream` right after a `Done` or an error,
/// so the mapped stream ends naturally after its terminal frame. The body error
/// type is `Infallible`: a mid-stream provider failure becomes an `error`
/// *frame* (data on the already-`200` stream), never a transport error on the
/// body.
fn sse_stream_body(
    stream: crate::llm::TokenStream,
    model: String,
    base_url: Option<String>,
) -> super::Body {
    let frames = stream.map(move |item| {
        let bytes = match item {
            Ok(StreamEvent::TextDelta(text)) => sse_delta_frame(&text),
            Ok(StreamEvent::Done { stop_reason, usage }) => {
                sse_done_frame(stop_reason.as_deref(), &usage, &model)
            }
            Err(error) => {
                // Same operator-visible line the buffered path writes, then the
                // full chain (with a local-server hint when that fits) goes to
                // the browser as an error frame.
                let message = describe_llm_error(&error, base_url.as_deref());
                log(&format!(
                    "/api/llm upstream failed: {}",
                    clip(&message, 200)
                ));
                sse_error_frame(&message, is_transient(&error))
            }
        };
        Ok::<_, std::convert::Infallible>(Frame::data(bytes))
    });

    StreamBody::new(frames).boxed_unsync()
}

/// The error message to surface, enriched for the one case the raw transport
/// error hides: a local provider whose server is down. A refused or unreachable
/// connection to `base_url` (set only for a local provider) reads as an opaque
/// "could not reach" chain, so name the server and how to start it. Anything
/// else (an Anthropic network blip, an API rejection) passes through as its
/// plain cause chain.
fn describe_llm_error(error: &LlmError, base_url: Option<&str>) -> String {
    let chain = error_chain(error);
    match base_url {
        Some(base) if looks_unreachable(&chain) => format!(
            "{chain} · the local model server at {base} is not responding; start LM Studio (or run `ollama serve`), or fix the provider's base_url in your aarg config"
        ),
        _ => chain,
    }
}

/// Whether an error chain looks like a refused, unreachable, or timed-out
/// connection, the shapes a down or hung local server produces. A server that
/// is listening but never answers (a wedged process, a model stuck loading)
/// surfaces as reqwest's timeout rather than a refusal, and deserves the same
/// start-the-server hint. Loose substring matching, since the exact phrasing
/// varies by platform and reqwest version.
fn looks_unreachable(chain: &str) -> bool {
    let lower = chain.to_lowercase();
    lower.contains("connection refused")
        || lower.contains("tcp connect")
        || lower.contains("connect error")
        || lower.contains("could not reach")
        || lower.contains("timed out")
        || lower.contains("timeout")
}

/// Whether a request's `Accept` header asks for an SSE stream. A browser
/// `fetch` sets exactly `text/event-stream`; the parse is lenient (split the
/// comma list, drop any `;q=` parameter) so it also matches when the type is
/// offered among several.
fn wants_event_stream(headers: &hyper::HeaderMap) -> bool {
    headers
        .get(hyper::header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(accept_has_event_stream)
}

/// Whether an `Accept` header value lists `text/event-stream` among its media
/// types (ignoring any `;q=`/other parameter on each).
fn accept_has_event_stream(accept: &str) -> bool {
    accept
        .split(',')
        .any(|media| media.split(';').next().map(str::trim) == Some("text/event-stream"))
}

/// The streaming decision: stream only when the client asked for SSE AND the
/// request carries no tools. A tool-bearing request must be buffered — the SSE
/// parser skips non-text content deltas, so streaming one would silently drop
/// its tool calls (a correctness bug, not just a formatting one).
fn stream_mode(accepts_event_stream: bool, request: &CompletionRequest) -> bool {
    accepts_event_stream && request.tools.is_empty()
}

/// One SSE `data:` frame: `data: <json>\n\n`. The blank line terminates the
/// event, which is what makes hyper (and any intermediary) flush it.
fn sse_data_frame(payload: &Value) -> hyper::body::Bytes {
    hyper::body::Bytes::from(format!("data: {payload}\n\n"))
}

/// A token-delta frame: `data: {"delta":"<text>"}\n\n`. `serde_json` does the
/// escaping, so a delta with a quote, newline, or backslash stays valid JSON.
fn sse_delta_frame(text: &str) -> hyper::body::Bytes {
    sse_data_frame(&json!({ "delta": text }))
}

/// The terminal frame: `data: {"done":{...}}\n\n`, carrying the final stop
/// reason, the token usage, and the model the request named (the stream's own
/// `Done` event doesn't echo the model, so it's threaded through from the
/// request).
fn sse_done_frame(
    stop_reason: Option<&str>,
    usage: &TokenUsage,
    model: &str,
) -> hyper::body::Bytes {
    sse_data_frame(&json!({
        "done": { "stop_reason": stop_reason, "usage": usage, "model": model }
    }))
}

/// A mid-stream error frame: `data: {"error":"<chain>"}\n\n`. The message is
/// the full cause chain (see [`error_chain`]); like every other error surface
/// here, it never carries key material.
fn sse_error_frame(message: &str, retryable: bool) -> hyper::body::Bytes {
    sse_data_frame(&json!({ "error": message, "retryable": retryable }))
}

/// Whether an in-stream failure is the transient class a client should retry.
/// Anthropic delivers overloads inside the stream (the streaming twin of an
/// HTTP 529), so without this tag a browser run would abort at peak hours
/// where the buffered path would have backed off and continued.
fn is_transient(error: &crate::llm::LlmError) -> bool {
    let chain = error_chain(error).to_lowercase();
    chain.contains("overloaded") || chain.contains("rate_limit") || chain.contains("rate limit")
}

/// Render an error with its full cause chain ("top: cause: cause"), because
/// the top Display alone can be content-free: reqwest's "could not reach the
/// LLM API" hides whether DNS failed, the connection was refused, or a
/// timeout fired, and that cause is exactly what an operator (or the browser
/// toast) needs.
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut out = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        out.push_str(": ");
        out.push_str(&cause.to_string());
        source = cause.source();
    }
    out
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
/// missing key is a 503 (server misconfigured); a request the provider cannot
/// serve (a PDF on a local model, a clipped context) is a 400 — the client
/// asked for something this provider can't do, and no upstream failed; a
/// provider rejection passes its own HTTP status through; anything else is a
/// 502 upstream error.
fn llm_error_response(error: LlmError, base_url: Option<&str>) -> Resp {
    match error {
        LlmError::MissingApiKey { .. } => {
            error_response(503, "no_credentials", error_chain(&error))
        }
        LlmError::Unsupported(ref message) => error_response(400, "unsupported", message.clone()),
        LlmError::Api {
            status,
            ref kind,
            ref message,
        } => {
            // Reuse the provider's own status when it's a valid HTTP error
            // code; the message is the provider's, which never echoes the
            // key. A 2xx here means the provider hid an error inside a
            // success reply (LM Studio does this when a reasoning budget
            // runs out), and passing it through would let a client read the
            // error body as a completed call.
            let code = match StatusCode::from_u16(status) {
                Ok(_) if status >= 400 => status,
                _ => 502,
            };
            error_response(code, kind, message.clone())
        }
        // A transport failure, including a down local server, named via
        // `base_url` when that's the cause.
        other => error_response(502, "upstream", describe_llm_error(&other, base_url)),
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
    let Ok(root) = crate::builds::builds_root() else {
        return error_response(500, "internal", "could not locate the builds directory");
    };
    get_build_in(&root, id)
}

/// The disk-touching half of [`get_build`], with the builds root injected so a
/// test can drive it against a tempdir — the same split [`delete_build_in`]
/// uses for the same reason.
fn get_build_in(root: &Path, id: &str) -> Resp {
    if !is_numeric_id(id) {
        return error_response(404, "not_found", format!("no build {id:?}"));
    }
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
        // The parsed cover letter (not just its rendered PDF, which the browser
        // already reaches via `GET .../files/cover_letter.pdf`). Present only
        // once a cover letter has been drafted for this build — a build can be
        // a résumé alone — so a later editing view has the structured data to
        // feed the provenance checker without re-parsing the PDF.
        ("cover_payload", "cover_payload.json"),
        // The persisted cover-letter interview brief, when either surface saved
        // one for this build (the CLI's `aarg cover --interactive`, the
        // browser's "Draft with copilot", or a paragraph confirmed as evidence
        // via `POST .../cover-brief`). Feeds the Editing view's local
        // provenance re-check as grounding, the same way it grounds a draft.
        ("cover_brief", "cover_brief.json"),
    ] {
        if let Some(value) = read_json_artifact(&dir.join(file)) {
            obj.insert(key.into(), value);
        }
    }
    // Triage is always present (unlike the best-effort artifacts above): a build
    // with no `triage.json` yet has left nothing, so it reads as an empty list
    // rather than an omitted key — the browser initializes its "left for now"
    // set straight from `triage.left` without a missing-key branch.
    obj.insert("triage".into(), json!({ "left": read_triage(&dir).left }));
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

// ---------------------------------------------------------------------
// DELETE /api/builds/:id — remove a build and all its artifacts
// ---------------------------------------------------------------------

/// `DELETE /api/builds/:id` — delete a build's directory and everything under
/// it, the same on-disk removal `aarg history rm` performs. Both paths go
/// through [`crate::history::remove_in`], so there is one deletion code path,
/// not two. The write is serialized through `AppState.build_write`, the mutex
/// the edit and triage saves also take, so a delete can never race a save on
/// the same build's files. A missing (or already-deleted) build is a 404;
/// success returns `{"removed": "<id>"}`.
pub(super) async fn delete_build(id: &str, state: &AppState) -> Resp {
    let Ok(root) = crate::builds::builds_root() else {
        return error_response(500, "internal", "could not locate the builds directory");
    };
    let _guard = state.build_write.lock().await;
    delete_build_in(&root, id)
}

/// The disk-touching half of [`delete_build`], split out with the builds root
/// injected so a test can drive it against a tempdir. The id is validated with
/// the same [`is_numeric_id`] gate the other build routes use before any path
/// is built; `remove_in` guards the id a second time as defense in depth.
fn delete_build_in(root: &Path, id: &str) -> Resp {
    if !is_numeric_id(id) {
        return error_response(404, "not_found", format!("no build {id:?}"));
    }
    match crate::history::remove_in(root, id) {
        Ok(()) => json_response(200, &json!({ "removed": id })),
        Err(crate::history::HistoryError::NotFound { .. }) => {
            error_response(404, "not_found", format!("no build {id:?}"))
        }
        Err(error) => error_response(500, "internal", error.to_string()),
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
    // stamps into `meta.json`. Read from `Config` alone (via [`Config::billing`])
    // so persisting a build never touches the keychain: this route saves a
    // build, it does not spend the key. A local build is not a plan, so this is
    // false there and the cost column falls back to tokens.
    let subscription = matches!(config.billing(), crate::config::Billing::Subscription);

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
// POST /api/builds/:id/triage — persist objection triage for a build
// ---------------------------------------------------------------------

/// A build's objection triage (`triage.json`), stored alongside `edit_log.json`:
/// which objection ids the user has *left for now*. Where an Accept persists a
/// dismissal into the dataset (so the reviewer never raises it again), Leave is
/// a per-build "I'll come back to this" that must survive a reload — hence a
/// small file per build rather than dataset state. The body is a full
/// replacement (`{"left": [...]}`), so a save is idempotent and a reopen is just
/// a save with the id removed.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
struct BuildTriage {
    #[serde(default)]
    left: Vec<String>,
}

/// The most objection ids a single build's triage can carry. A build has a
/// handful of objections, never hundreds, so this cap only exists to refuse a
/// client that tried to smuggle an unbounded list into the file.
const MAX_TRIAGE_IDS: usize = 200;

/// Parse and validate a triage body: it must deserialize to `{"left": [<string>,
/// ...]}` (a non-string element, or a non-object body, is a parse error) and
/// carry no more than [`MAX_TRIAGE_IDS`] ids. Split from the handler so a test
/// can exercise the garbage/oversized rejections without a socket.
fn parse_triage(body: &[u8]) -> Result<BuildTriage, String> {
    // Parse to a `Value` first and insist it's an object: serde will otherwise
    // deserialize a struct from a JSON *array* (matching fields positionally), so
    // a bare `[]` would sneak through as an empty `left`. The triage body is an
    // object or it's garbage.
    let value: Value =
        serde_json::from_slice(body).map_err(|error| format!("invalid triage JSON: {error}"))?;
    if !value.is_object() {
        return Err("triage body must be an object like {\"left\": [...]}".to_string());
    }
    let triage: BuildTriage =
        serde_json::from_value(value).map_err(|error| format!("invalid triage JSON: {error}"))?;
    if triage.left.len() > MAX_TRIAGE_IDS {
        return Err(format!(
            "too many triage ids: {} exceeds the {MAX_TRIAGE_IDS}-id cap",
            triage.left.len()
        ));
    }
    Ok(triage)
}

/// Read a build's stored triage, defaulting to empty when the file is absent or
/// unreadable — a build with no `triage.json` has left nothing. Used by the GET
/// bundle so the browser always gets a `left` list to initialize from.
fn read_triage(dir: &std::path::Path) -> BuildTriage {
    read_json_artifact(&dir.join("triage.json"))
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

/// Replace a build's objection triage with the posted `{"left": [...]}`. Writes
/// are serialized through `AppState.build_write` — the same mutex the edit log
/// uses — so a triage save can't race an edit save on the same build's files.
/// The body is validated before the id/dir checks touch disk, and the write
/// itself is a single tiny JSON file (no render), so it runs inline under the
/// guard rather than on a blocking thread the way `save_build_edits` does.
pub(super) async fn save_build_triage(req: Request<Incoming>, id: &str, state: &AppState) -> Resp {
    if !is_numeric_id(id) {
        return error_response(404, "not_found", format!("no build {id:?}"));
    }
    let body = match read_body(req).await {
        Ok(body) => body,
        Err(resp) => return resp,
    };
    let triage = match parse_triage(&body) {
        Ok(triage) => triage,
        Err(message) => return error_response(400, "bad_request", message),
    };
    let Ok(root) = crate::builds::builds_root() else {
        return error_response(500, "internal", "could not locate the builds directory");
    };
    let dir = root.join(id);
    if !dir.is_dir() {
        return error_response(404, "not_found", format!("no build {id:?}"));
    }

    let _guard = state.build_write.lock().await;
    match builds::write_json(&dir, "triage.json", &triage) {
        Ok(()) => json_response(200, &json!({ "status": "saved" })),
        Err(error) => error_response(500, "build_error", error.to_string()),
    }
}

// ---------------------------------------------------------------------
// POST /api/builds/:id/cover — draft (or redraft) a cover letter server-side
// ---------------------------------------------------------------------

/// The `POST /api/builds/:id/cover` success body: the drafted letter (so the
/// browser can show it at once), any never-fabricate warnings the draft raised,
/// the persisted PDF filename the pixel preview fetches, and the model + token
/// usage for a cost line. The letter's `contact` block and `signoff` are the
/// resume's, filled by code inside the agent — never the model's — so the same
/// honesty posture `aarg cover` has carries over unchanged.
#[derive(Serialize)]
struct GenerateCoverResponse {
    letter: CoverLetter,
    warnings: Vec<String>,
    pdf: String,
    model: String,
    usage: TokenUsage,
}

/// The `POST /api/builds/:id/cover` request body — entirely optional. An absent
/// `brief` (or an absent/empty body) means a plain draft with no interview
/// grounding, exactly as before this field existed.
#[derive(Deserialize, Default)]
struct GenerateCoverRequest {
    #[serde(default)]
    brief: Option<CoverBrief>,
}

/// Whether a `CoverBrief` carries nothing at all — every scalar blank, both
/// lists empty. Mirrors the browser's own `isEmptyBrief` exactly (see
/// `cover-view.ts`), so an interview the person backed out of before answering
/// anything is never written to disk as a brief with nothing in it.
fn cover_brief_is_empty(brief: &CoverBrief) -> bool {
    fn blank(value: &Option<String>) -> bool {
        value.as_deref().map(str::trim).unwrap_or("").is_empty()
    }
    blank(&brief.angle)
        && brief.emphasis.is_empty()
        && blank(&brief.tone)
        && blank(&brief.motivation)
        && brief.constraints.is_empty()
}

/// Persist a non-empty interview brief to `cover_brief.json` in `dir`, the
/// piece `generate_build_cover` was missing: it drafted from a request-body
/// `brief` but never saved it, so a browser-drafted letter had no saved brief
/// to recover later, and a fact recorded only in the "Draft with copilot"
/// interview could never ground a later provenance re-check (it had nothing to
/// read). Mirrors the CLI's own save (`commands::cover::interview_brief`,
/// which writes `cover_brief.json` next to a build's other artifacts) so a
/// build carries the same recoverable brief regardless of which surface drafted
/// it. An absent or empty brief is left unwritten — nothing to recover.
fn persist_cover_brief_if_present(
    dir: &std::path::Path,
    brief: Option<&CoverBrief>,
) -> Result<(), BuildError> {
    match brief {
        Some(brief) if !cover_brief_is_empty(brief) => {
            builds::write_json(dir, "cover_brief.json", brief)
        }
        _ => Ok(()),
    }
}

/// `POST /api/builds/:id/cover` — draft a cover letter for an existing build and
/// render it into the build directory: the browser's way into the same
/// [`CoverLetterAgent`](crate::cover::CoverLetterAgent) the CLI's `aarg cover`
/// runs. Loads the build's canonical resume and JD (the evidence-traced facts
/// the letter is grounded in) plus the dataset's voice samples for tone, spends
/// the key through [`configured_client`] (like `/api/llm` — this is the one
/// build route that does), runs the agent, then renders `cover_letter.pdf` on a
/// blocking thread (the `typst` subprocess + file writes). Regenerating simply
/// overwrites the build's prior cover in place.
///
/// Because it spends the key, this route sits behind the same content-type gate
/// `/api/llm` does (see `serve::requires_json_body`), which is what keeps a
/// drive-by cross-origin POST from triggering a paid draft. The request body is
/// optional JSON, `{"brief"?: CoverBrief}` — an absent field, an absent `brief`
/// key, or an empty body all mean "no interview", the same as `aarg cover`
/// without `--interactive`. When present, `brief` is the result of a prior
/// `cover_interview_interactive` session (the browser's cover-letter copilot)
/// and is handed straight to `write_cover_letter` as grounding; every string it
/// carries already passed that interview's own never-fabricate guard (see
/// `cover_interview`'s module doc), so this route adds no new claim path. A
/// non-empty `brief` is also persisted to the build's `cover_brief.json`
/// ([`persist_cover_brief_if_present`]) before the draft is spent, so the
/// interview survives the request regardless of how the draft itself turns out.
pub(super) async fn generate_build_cover(
    req: Request<Incoming>,
    id: &str,
    state: &AppState,
) -> Resp {
    // Checked before touching the body, like every other id-guarded route: a
    // traversal-shaped id is refused before any bytes are read off the wire.
    if !is_numeric_id(id) {
        return error_response(404, "not_found", format!("no build {id:?}"));
    }
    let body = match read_body(req).await {
        Ok(body) => body,
        Err(resp) => return resp,
    };
    let brief: Option<CoverBrief> = if body.is_empty() {
        None
    } else {
        match serde_json::from_slice::<GenerateCoverRequest>(&body) {
            Ok(request) => request.brief,
            Err(error) => {
                return error_response(
                    400,
                    "bad_request",
                    format!("invalid cover request: {error}"),
                );
            }
        }
    };
    // Fail fast with the install message if typst is absent, before spending the
    // key on a draft that could never render — the same gate create_build uses.
    if let Err(error) = render::ensure_available() {
        return render_error_response(error);
    }

    let Ok(root) = crate::builds::builds_root() else {
        return error_response(500, "internal", "could not locate the builds directory");
    };
    let dir = root.join(id);
    if !dir.is_dir() {
        return error_response(404, "not_found", format!("no build {id:?}"));
    }

    // Save a non-empty brief before spending the key: the interview it came
    // from is worth recovering even if the draft below fails.
    if let Err(error) = persist_cover_brief_if_present(&dir, brief.as_ref()) {
        return error_response(500, "build_error", error.to_string());
    }

    // The resume and JD the letter is grounded in come straight off the build; a
    // build missing either cannot have a letter drawn honestly from it, so an
    // absent artifact is a 404 that names it (via `read_artifact`'s message).
    let resume: TailoredResume = match crate::history::read_artifact(id, "canonical.json") {
        Ok(resume) => resume,
        Err(error) => return error_response(404, "not_found", error.to_string()),
    };
    let jd: JobRequirements = match crate::history::read_artifact(id, "jd.json") {
        Ok(jd) => jd,
        Err(error) => return error_response(404, "not_found", error.to_string()),
    };
    // Voice samples only anchor tone, so they're optional: a workspace with no
    // dataset yet drafts plainly rather than failing. A dataset that exists but
    // won't read is a real 500.
    let samples: Vec<String> = match store::load() {
        Ok(dataset) => dataset
            .voice_samples
            .iter()
            .map(|sample| sample.text.clone())
            .collect(),
        Err(store::DatasetError::NotFound { .. }) => Vec::new(),
        Err(error) => return error_response(500, "dataset_error", error.to_string()),
    };

    // Spend the key through the same credential resolution the CLI and /api/llm
    // use; a credential problem is a 503 the browser can't fix, never a leak of
    // what the key is. A local provider with no model named is a distinct 503.
    let (client, config) = match configured_client().await {
        Ok(pair) => pair,
        Err(error) => {
            let kind = match &error {
                crate::commands::CliError::MissingLocalModel { .. } => "no_model",
                _ => "no_credentials",
            };
            return error_response(503, kind, error_chain(&error));
        }
    };
    let base_url = config.active_base_url().map(str::to_string);
    let model = config
        .active_resolver()
        .resolve("cover_letter_v1", crate::agent::ModelTier::Mid)
        .to_string();

    // Trace to the workspace like the CLI does; a workspace with no data dir just
    // means no trace, never a failed request.
    let tracer = crate::commands::default_tracer().unwrap_or(crate::trace::Tracer::DISABLED);
    let ctx = crate::agent::AgentContext {
        llm: &*client,
        model: config.active_resolver(),
        tracer: &tracer,
        sink: None,
    };
    let (letter, warnings, usage) = match crate::cover::write_cover_letter(
        &ctx,
        &resume,
        &jd,
        &samples,
        brief.as_ref(),
    )
    .await
    {
        Ok(triple) => triple,
        Err(error) => return cover_error_response(error, base_url.as_deref()),
    };

    // Render + persist off the async worker (the `typst` subprocess and file
    // writes are blocking), serialized through the build-write mutex so it can't
    // race a delete of the same build. `render_cover` writes `cover_letter.pdf`
    // and `cover_payload.json` into the build dir, overwriting any prior cover.
    // The LLM call above already finished, so the lock is held only for the brief
    // render, not across the network round-trip.
    let letter_for_render = letter.clone();
    let _guard = state.build_write.lock().await;
    let rendered = tokio::task::spawn_blocking(move || {
        render::render_cover(&dir, &letter_for_render, &render::Template::cover())
    })
    .await;
    match rendered {
        Ok(Ok(pdf)) => {
            let pdf = pdf
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("cover_letter.pdf")
                .to_string();
            json_response(
                200,
                &GenerateCoverResponse {
                    letter,
                    warnings,
                    pdf,
                    model,
                    usage,
                },
            )
        }
        Ok(Err(error)) => render_error_response(error),
        Err(join) => error_response(
            500,
            "internal",
            format!("the cover render task did not complete: {join}"),
        ),
    }
}

/// Map a [`crate::cover::CoverLetterError`] to a response. An LLM failure gets
/// the exact shape `/api/llm` produces (via [`llm_error_response`]), so the
/// browser's existing non-2xx handling covers it unchanged; a malformed or empty
/// model reply is the upstream model's fault, not the client's, so it's a 502.
/// No message carries key material.
fn cover_error_response(error: crate::cover::CoverLetterError, base_url: Option<&str>) -> Resp {
    match error {
        crate::cover::CoverLetterError::Llm(error) => llm_error_response(error, base_url),
        other => error_response(502, "cover_failed", other.to_string()),
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
                clip(&error_chain(&error), 200)
            ));
            error_response(502, "fetch_failed", error_chain(&error))
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
/// needs or should pay for just to answer "would this be free." How the active
/// provider is billed is knowable from `Config` alone (via [`Config::billing`]),
/// so this route only ever touches the config file, never the keychain.
/// `GET /api/models` — the model tiers the browser should send with its
/// in-browser LLM work, read from the same `Config` the CLI resolves against.
/// Returning the three *resolved* tiers (not one hardcoded model) lets a
/// browser build spend the cheap tier where the CLI does and reserve the smart
/// model for the reviewer/tailor — so an in-browser build costs and reads like
/// a terminal one instead of running everything on the priciest model. Config
/// only; no keychain, no key spend.
pub(super) async fn models() -> Resp {
    use crate::agent::ModelTier;
    let config = match Config::load() {
        Ok(config) => config,
        Err(error) => return error_response(500, "config_error", error.to_string()),
    };
    // Resolve through the active provider, so a browser build on a local
    // provider spends the local models the CLI would, not the Anthropic tiers.
    let resolver = config.active_resolver();
    json_response(
        200,
        &json!({
            "cheap": resolver.resolve("", ModelTier::Cheap),
            "mid": resolver.resolve("", ModelTier::Mid),
            "smart": resolver.resolve("", ModelTier::Smart),
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
    // surface as an error; treat it as the default (metered Anthropic, no
    // price overrides) and still answer off the built-in family rates.
    let config = Config::load().unwrap_or_default();

    json_response(200, &cost_body(&query, config.billing(), &config.prices))
}

/// Build the `{"usd": ..., "subscription_note": ...}` body from how the active
/// provider is billed. `usd` is a real figure only for a metered API key (and
/// even then `null` for a model absent from the price table; an unpriced model
/// is never guessed at). For a Claude plan or a local model a dollar figure
/// would mislead, so `usd` is `null` and `subscription_note` carries the plain
/// reason the browser shows instead (the same field the frontend already reads,
/// so no client change is needed). Split out from [`cost`] so the calculation
/// is unit-testable without a keychain or a config file on disk.
fn cost_body(
    query: &CostQuery,
    billing: crate::config::Billing,
    prices: &std::collections::BTreeMap<String, pricing::Price>,
) -> Value {
    use crate::config::Billing;
    match billing {
        Billing::Subscription => {
            json!({ "usd": Value::Null, "subscription_note": "covered by your Claude plan" })
        }
        Billing::Local => {
            json!({ "usd": Value::Null, "subscription_note": "local model, no cost" })
        }
        Billing::Metered => {
            let usage = TokenUsage {
                input_tokens: query.input,
                output_tokens: query.output,
            };
            let usd = pricing::cost_usd(&query.model, &usage, prices);
            json!({ "usd": usd, "subscription_note": Value::Null })
        }
    }
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

    /// A tool-free `CompletionRequest`, the shape a browser build sends for
    /// every streamable call.
    fn tool_free_request() -> CompletionRequest {
        serde_json::from_value(json!({
            "model": "claude-opus-4-8",
            "max_tokens": 64,
            "system": null,
            "messages": [{ "role": "user", "content": "hi" }],
            "temperature": null
        }))
        .unwrap()
    }

    #[test]
    fn accept_header_detects_event_stream_leniently() {
        assert!(accept_has_event_stream("text/event-stream"));
        // Offered among several types, with a q-value, still counts.
        assert!(accept_has_event_stream(
            "text/html, text/event-stream;q=0.9"
        ));
        assert!(accept_has_event_stream("text/event-stream; charset=utf-8"));
        // A plain JSON or wildcard accept does not opt into streaming.
        assert!(!accept_has_event_stream("application/json"));
        assert!(!accept_has_event_stream("*/*"));
        assert!(!accept_has_event_stream(""));
    }

    #[test]
    fn stream_mode_needs_both_the_accept_header_and_no_tools() {
        // The happy case: the client asked for SSE and the request is tool-free.
        assert!(stream_mode(true, &tool_free_request()));
        // No Accept header: buffered, even without tools.
        assert!(!stream_mode(false, &tool_free_request()));
        // A tool-bearing request is buffered even when SSE was requested — the
        // SSE parser drops non-text deltas and would lose the tool calls.
        let mut with_tools = tool_free_request();
        with_tools.tools.push(crate::llm::ToolSpec {
            name: "lookup".into(),
            description: "look something up".into(),
            input_schema: json!({ "type": "object" }),
        });
        assert!(!stream_mode(true, &with_tools));
    }

    #[test]
    fn looks_unreachable_matches_down_and_hung_servers_only() {
        // A refused TCP connect (server down) and a timeout (server hung or a
        // model stuck loading) both get the hint.
        assert!(looks_unreachable(
            "could not reach the LLM API: error sending request: tcp connect error: Connection refused (os error 61)"
        ));
        assert!(looks_unreachable(
            "could not reach the LLM API: error sending request: operation timed out"
        ));
        // A provider rejection is not a connectivity problem.
        assert!(!looks_unreachable(
            "the API rejected the request (HTTP 400, invalid_request_error): bad model"
        ));
    }

    #[test]
    fn describe_llm_error_adds_the_local_hint_only_when_it_applies() {
        let base = Some("http://127.0.0.1:1234");

        // Refused connection with a local base_url: the hint names the server.
        let refused = LlmError::Stream("tcp connect error: Connection refused".to_string());
        let message = describe_llm_error(&refused, base);
        assert!(message.contains("http://127.0.0.1:1234"), "got: {message}");
        assert!(message.contains("start LM Studio"), "got: {message}");

        // Timeout with a local base_url: same hint (a hung server needs the
        // same remedy as a down one).
        let hung = LlmError::Stream("error sending request: operation timed out".to_string());
        let message = describe_llm_error(&hung, base);
        assert!(message.contains("ollama serve"), "got: {message}");

        // A non-network error keeps its plain chain, no hint.
        let rejected = LlmError::Api {
            status: 400,
            kind: "invalid_request_error".to_string(),
            message: "bad model".to_string(),
        };
        let message = describe_llm_error(&rejected, base);
        assert!(!message.contains("start LM Studio"), "got: {message}");

        // No base_url (Anthropic): never a local hint, even on a refusal.
        let message = describe_llm_error(&refused, None);
        assert!(!message.contains("start LM Studio"), "got: {message}");
    }

    #[test]
    fn a_provider_error_hidden_in_a_success_status_becomes_a_502() {
        // LM Studio reports an exhausted reasoning budget as an error inside
        // an HTTP 200. The route must not hand that 200 to the browser: a
        // client checking `res.ok` would parse the error body as a
        // completion.
        let hidden = LlmError::Api {
            status: 200,
            kind: "empty_reply".to_string(),
            message: "the model produced no text".to_string(),
        };
        let resp = llm_error_response(hidden, None);
        assert_eq!(resp.status(), 502);

        // Real provider rejections keep their own status.
        let rejected = LlmError::Api {
            status: 429,
            kind: "rate_limit".to_string(),
            message: "slow down".to_string(),
        };
        let resp = llm_error_response(rejected, None);
        assert_eq!(resp.status(), 429);
    }

    #[test]
    fn sse_frames_serialize_to_the_documented_wire_shape() {
        // A delta frame: `data: {"delta":"..."}\n\n`, with JSON escaping.
        let delta = sse_delta_frame("he\"llo\n");
        assert_eq!(
            std::str::from_utf8(&delta).unwrap(),
            "data: {\"delta\":\"he\\\"llo\\n\"}\n\n"
        );

        // A done frame carries stop_reason, usage, and the threaded-through model.
        let usage = TokenUsage {
            input_tokens: 12,
            output_tokens: 34,
        };
        let done = sse_done_frame(Some("end_turn"), &usage, "claude-opus-4-8");
        let text = std::str::from_utf8(&done).unwrap();
        assert!(text.starts_with("data: "));
        assert!(text.ends_with("\n\n"));
        let payload: Value =
            serde_json::from_str(text.trim_start_matches("data: ").trim_end()).unwrap();
        assert_eq!(payload["done"]["stop_reason"], "end_turn");
        assert_eq!(payload["done"]["usage"]["input_tokens"], 12);
        assert_eq!(payload["done"]["usage"]["output_tokens"], 34);
        assert_eq!(payload["done"]["model"], "claude-opus-4-8");

        // An error frame carries the chained message verbatim, tagged with
        // whether the client should retry it.
        let err = sse_error_frame("could not reach the LLM API: connection refused", false);
        let payload: Value = serde_json::from_str(
            std::str::from_utf8(&err)
                .unwrap()
                .trim_start_matches("data: ")
                .trim_end(),
        )
        .unwrap();
        assert_eq!(
            payload["error"],
            "could not reach the LLM API: connection refused"
        );
        assert_eq!(payload["retryable"], false);

        // In-stream overloads are the streaming twin of an HTTP 529 and get
        // tagged for the client's retry budget.
        let overload = sse_error_frame("the stream reported an error: overloaded_error", true);
        let payload: Value = serde_json::from_str(
            std::str::from_utf8(&overload)
                .unwrap()
                .trim_start_matches("data: ")
                .trim_end(),
        )
        .unwrap();
        assert_eq!(payload["retryable"], true);
        assert!(is_transient(&crate::llm::LlmError::Stream(
            "overloaded_error: try again".into()
        )));

        // A done frame with no stop reason nulls it rather than omitting it.
        let done = sse_done_frame(None, &TokenUsage::default(), "m");
        let payload: Value = serde_json::from_str(
            std::str::from_utf8(&done)
                .unwrap()
                .trim_start_matches("data: ")
                .trim_end(),
        )
        .unwrap();
        assert!(payload["done"]["stop_reason"].is_null());
    }

    /// Collect a boxed body into one string — the bytes hyper would write to
    /// the socket, concatenated.
    async fn body_to_string(body: super::super::Body) -> String {
        let bytes = body.collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn sse_stream_body_emits_one_frame_per_event_in_order() {
        // A hand-built provider stream: two text deltas then a Done — the shape
        // `AnthropicClient::stream` produces. `sse_stream_body` isn't reachable
        // through a live socket without a credential, so drive it directly.
        let events: Vec<Result<StreamEvent, LlmError>> = vec![
            Ok(StreamEvent::TextDelta("Hel".into())),
            Ok(StreamEvent::TextDelta("lo".into())),
            Ok(StreamEvent::Done {
                stop_reason: Some("end_turn".into()),
                usage: TokenUsage {
                    input_tokens: 3,
                    output_tokens: 2,
                },
            }),
        ];
        let stream: crate::llm::TokenStream = Box::pin(futures_util::stream::iter(events));
        let body = sse_stream_body(stream, "claude-mock".into(), None);
        let wire = body_to_string(body).await;

        // Exactly three frames, each its own `data: ...\n\n`, in order.
        let frames: Vec<&str> = wire.split_terminator("\n\n").collect();
        assert_eq!(frames.len(), 3, "wire = {wire:?}");
        assert_eq!(frames[0], r#"data: {"delta":"Hel"}"#);
        assert_eq!(frames[1], r#"data: {"delta":"lo"}"#);
        let done: Value = serde_json::from_str(frames[2].trim_start_matches("data: ")).unwrap();
        assert_eq!(done["done"]["stop_reason"], "end_turn");
        assert_eq!(done["done"]["model"], "claude-mock");
        assert_eq!(done["done"]["usage"]["output_tokens"], 2);
    }

    #[tokio::test]
    async fn sse_stream_body_forwards_a_midstream_error_as_a_final_error_frame() {
        // One delta, then the provider stream yields an error: it must reach the
        // browser as a trailing `error` frame, not tear the body.
        let events: Vec<Result<StreamEvent, LlmError>> = vec![
            Ok(StreamEvent::TextDelta("partial".into())),
            Err(LlmError::Stream("the upstream fell over".into())),
        ];
        let stream: crate::llm::TokenStream = Box::pin(futures_util::stream::iter(events));
        let wire = body_to_string(sse_stream_body(stream, "m".into(), None)).await;

        let frames: Vec<&str> = wire.split_terminator("\n\n").collect();
        assert_eq!(frames.len(), 2, "wire = {wire:?}");
        assert_eq!(frames[0], r#"data: {"delta":"partial"}"#);
        let err: Value = serde_json::from_str(frames[1].trim_start_matches("data: ")).unwrap();
        assert!(
            err["error"]
                .as_str()
                .unwrap()
                .contains("the upstream fell over"),
            "error frame = {frames:?}"
        );
    }

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

    /// `cover_payload` follows the exact best-effort convention as
    /// `human_payload`/`ats_payload`: present (and parsed) when
    /// `cover_payload.json` is on disk, quietly absent for a build that has
    /// only a résumé and no cover letter yet — never an error either way.
    #[tokio::test]
    async fn get_build_includes_cover_payload_when_present_and_omits_it_when_absent() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("001");
        std::fs::create_dir_all(&dir).unwrap();
        // A build needs at least one artifact to read as present at all.
        builds::write_json(&dir, "jd.json", &json!({})).unwrap();

        // No cover letter drafted yet: the key is simply missing.
        let resp = get_build_in(root.path(), "001");
        assert_eq!(resp.status(), 200);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            body.get("cover_payload").is_none(),
            "unexpected cover_payload: {body:?}"
        );

        // Draft one, write it the same way `POST .../cover` does, and it
        // shows up parsed, not as an opaque blob.
        let letter = CoverLetter {
            contact: crate::dataset::types::Contact {
                full_name: "Ada Lovelace".into(),
                email: "ada@example.com".into(),
                phone: None,
                location: None,
                links: Vec::new(),
            },
            company: "Acme".into(),
            title: "Engineer".into(),
            greeting: "Dear Hiring Manager,".into(),
            paragraphs: vec!["I would love to build things.".into()],
            signoff: "Sincerely,".into(),
        };
        builds::write_json(&dir, "cover_payload.json", &letter).unwrap();

        let resp = get_build_in(root.path(), "001");
        assert_eq!(resp.status(), 200);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["cover_payload"]["company"], "Acme");
        assert_eq!(
            body["cover_payload"]["paragraphs"][0],
            "I would love to build things."
        );
    }

    /// A non-empty brief is written verbatim; an empty one (every field blank,
    /// both lists empty — the shape an abandoned interview yields) and an
    /// absent one are both left unwritten. This is the disk-touching core of
    /// the brief-persistence fix `generate_build_cover` was missing.
    #[test]
    fn persist_cover_brief_if_present_writes_only_a_non_empty_brief() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("001");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cover_brief.json");

        // No brief at all: nothing written.
        persist_cover_brief_if_present(&dir, None).unwrap();
        assert!(!path.exists());

        // An entirely empty brief (an abandoned interview): still nothing.
        persist_cover_brief_if_present(&dir, Some(&CoverBrief::default())).unwrap();
        assert!(!path.exists());

        // A brief with even one answer is written verbatim.
        let brief = CoverBrief {
            angle: Some("position me as a builder, not just an IC".into()),
            ..CoverBrief::default()
        };
        persist_cover_brief_if_present(&dir, Some(&brief)).unwrap();
        let stored: CoverBrief =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(stored, brief);
    }

    /// `cover_brief` follows the same best-effort convention `cover_payload`
    /// does: present when `cover_brief.json` is on disk (whether the CLI, the
    /// copilot interview, or a "confirm as evidence" action wrote it), quietly
    /// absent otherwise — so the Editing view's re-check can thread it in as
    /// grounding the moment a build has one.
    #[tokio::test]
    async fn get_build_includes_cover_brief_when_present_and_omits_it_when_absent() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("001");
        std::fs::create_dir_all(&dir).unwrap();
        builds::write_json(&dir, "jd.json", &json!({})).unwrap();

        let resp = get_build_in(root.path(), "001");
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            body.get("cover_brief").is_none(),
            "unexpected cover_brief: {body:?}"
        );

        let brief = CoverBrief {
            emphasis: vec!["shipped ChessCoach, a chess training app".into()],
            ..CoverBrief::default()
        };
        builds::write_json(&dir, "cover_brief.json", &brief).unwrap();

        let resp = get_build_in(root.path(), "001");
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            body["cover_brief"]["emphasis"][0],
            "shipped ChessCoach, a chess training app"
        );
    }

    #[tokio::test]
    async fn get_build_file_rejects_a_bad_filename_before_touching_disk() {
        let resp = get_build_file("041", "../../secret").await;
        assert_eq!(resp.status(), 400);
    }

    // `generate_build_cover` now takes a `Request<Incoming>` (the optional
    // `brief` body), so it joins every other body-taking route in this file
    // (`put_dataset`, `save_build_edits`, `save_build_triage`, `create_build`,
    // `render`, `llm`, `fetch_jd`) in having no direct handler-level test —
    // `hyper::body::Incoming` isn't constructible outside a real connection.
    // Its id guard is still the same `is_numeric_id` call (see the function
    // body, checked before the body is ever read) covered by
    // `the_build_id_gate_accepts_only_plain_digits` above.

    /// An LLM failure from the cover agent is mapped to the same status the
    /// `/api/llm` route would give it (a missing key is a 503), so the browser's
    /// existing error handling covers this route too; a malformed or empty model
    /// reply is a 502 upstream error, never a leak of key material.
    #[test]
    fn cover_error_response_mirrors_the_llm_route_and_502s_a_bad_reply() {
        let missing = crate::cover::CoverLetterError::Llm(LlmError::MissingApiKey {
            provider: "anthropic".into(),
        });
        assert_eq!(cover_error_response(missing, None).status(), 503);

        let empty = crate::cover::CoverLetterError::Empty;
        assert_eq!(cover_error_response(empty, None).status(), 502);
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
        let body = cost_body(&known, crate::config::Billing::Metered, &prices);
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
        let body = cost_body(&unknown, crate::config::Billing::Metered, &prices);
        assert!(body["usd"].is_null());
        assert!(body["subscription_note"].is_null());
    }

    #[test]
    fn cost_body_on_a_subscription_nulls_usd_and_explains_why() {
        let prices = std::collections::BTreeMap::new();
        // Even a perfectly priceable model is suppressed on a plan; the run's
        // cost is covered by the flat fee.
        let query = CostQuery {
            model: "claude-opus-4-8".to_string(),
            input: 1000,
            output: 500,
        };
        let body = cost_body(&query, crate::config::Billing::Subscription, &prices);
        assert!(body["usd"].is_null());
        assert_eq!(body["subscription_note"], "covered by your Claude plan");
    }

    #[test]
    fn cost_body_on_a_local_model_reports_no_cost() {
        let prices = std::collections::BTreeMap::new();
        // A local model is free, so usd is null and the note says so, shaped
        // exactly like the subscription case the frontend already renders.
        let query = CostQuery {
            model: "qwen2.5-coder".to_string(),
            input: 1000,
            output: 500,
        };
        let body = cost_body(&query, crate::config::Billing::Local, &prices);
        assert!(body["usd"].is_null());
        assert_eq!(body["subscription_note"], "local model, no cost");
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

    /// Deleting a build removes its directory and answers 200; a second delete
    /// of the same id is a 404, and a traversal-shaped id never resolves to a
    /// path. This drives the route's disk half against a tempdir, so it needs
    /// neither a socket nor the active workspace.
    #[test]
    fn delete_build_removes_the_dir_then_404s_and_rejects_traversal() {
        let root = tempfile::tempdir().unwrap();
        let build_dir = root.path().join("041");
        std::fs::create_dir_all(&build_dir).unwrap();
        std::fs::write(build_dir.join("meta.json"), "{}").unwrap();

        let resp = delete_build_in(root.path(), "041");
        assert_eq!(resp.status(), 200);
        assert!(!build_dir.is_dir());

        // The list no longer has it: deleting it again is a 404.
        assert_eq!(delete_build_in(root.path(), "041").status(), 404);

        // A traversal-shaped id is rejected before any path is built.
        assert_eq!(delete_build_in(root.path(), "../etc").status(), 404);
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

    /// A triage body written to disk round-trips back through the same read the
    /// GET bundle uses, preserving the exact `left` list and its order.
    #[test]
    fn triage_round_trips_through_write_and_read() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("001");
        std::fs::create_dir_all(&dir).unwrap();

        let triage =
            parse_triage(br#"{"left":["summary::no_metric","skills::jd_mismatch"]}"#).unwrap();
        builds::write_json(&dir, "triage.json", &triage).unwrap();

        let back = read_triage(&dir);
        assert_eq!(back.left, ["summary::no_metric", "skills::jd_mismatch"]);
    }

    /// A build with no `triage.json` reads as an empty list — the bundle's
    /// "left for now" set is empty, not a missing key.
    #[test]
    fn triage_missing_file_reads_empty() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("001");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(read_triage(&dir), BuildTriage::default());
        assert!(read_triage(&dir).left.is_empty());
    }

    /// The body must be `{"left": [<string>, ...]}`: a bare list, an object with
    /// a non-list `left`, and a list holding a non-string are all rejected before
    /// anything touches disk.
    #[test]
    fn triage_rejects_garbage_bodies() {
        assert!(parse_triage(b"[]").is_err());
        assert!(parse_triage(br#"{"left":"nope"}"#).is_err());
        assert!(parse_triage(br#"{"left":[1,2,3]}"#).is_err());
        assert!(parse_triage(b"not json at all").is_err());
        // An empty object is fine: `left` defaults to empty.
        assert_eq!(parse_triage(b"{}").unwrap(), BuildTriage::default());
    }

    /// A `left` list past the cap is refused (the message names the cap), while a
    /// list exactly at the cap is accepted.
    #[test]
    fn triage_rejects_an_oversized_list() {
        let at_cap = format!(
            r#"{{"left":[{}]}}"#,
            (0..MAX_TRIAGE_IDS)
                .map(|n| format!("\"id{n}\""))
                .collect::<Vec<_>>()
                .join(",")
        );
        assert_eq!(
            parse_triage(at_cap.as_bytes()).unwrap().left.len(),
            MAX_TRIAGE_IDS
        );

        let over_cap = format!(
            r#"{{"left":[{}]}}"#,
            (0..=MAX_TRIAGE_IDS)
                .map(|n| format!("\"id{n}\""))
                .collect::<Vec<_>>()
                .join(",")
        );
        assert!(parse_triage(over_cap.as_bytes()).is_err());
    }
}
