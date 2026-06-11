//! `aarg llm ping` — send the smallest possible request to the
//! configured provider and report what came back. Verifies the stored
//! key, the model name, and connectivity in one shot.

use std::time::Instant;

use crate::commands::CliError;
use crate::config::Config;
use crate::llm::{AnthropicClient, CompletionRequest, LlmClient, LlmError, Message};
use crate::secrets;

pub async fn run() -> Result<(), CliError> {
    let config = Config::load()?;
    let provider = config.provider;
    let key = secrets::load_api_key(provider.name())
        .await?
        .ok_or_else(|| LlmError::MissingApiKey {
            provider: provider.name().to_string(),
        })?;

    let client = AnthropicClient::new(key);
    let request = CompletionRequest {
        model: config.anthropic.model.clone(),
        max_tokens: 16,
        system: None,
        messages: vec![Message::user("Reply with the single word: pong")],
        temperature: None,
    };

    let started = Instant::now();
    let response = client.complete(request).await?;
    let elapsed = started.elapsed();

    println!("model:    {}", response.model);
    println!("reply:    {}", response.text.trim());
    println!("latency:  {} ms", elapsed.as_millis());
    println!(
        "tokens:   {} in, {} out",
        response.usage.input_tokens, response.usage.output_tokens
    );
    Ok(())
}
