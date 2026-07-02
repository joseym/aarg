//! The Send-preserving bridge that lets a browser-provided async callback
//! answer `aarg-core`'s `LlmClient` — without ever violating its `Send`
//! bounds.
//!
//! The constraint: `LlmClient` is `Send + Sync`, so `complete`/`stream`
//! return `Send` futures. A JS callback returns a `Promise`, and awaiting one
//! means holding a `JsValue`/`JsFuture` across an `.await` — both `!Send`. A
//! client that talked to JS directly could not satisfy the trait.
//!
//! The fix is to keep the `!Send` JS work out of the `LlmClient` future
//! entirely. `BridgeClient` holds only a channel sender; every `complete`
//! sends the (plain-data, `Send`) request down the channel and awaits a
//! one-shot reply. Both channel halves and both request/response types are
//! `Send`, so the whole `LlmClient` future is `Send` — structurally, with no
//! `unsafe`. The JS-touching half lives in the **pump** (see `spawn_pump`),
//! a separate `spawn_local` task that owns the `js_sys::Function`, reads
//! requests off the channel, calls JS, and sends the parsed response back.
//! Wasm is single-threaded, so the two never actually run in parallel — the
//! `Send` bound is a type-level obligation the channel discharges, not a
//! threading requirement.
//!
//! Everything above the pump is cfg-free: the channels and `BridgeClient`
//! compile on the host, which is what lets the native test in this file run a
//! real domain agent over the bridge with a canned local pump and no JS at
//! all. Only `spawn_pump` and its `call_callback` helper are wasm-only.

use async_trait::async_trait;
use futures_channel::{mpsc, oneshot};
use serde::Deserialize;

use aarg_domain::agent::{ModelResolver, ModelTier};
use aarg_domain::llm::{
    CompletionRequest, CompletionResponse, LlmClient, LlmError, StreamEvent, TokenStream,
};
use aarg_domain::user::{Answer, AskError, Question, UserHandle};

// The pump is the only part that touches JS, so its imports are wasm-only.
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::{JsCast, JsValue};

/// One unit of work handed to the pump: the request to run, paired with the
/// one-shot channel to answer it on. Every field is plain serde data or a
/// `Send` channel half, which is the whole point — a `Job` can cross into the
/// `Send`-bounded `LlmClient` future freely.
pub type Job = (
    CompletionRequest,
    oneshot::Sender<Result<CompletionResponse, LlmError>>,
);

/// An `LlmClient` that forwards every request to a pump over a channel. It
/// holds nothing but the sender, so it (and every future it produces) is
/// `Send` — the pump, not the client, owns the `!Send` JS callback.
pub struct BridgeClient {
    tx: mpsc::UnboundedSender<Job>,
}

impl BridgeClient {
    /// Build a client and the receiving half the pump consumes. Returns both
    /// like `channel()` does: the caller keeps the client for the agents and
    /// hands the receiver to [`spawn_pump`]. When the client drops, the
    /// channel closes and the pump's loop ends.
    pub fn new() -> (Self, mpsc::UnboundedReceiver<Job>) {
        let (tx, rx) = mpsc::unbounded();
        (Self { tx }, rx)
    }
}

