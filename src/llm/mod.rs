//! The LLM provider layer: one small trait, shared types, and the client
//! implementations behind it.

pub mod client;
pub mod types;

pub use client::LlmClient;
pub use types::{
    CompletionRequest, CompletionResponse, LlmError, Message, Role, StreamEvent, TokenStream,
    TokenUsage,
};
