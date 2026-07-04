//! A hand-rolled client for the OpenAI-compatible chat-completions API, the
//! dialect LM Studio (and many other local servers) speak.
//!
//! Written directly against the HTTP wire format with `reqwest` — no SDK. The
//! request/response shapes (`/v1/chat/completions`, the `choices` envelope, the
//! `data:` SSE stream) live entirely here; the rest of the codebase speaks the
//! provider-agnostic `llm::types`.
//!
//! AARG's prompts run roughly 4k-8k tokens, and a local model is usually loaded
//! with a fixed context window this client cannot resize per request. What
//! happens on overflow depends on the server's overflow policy. With LM
//! Studio's error policy the server answers HTTP 400 naming `n_ctx`/`n_keep`;
//! this client recognizes that shape and rewrites it into advice a user can act
//! on (reload the model with a larger context), because a raw `n_ctx` error is
//! otherwise opaque. With the Truncate Middle or Rolling Window policies the
//! server instead returns 200 with a silently clipped prompt, so the client
//! also compares the reported `usage.prompt_tokens` against its own estimate
//! and refuses a reply whose prompt came back materially short (see
//! [`crate::llm::context`]); a clipped prompt is dropped evidence, not a
//! smaller request. That comparison is safe against prompt caching: probed
//! live, LM Studio reports the full prompt size in `usage.prompt_tokens` even
//! when the prefix KV cache is hit (repeats of a shared-prefix request kept
//! reporting the full count while wall time collapsed 14x).

use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::llm::client::LlmClient;
use crate::llm::context::{estimate_prompt_tokens, looks_clamped};
use crate::llm::lines::drain_lines;
use crate::llm::types::{
    Attachment, CompletionRequest, CompletionResponse, LlmError, Message, StreamEvent, TokenStream,
    TokenUsage, ToolCall,
};

/// How long to wait for the TCP connection before giving up. A local server
/// that isn't running should fail fast, not hang.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Total budget for a non-streaming request. Local models on modest hardware
/// can take minutes to finish a long completion, so this is generous; it exists
/// only to bound a request that has genuinely wedged, not to clip slow output.
const COMPLETE_TIMEOUT: Duration = Duration::from_secs(600);

/// How long to wait for the next byte of a *stream* before declaring it stalled.
/// Applied per chunk, not to the whole response, so a slow-but-alive model keeps
/// streaming while a truly dead connection is cut loose.
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// An `LlmClient` backed by an OpenAI-compatible chat-completions server, such
/// as LM Studio.
pub struct OpenAiCompatClient {
    http: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
}

impl OpenAiCompatClient {
    /// Build a client pointed at `base_url` (e.g. `http://127.0.0.1:1234`),
    /// with no authentication. Most local servers accept anonymous requests.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            http: build_http(),
            base_url: base_url.into(),
            api_key: None,
        }
    }

    /// Send `Authorization: Bearer <key>` with every request. LM Studio can be
    /// configured to require a key; hosted OpenAI-compatible gateways always do.
    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    async fn post_chat(
        &self,
        request: &CompletionRequest,
        stream: bool,
    ) -> Result<reqwest::Response, LlmError> {
        let mut builder = self
            .http
            .post(format!("{}/v1/chat/completions", self.base_url));
        if let Some(key) = &self.api_key {
            builder = builder.header("authorization", format!("Bearer {key}"));
        }
        // A non-streaming call gets a total timeout; a stream is bounded by the
        // per-chunk idle timeout instead, so a long-but-live stream isn't cut.
        if !stream {
            builder = builder.timeout(COMPLETE_TIMEOUT);
        }
        let body = request_body(request, stream)?;
        Ok(builder.json(&body).send().await?)
    }
}

/// Build the HTTP client with a connect timeout. If the builder somehow fails
/// (it does not, for these settings), fall back to a default client rather than
/// panic — the connect timeout is an optimization, not a correctness need.
fn build_http() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

// ---- request assembly ---------------------------------------------------

