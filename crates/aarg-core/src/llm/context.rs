//! Prompt-size estimation and the silent-truncation guard the two local
//! providers share.
//!
//! AARG's prompts run roughly 4k-8k tokens (the dataset, the job posting, the
//! never-fabricate instructions), and a truncated prompt is silent evidence
//! loss: the model answers from a partial dataset without saying so, which for
//! a tool whose whole point is not to fabricate is a correctness bug, not a
//! performance one. Ollama in particular clips a prompt that overflows its
//! `num_ctx` window with no error, so a client that only *sets* the window
//! isn't enough — it has to size the window from an estimate of the prompt and
//! then check the server's own token count to confirm nothing was clipped.
//! Both jobs need a rough token estimate, so it lives here, once.

use crate::llm::types::CompletionRequest;

/// A deliberately rough token estimate for a whole request: characters across
/// the system prompt, every message's text and tool traffic, and the
/// serialized tool specs, divided by four.
///
/// The four-characters-per-token ratio is the usual English-text rule of
/// thumb; it counts UTF-8 *bytes* (`str::len`), not grapheme clusters, and
/// ignores the JSON structural overhead the provider adds around the content,
/// so it is an approximation, not a tokenizer. That is fine for its two uses:
/// picking a context window big enough to hold the prompt with margin, and
/// deciding whether the server's real token count came back suspiciously
/// small. Both want an order-of-magnitude number, not an exact one. Never
/// returns zero, so callers can divide by it without guarding.
pub fn estimate_prompt_tokens(request: &CompletionRequest) -> u64 {
    let mut chars = request.system.as_ref().map_or(0, String::len);
    for message in &request.messages {
        chars += message.content.len();
        for call in &message.tool_calls {
            chars += call.name.len() + call.args.to_string().len();
        }
        for result in &message.tool_results {
            chars += result.content.len();
        }
    }
    if let Ok(tools) = serde_json::to_string(&request.tools) {
        chars += tools.len();
    }
    ((chars as u64) / 4).max(1)
}

/// The context window to request for a prompt of the given estimate: the
/// configured floor, or `estimate + max_tokens + 512` when that is larger, so
/// the window always has room for the prompt, the completion, and a small
/// margin. Saturating arithmetic keeps a pathological estimate from wrapping;
/// the result is clamped into `u32`, the width the wire field takes.
pub fn effective_num_ctx(floor: u32, estimate: u64, max_tokens: u32) -> u32 {
    let needed = estimate
        .saturating_add(u64::from(max_tokens))
        .saturating_add(512)
        .min(u64::from(u32::MAX));
    floor.max(needed as u32)
}

/// Whether a server's reported prompt-token count looks like a silent
/// truncation. Two things have to hold at once: the count sits at or within
/// ~2% of the context window (so the model filled the window to its brim), and
/// it is materially below what the prompt was estimated to need (so real
/// content went missing rather than the estimate simply running high). A count
/// of zero means the server didn't report one, which is not evidence of
/// truncation.
pub fn looks_truncated(prompt_eval_count: u64, effective_num_ctx: u32, estimate: u64) -> bool {
    if prompt_eval_count == 0 {
        return false;
    }
    let window = f64::from(effective_num_ctx);
    let at_the_brim = prompt_eval_count as f64 >= window * 0.98;
    let below_estimate = (prompt_eval_count as f64) < estimate as f64 * 0.90;
    at_the_brim && below_estimate
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::llm::types::{Message, ToolSpec};

    fn request() -> CompletionRequest {
        CompletionRequest {
            model: "local".to_string(),
            max_tokens: 256,
            system: None,
            messages: vec![Message::user("hello")],
            temperature: None,
            tools: Vec::new(),
        }
    }

    #[test]
    fn estimate_counts_system_messages_and_tools() {
        let mut req = request();
        req.system = Some("a".repeat(40)); // 40 chars
        req.messages = vec![Message::user("b".repeat(40))]; // 40 chars
        req.tools = vec![ToolSpec {
            name: "fetch".into(),
            description: "d".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }];
        let estimate = estimate_prompt_tokens(&req);
        // 80 chars of prose alone is 20 tokens; the serialized tool spec adds
        // more, so the estimate clears the prose-only floor.
        assert!(
            estimate > 20,
            "estimate {estimate} should include the tool spec"
        );
    }

    #[test]
    fn estimate_is_never_zero() {
        let mut req = request();
        req.messages = vec![Message::user("")];
        assert_eq!(estimate_prompt_tokens(&req), 1);
    }

    #[test]
    fn effective_num_ctx_holds_the_floor_for_a_small_prompt() {
        // A short prompt stays at the configured floor.
        assert_eq!(effective_num_ctx(8192, 100, 256), 8192);
    }

    #[test]
    fn effective_num_ctx_grows_above_the_floor_for_a_large_prompt() {
        // estimate 9000 + max_tokens 512 + 512 margin = 10024, over the floor.
        assert_eq!(effective_num_ctx(8192, 9000, 512), 10024);
    }

    #[test]
    fn looks_truncated_flags_a_count_pinned_at_the_window_and_under_the_estimate() {
        // The model processed exactly a full window of tokens, far short of the
        // ~10000 the prompt was estimated to need: the prompt was clipped.
        assert!(looks_truncated(8192, 8192, 10000));
    }

    #[test]
    fn looks_truncated_ignores_a_healthy_count() {
        // Count near the estimate, nowhere near the window: no truncation.
        assert!(!looks_truncated(4000, 8192, 4100));
        // No count reported at all.
        assert!(!looks_truncated(0, 8192, 10000));
    }
}
