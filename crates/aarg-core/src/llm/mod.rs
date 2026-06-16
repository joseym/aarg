//! The LLM provider layer: one small trait, shared types, and the client
//! implementations behind it.

pub mod anthropic;
pub mod client;
pub mod mock;
pub mod types;

pub use anthropic::{AnthropicClient, Auth};
pub use client::LlmClient;
pub use mock::MockLlmClient;
pub use types::{
    CompletionRequest, CompletionResponse, LlmError, Message, Role, StreamEvent, TokenStream,
    TokenUsage, ToolCall, ToolResult, ToolSpec,
};
