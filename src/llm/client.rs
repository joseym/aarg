//! The `LlmClient` trait: the one seam between everything that wants a
//! model response and the providers that produce one.
//!
//! This is the single abstraction this codebase permits itself before the
//! agent runtime is extracted from working code: tests need a mock from
//! day one, and a two-method trait is the smallest surface that allows it.

use async_trait::async_trait;

use crate::llm::types::{CompletionRequest, CompletionResponse, LlmError, TokenStream};

/// A connection to an LLM provider. Implemented by `AnthropicClient`
/// (real HTTP), `MockLlmClient` (tests and keyless replay), and later an
/// Ollama client for fully local operation.
#[async_trait]
pub trait LlmClient: Send + Sync {
    /// Send a request and wait for the complete response.
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, LlmError>;

    /// Send a request and receive the response incrementally.
    async fn stream(&self, request: CompletionRequest) -> Result<TokenStream, LlmError>;
}