#[async_trait]
impl LlmClient for BridgeClient {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        // A fresh one-shot per request: the pump answers on `reply_tx`, we
        // await `reply_rx`. Sending the request moves it out of this future,
        // so nothing `!Send` is ever held across the await below.
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .unbounded_send((request, reply_tx))
            .map_err(|_| LlmError::Transport("the LLM bridge is closed".to_string()))?;
        // A canceled one-shot (`Err` here) means the pump *task itself* was
        // dropped before it answered — the sender end of the one-shot went
        // away without ever being used. It does NOT cover "JS never
        // resolved": if the callback's promise simply never settles, the
        // pump is parked awaiting it, still holding `reply_tx` — the
        // one-shot neither cancels nor fires, and this `.await` hangs
        // forever. That is inherent to bridging over a foreign callback with
        // no way to force it to finish; the mitigation lives on the JS side
        // (wrap the callback's promise in a timeout that rejects). The inner
        // `Result` is the model's own success/failure, once the pump does
        // answer.
        reply_rx.await.map_err(|_| {
            LlmError::Transport("the LLM bridge dropped the request before replying".to_string())
        })?
    }

    async fn stream(&self, request: CompletionRequest) -> Result<TokenStream, LlmError> {
        // No incremental streaming across the bridge yet. The agents only
        // stream when a `StreamSink` is set, and every wasm export passes
        // `sink: None`, so this path is used only to satisfy the trait: run
        // the blocking completion, then replay the whole reply as the two
        // events the spine expects (all of the text, then `Done`). An iter
        // stream over owned data is `Send`, so it fits `TokenStream`.
        let response = self.complete(request).await?;
        let events = [
            Ok(StreamEvent::TextDelta(response.text)),
            Ok(StreamEvent::Done {
                stop_reason: response.stop_reason,
                usage: response.usage,
            }),
        ];
        Ok(Box::pin(futures_util::stream::iter(events)))
    }
}

/// Resolves each agent's model tier to a concrete model id from the
/// `{"cheap", "mid", "smart"}` (or single `"model"`) map JS supplies. Mirrors
/// the native config's `ModelResolver` in the smallest possible shape: three
/// owned names, one per tier.
#[derive(Debug, Clone)]
pub struct Models {
    cheap: String,
    mid: String,
    smart: String,
}

/// The lenient wire shape: every key optional, so a single `"model"` can
/// stand in for all three tiers, or any subset can be named explicitly.
#[derive(Deserialize)]
struct RawModels {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    cheap: Option<String>,
    #[serde(default)]
    mid: Option<String>,
    #[serde(default)]
    smart: Option<String>,
}

impl Models {
    /// Parse the models map. A tier takes its own key when present, else the
    /// shared `"model"` convenience key; a tier left with neither (or a blank
    /// name) is an error rather than a silent empty model id.
    pub fn from_json(json: &str) -> Result<Self, String> {
        let raw: RawModels =
            serde_json::from_str(json).map_err(|e| format!("invalid models json: {e}"))?;
        let shared = raw.model;
        let pick = |specific: Option<String>, tier: &str| -> Result<String, String> {
            specific
                .or_else(|| shared.clone())
                .map(|name| name.trim().to_string())
                .filter(|name| !name.is_empty())
                .ok_or_else(|| {
                    format!(
                        "models json has no model for the {tier} tier; \
                         provide \"{tier}\" or a single \"model\""
                    )
                })
        };
        Ok(Self {
            cheap: pick(raw.cheap, "cheap")?,
            mid: pick(raw.mid, "mid")?,
            smart: pick(raw.smart, "smart")?,
        })
    }
}

impl ModelResolver for Models {
    fn resolve(&self, _agent_id: &str, tier: ModelTier) -> &str {
        match tier {
            ModelTier::Cheap => &self.cheap,
            ModelTier::Mid => &self.mid,
            ModelTier::Smart => &self.smart,
        }
    }
}

/// Spawn the pump: the task that owns the JS callback and answers requests.
/// It reads each `Job` off the channel, calls the callback, awaits the
/// resolved response, and sends it back on the job's one-shot. This is the
/// only place a `JsValue`/`JsFuture` is held across an await — and it is a
/// `spawn_local` task, never an `LlmClient` future, so no `Send` bound
/// applies. When the last `BridgeClient` drops, `rx.next()` yields `None` and
/// the loop ends.
#[cfg(target_arch = "wasm32")]
pub fn spawn_pump(mut rx: mpsc::UnboundedReceiver<Job>, callback: js_sys::Function) {
    use futures_util::StreamExt;
    wasm_bindgen_futures::spawn_local(async move {
        while let Some((request, reply)) = rx.next().await {
            let result = call_callback(&callback, &request).await;
            // If the requester already went away, the one-shot send fails —
            // that just means no one is waiting for this answer, which is fine.
            let _ = reply.send(result);
        }
    });
}

