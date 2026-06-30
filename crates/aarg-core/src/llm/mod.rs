//! The LLM provider layer: one small trait, shared types, and the client
//! implementations behind it.

// The reqwest-backed Anthropic client is native-only; a wasm build supplies a
// host-provided `LlmClient` instead and never compiles this module.
#[cfg(feature = "native")]
pub mod anthropic;
pub mod client;
pub mod mock;
pub mod types;

#[cfg(feature = "native")]
pub use anthropic::{AnthropicClient, Auth};
pub use client::LlmClient;
pub use mock::MockLlmClient;
pub use types::{
    Attachment, CompletionRequest, CompletionResponse, LlmError, Message, Role, StreamEvent,
    TokenStream, TokenUsage, ToolCall, ToolResult, ToolSpec,
};
