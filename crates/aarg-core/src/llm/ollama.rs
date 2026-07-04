//! A hand-rolled client for Ollama's native chat API (`/api/chat`).
//!
//! Written directly against the wire format with `reqwest` — no SDK. Ollama's
//! native API differs from the OpenAI dialect in three ways this module owns:
//! the response is NDJSON (one JSON object per line, no `data:` prefix, no
//! `[DONE]`), tool-call arguments arrive as a JSON *object* rather than an
//! encoded string, and images ride in a message-level `images` array.
//!
//! The `num_ctx` option matters more than it looks. AARG's prompts run roughly
//! 4k-8k tokens, and Ollama silently clips a prompt that overflows the window —
//! no error, HTTP 200, and (probed live on 0.30.11) only `num_ctx / 2 + 2`
//! prompt tokens kept: a 3218-token prompt came back as 258 processed tokens
//! under a 512-token window, 514 under 1024, 1026 under 2048. Without an
//! explicit `num_ctx` the window is the model default (as small as 4096), so
//! half of a typical AARG prompt would vanish. For a tool built on never
//! fabricating from the dataset that is evidence loss, so this client guards
//! it twice: before sending, it sizes `num_ctx` from an estimate of the
//! prompt and verifies the request fits the model's own maximum context
//! length (from `/api/show`, cached per model) so the server can't quietly
//! cap the window below what was asked; after the response, it checks the
//! server's reported token count for the clip shapes
//! (see [`crate::llm::context`]).
//!
//! One counter caveat, probed the same way: `prompt_eval_count` on 0.30.11
//! reports the full prompt even when the prefix KV cache is hit (three
//! repeats of a shared-prefix request all reported the full size while
//! `prompt_eval_duration` collapsed from 6.97s to 0.10s), but other versions
//! have reported only newly evaluated tokens. The post-check therefore never
//! treats a low count alone as a clip — a cached healthy response must not
//! be refused.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::llm::client::LlmClient;
use crate::llm::context::{effective_num_ctx, estimate_prompt_tokens, looks_truncated};
use crate::llm::lines::drain_lines;
use crate::llm::types::{
    Attachment, CompletionRequest, CompletionResponse, LlmError, Message, StreamEvent, TokenStream,
    TokenUsage, ToolCall,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const COMPLETE_TIMEOUT: Duration = Duration::from_secs(600);
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// The default context-window floor, in tokens. Every request asks for at least
/// this many, growing larger when the estimated prompt needs it.
const DEFAULT_NUM_CTX: u32 = 8192;

/// How long the model stays resident after a request, by default. Keeping it
/// loaded avoids paying the multi-second load cost on the next call.
const DEFAULT_KEEP_ALIVE: &str = "5m";

/// How long to wait for `/api/show` before proceeding without the model's
/// context length. The lookup is an optimization for a loud pre-send error;
/// a slow or missing endpoint must not block the actual request.
const SHOW_TIMEOUT: Duration = Duration::from_secs(10);

/// An `LlmClient` backed by Ollama's native chat API.
pub struct OllamaClient {
    http: reqwest::Client,
    base_url: String,
    num_ctx: u32,
    keep_alive: String,
    /// Each model's maximum context length, fetched from `/api/show` once and
    /// cached. A std `Mutex` (not tokio's) because every critical section is a
    /// single map operation with no await inside.
    model_windows: Mutex<HashMap<String, u64>>,
}

impl OllamaClient {
    /// Build a client pointed at `base_url` (e.g. `http://127.0.0.1:11434`),
    /// with the default context floor and keep-alive.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            http: build_http(),
            base_url: base_url.into(),
            num_ctx: DEFAULT_NUM_CTX,
            keep_alive: DEFAULT_KEEP_ALIVE.to_string(),
            model_windows: Mutex::new(HashMap::new()),
        }
    }

    /// Set the context-window floor. Each request still grows the window above
    /// this when the prompt needs it; this is the minimum, not a cap.
    pub fn with_num_ctx(mut self, num_ctx: u32) -> Self {
        self.num_ctx = num_ctx;
        self
    }

    /// Set how long the model stays loaded after a request (Ollama duration
    /// syntax, e.g. `"5m"`, `"30s"`, `"0"` to unload immediately).
    pub fn with_keep_alive(mut self, keep_alive: impl Into<String>) -> Self {
        self.keep_alive = keep_alive.into();
        self
    }

    async fn post_chat(
        &self,
        request: &CompletionRequest,
        stream: bool,
        num_ctx: u32,
    ) -> Result<reqwest::Response, LlmError> {
        let mut builder = self.http.post(format!("{}/api/chat", self.base_url));
        if !stream {
            builder = builder.timeout(COMPLETE_TIMEOUT);
        }
        let body = request_body(request, stream, &self.keep_alive, num_ctx)?;
        Ok(builder.json(&body).send().await?)
    }

    /// The model's maximum context length, from `/api/show`, cached per model.
    /// `None` when the lookup fails or the response doesn't carry one — the
    /// caller then proceeds unverified rather than failing a healthy request
    /// over a metadata endpoint.
    async fn model_context_length(&self, model: &str) -> Option<u64> {
        if let Ok(cache) = self.model_windows.lock()
            && let Some(&max) = cache.get(model)
        {
            return Some(max);
        }
        let response = self
            .http
            .post(format!("{}/api/show", self.base_url))
            .timeout(SHOW_TIMEOUT)
            .json(&json!({"model": model}))
            .send()
            .await
            .ok()?;
        let body = response.text().await.ok()?;
        let max = context_length_from_show(&body)?;
        if let Ok(mut cache) = self.model_windows.lock() {
            cache.insert(model.to_string(), max);
        }
        Some(max)
    }

    /// The context window to send for this request: the effective window,
    /// verified against the model's own maximum so the server can't quietly
    /// cap `num_ctx` below what the prompt needs. Errors before anything is
    /// sent when the prompt can't fit the model at all.
    async fn window_for(
        &self,
        request: &CompletionRequest,
        estimate: u64,
    ) -> Result<u32, LlmError> {
        let effective = effective_num_ctx(self.num_ctx, estimate, request.max_tokens);
        let model_max = self.model_context_length(&request.model).await;
        verified_window(effective, estimate, request.max_tokens, model_max)
    }
}