/// Call the JS callback with one request and await its response. Serializes
/// the request to JSON, hands it to the callback, wraps whatever comes back in
/// `Promise.resolve` (so a synchronous string works as well as a real
/// Promise), awaits it, and parses the resolved JSON string into a
/// `CompletionResponse`. Any throw, rejection, or parse failure becomes an
/// `LlmError::Transport` carrying a best-effort message.
#[cfg(target_arch = "wasm32")]
async fn call_callback(
    callback: &js_sys::Function,
    request: &CompletionRequest,
) -> Result<CompletionResponse, LlmError> {
    let json = serde_json::to_string(request).map_err(|e| {
        LlmError::Transport(format!(
            "could not serialize the request for the JS bridge: {e}"
        ))
    })?;
    let returned = callback
        .call1(&JsValue::NULL, &JsValue::from_str(&json))
        .map_err(|e| {
            LlmError::Transport(format!("the JS LLM callback threw: {}", stringify(&e)))
        })?;
    // `Promise.resolve` is idempotent on a Promise and adopts a plain value,
    // so the callback may return either.
    let promise = js_sys::Promise::resolve(&returned);
    let resolved = wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map_err(|e| {
            LlmError::Transport(format!("the JS LLM callback rejected: {}", stringify(&e)))
        })?;
    let text = resolved.as_string().ok_or_else(|| {
        LlmError::Transport("the JS LLM callback did not resolve to a JSON string".to_string())
    })?;
    serde_json::from_str(&text).map_err(|e| {
        LlmError::Transport(format!(
            "the JS LLM callback returned invalid CompletionResponse JSON: {e}"
        ))
    })
}

/// Best-effort text for a thrown/rejected `JsValue`: the string itself, an
/// `Error` object's `.message`, or the debug form as a last resort.
#[cfg(target_arch = "wasm32")]
fn stringify(value: &JsValue) -> String {
    if let Some(text) = value.as_string() {
        return text;
    }
    if let Some(error) = value.dyn_ref::<js_sys::Error>() {
        return String::from(error.message());
    }
    format!("{value:?}")
}

// ---------------------------------------------------------------------
// The user bridge: the same Send-preserving split, applied to `UserHandle`
// ---------------------------------------------------------------------
//
// `UserHandle` is `Send + Sync` exactly like `LlmClient`, and the browser's
// only way to answer a question is an async JS callback that returns a
// `Promise` — so the same problem, and the same fix. `BridgeUser` holds
// nothing but a channel sender; every `ask`/`confirm` sends a plain-data
// `UserJob` (all `Send`) and awaits a one-shot reply. The `!Send` JS work —
// serializing the question to an envelope, calling the callback, awaiting its
// promise, parsing the answer — lives entirely in the wasm-only user pump
// (`spawn_user_pump`), a `spawn_local` task that owns the `js_sys::Function`.
//
// The mapping is done by hand in the pump because `Question`/`Answer` carry no
// serde derives (they live in `aarg-core`, which this crate must not edit).
// The wire vocabulary mirrors the MCP `ElicitationUser` (`src/mcp/client.rs`),
// which already solved the same JS-boundary mapping: a question serializes to
// `{kind, prompt, options?, default?}`, and the callback resolves to a JSON
// object using the field names `choice` / `choices` / `text` / `confirm`. A
// garbled or declined reply maps to the same skip/abort semantics
// `ElicitationUser` uses — never to invented content.

/// One unit of user work handed to the pump. Every variant carries only plain
/// data or a `Send` one-shot half, so a `UserJob` crosses freely into the
/// `Send`-bounded `UserHandle` future — the same discipline as [`Job`].
pub enum UserJob {
    /// A question to put to the person, answered on the one-shot.
    Ask {
        question: Question,
        reply: oneshot::Sender<Result<Answer, AskError>>,
    },
    /// An optional detour to confirm; the one-shot carries the yes/no.
    Confirm {
        prompt: String,
        default: bool,
        reply: oneshot::Sender<Result<bool, AskError>>,
    },
    /// A one-way notice; no reply is expected, so there is no one-shot.
    Notify(String),
}

