//! The shared vocabulary for talking to LLM providers: what a request
//! looks like, what comes back, and what can go wrong. Every client
//! implementation speaks in these types; provider-specific wire formats
//! stay inside the individual client modules.

use std::pin::Pin;

use futures_util::Stream;
use serde::{Deserialize, Serialize};

/// Who said a message in the conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// A non-text part of a user turn, sent inline as base64 so the model can
/// read a document the deterministic text path can't (a photo, a scanned
/// PDF). Each client decides the provider-specific source block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Attachment {
    /// A raster image. `media_type` is a provider-accepted image type, e.g.
    /// `image/png`, `image/jpeg`, `image/webp`, `image/gif`. `data` is the
    /// base64-encoded bytes.
    Image { media_type: String, data: String },
    /// A PDF document. `data` is the base64-encoded bytes.
    Pdf { data: String },
}

/// One turn of conversation sent to the model. Plain text turns leave
/// the tool vectors empty; tool-use turns (the model calling out, the
/// follow-up carrying results) fill them. Each client decides how the
/// fields become provider-specific wire blocks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// Tool invocations an assistant turn makes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Results a user turn carries back for earlier tool calls.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_results: Vec<ToolResult>,
    /// Images or PDFs this user turn carries for the model to read. Empty
    /// for ordinary text turns. `#[serde(default)]` keeps older serialized
    /// messages (traces) deserializing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
            attachments: Vec::new(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
            attachments: Vec::new(),
        }
    }

    /// The user turn that answers an assistant's tool calls.
    pub fn tool_results(results: Vec<ToolResult>) -> Self {
        Self {
            role: Role::User,
            content: String::new(),
            tool_calls: Vec::new(),
            tool_results: results,
            attachments: Vec::new(),
        }
    }

    /// A user turn carrying a document (image or PDF) for the model to read,
    /// alongside the instruction text. The one way `attachments` is set.
    pub fn user_with_attachment(content: impl Into<String>, attachment: Attachment) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
            attachments: vec![attachment],
        }
    }
}

/// A tool offered to the model: name, what it does, and the JSON
/// schema of its arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// The model asking for one tool invocation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Provider-assigned ID the result must echo back.
    pub id: String,
    pub name: String,
    pub args: serde_json::Value,
}

/// What one tool invocation produced, going back to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    /// The `ToolCall::id` this answers.
    pub call_id: String,
    pub content: String,
    /// Errors go back to the model too — it can adapt (retry with
    /// different arguments, or answer without the tool).
    pub is_error: bool,
}

/// A provider-agnostic completion request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub model: String,
    pub max_tokens: u32,
    /// Instructions that frame the whole conversation, if any.
    pub system: Option<String>,
    pub messages: Vec<Message>,
    /// Sampling temperature. Leave `None` for the provider default; newer
    /// Anthropic models reject the parameter outright.
    pub temperature: Option<f32>,
    /// Tools the model may call this turn. Empty for plain completions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolSpec>,
}

/// Token counts reported by the provider, used later for cost accounting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// A complete (non-streaming) model response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompletionResponse {
    /// The model's text output, with any non-text blocks filtered out.
    pub text: String,
    /// Tool invocations the model is asking for. Non-empty means the
    /// conversation isn't finished — the caller owes results.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// The model that actually served the request.
    pub model: String,
    /// Why generation stopped (e.g. "end_turn", "tool_use"), if reported.
    pub stop_reason: Option<String>,
    pub usage: TokenUsage,
}

/// One increment of a streaming response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    /// The next chunk of generated text.
    TextDelta(String),
    /// The stream finished; final metadata.
    Done {
        stop_reason: Option<String>,
        usage: TokenUsage,
    },
}

/// A live stream of events from the model, consumed with `.next().await`.
pub type TokenStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, LlmError>> + Send>>;

/// Everything that can go wrong while talking to an LLM provider.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("no API key stored for {provider}; run `aarg init` to set one")]
    MissingApiKey { provider: String },

    /// The native reqwest client failed to reach the API. Native-only: the
    /// `#[from] reqwest::Error` conversion exists only when the reqwest client
    /// is compiled in.
    #[cfg(feature = "native")]
    #[error("could not reach the LLM API")]
    Http(#[from] reqwest::Error),

    /// A non-reqwest transport failed — e.g. a host-provided client in a wasm
    /// build. Keeps the error type expressive when the native client is
    /// compiled out.
    #[error("the LLM transport failed: {0}")]
    Transport(String),

    #[error("the API rejected the request (HTTP {status}, {kind}): {message}")]
    Api {
        status: u16,
        kind: String,
        message: String,
    },

    #[error("could not parse the API response")]
    Parse(#[source] serde_json::Error),

    #[error("the response stream was malformed: {0}")]
    Stream(String),

    #[error("the model kept calling tools after {rounds} rounds without answering")]
    ToolLoop { rounds: u32 },

    #[error("the mock client has no queued response left")]
    MockExhausted,
}

impl LlmError {
    /// Whether this is an HTTP 429 rate-limit response. The remedy (wait, or
    /// switch credentials) differs from a key or model problem, so the CLI
    /// boundary gives a rate limit its own diagnostic rather than the generic
    /// "check your key" help.
    pub fn is_rate_limited(&self) -> bool {
        matches!(self, LlmError::Api { status: 429, .. })
    }
}