/// Build the chat-completions request body from our provider-agnostic request.
/// `temperature` and `tools` are included only when set (an explicit `null`
/// temperature is an error on some servers); `max_tokens` is always sent.
/// Returns an error only when a message carries a PDF attachment, which local
/// providers can't read (see [`wire_attachment`]).
fn request_body(request: &CompletionRequest, stream: bool) -> Result<Value, LlmError> {
    let messages = build_messages(&request.system, &request.messages)?;
    let mut body = json!({
        "model": request.model,
        "messages": messages,
        "max_tokens": request.max_tokens,
        "stream": stream,
    });
    if let Some(temperature) = request.temperature {
        body["temperature"] = json!(temperature);
    }
    if !request.tools.is_empty() {
        body["tools"] = json!(wire_tools(&request.tools));
    }
    if stream {
        // Ask the server to append a final chunk carrying token usage; without
        // this option most servers omit usage from a streamed response.
        body["stream_options"] = json!({"include_usage": true});
    }
    Ok(body)
}

/// The full `messages` array: a leading `system` message when the request has a
/// system prompt, then every conversation turn expanded to OpenAI shape.
fn build_messages(system: &Option<String>, messages: &[Message]) -> Result<Vec<Value>, LlmError> {
    let mut out = Vec::new();
    if let Some(system) = system {
        out.push(json!({"role": "system", "content": system}));
    }
    for message in messages {
        push_wire_messages(message, &mut out)?;
    }
    Ok(out)
}

/// Expand one provider-agnostic message into the one or more OpenAI messages it
/// becomes. Tool results each become a standalone `{"role": "tool"}` message;
/// the turn's own content, attachments, and tool calls become at most one more
/// message after them.
fn push_wire_messages(message: &Message, out: &mut Vec<Value>) -> Result<(), LlmError> {
    for result in &message.tool_results {
        out.push(json!({
            "role": "tool",
            "tool_call_id": result.call_id,
            "content": result.content,
        }));
    }
    let has_body = !message.content.is_empty()
        || !message.tool_calls.is_empty()
        || !message.attachments.is_empty();
    if !has_body {
        return Ok(());
    }
    let content = if message.attachments.is_empty() {
        json!(message.content)
    } else {
        // A turn with an image becomes the content-parts form: a text part
        // followed by an image part per attachment.
        let mut parts = vec![json!({"type": "text", "text": message.content})];
        for attachment in &message.attachments {
            parts.push(wire_attachment(attachment)?);
        }
        json!(parts)
    };
    let mut wire = json!({"role": message.role, "content": content});
    if !message.tool_calls.is_empty() {
        let calls: Result<Vec<Value>, LlmError> =
            message.tool_calls.iter().map(wire_tool_call).collect();
        wire["tool_calls"] = json!(calls?);
    }
    out.push(wire);
    Ok(())
}

/// One assistant tool call as an OpenAI `tool_calls` entry. The arguments cross
/// the wire as a *JSON-encoded string*, not a nested object — the one shape
/// quirk of this API — so `args` is re-serialized with `to_string`.
fn wire_tool_call(call: &ToolCall) -> Result<Value, LlmError> {
    let arguments = serde_json::to_string(&call.args).map_err(LlmError::Parse)?;
    Ok(json!({
        "id": call.id,
        "type": "function",
        "function": {"name": call.name, "arguments": arguments},
    }))
}

/// The `tools` array: each `ToolSpec` as `{"type": "function", "function":
/// {name, description, parameters}}`, where `parameters` is the spec's JSON
/// schema.
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

/// One attachment as an OpenAI content part. An image becomes an `image_url`
/// part with an inline `data:` URI. A PDF is refused loudly: local providers
/// can't read PDF bytes, and silently dropping the attachment would send the
/// model a prompt that references a document it never saw.
fn wire_attachment(attachment: &Attachment) -> Result<Value, LlmError> {
    match attachment {
        Attachment::Image { media_type, data } => Ok(json!({
            "type": "image_url",
            "image_url": {"url": format!("data:{media_type};base64,{data}")},
        })),
        Attachment::Pdf { .. } => Err(pdf_unsupported()),
    }
}