/// A `UserHandle` that forwards every question to the pump over a channel. Like
/// [`BridgeClient`] it holds nothing but the sender, so it (and every future it
/// produces) is `Send` — the pump, not the handle, owns the `!Send` callback.
pub struct BridgeUser {
    tx: mpsc::UnboundedSender<UserJob>,
}

impl BridgeUser {
    /// Build a handle and the receiving half the pump consumes, like
    /// `channel()`. The caller keeps the handle for the copilots and hands the
    /// receiver to [`spawn_user_pump`]. When the handle drops, the channel
    /// closes and the pump's loop ends.
    pub fn new() -> (Self, mpsc::UnboundedReceiver<UserJob>) {
        let (tx, rx) = mpsc::unbounded();
        (Self { tx }, rx)
    }
}

/// The typed error a closed bridge produces: the pump task went away before it
/// could answer. Modeled as an `Io` error carrying what was being asked, so the
/// message names the unanswered prompt the way a real terminal read failure
/// would.
fn bridge_closed(what: &str) -> AskError {
    AskError::Io {
        what: what.to_string(),
        source: Box::new(std::io::Error::other("the user bridge is closed")),
    }
}

#[async_trait]
impl UserHandle for BridgeUser {
    async fn ask(&self, question: Question) -> Result<Answer, AskError> {
        // Capture the prompt before the question moves into the job, so a
        // closed-bridge error can still name what went unanswered.
        let prompt = question.prompt().to_string();
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .unbounded_send(UserJob::Ask {
                question,
                reply: reply_tx,
            })
            .map_err(|_| bridge_closed(&prompt))?;
        // A canceled one-shot means the pump was dropped before answering —
        // the same closed-bridge failure. The inner `Result` is the pump's own
        // answer/decline once it does reply.
        reply_rx.await.map_err(|_| bridge_closed(&prompt))?
    }

    async fn confirm(&self, prompt: &str, default: bool) -> Result<bool, AskError> {
        // An optional detour: a closed or silent bridge is never an error here
        // (declining an offer isn't), so both the failed send and a canceled
        // one-shot fall back to the caller's default — the non-interactive
        // reading, matching `ElicitationUser::confirm`.
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .unbounded_send(UserJob::Confirm {
                prompt: prompt.to_string(),
                default,
                reply: reply_tx,
            })
            .is_err()
        {
            return Ok(default);
        }
        match reply_rx.await {
            Ok(answer) => answer,
            Err(_) => Ok(default),
        }
    }

    fn notify(&self, message: &str) {
        // Fire-and-forget: a notice needs no reply and must never block. If the
        // bridge is already closed, there's simply no one to tell.
        let _ = self.tx.unbounded_send(UserJob::Notify(message.to_string()));
    }

    fn is_interactive(&self) -> bool {
        // A `BridgeUser` is only ever built alongside a pump holding a real JS
        // callback, so a person can always be reached — the gate the copilots
        // check before offering an interactive step.
        true
    }
}

/// Spawn the user pump: the task that owns the JS callback and answers every
/// `UserJob`. It reads each job off the channel, does the `!Send` JS work
/// (serialize the question, call the callback, await its promise, parse the
/// answer), and sends the result back on the job's one-shot. Like the LLM pump
/// this is the only place a `JsValue`/`JsFuture` is held across an await, and it
/// is a `spawn_local` task, so no `Send` bound applies. When the last
/// [`BridgeUser`] drops, `rx.next()` yields `None` and the loop ends.
#[cfg(target_arch = "wasm32")]
pub fn spawn_user_pump(mut rx: mpsc::UnboundedReceiver<UserJob>, callback: js_sys::Function) {
    use futures_util::StreamExt;
    wasm_bindgen_futures::spawn_local(async move {
        while let Some(job) = rx.next().await {
            match job {
                UserJob::Ask { question, reply } => {
                    let answer = ask_over_js(&callback, question).await;
                    let _ = reply.send(answer);
                }
                UserJob::Confirm {
                    prompt,
                    default,
                    reply,
                } => {
                    let result = confirm_over_js(&callback, &prompt, default).await;
                    let _ = reply.send(result);
                }
                UserJob::Notify(message) => notify_over_js(&callback, &message),
            }
        }
    });
}