/// Pull the model's maximum context length out of an `/api/show` response.
/// The key is architecture-prefixed (`llama.context_length`,
/// `qwen3.context_length`, ...), so it's matched by suffix.
fn context_length_from_show(body: &str) -> Option<u64> {
    let show: Value = serde_json::from_str(body).ok()?;
    show.get("model_info")?
        .as_object()?
        .iter()
        .find(|(key, _)| key.ends_with(".context_length"))
        .and_then(|(_, value)| value.as_u64())
}

/// Reconcile the window this client wants with the model's maximum, when
/// known. A window at or under the maximum goes through as-is. A window over
/// it is only the floor being ambitious as long as the prompt itself still
/// fits, so it is capped to the maximum; when even the prompt plus completion
/// budget exceeds the maximum, the request fails loudly *before* it is sent,
/// because the server would otherwise cap `num_ctx` itself and clip the
/// prompt silently.
fn verified_window(
    effective: u32,
    estimate: u64,
    max_tokens: u32,
    model_max: Option<u64>,
) -> Result<u32, LlmError> {
    let Some(model_max) = model_max else {
        return Ok(effective);
    };
    if u64::from(effective) <= model_max {
        return Ok(effective);
    }
    // What the prompt actually needs, with no floor in play.
    let needed = u64::from(effective_num_ctx(0, estimate, max_tokens));
    if needed <= model_max {
        return Ok(model_max.min(u64::from(u32::MAX)) as u32);
    }
    Err(LlmError::Unsupported(format!(
        "the prompt (~{estimate} tokens) plus the completion budget \
         ({max_tokens} tokens) does not fit this model's maximum context \
         window of {model_max} tokens; use a model with a larger context \
         window, or shorten the input"
    )))
}

fn build_http() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

// ---- request assembly ---------------------------------------------------

/// Build the `/api/chat` request body. `num_ctx` and `num_predict` (our
/// `max_tokens`) always ride in `options`; `temperature` joins only when set.
/// Fails only when a message carries a PDF attachment (see [`push_wire_message`]).
fn request_body(
    request: &CompletionRequest,
    stream: bool,
    keep_alive: &str,
    num_ctx: u32,
) -> Result<Value, LlmError> {
    let messages = build_messages(&request.system, &request.messages)?;
    let mut options = json!({
        "num_ctx": num_ctx,
        "num_predict": request.max_tokens,
    });
    if let Some(temperature) = request.temperature {
        options["temperature"] = json!(temperature);
    }
    let mut body = json!({
        "model": request.model,
        "messages": messages,
        "stream": stream,
        "keep_alive": keep_alive,
        "options": options,
    });
    if !request.tools.is_empty() {
        body["tools"] = json!(wire_tools(&request.tools));
    }
    Ok(body)
}

