//! A hand-rolled client for the Anthropic Messages API.
//!
//! Written directly against the HTTP API with `reqwest` — no SDK, no
//! framework crate. The wire format (request body shape, response
//! envelope, SSE streaming events) lives entirely in this module;
//! everything outside speaks the provider-agnostic types.

use async_trait::async_trait;
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::llm::client::LlmClient;
use crate::llm::types::{
    CompletionRequest, CompletionResponse, LlmError, StreamEvent, TokenStream, TokenUsage,
};

/// The API version header the Messages API requires on every request.
const ANTHROPIC_VERSION: &str = "2023-06-01";

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// An `LlmClient` backed by the Anthropic Messages API.
pub struct AnthropicClient {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl AnthropicClient {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }

    /// Point the client at a different server (local proxy, test stub).
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    async fn post_messages(
        &self,
        request: &CompletionRequest,
        stream: bool,
    ) -> Result<reqwest::Response, LlmError> {
        let response = self
            .http
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&request_body(request, stream))
            .send()
            .await?;
        Ok(response)
    }
}

/// Build the Messages API request body from our provider-agnostic
/// request. `system` and `temperature` are added only when set: the API
/// rejects explicit `null`s, and newer models reject `temperature`
/// entirely, so absence is the only safe default.
fn request_body(request: &CompletionRequest, stream: bool) -> serde_json::Value {
    let messages: Vec<serde_json::Value> = request.messages.iter().map(wire_message).collect();
    let mut body = json!({
        "model": request.model,
        "max_tokens": request.max_tokens,
        "messages": messages,
        "stream": stream,
    });
    if let Some(system) = &request.system {
        body["system"] = json!(system);
    }
    if let Some(temperature) = request.temperature {
        body["temperature"] = json!(temperature);
    }
    if !request.tools.is_empty() {
        body["tools"] = json!(request.tools);
    }
    body
}

/// One message as the wire wants it. Plain text stays a bare string —
/// the compact form the API has always taken; turns carrying tool
/// traffic become content-block arrays.
fn wire_message(message: &crate::llm::Message) -> serde_json::Value {
    let role = json!(message.role);
    if message.tool_calls.is_empty() && message.tool_results.is_empty() {
        return json!({"role": role, "content": message.content});
    }
    let mut blocks = Vec::new();
    for result in &message.tool_results {
        let mut block = json!({
            "type": "tool_result",
            "tool_use_id": result.call_id,
            "content": result.content,
        });
        if result.is_error {
            block["is_error"] = json!(true);
        }
        blocks.push(block);
    }
    if !message.content.is_empty() {
        blocks.push(json!({"type": "text", "text": message.content}));
    }
    for call in &message.tool_calls {
        blocks.push(json!({
            "type": "tool_use",
            "id": call.id,
            "name": call.name,
            "input": call.args,
        }));
    }
    json!({"role": role, "content": blocks})
}

// ---- non-streaming response parsing ------------------------------------

#[derive(Deserialize)]
struct WireCompletion {
    model: String,
    content: Vec<WireContentBlock>,
    stop_reason: Option<String>,
    usage: WireUsage,
}

#[derive(Deserialize)]
struct WireContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
    // tool_use block fields; absent on text blocks.
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    input: serde_json::Value,
}

#[derive(Deserialize, Default, Clone, Copy)]
struct WireUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

#[derive(Deserialize)]
struct WireError {
    #[serde(rename = "type")]
    kind: String,
    message: String,
}

fn parse_completion(body: &str) -> Result<CompletionResponse, LlmError> {
    let wire: WireCompletion = serde_json::from_str(body).map_err(LlmError::Parse)?;
    let text: String = wire
        .content
        .iter()
        .filter(|block| block.kind == "text")
        .map(|block| block.text.as_str())
        .collect();
    let tool_calls = wire
        .content
        .into_iter()
        .filter(|block| block.kind == "tool_use")
        .map(|block| crate::llm::ToolCall {
            id: block.id,
            name: block.name,
            args: block.input,
        })
        .collect();
    Ok(CompletionResponse {
        text,
        tool_calls,
        model: wire.model,
        stop_reason: wire.stop_reason,
        usage: TokenUsage {
            input_tokens: wire.usage.input_tokens,
            output_tokens: wire.usage.output_tokens,
        },
    })
}

