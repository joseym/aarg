//! A fake `LlmClient` for tests and (later) keyless eval replay.
//!
//! Tests queue up canned response texts, run the code under test, then
//! inspect the requests the code actually sent. No network, no API key,
//! fully deterministic.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;
use futures_util::stream;

use crate::llm::client::LlmClient;
use crate::llm::types::{
    CompletionRequest, CompletionResponse, LlmError, StreamEvent, TokenStream, TokenUsage, ToolCall,
};

/// One scripted reply: plain text, or the model "deciding" to call
/// tools. Tests script the whole tool dance this way.
#[derive(Debug)]
enum MockReply {
    Text(String),
    ToolCalls(Vec<ToolCall>),
}

/// An `LlmClient` that replays queued responses instead of calling a
/// provider, recording every request it receives.
#[derive(Debug, Default)]
pub struct MockLlmClient {
    responses: Mutex<VecDeque<MockReply>>,
    requests: Mutex<Vec<CompletionRequest>>,
}

impl MockLlmClient {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a response text; each request consumes one, in order.
    pub fn enqueue(&self, text: impl Into<String>) {
        lock_ignoring_poison(&self.responses).push_back(MockReply::Text(text.into()));
    }

    /// Queue a tool-calling turn: the "model" asks for these
    /// invocations instead of answering.
    pub fn enqueue_tool_calls(&self, calls: Vec<ToolCall>) {
        lock_ignoring_poison(&self.responses).push_back(MockReply::ToolCalls(calls));
    }

    /// Every request this client has received, oldest first.
    pub fn requests(&self) -> Vec<CompletionRequest> {
        lock_ignoring_poison(&self.requests).clone()
    }

    fn record_and_pop(&self, request: CompletionRequest) -> Result<MockReply, LlmError> {
        lock_ignoring_poison(&self.requests).push(request);
        lock_ignoring_poison(&self.responses)
            .pop_front()
            .ok_or(LlmError::MockExhausted)
    }
}

/// Lock a mutex, recovering the data even if another thread panicked
/// while holding it. In this test-support type, the data can't be left
/// half-modified (every critical section is a single operation), so the
/// poison flag carries no information for us.
fn lock_ignoring_poison<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Token estimate for mock usage reporting: ~4 characters per token,
/// never zero. Deterministic, not accurate — tests only assert presence.
fn estimate_tokens(text: &str) -> u64 {
    (text.len() as u64 / 4).max(1)
}

#[async_trait]
impl LlmClient for MockLlmClient {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let model = request.model.clone();
        let prompt_len: usize = request.messages.iter().map(|m| m.content.len()).sum();
        let (text, tool_calls, stop_reason) = match self.record_and_pop(request)? {
            MockReply::Text(text) => (text, Vec::new(), "end_turn"),
            MockReply::ToolCalls(calls) => (String::new(), calls, "tool_use"),
        };
        Ok(CompletionResponse {
            usage: TokenUsage {
                input_tokens: (prompt_len as u64 / 4).max(1),
                output_tokens: estimate_tokens(&text),
            },
            stop_reason: Some(stop_reason.to_string()),
            model,
            text,
            tool_calls,
        })
    }

    async fn stream(&self, request: CompletionRequest) -> Result<TokenStream, LlmError> {
        // Streaming doesn't carry tool calls in this phase; a scripted
        // tool turn streams as its (empty) text.
        let text = match self.record_and_pop(request)? {
            MockReply::Text(text) => text,
            MockReply::ToolCalls(_) => String::new(),
        };
        let usage = TokenUsage {
            input_tokens: 1,
            output_tokens: estimate_tokens(&text),
        };

        // Re-chunk the canned text on word boundaries (keeping the
        // whitespace) so consumers see a realistic multi-event stream
        // that still concatenates back to the original.
        let mut events: Vec<Result<StreamEvent, LlmError>> = text
            .split_inclusive(' ')
            .map(|piece| Ok(StreamEvent::TextDelta(piece.to_string())))
            .collect();
        events.push(Ok(StreamEvent::Done {
            stop_reason: Some("end_turn".to_string()),
            usage,
        }));

        Ok(Box::pin(stream::iter(events)))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use futures_util::StreamExt;

    use super::*;
    use crate::llm::types::Message;

    fn request(prompt: &str) -> CompletionRequest {
        CompletionRequest {
            model: "mock-model".to_string(),
            max_tokens: 64,
            system: None,
            messages: vec![Message::user(prompt)],
            temperature: None,
            tools: Vec::new(),
        }
    }

    #[tokio::test]
    async fn complete_replays_queued_responses_in_order() {
        let mock = MockLlmClient::new();
        mock.enqueue("first");
        mock.enqueue("second");

        let a = mock.complete(request("one")).await.unwrap();
        let b = mock.complete(request("two")).await.unwrap();
        assert_eq!(a.text, "first");
        assert_eq!(b.text, "second");
        assert_eq!(a.model, "mock-model");
        assert!(a.usage.output_tokens > 0);
    }

    #[tokio::test]
    async fn requests_are_recorded() {
        let mock = MockLlmClient::new();
        mock.enqueue("pong");
        mock.complete(request("ping")).await.unwrap();

        let seen = mock.requests();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].messages[0].content, "ping");
    }

    #[tokio::test]
    async fn exhausted_queue_is_a_typed_error() {
        let mock = MockLlmClient::new();
        let err = mock.complete(request("ping")).await.unwrap_err();
        assert!(matches!(err, LlmError::MockExhausted));
    }

    // EXERCISE(EX-002)
    #[tokio::test]
    #[ignore = "exercise: the mock can only queue successes; add a way to queue errors (e.g. enqueue_error) so tests can script provider failures, then finish this test"]
    async fn ex_002_queued_errors_surface_from_complete() {
        // Once the queue can hold errors: queue an Api error with status
        // 429, call complete, and assert the same error comes back out.
        let error_queueing_implemented = false;
        assert!(error_queueing_implemented);
    }

    #[tokio::test]
    async fn stream_concatenates_back_to_the_queued_text() {
        let mock = MockLlmClient::new();
        mock.enqueue("a few words here");

        let mut stream = mock.stream(request("go")).await.unwrap();
        let mut text = String::new();
        let mut done = None;
        while let Some(event) = stream.next().await {
            match event.unwrap() {
                StreamEvent::TextDelta(piece) => text.push_str(&piece),
                StreamEvent::Done { stop_reason, .. } => done = stop_reason,
            }
        }
        assert_eq!(text, "a few words here");
        assert_eq!(done.as_deref(), Some("end_turn"));
    }
}