/// The `messages` array: a leading `system` message when present, then every
/// turn expanded to Ollama shape.
fn build_messages(system: &Option<String>, messages: &[Message]) -> Result<Vec<Value>, LlmError> {
    let mut out = Vec::new();
    if let Some(system) = system {
        out.push(json!({"role": "system", "content": system}));
    }
    for message in messages {
        push_wire_message(message, &mut out)?;
    }
    Ok(out)
}

/// Expand one message into the Ollama messages it becomes: a `{"role": "tool"}`
/// message per tool result, then at most one more message for the turn's own
/// content, images, and tool calls.
fn push_wire_message(message: &Message, out: &mut Vec<Value>) -> Result<(), LlmError> {
    for result in &message.tool_results {
        let mut tool = json!({"role": "tool", "content": result.content});
        // Ollama tolerates an extra `tool_call_id`, and passing it keeps a
        // result matched to its call when the model emitted an id.
        if !result.call_id.is_empty() {
            tool["tool_call_id"] = json!(result.call_id);
        }
        out.push(tool);
    }
    let has_body = !message.content.is_empty()
        || !message.tool_calls.is_empty()
        || !message.attachments.is_empty();
    if !has_body {
        return Ok(());
    }
    let mut wire = json!({"role": message.role, "content": message.content});
    let mut images = Vec::new();
    for attachment in &message.attachments {
        match attachment {
            // Ollama takes bare base64 in `images`; it detects the media type
            // itself, so the stored `media_type` isn't sent.
            Attachment::Image { data, .. } => images.push(json!(data)),
            Attachment::Pdf { .. } => return Err(pdf_unsupported()),
        }
    }
    if !images.is_empty() {
        wire["images"] = json!(images);
    }
    if !message.tool_calls.is_empty() {
        let calls: Vec<Value> = message
            .tool_calls
            .iter()
            .map(|call| json!({"function": {"name": call.name, "arguments": call.args}}))
            .collect();
        wire["tool_calls"] = json!(calls);
    }
    out.push(wire);
    Ok(())
}

/// The `tools` array, the same `{"type": "function", "function": {name,
/// description, parameters}}` shape the OpenAI dialect uses.
fn wire_tools(tools: &[crate::llm::ToolSpec]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema,
                },
            })
        })
        .collect()
}

/// The typed refusal for a PDF attachment. No request reaches the server, so
/// this is `Unsupported`, not an `Api` error with a made-up status.
fn pdf_unsupported() -> LlmError {
    LlmError::Unsupported(
        "local providers cannot read PDF attachments; extract the text and send \
         it through aarg's text ingest instead"
            .to_string(),
    )
}

/// The typed error for a prompt the model clipped to fit its context window. A
/// clipped prompt means the model answered from partial evidence, so the caller
/// must see this rather than a plausible-looking completion built on less than
/// it was given.
fn truncated(prompt_eval_count: u64, estimate: u64) -> LlmError {
    LlmError::Unsupported(format!(
        "the model's context window could not hold the prompt: it processed \
         ~{prompt_eval_count} tokens of an estimated ~{estimate}, so the answer \
         would be built on a partial prompt; use a model with a larger context \
         window, or shorten the input"
    ))
}

// ---- non-streaming response parsing -------------------------------------

#[derive(Deserialize)]
struct WireResponse {
    #[serde(default)]
    model: String,
    #[serde(default)]
    message: WireMessage,
    #[serde(default)]
    done_reason: Option<String>,
    #[serde(default)]
    prompt_eval_count: u64,
    #[serde(default)]
    eval_count: u64,
}

#[derive(Deserialize, Default)]
struct WireMessage {
    #[serde(default)]
    content: String,
    // `thinking` and any other field the server adds are ignored by default.
    #[serde(default)]
    tool_calls: Vec<WireToolCall>,
}

#[derive(Deserialize)]
struct WireToolCall {
    function: WireFunction,
}

#[derive(Deserialize)]
struct WireFunction {
    #[serde(default)]
    name: String,
    // Ollama usually sends an object here, but some builds send a JSON-encoded
    // string; `normalize_args` accepts either.
    #[serde(default)]
    arguments: Value,
}