/// Turn a non-2xx response body into a typed error. The API wraps errors
/// in `{"type": "error", "error": {"type": ..., "message": ...}}`; if the
/// body is anything else (a proxy's HTML page, an empty string), fall
/// back to carrying it raw so the user still sees what came back.
fn parse_api_error(status: u16, body: &str) -> LlmError {
    #[derive(Deserialize)]
    struct WireErrorEnvelope {
        error: WireError,
    }

    match serde_json::from_str::<WireErrorEnvelope>(body) {
        Ok(envelope) => LlmError::Api {
            status,
            kind: envelope.error.kind,
            message: envelope.error.message,
        },
        Err(_) => LlmError::Api {
            status,
            kind: "unknown".to_string(),
            message: body.trim().to_string(),
        },
    }
}

// ---- streaming (server-sent events) -------------------------------------

/// The SSE event payloads the Messages API sends, in wire shape, tagged
/// by their `type` field. Event types this client doesn't act on (pings,
/// content block boundaries) fall into `Unknown` and are skipped, so new
/// server-side event types can't break the stream.
#[derive(Deserialize)]
#[serde(tag = "type")]
enum WireEvent {
    #[serde(rename = "message_start")]
    MessageStart { message: WireMessageStart },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta { delta: WireDelta },
    #[serde(rename = "message_delta")]
    MessageDelta {
        delta: WireMessageDelta,
        usage: WireUsage,
    },
    #[serde(rename = "message_stop")]
    MessageStop,
    #[serde(rename = "error")]
    Error { error: WireError },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
struct WireMessageStart {
    usage: WireUsage,
}

#[derive(Deserialize)]
struct WireDelta {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

#[derive(Deserialize)]
struct WireMessageDelta {
    stop_reason: Option<String>,
}

/// Metadata that arrives in pieces across the stream — input tokens at
/// `message_start`, output tokens and stop reason at `message_delta` —
/// accumulated here and emitted once, with `Done`, at `message_stop`.
#[derive(Default)]
struct StreamState {
    usage: TokenUsage,
    stop_reason: Option<String>,
}

/// Pull every complete SSE frame off the front of `buffer` and return
/// their `data:` payloads. Frames are separated by a blank line; a
/// trailing partial frame stays in the buffer until more bytes arrive.
// EXERCISE(EX-003)
fn drain_sse_data(buffer: &mut Vec<u8>) -> Vec<String> {
    let mut payloads = Vec::new();
    while let Some(end) = buffer.windows(2).position(|pair| pair == b"\n\n") {
        let frame: Vec<u8> = buffer.drain(..end + 2).collect();
        let frame = String::from_utf8_lossy(&frame);
        for line in frame.lines() {
            if let Some(data) = line.strip_prefix("data:") {
                payloads.push(data.trim_start().to_string());
            }
        }
    }
    payloads
}

/// Interpret one SSE data payload: update the accumulated state, and
/// return the event to surface to the consumer, if any.
fn handle_payload(payload: &str, state: &mut StreamState) -> Result<Option<StreamEvent>, LlmError> {
    let event: WireEvent = serde_json::from_str(payload).map_err(LlmError::Parse)?;
    Ok(match event {
        WireEvent::MessageStart { message } => {
            state.usage.input_tokens = message.usage.input_tokens;
            None
        }
        WireEvent::ContentBlockDelta { delta } if delta.kind == "text_delta" => {
            Some(StreamEvent::TextDelta(delta.text))
        }
        WireEvent::ContentBlockDelta { .. } => None,
        WireEvent::MessageDelta { delta, usage } => {
            state.stop_reason = delta.stop_reason;
            state.usage.output_tokens = usage.output_tokens;
            None
        }
        WireEvent::MessageStop => Some(StreamEvent::Done {
            stop_reason: state.stop_reason.take(),
            usage: state.usage,
        }),
        WireEvent::Error { error } => {
            return Err(LlmError::Stream(format!(
                "{}: {}",
                error.kind, error.message
            )));
        }
        WireEvent::Unknown => None,
    })
}

#[async_trait]
impl LlmClient for AnthropicClient {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let response = self.post_messages(&request, false).await?;
        let status = response.status().as_u16();
        let body = response.text().await?;
        if !(200..300).contains(&status) {
            return Err(parse_api_error(status, &body));
        }
        parse_completion(&body)
    }

