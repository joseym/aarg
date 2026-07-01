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