fn parse_completion(body: &str) -> Result<CompletionResponse, LlmError> {
    let wire: WireResponse = serde_json::from_str(body).map_err(LlmError::Parse)?;
    let tool_calls = wire
        .message
        .tool_calls
        .into_iter()
        .enumerate()
        .map(|(index, call)| parse_tool_call(index, call))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CompletionResponse {
        text: wire.message.content,
        tool_calls,
        model: wire.model,
        stop_reason: wire.done_reason,
        usage: TokenUsage {
            input_tokens: wire.prompt_eval_count,
            output_tokens: wire.eval_count,
        },
    })
}

/// Turn one wire tool call into a `ToolCall`. Ollama assigns no call id, so one
/// is synthesized from the call's position, giving a later tool result a stable
/// handle to echo back.
fn parse_tool_call(index: usize, wire: WireToolCall) -> Result<ToolCall, LlmError> {
    Ok(ToolCall {
        id: format!("call_{index}"),
        name: wire.function.name,
        args: normalize_args(wire.function.arguments)?,
    })
}

/// Coerce a tool call's arguments to a `Value` whether they arrived as an
/// object (the usual case) or a JSON-encoded string (some builds). A malformed
/// string is a loud `Parse` error, never a silently-empty call.
fn normalize_args(arguments: Value) -> Result<Value, LlmError> {
    match arguments {
        Value::String(text) => {
            if text.trim().is_empty() {
                Ok(json!({}))
            } else {
                serde_json::from_str(&text).map_err(LlmError::Parse)
            }
        }
        Value::Null => Ok(json!({})),
        other => Ok(other),
    }
}

/// Turn a non-2xx response into a typed error. Ollama reports errors as
/// `{"error": "message"}`; a bogus model name comes back as a 404. Anything
/// that isn't the error envelope falls back to the raw body.
fn parse_api_error(status: u16, body: &str) -> LlmError {
    #[derive(Deserialize)]
    struct Envelope {
        error: String,
    }
    let message = match serde_json::from_str::<Envelope>(body) {
        Ok(envelope) => envelope.error,
        Err(_) => body.trim().to_string(),
    };
    LlmError::Api {
        status,
        kind: "error".to_string(),
        message,
    }
}

// ---- streaming (NDJSON) -------------------------------------------------

#[derive(Deserialize)]
struct WireStreamChunk {
    #[serde(default)]
    message: WireStreamMessage,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    done_reason: Option<String>,
    #[serde(default)]
    prompt_eval_count: u64,
    #[serde(default)]
    eval_count: u64,
    // A mid-stream failure arrives as an `error` line rather than an HTTP status.
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize, Default)]
struct WireStreamMessage {
    #[serde(default)]
    content: String,
    // `thinking` is ignored.
}

/// The final token stats a `done` line reports, handed back so the caller can
/// run the truncation check against them.
#[derive(Default)]
struct DoneStats {
    prompt_eval_count: u64,
}

/// Interpret one NDJSON line. A non-final line contributes a text delta; the
/// `done` line emits `Done` and reports its stats through `stats`; an `error`
/// line becomes a `Stream` error. Blank lines are ignored.
fn handle_ndjson_line(line: &str, stats: &mut DoneStats) -> Result<Option<StreamEvent>, LlmError> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(None);
    }
    let chunk: WireStreamChunk = serde_json::from_str(line).map_err(LlmError::Parse)?;
    if let Some(error) = chunk.error {
        return Err(LlmError::Stream(error));
    }
    if chunk.done {
        stats.prompt_eval_count = chunk.prompt_eval_count;
        return Ok(Some(StreamEvent::Done {
            stop_reason: chunk.done_reason,
            usage: TokenUsage {
                input_tokens: chunk.prompt_eval_count,
                output_tokens: chunk.eval_count,
            },
        }));
    }
    if !chunk.message.content.is_empty() {
        return Ok(Some(StreamEvent::TextDelta(chunk.message.content)));
    }
    Ok(None)
}