    async fn stream(&self, request: CompletionRequest) -> Result<TokenStream, LlmError> {
        let response = self.post_messages(&request, true).await?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let body = response.text().await?;
            return Err(parse_api_error(status, &body));
        }

        // Read the response body on a background task and forward parsed
        // events through a channel; the receiving half is the TokenStream
        // handed to the caller. If the caller drops the stream early, the
        // send fails and the task stops reading.
        let (tx, rx) = mpsc::channel(32);
        let mut bytes = response.bytes_stream();
        tokio::spawn(async move {
            let mut buffer: Vec<u8> = Vec::new();
            let mut state = StreamState::default();
            while let Some(chunk) = bytes.next().await {
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        let _ = tx.send(Err(LlmError::Http(error))).await;
                        return;
                    }
                };
                buffer.extend_from_slice(&chunk);
                for payload in drain_sse_data(&mut buffer) {
                    match handle_payload(&payload, &mut state) {
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
        });

        Ok(Box::pin(ReceiverStream::new(rx)))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::llm::types::Message;

    fn request() -> CompletionRequest {
        CompletionRequest {
            model: "claude-opus-4-8".to_string(),
            max_tokens: 64,
            system: None,
            messages: vec![Message::user("hello")],
            temperature: None,
            tools: Vec::new(),
        }
    }

    #[test]
    fn request_body_omits_unset_optional_fields() {
        let body = request_body(&request(), false);
        assert_eq!(body["model"], "claude-opus-4-8");
        assert_eq!(body["stream"], false);
        assert!(body.get("system").is_none());
        assert!(body.get("temperature").is_none());
    }

    #[test]
    fn request_body_includes_optional_fields_when_set() {
        let mut req = request();
        req.system = Some("be brief".to_string());
        req.temperature = Some(0.5);
        let body = request_body(&req, true);
        assert_eq!(body["system"], "be brief");
        assert_eq!(body["stream"], true);
        assert!((body["temperature"].as_f64().unwrap() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn request_body_writes_tool_specs_and_tool_turns_as_blocks() {
        use crate::llm::{ToolCall, ToolResult, ToolSpec};
        let mut request = request();
        request.tools = vec![ToolSpec {
            name: "fetch_jd".into(),
            description: "Fetch a posting".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }];
        request.messages = vec![
            Message::user("get it"),
            Message {
                role: crate::llm::Role::Assistant,
                content: String::new(),
                tool_calls: vec![ToolCall {
                    id: "tu_1".into(),
                    name: "fetch_jd".into(),
                    args: serde_json::json!({"url": "https://x"}),
                }],
                tool_results: Vec::new(),
            },
            Message::tool_results(vec![ToolResult {
                call_id: "tu_1".into(),
                content: "the posting".into(),
                is_error: false,
            }]),
        ];

        let body = request_body(&request, false);

        assert_eq!(body["tools"][0]["name"], "fetch_jd");
        // Plain text stays a bare string...
        assert_eq!(body["messages"][0]["content"], "get it");
        // ...tool turns become content-block arrays.
        let call = &body["messages"][1]["content"][0];
        assert_eq!(call["type"], "tool_use");
        assert_eq!(call["id"], "tu_1");
        assert_eq!(call["input"]["url"], "https://x");
        let result = &body["messages"][2]["content"][0];
        assert_eq!(result["type"], "tool_result");
        assert_eq!(result["tool_use_id"], "tu_1");
        assert_eq!(result["content"], "the posting");
        assert!(result.get("is_error").is_none());
    }

    #[test]
    fn parse_completion_collects_tool_use_blocks() {
        let body = r#"{
            "model": "claude-haiku-4-5-20251001",
            "content": [
                {"type": "text", "text": "Let me fetch that."},
                {"type": "tool_use", "id": "tu_9", "name": "fetch_jd",
                 "input": {"url": "https://jobs.lever.co/acme/x"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 10, "output_tokens": 20}
        }"#;
        let response = parse_completion(body).unwrap();
        assert_eq!(response.text, "Let me fetch that.");
        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].id, "tu_9");
        assert_eq!(response.tool_calls[0].name, "fetch_jd");
        assert_eq!(
            response.tool_calls[0].args["url"],
            "https://jobs.lever.co/acme/x"
        );
        assert_eq!(response.stop_reason.as_deref(), Some("tool_use"));
    }

    #[test]
    fn parse_completion_concatenates_text_blocks_only() {
        let body = r#"{
            "id": "msg_01",
            "type": "message",
            "model": "claude-opus-4-8",
            "content": [
                {"type": "text", "text": "Hello"},
                {"type": "tool_use", "id": "tu_01", "name": "noop", "input": {}},
                {"type": "text", "text": ", world"}
            ],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 12, "output_tokens": 5}
        }"#;
        let response = parse_completion(body).unwrap();
        assert_eq!(response.text, "Hello, world");
        assert_eq!(response.model, "claude-opus-4-8");
        assert_eq!(response.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(response.usage.input_tokens, 12);
        assert_eq!(response.usage.output_tokens, 5);
    }

    #[test]
    fn parse_api_error_reads_the_error_envelope() {
        let body = r#"{
            "type": "error",
            "error": {"type": "rate_limit_error", "message": "slow down"}
        }"#;
        let err = parse_api_error(429, body);
        match err {
            LlmError::Api {
                status,
                kind,
                message,
            } => {
                assert_eq!(status, 429);
                assert_eq!(kind, "rate_limit_error");
                assert_eq!(message, "slow down");
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[test]
    fn parse_api_error_falls_back_to_the_raw_body() {
        let err = parse_api_error(502, "<html>bad gateway</html>");
        match err {
            LlmError::Api { kind, message, .. } => {
                assert_eq!(kind, "unknown");
                assert_eq!(message, "<html>bad gateway</html>");
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[test]
    fn drain_sse_data_handles_frames_split_across_chunks() {
        let mut buffer = Vec::new();

        buffer.extend_from_slice(b"event: content_block_delta\ndata: {\"a\"");
        assert!(drain_sse_data(&mut buffer).is_empty());

        buffer.extend_from_slice(b": 1}\n\ndata: {\"b\": 2}\n\ndata: par");
        let payloads = drain_sse_data(&mut buffer);
        assert_eq!(payloads, vec![r#"{"a": 1}"#, r#"{"b": 2}"#]);

        buffer.extend_from_slice(b"tial}\n\n");
        assert_eq!(drain_sse_data(&mut buffer), vec!["partial}"]);
        assert!(buffer.is_empty());
    }

    #[tokio::test]
    #[ignore = "exercise: drain_sse_data assumes frames end in \\n\\n, but the SSE spec also allows \\r\\n\\r\\n; make the parser accept both, then finish this test"]
    async fn ex_003_crlf_separated_frames_are_parsed() {
        let mut buffer = Vec::new();
        buffer.extend_from_slice(b"data: {\"a\": 1}\r\n\r\ndata: {\"b\": 2}\r\n\r\n");
        let payloads = drain_sse_data(&mut buffer);
        assert_eq!(payloads, vec![r#"{"a": 1}"#, r#"{"b": 2}"#]);
        assert!(buffer.is_empty());
    }

    #[test]
    fn handle_payload_accumulates_state_and_emits_done() {
        let mut state = StreamState::default();

        let start = r#"{"type": "message_start", "message": {"id": "msg_01", "usage": {"input_tokens": 9, "output_tokens": 1}}}"#;
        assert!(handle_payload(start, &mut state).unwrap().is_none());

        let delta = r#"{"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hi"}}"#;
        let event = handle_payload(delta, &mut state).unwrap();
        assert_eq!(event, Some(StreamEvent::TextDelta("Hi".to_string())));

        let meta = r#"{"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 7}}"#;
        assert!(handle_payload(meta, &mut state).unwrap().is_none());

        let stop = r#"{"type": "message_stop"}"#;
        let done = handle_payload(stop, &mut state).unwrap();
        match done {
            Some(StreamEvent::Done { stop_reason, usage }) => {
                assert_eq!(stop_reason.as_deref(), Some("end_turn"));
                assert_eq!(usage.input_tokens, 9);
                assert_eq!(usage.output_tokens, 7);
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[test]
    fn handle_payload_skips_unknown_event_types() {
        let mut state = StreamState::default();
        let ping = r#"{"type": "ping"}"#;
        assert!(handle_payload(ping, &mut state).unwrap().is_none());
    }

    #[test]
    fn handle_payload_surfaces_mid_stream_errors() {
        let mut state = StreamState::default();
        let error =
            r#"{"type": "error", "error": {"type": "overloaded_error", "message": "busy"}}"#;
        let err = handle_payload(error, &mut state).unwrap_err();
        assert!(matches!(err, LlmError::Stream(_)));
    }
}