/// Put one question to the JS callback and map its reply back into an `Answer`.
/// Serializes the question to the `{kind, prompt, options?}` envelope, calls the
/// callback, and interprets the resolved JSON object by kind — matching
/// `ElicitationUser`'s accept/decline reading exactly, so a declined or garbled
/// reply skips or aborts the step rather than inventing an answer.
#[cfg(target_arch = "wasm32")]
async fn ask_over_js(callback: &js_sys::Function, question: Question) -> Result<Answer, AskError> {
    match question {
        Question::Select { prompt, options } => {
            let envelope = serde_json::json!({
                "kind": "select", "prompt": prompt, "options": options,
            })
            .to_string();
            // Accept only a `choice` that names one of the options; anything
            // else (decline, dismiss, or an unknown value) aborts the step,
            // exactly as `ElicitationUser` treats a declined required choice.
            // No `abort` check is needed here: a dismissed modal resolving
            // `{"abort": true}` carries no valid `choice`, so it already falls
            // through to the same `NotInteractive` error the sentinel forces in
            // the MultiSelect arm.
            match call_user_callback(callback, &envelope).await {
                Some(value) => match choice_index(&value, &options) {
                    Some(index) => Ok(Answer::Choice(index)),
                    None => Err(AskError::NotInteractive { what: prompt }),
                },
                None => Err(AskError::NotInteractive { what: prompt }),
            }
        }
        Question::MultiSelect { prompt, options } => {
            let envelope = serde_json::json!({
                "kind": "multi_select", "prompt": prompt, "options": options,
            })
            .to_string();
            // A dismissed multi-select modal now resolves `{"abort": true}` —
            // a deliberate session end, mapped to the same typed error a
            // dismissed Select produces. Without this, a dismissed modal
            // resolving `{choices: []}` would read as "none of them", which
            // once declines persist means "decline every keyword" by accident.
            // A present `choices` array (even empty) is still a legit answer;
            // a thrown callback (`None`) still means "none of them".
            match call_user_callback(callback, &envelope).await {
                Some(value)
                    if value
                        .get("abort")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false) =>
                {
                    Err(AskError::NotInteractive { what: prompt })
                }
                // A reply with neither `abort` nor a `choices` array is garbled
                // — treat it like a dismissal, not "none of them": once declines
                // persist, a malformed reply must not mass-decline keywords.
                Some(value) if value.get("choices").is_none() => {
                    Err(AskError::NotInteractive { what: prompt })
                }
                Some(value) => Ok(Answer::Choices(choices_indices(&value, &options))),
                None => Ok(Answer::Choices(Vec::new())),
            }
        }
        Question::Text { prompt } => {
            let envelope = serde_json::json!({ "kind": "text", "prompt": prompt }).to_string();
            // A resolved `text` (even empty, a valid "skip") is accepted; a
            // decline or garbled reply aborts, as `ElicitationUser` does.
            match call_user_callback(callback, &envelope).await {
                Some(value) => match text_value(&value) {
                    Some(text) => Ok(Answer::Text(text)),
                    None => Err(AskError::NotInteractive { what: prompt }),
                },
                None => Err(AskError::NotInteractive { what: prompt }),
            }
        }
    }
}

/// Confirm an optional detour over the JS callback. A resolved `confirm`
/// boolean is the answer; a resolved value without one is a decline (`false`);
/// a thrown/rejected callback means no channel, so the caller's `default`
/// stands — the same trichotomy as `ElicitationUser::confirm`.
#[cfg(target_arch = "wasm32")]
async fn confirm_over_js(
    callback: &js_sys::Function,
    prompt: &str,
    default: bool,
) -> Result<bool, AskError> {
    let envelope = serde_json::json!({
        "kind": "confirm", "prompt": prompt, "default": default,
    })
    .to_string();
    match call_user_callback(callback, &envelope).await {
        Some(value) => Ok(value
            .get("confirm")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)),
        None => Ok(default),
    }
}

