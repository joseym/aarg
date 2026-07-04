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

/// Tokens reserved beyond the estimated prompt and the completion budget when
/// sizing a window, and treated as off-limits to the prompt when checking one.
const HEADROOM: u64 = 512;

/// The context window to request for a prompt of the given estimate: the
/// configured floor, or `estimate + max_tokens + HEADROOM` when that is
/// larger, so the window always has room for the prompt, the completion, and
/// a margin for the estimate running low. Saturating arithmetic keeps a
/// pathological estimate from wrapping; the result is clamped into `u32`, the
/// width the wire field takes.
pub fn effective_num_ctx(floor: u32, estimate: u64, max_tokens: u32) -> u32 {
    let needed = estimate
        .saturating_add(u64::from(max_tokens))
        .saturating_add(HEADROOM)
        .min(u64::from(u32::MAX));
    floor.max(needed as u32)
}

/// The clamp ratio: a reported prompt-token count below this fraction of the
/// estimate means the server processed far less prompt than was sent.
///
/// False-positive analysis for 0.70: the check misfires only when chars/4
/// *overestimates* the true token count by more than 30%, i.e. when text
/// averages over 5.7 characters per token. Everything AARG sends errs the
/// other way: English prose runs ~4 chars/token, dense JSON ~3 (punctuation
/// and short keys are their own tokens), CJK text 1-2, base64 ~2.5-3. All of
/// those make the estimate run *low*, which this check ignores. A real clamp,
/// by contrast, is not subtle: Ollama keeps half the window and LM Studio's
/// truncate-middle keeps a window's worth of an oversized prompt, both far
/// below 70% of an honest estimate.
const CLAMP_RATIO: f64 = 0.70;

/// How far a reported count may sit from the exact half-window clip point and
/// still match the fingerprint. The observed clip is exact (see
/// [`looks_truncated`]); the tolerance absorbs off-by-a-few variation across
/// model templates and versions.
const HALF_CLIP_TOLERANCE: u64 = 8;

/// Whether a server's reported prompt-token count is materially below the
/// estimate for what was sent: the estimate-only truncation signal, for
/// providers whose context window this client cannot size (LM Studio's
/// Truncate Middle and Rolling Window overflow policies return 200 with a
/// silently clipped prompt). A count of zero means the server reported
/// nothing, which is not evidence of truncation.
pub fn looks_clamped(prompt_tokens: u64, estimate: u64) -> bool {
    if prompt_tokens == 0 {
        return false;
    }
    (prompt_tokens as f64) < estimate as f64 * CLAMP_RATIO
}