#[async_trait]
impl LlmClient for OllamaClient {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let estimate = estimate_prompt_tokens(&request);
        let num_ctx = self.window_for(&request, estimate).await?;
        let response = self.post_chat(&request, false, num_ctx).await?;
        let status = response.status().as_u16();
        let body = response.text().await?;
        if !(200..300).contains(&status) {
            return Err(parse_api_error(status, &body));
        }
        let completion = parse_completion(&body)?;
        if looks_truncated(
            completion.usage.input_tokens,
            num_ctx,
            request.max_tokens,
            estimate,
        ) {
            return Err(truncated(completion.usage.input_tokens, estimate));
        }
        Ok(completion)
    }

    async fn stream(&self, request: CompletionRequest) -> Result<TokenStream, LlmError> {
        let estimate = estimate_prompt_tokens(&request);
        let num_ctx = self.window_for(&request, estimate).await?;
        let response = self.post_chat(&request, true, num_ctx).await?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let body = response.text().await?;
            return Err(parse_api_error(status, &body));
        }

        let (tx, rx) = mpsc::channel(32);
        tokio::spawn(pump_ndjson(
            response.bytes_stream(),
            tx,
            num_ctx,
            request.max_tokens,
            estimate,
        ));
        Ok(Box::pin(ReceiverStream::new(rx)))
    }
}

