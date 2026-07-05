//! `aarg llm ping`: send the smallest possible request to the configured
//! provider and report what came back. Verifies the credential (or, for a local
//! provider, connectivity), the model name, and latency in one shot. For a
//! local provider it also checks the loaded context window, since a model
//! loaded with too small a window silently clips aarg's prompts, and warns
//! when the reply spent tokens on hidden reasoning, since a reasoning model
//! makes slow builds and empty replies likely.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::commands::{CliError, configured_client};
use crate::config::{Config, Provider};
use crate::llm::{CompletionRequest, Message};
use crate::style;

/// The context window aarg wants headroom against: its prompts (the dataset,
/// the posting, the never-fabricate instructions) run roughly 4k-8k tokens, so
/// a window under this clips them.
const MIN_CONTEXT_TOKENS: u64 = 8192;

/// The reply budget for the ping. Sixteen tokens would cover "pong", but a
/// reasoning model spends hundreds on hidden reasoning before the visible
/// word (probed live: qwen3-1.7b on LM Studio used 210 reasoning tokens and
/// still ran over a 256 budget on one try), and a ping that dies with an
/// empty reply can't report the diagnosis below. The budget only bounds the
/// reply; a non-reasoning model still answers in a few tokens.
const PING_MAX_TOKENS: u32 = 1024;

/// How long to wait on a local server's metadata endpoint before giving up. The
/// check is a courtesy warning, so it must never hang the command.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

pub async fn run() -> Result<(), CliError> {
    let (client, config) = configured_client().await?;
    let request = CompletionRequest {
        model: config.active_model().to_string(),
        max_tokens: PING_MAX_TOKENS,
        system: None,
        messages: vec![Message::user("Reply with the single word: pong")],
        temperature: None,
        tools: Vec::new(),
    };

    let started = Instant::now();
    let response = client.complete(request).await?;
    let elapsed = started.elapsed();

    // Human status block on stderr (the stream the color helpers detect on);
    // ping has no machine mode, so nothing goes to stdout.
    eprintln!("{}", style::success("pong"));
    eprintln!("{}", style::kv("model", response.model, 8));
    eprintln!("{}", style::kv("reply", response.text.trim(), 8));
    eprintln!(
        "{}",
        style::kv(
            "latency",
            style::dim(format!("{} ms", elapsed.as_millis())),
            8
        )
    );
    eprintln!(
        "{}",
        style::kv(
            "tokens",
            style::dim(format!(
                "{} in, {} out",
                response.usage.input_tokens, response.usage.output_tokens
            )),
            8
        )
    );

    // When the reply spent tokens on hidden reasoning (LM Studio reports the
    // count), say so: the same model on a build-sized prompt will be slow and
    // can burn its whole budget before any visible text.
    if let Some(count) = client.hidden_reasoning_tokens() {
        eprintln!("{}", style::warn(reasoning_note(count)));
    }

    // For a local provider, warn when the loaded/max context can't hold a
    // typical aarg prompt. Best-effort: a server that doesn't report it stays
    // quiet rather than nagging.
    report_local_context(&config).await;
    Ok(())
}

/// The one-line warning for a model that reasons. Factored out so the wording
/// is testable without a live server.
fn reasoning_note(count: u64) -> String {
    format!(
        "this model spent {count} tokens on hidden reasoning before answering · \
         reasoning models make builds slow and empty-reply failures likely; \
         prefer an instruct model"
    )
}

/// Probe a local server for its context window and warn when it's under what
/// aarg needs. A no-op for Anthropic (hosted, no small-window failure mode) and
/// whenever the server doesn't surface the number.
async fn report_local_context(config: &Config) {
    let Some(base_url) = config.active_base_url() else {
        return; // Anthropic; nothing to probe.
    };
    let model = config.active_model();
    let http = match reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() {
        Ok(http) => http,
        Err(_) => return,
    };
    let context = match config.provider {
        Provider::LmStudio => lmstudio_context(&http, base_url, model).await,
        Provider::Ollama => ollama_context(&http, base_url, model).await,
        Provider::Anthropic => None,
    };
    let Some(context) = context else {
        return; // Couldn't determine it; say nothing rather than guess.
    };
    if context < MIN_CONTEXT_TOKENS {
        eprintln!(
            "{}",
            style::warn(format!(
                "context window is {context} tokens · aarg prompts run 4 to 8k tokens and a smaller window silently clips them; reload {model} with at least {MIN_CONTEXT_TOKENS}"
            ))
        );
    } else {
        eprintln!(
            "{}",
            style::kv("context", style::dim(format!("{context} tokens")), 8)
        );
    }
}

/// The loaded context length LM Studio reports for `model` via its native REST
/// API (`GET /api/v0/models`), falling back to the model's maximum when the
/// loaded figure is absent. `None` on any transport or shape failure.
async fn lmstudio_context(http: &reqwest::Client, base_url: &str, model: &str) -> Option<u64> {
    let response = http
        .get(format!("{base_url}/api/v0/models"))
        .send()
        .await
        .ok()?;
    let body = response.text().await.ok()?;
    let parsed: Value = serde_json::from_str(&body).ok()?;
    let entry = parsed
        .get("data")?
        .as_array()?
        .iter()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(model))?;
    entry
        .get("loaded_context_length")
        .and_then(Value::as_u64)
        .or_else(|| entry.get("max_context_length").and_then(Value::as_u64))
}

/// The model's maximum context length from Ollama's `/api/show`, parsed with
/// the client's own arch-prefixed key match (aarg sizes each request's window
/// up to this maximum, so a maximum under 8k is the ceiling that matters).
/// `None` on any failure.
async fn ollama_context(http: &reqwest::Client, base_url: &str, model: &str) -> Option<u64> {
    let response = http
        .post(format!("{base_url}/api/show"))
        .json(&json!({ "model": model }))
        .send()
        .await
        .ok()?;
    let body = response.text().await.ok()?;
    crate::llm::ollama::context_length_from_show(&body)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn the_reasoning_note_names_the_count_and_the_remedy() {
        let note = reasoning_note(210);
        assert!(note.contains("210 tokens on hidden reasoning"));
        assert!(note.contains("instruct model"));
    }
}