/// Hand a one-way notice to the JS callback (`{kind: "notify", message}`) and
/// return immediately — a notice has no reply and must never block the pump's
/// queue, so its promise (if any) is deliberately not awaited.
#[cfg(target_arch = "wasm32")]
fn notify_over_js(callback: &js_sys::Function, message: &str) {
    let envelope = serde_json::json!({ "kind": "notify", "message": message }).to_string();
    let _ = callback.call1(&JsValue::NULL, &JsValue::from_str(&envelope));
}

/// Call the user callback with one envelope and return its resolved JSON value,
/// or `None` if it threw, rejected, or didn't resolve to a JSON string. Mirrors
/// [`call_callback`] but yields a lenient `serde_json::Value` (the answer shape
/// varies by question kind) rather than a fixed type.
#[cfg(target_arch = "wasm32")]
async fn call_user_callback(
    callback: &js_sys::Function,
    envelope: &str,
) -> Option<serde_json::Value> {
    let returned = callback
        .call1(&JsValue::NULL, &JsValue::from_str(envelope))
        .ok()?;
    let promise = js_sys::Promise::resolve(&returned);
    let resolved = wasm_bindgen_futures::JsFuture::from(promise).await.ok()?;
    let text = resolved.as_string()?;
    serde_json::from_str(&text).ok()
}

/// Map a `select` reply to an option index: the reply's `choice` string is
/// matched against the offered options (the same by-name mapping
/// `ElicitationUser` uses). An unknown or absent choice yields `None`, so the
/// caller aborts rather than guessing an index the user didn't pick.
#[cfg(target_arch = "wasm32")]
fn choice_index(value: &serde_json::Value, options: &[String]) -> Option<usize> {
    let chosen = value.get("choice").and_then(serde_json::Value::as_str)?;
    options.iter().position(|option| option == chosen)
}

/// Map a `multi_select` reply to option indices: each string in the reply's
/// `choices` array that names an offered option becomes its index; unknown
/// names are dropped. An absent array is simply no selections.
#[cfg(target_arch = "wasm32")]
fn choices_indices(value: &serde_json::Value, options: &[String]) -> Vec<usize> {
    let picked: Vec<&str> = value
        .get("choices")
        .and_then(serde_json::Value::as_array)
        .map(|items| items.iter().filter_map(serde_json::Value::as_str).collect())
        .unwrap_or_default();
    options
        .iter()
        .enumerate()
        .filter(|(_, option)| picked.contains(&option.as_str()))
        .map(|(index, _)| index)
        .collect()
}