/// Read the NDJSON byte stream and forward parsed events into `tx`. Factored
/// out of `stream` and generic over the byte source so tests can drive it with
/// fixture chunks instead of a live socket. `num_ctx`, `max_tokens`, and
/// `estimate` feed the truncation check run against the `done` line's stats.
async fn pump_ndjson<B, S>(
    mut bytes: S,
    tx: mpsc::Sender<Result<StreamEvent, LlmError>>,
    num_ctx: u32,
    max_tokens: u32,
    estimate: u64,
) where
    B: AsRef<[u8]>,
    S: futures_util::Stream<Item = Result<B, reqwest::Error>> + Unpin,
{
    let mut buffer: Vec<u8> = Vec::new();
    let mut stats = DoneStats::default();
    // The first chunk only arrives after the model has evaluated the whole
    // prompt, and that prefill can take minutes for an 8k-token prompt on
    // busy hardware, so the first wait gets the same total budget a
    // non-streaming request does. Once tokens are flowing, two minutes of
    // silence means a dead stream, not a slow one.
    let mut idle = COMPLETE_TIMEOUT;
    loop {
        let next = tokio::time::timeout(idle, bytes.next()).await;
        let chunk = match next {
            Err(_elapsed) => {
                let _ = tx
                    .send(Err(LlmError::Stream(format!(
                        "no data from the model for {}s",
                        idle.as_secs()
                    ))))
                    .await;
                return;
            }
            Ok(None) => break,
            Ok(Some(Ok(chunk))) => chunk,
            Ok(Some(Err(error))) => {
                let _ = tx.send(Err(LlmError::Http(error))).await;
                return;
            }
        };
        idle = STREAM_IDLE_TIMEOUT;
        buffer.extend_from_slice(chunk.as_ref());
        for line in drain_lines(&mut buffer) {
            match handle_ndjson_line(&line, &mut stats) {
                Ok(Some(StreamEvent::Done { stop_reason, usage })) => {
                    // The same truncation guard as `complete`, applied to
                    // the stream's final stats before Done goes out. The
                    // `done: true` line is the end of the response, so the
                    // task stops reading either way.
                    if looks_truncated(stats.prompt_eval_count, num_ctx, max_tokens, estimate) {
                        let _ = tx
                            .send(Err(truncated(stats.prompt_eval_count, estimate)))
                            .await;
                        return;
                    }
                    let _ = tx.send(Ok(StreamEvent::Done { stop_reason, usage })).await;
                    return;
                }
                Ok(Some(event)) => {
                    if tx.send(Ok(event)).await.is_err() {
                        return;
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    let _ = tx.send(Err(error)).await;
                    return;
                }
            }
        }
    }
    // The byte stream ended without a `done: true` line: the connection
    // dropped mid-generation. Ollama always ends a completed response with
    // one, so the text so far is a truncated reply, and ending the stream
    // without a Done would let the caller mistake it for a finished one.
    let _ = tx
        .send(Err(LlmError::Stream(
            "stream ended before done: true".to_string(),
        )))
        .await;
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::llm::types::{Message, Role, ToolResult, ToolSpec};

    fn request() -> CompletionRequest {
        CompletionRequest {
            model: "qwen3:8b".to_string(),
            max_tokens: 64,
            system: None,
            messages: vec![Message::user("hello")],
            temperature: None,
            tools: Vec::new(),
        }
    }

    #[test]
    fn request_body_places_system_first_and_sets_options() {
        let mut req = request();
        req.system = Some("be brief".to_string());
        let body = request_body(&req, false, "5m", 8192).unwrap();
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "be brief");
        assert_eq!(body["stream"], false);
        assert_eq!(body["keep_alive"], "5m");
        assert_eq!(body["options"]["num_ctx"], 8192);
        assert_eq!(body["options"]["num_predict"], 64);
        assert!(body["options"].get("temperature").is_none());
    }

    #[test]
    fn request_body_includes_temperature_when_set() {
        let mut req = request();
        req.temperature = Some(0.5);
        let body = request_body(&req, false, "5m", 8192).unwrap();
        assert!((body["options"]["temperature"].as_f64().unwrap() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn request_body_writes_the_tools_array_shape() {
        let mut req = request();
        req.tools = vec![ToolSpec {
            name: "fetch_jd".into(),
            description: "Fetch a posting".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }];
        let body = request_body(&req, false, "5m", 8192).unwrap();
        let tool = &body["tools"][0];
        assert_eq!(tool["type"], "function");
        assert_eq!(tool["function"]["name"], "fetch_jd");
        assert_eq!(tool["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn request_body_round_trips_tool_calls_and_results() {
        let mut req = request();
        req.messages = vec![
            Message::user("get it"),
            Message {
                role: Role::Assistant,
                content: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call_0".into(),
                    name: "fetch_jd".into(),
                    args: serde_json::json!({"url": "https://x"}),
                }],
                tool_results: Vec::new(),
                attachments: Vec::new(),
            },
            Message::tool_results(vec![ToolResult {
                call_id: "call_0".into(),
                content: "the posting".into(),
                is_error: false,
            }]),
        ];
        let body = request_body(&req, false, "5m", 8192).unwrap();
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        // Native Ollama carries the arguments as an object, not a string.
        let call = &messages[1]["tool_calls"][0];
        assert_eq!(call["function"]["name"], "fetch_jd");
        assert_eq!(call["function"]["arguments"]["url"], "https://x");
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[2]["content"], "the posting");
        assert_eq!(messages[2]["tool_call_id"], "call_0");
    }

    #[test]
    fn request_body_writes_an_image_into_the_images_array() {
        let mut req = request();
        req.messages = vec![Message::user_with_attachment(
            "describe this",
            Attachment::Image {
                media_type: "image/png".into(),
                data: "aGVsbG8=".into(),
            },
        )];
        let body = request_body(&req, false, "5m", 8192).unwrap();
        let message = &body["messages"][0];
        assert_eq!(message["content"], "describe this");
        // Bare base64, no data: URI, no media type.
        assert_eq!(message["images"][0], "aGVsbG8=");
    }

    #[test]
    fn a_pdf_attachment_is_refused_loudly() {
        let mut req = request();
        req.messages = vec![Message::user_with_attachment(
            "read this",
            Attachment::Pdf {
                data: "JVBERi0=".into(),
            },
        )];
        let err = request_body(&req, false, "5m", 8192).unwrap_err();
        match err {
            LlmError::Unsupported(message) => {
                assert!(message.contains("PDF"));
            }
            other => panic!("expected Unsupported error, got {other:?}"),
        }
    }

    #[test]
    fn effective_num_ctx_escalates_above_the_floor_for_a_long_prompt() {
        let mut req = request();
        // ~40000 chars of prompt ≈ 10000 tokens, over the 8192 floor.
        req.messages = vec![Message::user("x".repeat(40_000))];
        let estimate = estimate_prompt_tokens(&req);
        let num_ctx = effective_num_ctx(DEFAULT_NUM_CTX, estimate, req.max_tokens);
        assert!(
            num_ctx > DEFAULT_NUM_CTX,
            "num_ctx {num_ctx} should exceed the floor"
        );
        let body = request_body(&req, false, "5m", num_ctx).unwrap();
        assert_eq!(body["options"]["num_ctx"], num_ctx);
    }

    #[test]
    fn parse_completion_reads_content_usage_and_tool_calls() {
        let body = r#"{
            "model": "qwen3:8b",
            "message": {"role": "assistant", "content": "",
                "thinking": "hmm",
                "tool_calls": [
                    {"function": {"name": "fetch_jd", "arguments": {"url": "https://x"}}}
                ]},
            "done_reason": "stop",
            "done": true,
            "prompt_eval_count": 30,
            "eval_count": 12
        }"#;
        let response = parse_completion(body).unwrap();
        assert_eq!(response.text, "");
        assert_eq!(response.tool_calls.len(), 1);
        // Native arguments arrive as an object; the synthesized id is index-based.
        assert_eq!(response.tool_calls[0].id, "call_0");
        assert_eq!(response.tool_calls[0].name, "fetch_jd");
        assert_eq!(response.tool_calls[0].args["url"], "https://x");
        assert_eq!(response.stop_reason.as_deref(), Some("stop"));
        assert_eq!(response.usage.input_tokens, 30);
        assert_eq!(response.usage.output_tokens, 12);
    }

    #[test]
    fn parse_completion_accepts_string_encoded_tool_arguments() {
        // Some builds send arguments as a JSON-encoded string; handle both.
        let body = r#"{
            "model": "m",
            "message": {"role": "assistant", "content": "",
                "tool_calls": [
                    {"function": {"name": "x", "arguments": "{\"a\": 1}"}}
                ]},
            "done": true
        }"#;
        let response = parse_completion(body).unwrap();
        assert_eq!(response.tool_calls[0].args["a"], 1);
    }

    #[test]
    fn parse_completion_rejects_malformed_string_arguments() {
        let body = r#"{
            "message": {"tool_calls": [{"function": {"name": "x", "arguments": "{bad"}}]}
        }"#;
        let err = parse_completion(body).unwrap_err();
        assert!(matches!(err, LlmError::Parse(_)));
    }

    #[test]
    fn parse_api_error_reads_the_error_string() {
        let body = r#"{"error": "model 'bogus' not found, try pulling it first"}"#;
        match parse_api_error(404, body) {
            LlmError::Api {
                status,
                kind,
                message,
            } => {
                assert_eq!(status, 404);
                assert_eq!(kind, "error");
                assert!(message.contains("not found"));
            }
            other => panic!("expected Api error, got {other:?}"),
        }
        // A non-envelope body falls back to raw.
        match parse_api_error(502, "upstream down") {
            LlmError::Api { message, .. } => assert_eq!(message, "upstream down"),
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[test]
    fn complete_detects_a_truncated_prompt() {
        // The probed clip shape: the window is the one effective_num_ctx
        // produces for this estimate, and the count sits at window / 2 + 2,
        // where Ollama clips an oversized prompt (verified live at windows of
        // 512, 1024, and 2048).
        let estimate = 12000;
        let max_tokens = 512;
        let window = effective_num_ctx(DEFAULT_NUM_CTX, estimate, max_tokens);
        assert_eq!(u64::from(window), 12000 + 512 + 512);
        let clipped = u64::from(window) / 2 + 2;
        assert!(looks_truncated(clipped, window, max_tokens, estimate));
        // And the typed error names the shortfall.
        match truncated(8192, 12000) {
            LlmError::Unsupported(message) => {
                assert!(message.contains("~8192"));
                assert!(message.contains("~12000"));
                assert!(message.contains("larger context"));
            }
            other => panic!("expected Unsupported error, got {other:?}"),
        }
    }

    #[test]
    fn context_length_is_read_from_the_arch_prefixed_key() {
        // The real /api/show shape: model_info keys carry the architecture as
        // a prefix (llama.context_length, qwen3.context_length, ...).
        let body = r#"{
            "details": {"family": "llama"},
            "model_info": {
                "general.architecture": "llama",
                "llama.context_length": 131072,
                "llama.embedding_length": 8192
            }
        }"#;
        assert_eq!(context_length_from_show(body), Some(131072));
        // No context_length key, or a body that isn't the show envelope.
        assert_eq!(context_length_from_show(r#"{"model_info": {}}"#), None);
        assert_eq!(context_length_from_show("not json"), None);
    }

    #[test]
    fn verified_window_passes_caps_or_refuses() {
        // Under the model maximum: unchanged.
        assert_eq!(
            verified_window(8192, 2000, 256, Some(131072)).unwrap(),
            8192
        );
        // Unknown maximum: proceed unverified.
        assert_eq!(verified_window(8192, 2000, 256, None).unwrap(), 8192);
        // The floor exceeds the model maximum but the prompt itself fits:
        // capped to the maximum instead of letting the server cap it quietly.
        assert_eq!(verified_window(8192, 2000, 256, Some(4096)).unwrap(), 4096);
        // The prompt plus completion budget cannot fit the model at all:
        // refused before anything is sent.
        let err = verified_window(8192, 6000, 512, Some(4096)).unwrap_err();
        match err {
            LlmError::Unsupported(message) => {
                assert!(message.contains("~6000"));
                assert!(message.contains("4096"));
                assert!(message.contains("larger context"));
            }
            other => panic!("expected Unsupported error, got {other:?}"),
        }
    }

    #[test]
    fn handle_ndjson_line_ignores_blank_lines() {
        let mut stats = DoneStats::default();
        assert!(handle_ndjson_line("", &mut stats).unwrap().is_none());
        assert!(handle_ndjson_line("  ", &mut stats).unwrap().is_none());
    }

    #[test]
    fn handle_ndjson_line_streams_deltas_then_done_with_stats() {
        let mut stats = DoneStats::default();

        let delta = r#"{"message": {"role": "assistant", "content": "Hi", "thinking": "x"}, "done": false}"#;
        assert_eq!(
            handle_ndjson_line(delta, &mut stats).unwrap(),
            Some(StreamEvent::TextDelta("Hi".to_string()))
        );

        let done = r#"{"message": {"role": "assistant", "content": ""}, "done": true, "done_reason": "stop", "prompt_eval_count": 15, "eval_count": 6}"#;
        match handle_ndjson_line(done, &mut stats).unwrap() {
            Some(StreamEvent::Done { stop_reason, usage }) => {
                assert_eq!(stop_reason.as_deref(), Some("stop"));
                assert_eq!(usage.input_tokens, 15);
                assert_eq!(usage.output_tokens, 6);
            }
            other => panic!("expected Done, got {other:?}"),
        }
        assert_eq!(stats.prompt_eval_count, 15);
    }

    /// Drive `pump_ndjson` with fixture byte chunks and collect everything it
    /// forwards, exactly as a caller would read the `TokenStream`.
    async fn run_pump(chunks: Vec<&'static [u8]>) -> Vec<Result<StreamEvent, LlmError>> {
        let (tx, mut rx) = mpsc::channel(32);
        let bytes = futures_util::stream::iter(chunks.into_iter().map(Ok::<_, reqwest::Error>));
        pump_ndjson(bytes, tx, 8192, 64, 100).await;
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        events
    }

    #[tokio::test]
    async fn a_stream_that_ends_without_done_true_is_an_error() {
        // The connection drops mid-generation: delta lines arrived but the
        // final done: true line never did. Ending the TokenStream silently
        // here would let the caller mistake the partial text for a finished
        // reply with default usage.
        let events = run_pump(vec![
            b"{\"message\": {\"role\": \"assistant\", \"content\": \"Hi\"}, \"done\": false}\n",
            b"{\"message\": {\"role\": \"assistant\", \"content\": \"!\"}, \"done\": false}\n",
        ])
        .await;
        assert_eq!(events.len(), 3);
        assert_eq!(
            events[0].as_ref().unwrap(),
            &StreamEvent::TextDelta("Hi".to_string())
        );
        match &events[2] {
            Err(LlmError::Stream(message)) => {
                assert!(message.contains("before done: true"), "got: {message}");
            }
            other => panic!("expected a Stream error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_terminated_stream_ends_with_one_done_and_stops_reading() {
        let events = run_pump(vec![
            b"{\"message\": {\"role\": \"assistant\", \"content\": \"Hi\"}, \"done\": false}\n",
            b"{\"message\": {\"content\": \"\"}, \"done\": true, \"done_reason\": \"stop\", \"prompt_eval_count\": 100, \"eval_count\": 2}\n",
            b"{not even json\n",
        ])
        .await;
        // Delta, then Done; the garbage after done: true was never read.
        assert_eq!(events.len(), 2);
        match &events[1] {
            Ok(StreamEvent::Done { stop_reason, usage }) => {
                assert_eq!(stop_reason.as_deref(), Some("stop"));
                assert_eq!(usage.input_tokens, 100);
                assert_eq!(usage.output_tokens, 2);
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[test]
    fn handle_ndjson_line_surfaces_a_mid_stream_error() {
        let mut stats = DoneStats::default();
        let err = handle_ndjson_line(r#"{"error": "out of memory"}"#, &mut stats).unwrap_err();
        assert!(matches!(err, LlmError::Stream(_)));
    }
}