/// Whether a response from a window this client sized (via
/// [`effective_num_ctx`]) shows the prompt was clipped or outgrew its window.
/// Three signals, any of which fires; a count of zero means the server
/// reported nothing and is not evidence.
///
/// 1. **Headroom consumed** — the count reaches `window - max_tokens -
///    HEADROOM`. By construction that boundary sits at or above the estimate,
///    so an honest prompt that matched its estimate never reaches it;
///    reaching it means the prompt outgrew the estimate into the space
///    reserved for generation, where clipping (by a server that trims to
///    `window - num_predict`) or a mid-generation context shift (which drops
///    prompt tokens to make room) can no longer be ruled out. Fails closed.
///
/// 2. **The half-window clip fingerprint** — Ollama does not trim an
///    oversized prompt to just-fit: it keeps half the window. Probed live
///    (Ollama 0.30.11, llama3.3, a 3218-token prompt): `num_ctx` 512 kept
///    258 tokens, 1024 kept 514, 2048 kept 1026 — `window / 2 + 2` exactly,
///    independent of `num_predict`. A count in that narrow band *and*
///    disagreeing with the estimate by more than 25% either way is a clip;
///    the disagreement condition keeps an honest prompt that happens to be
///    half the window from matching, since chars/4 tracks real counts well
///    inside 25% for prose and errs low (never high) for JSON and CJK.
///
/// 3. **The clamp shape** — the count is materially below the estimate
///    ([`looks_clamped`]), regardless of where the window sits. Catches a
///    server that ignored the requested window and clipped to its own.
pub fn looks_truncated(
    prompt_eval_count: u64,
    effective_num_ctx: u32,
    max_tokens: u32,
    estimate: u64,
) -> bool {
    if prompt_eval_count == 0 {
        return false;
    }
    let window = u64::from(effective_num_ctx);

    let reserve = window
        .saturating_sub(u64::from(max_tokens))
        .saturating_sub(HEADROOM);
    if prompt_eval_count >= reserve {
        return true;
    }

    let clip_point = window / 2 + 2;
    let in_clip_band = prompt_eval_count.abs_diff(clip_point) <= HALF_CLIP_TOLERANCE;
    let ratio = prompt_eval_count as f64 / estimate as f64;
    if in_clip_band && !(0.75..=1.25).contains(&ratio) {
        return true;
    }

    looks_clamped(prompt_eval_count, estimate)
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
    fn looks_truncated_fires_on_the_probed_half_window_clip() {
        // The live-probed scenario: a CJK-heavy prompt whose true size (chars
        // per token near 1) far exceeds its chars/4 estimate of 2881. The
        // window comes from effective_num_ctx, exactly as complete() computes
        // it, and Ollama clips the oversized prompt to window / 2 + 2 (probed:
        // 258 of 512, 514 of 1024, 1026 of 2048).
        let estimate = 2881;
        let max_tokens = 64;
        let window = effective_num_ctx(8192, estimate, max_tokens);
        assert_eq!(window, 8192);
        let clipped_eval = u64::from(window) / 2 + 2; // 4098
        assert!(looks_truncated(clipped_eval, window, max_tokens, estimate));
        // The old guard demanded eval >= 98% of the window; this real clip
        // sits at 50% and could never have been caught.
        assert!((clipped_eval as f64) < f64::from(window) * 0.98);
    }

    #[test]
    fn looks_truncated_fires_when_the_prompt_eats_the_generation_reserve() {
        // The prompt outgrew its estimate into the window space reserved for
        // generation. The window is computed, not assumed: 8192 + 256 + 512.
        let estimate = 8192;
        let max_tokens = 256;
        let window = effective_num_ctx(8192, estimate, max_tokens);
        assert_eq!(u64::from(window), estimate + 256 + 512);
        // A count at the reserve boundary (window - max_tokens - 512, which by
        // construction equals the estimate here) fires.
        let reserve = u64::from(window) - 256 - 512;
        assert!(looks_truncated(reserve, window, max_tokens, estimate));
        // A count safely below it does not.
        assert!(!looks_truncated(
            reserve - 100,
            window,
            max_tokens,
            estimate
        ));
    }

    #[test]
    fn looks_truncated_fires_on_a_clamp_far_below_the_estimate() {
        // A server that ignored the requested window and clipped at its own
        // default: 2050 tokens processed of an estimated 6200, window 8192.
        let estimate = 6200;
        let max_tokens = 64;
        let window = effective_num_ctx(8192, estimate, max_tokens);
        assert!(looks_truncated(2050, window, max_tokens, estimate));
        assert!(looks_clamped(2050, estimate));
    }

    #[test]
    fn looks_truncated_ignores_healthy_counts() {
        // The real numbers from the live probe of an English prompt that fit:
        // estimate 2881, actual 3218 (chars/4 ran ~10% low), window 8192.
        let window = effective_num_ctx(8192, 2881, 64);
        assert!(!looks_truncated(3218, window, 64, 2881));
        // An accurate estimate, count right on it.
        assert!(!looks_truncated(6200, window, 64, 6200));
        // No count reported at all.
        assert!(!looks_truncated(0, window, 64, 10000));
        assert!(!looks_clamped(0, 10000));
    }

    #[test]
    fn an_honest_half_window_prompt_does_not_match_the_clip_fingerprint() {
        // A prompt that genuinely is about half the window, with an estimate
        // that agrees: in the clip band, but the estimate rules out a clip.
        let max_tokens = 64;
        let window = effective_num_ctx(8192, 4000, max_tokens);
        let half = u64::from(window) / 2 + 2; // 4098, within 25% of 4000
        assert!(!looks_truncated(half, window, max_tokens, 4000));
    }
}
