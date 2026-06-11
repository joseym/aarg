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

/// One turn of conversation sent to the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
        }
    }
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
}

/// Token counts reported by the provider, used later for cost accounting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// A complete (non-streaming) model response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionResponse {
    /// The model's text output, with any non-text blocks filtered out.
    pub text: String,
    /// The model that actually served the request.
    pub model: String,
    /// Why generation stopped (e.g. "end_turn", "max_tokens"), if reported.
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

    #[error("could not reach the LLM API")]
    Http(#[from] reqwest::Error),

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

    #[error("the mock client has no queued response left")]
    MockExhausted,
}
