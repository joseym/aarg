//! `aarg llm ping` — send the smallest possible request to the
//! configured provider and report what came back. Verifies the stored
//! key, the model name, and connectivity in one shot.

use std::time::Instant;

use crate::commands::{CliError, configured_client};
use crate::llm::{CompletionRequest, LlmClient, Message};
use crate::style;

pub async fn run() -> Result<(), CliError> {
    let (client, config) = configured_client().await?;
    let request = CompletionRequest {
        model: config.anthropic.model.clone(),
        max_tokens: 16,
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
    Ok(())
}