/// The typed refusal for a reply whose prompt came back materially short of
/// the estimate: the server's overflow policy (LM Studio's Truncate Middle or
/// Rolling Window) clipped the prompt and returned 200 as if nothing happened.
fn clamped(prompt_tokens: u64, estimate: u64) -> LlmError {
    LlmError::Unsupported(format!(
        "the server processed only ~{prompt_tokens} prompt tokens of an \
         estimated ~{estimate}, so its overflow policy clipped the prompt and \
         the answer would be built on a partial prompt; reload the model with \
         a larger context length, or shorten the input"
    ))
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

// ---- non-streaming response parsing -------------------------------------

#[derive(Deserialize)]
struct WireCompletion {
    #[serde(default)]
    model: String,
    #[serde(default)]
    choices: Vec<WireChoice>,
    #[serde(default)]
    usage: WireUsage,
}

#[derive(Deserialize)]
struct WireChoice {
    message: WireMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct WireMessage {
    // Absent or null when the turn is only tool calls; `reasoning_content` and
    // any other field the server adds is ignored by serde's default.
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<WireToolCall>,
}

#[derive(Deserialize)]
struct WireToolCall {
    #[serde(default)]
    id: String,
    function: WireFunction,
}

#[derive(Deserialize)]
struct WireFunction {
    #[serde(default)]
    name: String,
    // The arguments arrive as a JSON-encoded *string*, parsed in `parse_args`.
    #[serde(default)]
    arguments: String,
}

#[derive(Deserialize, Default, Clone, Copy)]
struct WireUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}

impl From<WireUsage> for TokenUsage {
    fn from(usage: WireUsage) -> Self {
        TokenUsage {
            input_tokens: usage.prompt_tokens,
            output_tokens: usage.completion_tokens,
        }
    }
}

fn parse_completion(body: &str) -> Result<CompletionResponse, LlmError> {
    let wire: WireCompletion = serde_json::from_str(body).map_err(LlmError::Parse)?;
    let (text, tool_calls, stop_reason) = match wire.choices.into_iter().next() {
        Some(choice) => {
            let text = choice.message.content.unwrap_or_default();
            let tool_calls = choice
                .message
                .tool_calls
                .into_iter()
                .map(parse_tool_call)
                .collect::<Result<Vec<_>, _>>()?;
            (text, tool_calls, choice.finish_reason)
        }
        None => (String::new(), Vec::new(), None),
    };
    Ok(CompletionResponse {
        text,
        tool_calls,
        model: wire.model,
        stop_reason,
        usage: wire.usage.into(),
    })
}

/// Turn one wire tool call into a `ToolCall`, parsing its JSON-encoded argument
/// string into a `Value`. A malformed argument string is a loud `Parse` error,
/// never a silently-empty call — the model asked for something specific and the
/// caller must not act on a corrupted version of it.
fn parse_tool_call(wire: WireToolCall) -> Result<ToolCall, LlmError> {
    let args = parse_args(&wire.function.arguments)?;
    Ok(ToolCall {
        id: wire.id,
        name: wire.function.name,
        args,
    })
}

fn parse_args(arguments: &str) -> Result<Value, LlmError> {
    if arguments.trim().is_empty() {
        // A no-argument tool comes back as "" on some servers; that is an empty
        // object, not a parse failure.
        return Ok(json!({}));
    }
    serde_json::from_str(arguments).map_err(LlmError::Parse)
}

// ---- error bodies -------------------------------------------------------

/// Turn a non-2xx response into a typed error. OpenAI-compatible servers report
/// errors two ways — `{"error": {"message", "type", "code"}}` and the bare
/// `{"error": "message"}` — so both are handled, falling back to the raw body
/// for anything else (a proxy's HTML page). A body that names a context-length
/// overflow is rewritten into actionable advice, keeping the server's own words
/// as the tail; `estimate` is the prompt size to report there.
fn parse_api_error(status: u16, body: &str, estimate: u64) -> LlmError {
    let (kind, message) = extract_error(body);
    let message = if is_context_overflow(&message) {
        format!(
            "the prompt (~{estimate} tokens) exceeds the model's loaded context \
             window; reload the model with a larger context in LM Studio \
             (server said: {})",
            message.trim()
        )
    } else {
        message
    };
    LlmError::Api {
        status,
        kind,
        message,
    }
}

fn extract_error(body: &str) -> (String, String) {
    #[derive(Deserialize)]
    struct Envelope {
        error: Value,
    }
    match serde_json::from_str::<Envelope>(body) {
        Ok(Envelope {
            error: Value::String(message),
        }) => ("unknown".to_string(), message),
        Ok(Envelope {
            error: Value::Object(map),
        }) => {
            let message = map
                .get("message")
                .and_then(Value::as_str)
                .map_or_else(|| body.trim().to_string(), str::to_string);
            let kind = map
                .get("code")
                .and_then(kind_from_value)
                .or_else(|| map.get("type").and_then(kind_from_value))
                .unwrap_or_else(|| "unknown".to_string());
            (kind, message)
        }
        _ => ("unknown".to_string(), body.trim().to_string()),
    }
}

/// A `code`/`type` field can be a string or a number; render either as the
/// error kind, treating an empty string or a null as absent.
fn kind_from_value(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Whether an error message is LM Studio's context-window overflow. The server
/// phrases it around `n_ctx`/`n_keep` or "context length"; matching the
/// substrings is loose on purpose, since the exact wording varies by version.
fn is_context_overflow(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("n_ctx")
        || lower.contains("n_keep")
        || lower.contains("context length")
        || lower.contains("context window")
}

// ---- streaming (server-sent events) -------------------------------------

#[derive(Deserialize)]
struct WireChunk {
    #[serde(default)]
    choices: Vec<WireChunkChoice>,
    // Present only on the final usage chunk (thanks to `stream_options`).
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct WireChunkChoice {
    #[serde(default)]
    delta: WireDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct WireDelta {
    // `reasoning_content` and other fields are ignored.
    #[serde(default)]
    content: Option<String>,
}

/// Metadata accumulated across the stream, emitted once with `Done`.
#[derive(Default)]
struct StreamState {
    usage: TokenUsage,
    finish_reason: Option<String>,
}

/// Interpret one SSE line: update the accumulated state and return the event to
/// surface, if any. `data: [DONE]` ends the stream and emits `Done` with the
/// captured usage and finish reason; a `data:` chunk contributes a text delta
/// and/or updates the pending metadata; anything else (blank keep-alive lines,
/// `event:` lines) is ignored.
fn handle_sse_line(line: &str, state: &mut StreamState) -> Result<Option<StreamEvent>, LlmError> {
    let Some(data) = line.strip_prefix("data:") else {
        return Ok(None);
    };
    let data = data.trim();
    if data.is_empty() {
        return Ok(None);
    }
    if data == "[DONE]" {
        return Ok(Some(StreamEvent::Done {
            stop_reason: state.finish_reason.take(),
            usage: state.usage,
        }));
    }
    let chunk: WireChunk = serde_json::from_str(data).map_err(LlmError::Parse)?;
    if let Some(usage) = chunk.usage {
        // The final chunk carries usage with an empty `choices` array.
        state.usage = usage.into();
    }
    if let Some(choice) = chunk.choices.into_iter().next() {
        if let Some(reason) = choice.finish_reason {
            state.finish_reason = Some(reason);
        }
        if let Some(content) = choice.delta.content
            && !content.is_empty()
        {
            return Ok(Some(StreamEvent::TextDelta(content)));
        }
    }
    Ok(None)
}

#[async_trait]
impl LlmClient for OpenAiCompatClient {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let estimate = estimate_prompt_tokens(&request);
        let response = self.post_chat(&request, false).await?;
        let status = response.status().as_u16();
        let body = response.text().await?;
        if !(200..300).contains(&status) {
            return Err(parse_api_error(status, &body, estimate));
        }
        let completion = parse_completion(&body)?;
        if looks_clamped(completion.usage.input_tokens, estimate) {
            return Err(clamped(completion.usage.input_tokens, estimate));
        }
        Ok(completion)
    }

    async fn stream(&self, request: CompletionRequest) -> Result<TokenStream, LlmError> {
        let estimate = estimate_prompt_tokens(&request);
        let response = self.post_chat(&request, true).await?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let body = response.text().await?;
            return Err(parse_api_error(status, &body, estimate));
        }

        let (tx, rx) = mpsc::channel(32);
        tokio::spawn(pump_sse(response.bytes_stream(), tx, estimate));
        Ok(Box::pin(ReceiverStream::new(rx)))
    }
}

/// The clamp check a stream's final usage gets before `Done` goes out: the
/// same estimate-versus-reported-count comparison `complete` runs, so a
/// silently clipped prompt is refused on both paths. Returns the event to
/// forward.
fn checked_done(
    stop_reason: Option<String>,
    usage: TokenUsage,
    estimate: u64,
) -> Result<StreamEvent, LlmError> {
    if looks_clamped(usage.input_tokens, estimate) {
        return Err(clamped(usage.input_tokens, estimate));
    }
    Ok(StreamEvent::Done { stop_reason, usage })
}

/// Read the SSE byte stream and forward parsed events into `tx`. Factored out
/// of `stream` and generic over the byte source so tests can drive it with
/// fixture chunks instead of a live socket. `estimate` feeds the clamp check
/// the final usage gets before `Done` is forwarded.
///
/// The task stops reading the moment the stream's outcome is sent: `data:
/// [DONE]` is the end of the response, and reading past it would misread
/// anything a misbehaving server tacks on (a duplicate sentinel would
/// double-send `Done`).
async fn pump_sse<B, S>(
    mut bytes: S,
    tx: mpsc::Sender<Result<StreamEvent, LlmError>>,
    estimate: u64,
) where
    B: AsRef<[u8]>,
    S: futures_util::Stream<Item = Result<B, reqwest::Error>> + Unpin,
{
    let mut buffer: Vec<u8> = Vec::new();
    let mut state = StreamState::default();
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
            match handle_sse_line(&line, &mut state) {
                Ok(Some(StreamEvent::Done { stop_reason, usage })) => {
                    let _ = tx.send(checked_done(stop_reason, usage, estimate)).await;
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
    // The byte stream ended without `data: [DONE]`. If a finish_reason already
    // arrived, generation provably completed and only the sentinel was lost in
    // transit, so emit the Done it implied. Otherwise the connection dropped
    // mid-generation, and the text so far is a truncated reply that must not
    // present as a complete one.
    if state.finish_reason.is_some() {
        let _ = tx
            .send(checked_done(
                state.finish_reason.take(),
                state.usage,
                estimate,
            ))
            .await;
    } else {
        let _ = tx
            .send(Err(LlmError::Stream(
                "stream ended before data: [DONE]".to_string(),
            )))
            .await;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::llm::types::{Message, Role, ToolResult, ToolSpec};

    fn request() -> CompletionRequest {
        CompletionRequest {
            model: "qwen3-1.7b".to_string(),
            max_tokens: 64,
            system: None,
            messages: vec![Message::user("hello")],
            temperature: None,
            tools: Vec::new(),
        }
    }

    #[test]
    fn request_body_places_the_system_prompt_first() {
        let mut req = request();
        req.system = Some("be brief".to_string());
        let body = request_body(&req, false).unwrap();
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "be brief");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"], "hello");
    }

    #[test]
    fn request_body_omits_temperature_when_none_and_includes_it_when_set() {
        let bare = request_body(&request(), false).unwrap();
        assert!(bare.get("temperature").is_none());
        assert_eq!(bare["max_tokens"], 64);
        assert!(bare.get("stream_options").is_none());

        let mut req = request();
        req.temperature = Some(0.5);
        let body = request_body(&req, true).unwrap();
        assert!((body["temperature"].as_f64().unwrap() - 0.5).abs() < f64::EPSILON);
        // Streaming asks for the trailing usage chunk.
        assert_eq!(body["stream_options"]["include_usage"], true);
    }

    #[test]
    fn request_body_writes_the_tools_array_shape() {
        let mut req = request();
        req.tools = vec![ToolSpec {
            name: "fetch_jd".into(),
            description: "Fetch a posting".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }];
        let body = request_body(&req, false).unwrap();
        let tool = &body["tools"][0];
        assert_eq!(tool["type"], "function");
        assert_eq!(tool["function"]["name"], "fetch_jd");
        assert_eq!(tool["function"]["description"], "Fetch a posting");
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
                    id: "call_1".into(),
                    name: "fetch_jd".into(),
                    args: serde_json::json!({"url": "https://x"}),
                }],
                tool_results: Vec::new(),
                attachments: Vec::new(),
            },
            Message::tool_results(vec![ToolResult {
                call_id: "call_1".into(),
                content: "the posting".into(),
                is_error: false,
            }]),
        ];
        let body = request_body(&req, false).unwrap();
        let messages = body["messages"].as_array().unwrap();
        // user, assistant(tool_calls), tool — the tool-results turn carries no
        // extra assistant/user message of its own.
        assert_eq!(messages.len(), 3);
        let call = &messages[1]["tool_calls"][0];
        assert_eq!(call["id"], "call_1");
        assert_eq!(call["type"], "function");
        assert_eq!(call["function"]["name"], "fetch_jd");
        // The arguments cross the wire as a JSON-encoded *string*.
        assert_eq!(call["function"]["arguments"], r#"{"url":"https://x"}"#);
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[2]["tool_call_id"], "call_1");
        assert_eq!(messages[2]["content"], "the posting");
    }

    #[test]
    fn request_body_writes_an_image_as_content_parts() {
        let mut req = request();
        req.messages = vec![Message::user_with_attachment(
            "describe this",
            Attachment::Image {
                media_type: "image/png".into(),
                data: "aGVsbG8=".into(),
            },
        )];
        let body = request_body(&req, false).unwrap();
        let content = &body["messages"][0]["content"];
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "describe this");
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(
            content[1]["image_url"]["url"],
            "data:image/png;base64,aGVsbG8="
        );
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
        let err = request_body(&req, false).unwrap_err();
        match err {
            LlmError::Unsupported(message) => {
                assert!(message.contains("PDF"));
                assert!(message.contains("text ingest"));
            }
            other => panic!("expected Unsupported error, got {other:?}"),
        }
    }

    #[test]
    fn parse_completion_reads_text_finish_reason_and_usage() {
        let body = r#"{
            "model": "qwen3-1.7b",
            "choices": [
                {"index": 0, "message": {"role": "assistant", "content": "Hi there"},
                 "finish_reason": "stop"}
            ],
            "usage": {"prompt_tokens": 11, "completion_tokens": 3}
        }"#;
        let response = parse_completion(body).unwrap();
        assert_eq!(response.text, "Hi there");
        assert_eq!(response.model, "qwen3-1.7b");
        assert_eq!(response.stop_reason.as_deref(), Some("stop"));
        assert_eq!(response.usage.input_tokens, 11);
        assert_eq!(response.usage.output_tokens, 3);
    }

    #[test]
    fn parse_completion_parses_the_tool_call_arguments_string() {
        // content is null and the model asks for a tool; arguments arrive as a
        // JSON-encoded string that must be parsed into a Value.
        let body = r#"{
            "model": "qwen3-1.7b",
            "choices": [
                {"index": 0, "message": {"role": "assistant", "content": null,
                 "reasoning_content": "hmm",
                 "tool_calls": [
                    {"id": "call_9", "type": "function",
                     "function": {"name": "fetch_jd", "arguments": "{\"url\": \"https://x\"}"}}
                 ]},
                 "finish_reason": "tool_calls"}
            ],
            "usage": {"prompt_tokens": 20, "completion_tokens": 8}
        }"#;
        let response = parse_completion(body).unwrap();
        assert_eq!(response.text, "");
        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].id, "call_9");
        assert_eq!(response.tool_calls[0].name, "fetch_jd");
        assert_eq!(response.tool_calls[0].args["url"], "https://x");
        assert_eq!(response.stop_reason.as_deref(), Some("tool_calls"));
    }

    #[test]
    fn parse_completion_rejects_a_malformed_arguments_string() {
        let body = r#"{
            "model": "m",
            "choices": [
                {"index": 0, "message": {"role": "assistant", "content": null,
                 "tool_calls": [
                    {"id": "call_1", "type": "function",
                     "function": {"name": "x", "arguments": "{not json"}}
                 ]}}
            ]
        }"#;
        let err = parse_completion(body).unwrap_err();
        assert!(matches!(err, LlmError::Parse(_)));
    }

    #[test]
    fn parse_api_error_reads_the_object_shape() {
        let body = r#"{"error": {"message": "no such model", "type": "invalid_request_error", "code": "model_not_found"}}"#;
        match parse_api_error(400, body, 100) {
            LlmError::Api {
                status,
                kind,
                message,
            } => {
                assert_eq!(status, 400);
                assert_eq!(kind, "model_not_found");
                assert_eq!(message, "no such model");
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[test]
    fn parse_api_error_reads_the_bare_string_shape_and_raw_fallback() {
        match parse_api_error(400, r#"{"error": "model not loaded"}"#, 100) {
            LlmError::Api { kind, message, .. } => {
                assert_eq!(kind, "unknown");
                assert_eq!(message, "model not loaded");
            }
            other => panic!("expected Api error, got {other:?}"),
        }
        // Anything that isn't the error envelope falls back to the raw body.
        match parse_api_error(502, "<html>bad gateway</html>", 100) {
            LlmError::Api { kind, message, .. } => {
                assert_eq!(kind, "unknown");
                assert_eq!(message, "<html>bad gateway</html>");
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[test]
    fn parse_api_error_translates_a_context_overflow_400() {
        let body = r#"{"error": {"message": "the prompt exceeds n_ctx (4096) and n_keep", "type": "invalid_request_error"}}"#;
        match parse_api_error(400, body, 6200) {
            LlmError::Api { message, .. } => {
                assert!(message.contains("~6200 tokens"));
                assert!(message.contains("larger context"));
                // The server's own words survive as the tail.
                assert!(message.contains("n_ctx"));
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[test]
    fn handle_sse_line_accumulates_deltas_usage_and_emits_done() {
        let mut state = StreamState::default();

        let delta = r#"data: {"choices": [{"index": 0, "delta": {"content": "Hel"}}]}"#;
        assert_eq!(
            handle_sse_line(delta, &mut state).unwrap(),
            Some(StreamEvent::TextDelta("Hel".to_string()))
        );

        let finish = r#"data: {"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}"#;
        assert!(handle_sse_line(finish, &mut state).unwrap().is_none());

        // The final usage chunk has an empty choices array.
        let usage =
            r#"data: {"choices": [], "usage": {"prompt_tokens": 7, "completion_tokens": 4}}"#;
        assert!(handle_sse_line(usage, &mut state).unwrap().is_none());

        let done = handle_sse_line("data: [DONE]", &mut state).unwrap();
        match done {
            Some(StreamEvent::Done { stop_reason, usage }) => {
                assert_eq!(stop_reason.as_deref(), Some("stop"));
                assert_eq!(usage.input_tokens, 7);
                assert_eq!(usage.output_tokens, 4);
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    /// Drive `pump_sse` with fixture byte chunks and collect everything it
    /// forwards, exactly as a caller would read the `TokenStream`. The
    /// estimate is set low so the clamp check stays quiet unless a test
    /// wants it.
    async fn run_pump(chunks: Vec<&'static [u8]>) -> Vec<Result<StreamEvent, LlmError>> {
        run_pump_with_estimate(chunks, 5).await
    }

    async fn run_pump_with_estimate(
        chunks: Vec<&'static [u8]>,
        estimate: u64,
    ) -> Vec<Result<StreamEvent, LlmError>> {
        let (tx, mut rx) = mpsc::channel(32);
        let bytes = futures_util::stream::iter(chunks.into_iter().map(Ok::<_, reqwest::Error>));
        pump_sse(bytes, tx, estimate).await;
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        events
    }

    #[tokio::test]
    async fn a_stream_whose_prompt_came_back_clamped_is_refused() {
        // The server's overflow policy (Truncate Middle) clipped an ~8000
        // token prompt to a 4096 window and streamed a reply anyway; the
        // usage chunk gives it away and the Done becomes a refusal.
        let events = run_pump_with_estimate(
            vec![
                b"data: {\"choices\": [{\"index\": 0, \"delta\": {\"content\": \"Hi\"}, \"finish_reason\": \"stop\"}]}\n",
                b"data: {\"choices\": [], \"usage\": {\"prompt_tokens\": 4096, \"completion_tokens\": 2}}\n",
                b"data: [DONE]\n",
            ],
            8000,
        )
        .await;
        match events.last() {
            Some(Err(LlmError::Unsupported(message))) => {
                assert!(message.contains("~4096"), "got: {message}");
                assert!(message.contains("~8000"), "got: {message}");
            }
            other => panic!("expected an Unsupported refusal, got {other:?}"),
        }
    }

    #[test]
    fn checked_done_passes_a_healthy_usage_through() {
        let usage = TokenUsage {
            input_tokens: 7600,
            output_tokens: 40,
        };
        // 7600 of an estimated 8000: the estimate ran a little high, fine.
        match checked_done(Some("stop".to_string()), usage, 8000) {
            Ok(StreamEvent::Done { stop_reason, .. }) => {
                assert_eq!(stop_reason.as_deref(), Some("stop"));
            }
            other => panic!("expected Done, got {other:?}"),
        }
        // A server that reported no usage at all is not evidence of a clamp.
        let unreported = TokenUsage::default();
        assert!(checked_done(None, unreported, 8000).is_ok());
    }

    #[tokio::test]
    async fn a_stream_that_ends_without_the_done_sentinel_is_an_error() {
        // The connection drops mid-generation: deltas arrived, no
        // finish_reason, no [DONE]. The text so far must not present as a
        // complete reply.
        let events = run_pump(vec![
            b"data: {\"choices\": [{\"index\": 0, \"delta\": {\"content\": \"Hel\"}}]}\n",
            b"data: {\"choices\": [{\"index\": 0, \"delta\": {\"content\": \"lo\"}}]}\n",
        ])
        .await;
        assert_eq!(events.len(), 3);
        assert_eq!(
            events[0].as_ref().unwrap(),
            &StreamEvent::TextDelta("Hel".to_string())
        );
        match &events[2] {
            Err(LlmError::Stream(message)) => {
                assert!(message.contains("before data: [DONE]"), "got: {message}");
            }
            other => panic!("expected a Stream error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_lost_sentinel_after_a_finish_reason_still_yields_done() {
        // Generation provably finished (finish_reason arrived) and only the
        // [DONE] sentinel was lost in transit; the lenient branch emits the
        // Done the server implied.
        let events = run_pump(vec![
            b"data: {\"choices\": [{\"index\": 0, \"delta\": {\"content\": \"Hi\"}, \"finish_reason\": \"stop\"}]}\n",
            b"data: {\"choices\": [], \"usage\": {\"prompt_tokens\": 5, \"completion_tokens\": 1}}\n",
        ])
        .await;
        match events.last() {
            Some(Ok(StreamEvent::Done { stop_reason, usage })) => {
                assert_eq!(stop_reason.as_deref(), Some("stop"));
                assert_eq!(usage.input_tokens, 5);
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_pump_stops_reading_after_the_done_sentinel() {
        // A misbehaving server keeps talking after [DONE]; the pump must have
        // already returned, so exactly one Done and nothing after it.
        let events = run_pump(vec![
            b"data: {\"choices\": [{\"index\": 0, \"delta\": {}, \"finish_reason\": \"stop\"}]}\n",
            b"data: [DONE]\n",
            b"data: [DONE]\ndata: {not even json\n",
        ])
        .await;
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Ok(StreamEvent::Done { .. })));
    }

    #[test]
    fn handle_sse_line_ignores_non_data_lines() {
        let mut state = StreamState::default();
        assert!(handle_sse_line("", &mut state).unwrap().is_none());
        assert!(
            handle_sse_line(": keep-alive", &mut state)
                .unwrap()
                .is_none()
        );
        assert!(
            handle_sse_line("event: message", &mut state)
                .unwrap()
                .is_none()
        );
    }
}