/// Read a `text` reply: the reply's `text` field, or a bare JSON string for
/// convenience. An empty string is a valid answer (the user's "skip"); a reply
/// with neither shape yields `None`, so the caller aborts rather than inventing
/// text.
#[cfg(target_arch = "wasm32")]
fn text_value(value: &serde_json::Value) -> Option<String> {
    if let Some(text) = value.get("text").and_then(serde_json::Value::as_str) {
        return Some(text.to_string());
    }
    value.as_str().map(str::to_string)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use aarg_domain::agent::AgentContext;
    use aarg_domain::llm::TokenUsage;
    use aarg_domain::trace::Tracer;
    use futures_util::StreamExt;

    // Cribbed from `aarg-domain`'s own jd.rs test fixture (GOOD_REPLY): a
    // complete, well-formed parser reply. This is the model's output the
    // canned pump hands back, proving a real domain agent parses it into
    // `JobRequirements` over the bridge.
    const GOOD_REPLY: &str = r#"{
        "company": "Acme Corp",
        "title": "Director of Engineering",
        "seniority": "director",
        "location": "New York, NY",
        "remote": "hybrid",
        "domain_keywords": ["fintech", "payments"],
        "required_skills": [
            {"name": "Engineering management", "category": "soft",
             "importance": "critical",
             "context_phrases": ["7+ years leading engineering teams"]},
            {"name": "Node.js", "category": "framework",
             "context_phrases": ["our stack is Node.js and TypeScript"]}
        ],
        "preferred_skills": [
            {"name": "Rust", "category": "language", "context_phrases": []}
        ],
        "responsibilities": ["Own delivery across four teams"],
        "ats_phrases": ["Director of Engineering", "engineering management"]
    }"#;

    /// The bridge's reason to exist, proven without a browser: a real domain
    /// agent (`jd::parse_jd`) runs over `BridgeClient`, while a spawned local
    /// task plays the pump and answers with a canned `CompletionResponse`.
    /// The pump task owns the receiver (never the client), so when the client
    /// drops at the end its loop terminates.
    #[tokio::test]
    async fn a_domain_agent_runs_over_the_bridge() {
        let (client, mut rx) = BridgeClient::new();

        // The local stand-in for the JS pump: read each request, answer with
        // the canned reply. Owns `rx` only, so it is `Send + 'static`.
        let pump = tokio::spawn(async move {
            let mut answered = 0;
            while let Some((request, reply)) = rx.next().await {
                let response = CompletionResponse {
                    text: GOOD_REPLY.to_string(),
                    tool_calls: Vec::new(),
                    model: request.model.clone(),
                    stop_reason: Some("end_turn".to_string()),
                    usage: TokenUsage {
                        input_tokens: 10,
                        output_tokens: 20,
                    },
                };
                let _ = reply.send(Ok(response));
                answered += 1;
            }
            answered
        });

        let models = Models::from_json(r#"{"model": "test-model"}"#).unwrap();
        let ctx = AgentContext {
            llm: &client,
            model: &models,
            tracer: &Tracer::DISABLED,
            sink: None,
        };

        let jd = aarg_domain::jd::parse_jd(&ctx, "the jd text")
            .await
            .expect("the agent should parse the canned reply over the bridge");

        // The reply parsed into the expected requirements — the agent really
        // ran, and its request really reached the pump (model id echoed).
        assert_eq!(jd.company, "Acme Corp");
        assert_eq!(jd.title, "Director of Engineering");
        assert_eq!(jd.required_skills.len(), 2);
        // Ground truth still travels with the structure.
        assert_eq!(jd.raw_text, "the jd text");

        // `ctx` borrowed `client`; that borrow ends at the `parse_jd` call
        // above (NLL), so dropping the client here closes the channel — and
        // the pump loop, reading a now-closed receiver, then ends.
        drop(client);
        let answered = pump.await.expect("the pump task should not panic");
        assert_eq!(answered, 1, "the agent made exactly one model call");
    }

    /// The user bridge's reason to exist, proven without a browser: a real
    /// domain copilot (`refine_summary`) runs over `BridgeUser` while a spawned
    /// local task plays the pump, answering scripted questions with `Answer`s
    /// directly (the JS envelope mapping is proven by the node smoke). The
    /// user's own verbatim words land in the mutated dataset — nothing invents
    /// content on their behalf.
    #[tokio::test]
    async fn a_copilot_runs_over_the_user_bridge() {
        use aarg_domain::dataset::types::{
            Bullet, BulletId, Contact, EmploymentType, ResumeDataset, Role, RoleId, Strength,
            YearMonth,
        };
        use aarg_domain::llm::MockLlmClient;
        use std::collections::VecDeque;

        // A small history so the summary agent has something to ground on: one
        // role with one bullet carrying a number (so the digit guard has an
        // allowed figure), and a weak current summary to improve.
        let mut dataset = ResumeDataset::new(Contact {
            full_name: "Ada".into(),
            email: "ada@example.com".into(),
            phone: None,
            location: None,
            links: Vec::new(),
        });
        dataset.summary = Some("Generic opener.".into());
        dataset.roles.push(Role {
            id: RoleId("role-1".into()),
            company: "Acme".into(),
            title: "Director of Engineering".into(),
            start: YearMonth {
                year: 2020,
                month: 1,
            },
            end: None,
            location: None,
            employment_type: EmploymentType::FullTime,
            bullets: vec![Bullet {
                id: BulletId("bullet-1".into()),
                text: "Grew the team from 1 engineer to a 20 person org".into(),
                skill_ids: Vec::new(),
                metric: None,
                theme: Vec::new(),
                strength: Strength::High,
                variants: Vec::new(),
            }],
            skill_ids: Vec::new(),
            context: None,
        });

        // The model's grounded suggestion (no invented number, so it passes the
        // guard and the "Use this wording" option is offered).
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"summary": "Engineering leader who built a team."}"#);

        let (user, mut rx) = BridgeUser::new();

        // The local stand-in for the JS pump: answer each question from a
        // script (Select "Write my own", then the user's own summary text),
        // confirms take their default, notices are dropped. Owns `rx` only, so
        // it is `Send + 'static`.
        let pump = tokio::spawn(async move {
            let mut answers: VecDeque<Answer> = VecDeque::from(vec![
                // Menu is [Use this wording, Tweak it, Write my own, Skip].
                Answer::Choice(2),
                Answer::Text("Engineering leader in Ada's own verbatim words.".into()),
            ]);
            let mut asked = 0;
            while let Some(job) = rx.next().await {
                match job {
                    UserJob::Ask { reply, .. } => {
                        asked += 1;
                        let answer = answers
                            .pop_front()
                            .expect("the copilot asked more questions than were scripted");
                        let _ = reply.send(Ok(answer));
                    }
                    UserJob::Confirm { default, reply, .. } => {
                        let _ = reply.send(Ok(default));
                    }
                    UserJob::Notify(_) => {}
                }
            }
            asked
        });

        let models = Models::from_json(r#"{"model": "test-model"}"#).unwrap();
        let ctx = AgentContext {
            llm: &mock,
            model: &models,
            tracer: &Tracer::DISABLED,
            sink: None,
        };

        let changed = aarg_domain::summary::refine_summary(
            &mut dataset,
            "front-loads everything",
            &user,
            &ctx,
            3,
        )
        .await
        .expect("the copilot should run to completion over the bridge");

        // The user's own words were recorded verbatim and marked authoritative
        // — the copilot really ran, and the bridge really carried its questions.
        assert!(changed);
        assert!(dataset.summary_confirmed);
        assert_eq!(
            dataset.summary.as_deref(),
            Some("Engineering leader in Ada's own verbatim words.")
        );

        drop(user);
        let asked = pump.await.expect("the pump task should not panic");
        assert_eq!(asked, 2, "the copilot asked exactly two questions");
    }

    #[test]
    fn a_single_model_key_fills_every_tier() {
        let models = Models::from_json(r#"{"model": "one-model"}"#).unwrap();
        assert_eq!(models.resolve("any", ModelTier::Cheap), "one-model");
        assert_eq!(models.resolve("any", ModelTier::Mid), "one-model");
        assert_eq!(models.resolve("any", ModelTier::Smart), "one-model");
    }

    #[test]
    fn per_tier_keys_win_over_the_shared_default() {
        let models =
            Models::from_json(r#"{"model": "base", "smart": "big", "cheap": "small"}"#).unwrap();
        assert_eq!(models.resolve("any", ModelTier::Cheap), "small");
        assert_eq!(models.resolve("any", ModelTier::Mid), "base");
        assert_eq!(models.resolve("any", ModelTier::Smart), "big");
    }

    #[test]
    fn a_missing_tier_with_no_shared_model_is_an_error() {
        let err = Models::from_json(r#"{"cheap": "small"}"#).unwrap_err();
        assert!(err.contains("mid"), "got {err:?}");
    }

    #[test]
    fn malformed_models_json_is_an_error_not_a_panic() {
        let err = Models::from_json("{ not json").unwrap_err();
        assert!(err.contains("invalid models json"), "got {err:?}");
    }
}
