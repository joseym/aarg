//! The LLM provider layer: one small trait, shared types, and the client
//! implementations behind it.

// The reqwest-backed Anthropic client is native-only; a wasm build supplies a
// host-provided `LlmClient` instead and never compiles this module.
#[cfg(feature = "native")]
pub mod anthropic;
pub mod client;
// Prompt-size estimation and the silent-truncation guard the local providers
// share. Native-only: only the reqwest-backed local clients use it.
#[cfg(feature = "native")]
pub mod context;
pub mod mock;
// The OpenAI-compatible chat client (LM Studio and other local servers),
// native-only for the same reason as `anthropic`.
#[cfg(feature = "native")]
pub mod openai_compat;
pub mod types;

#[cfg(feature = "native")]
pub use anthropic::{AnthropicClient, Auth};
pub use client::LlmClient;
pub use mock::MockLlmClient;
#[cfg(feature = "native")]
pub use openai_compat::OpenAiCompatClient;
pub use types::{
    Attachment, CompletionRequest, CompletionResponse, LlmError, Message, Role, StreamEvent,
    TokenStream, TokenUsage, ToolCall, ToolResult, ToolSpec,
};
