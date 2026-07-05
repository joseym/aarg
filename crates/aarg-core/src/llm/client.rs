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

    /// How many tokens the most recent `complete` call spent on hidden
    /// reasoning, when the provider reports a count. `None` when it reports
    /// none or the client doesn't track it (the default). LM Studio's
    /// OpenAI-compatible server reports one, and `aarg llm ping` uses it to
    /// warn that a reasoning model will make slow builds and empty replies
    /// likely; hosted providers bill visible and hidden tokens the same way,
    /// so nothing else needs it.
    fn hidden_reasoning_tokens(&self) -> Option<u64> {
        None
    }
}
