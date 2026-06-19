//! `aarg trace last|show <id>` — read back the runtime's flight
//! recorder. `last` answers "what just happened?"; `show` pulls up a
//! specific run by ID (the trace's filename works too).
//!
//! Pure read-side: no LLM, no network. The whole conversation is
//! printed — system prompt, every turn (retry corrections included),
//! and the raw final reply — because a trace's job is to settle
//! arguments about what was actually said.

use crate::commands::CliError;
use crate::llm::Role;
use crate::style;
use crate::trace::{self, Trace, TraceOutcome};

pub async fn last() -> Result<(), CliError> {
    let trace = trace::latest_in(&trace::default_dir()?)?;
    print_trace(&trace)
}

pub async fn show(id: String) -> Result<(), CliError> {
    let trace = trace::load_from(&trace::default_dir()?, &id)?;
    print_trace(&trace)
}

fn print_trace(trace: &Trace) -> Result<(), CliError> {
    // The framing — headers, the metadata block, the section dividers — is
    // human chrome and goes to stderr. The trace's actual content (the input
    // JSON, the system prompt, every turn, the raw reply) is what a reader
    // pipes or greps, so it stays on stdout. On a terminal the JSON parts are
    // syntax-highlighted for reading; piped or redirected, the color drops out
    // and the content is byte-for-byte verbatim, so `| jq` and `> file` work.
    eprintln!("{}", style::kv("trace", trace.trace_id.0.clone(), 9));
    eprintln!(
        "{}",
        style::kv(
            "agent",
            format!("{} · model {}", trace.agent, trace.model),
            9
        )
    );
    eprintln!(
        "{}",
        style::kv(
            "started",
            format!(
                "{} · took {} ms",
                trace.started_at.format("%Y-%m-%d %H:%M:%S UTC"),
                trace.duration_ms
            ),
            9
        )
    );
    match &trace.outcome {
        TraceOutcome::Succeeded => eprintln!(
            "{}",
            style::kv(
                "outcome",
                style::green(format!(
                    "succeeded · {} tokens in, {} out",
                    trace.usage.input_tokens, trace.usage.output_tokens
                )),
                9
            )
        ),
        TraceOutcome::Failed { error } => eprintln!(
            "{}",
            style::kv(
                "outcome",
                style::red(format!(
                    "FAILED ({error}) · {} tokens in, {} out",
                    trace.usage.input_tokens, trace.usage.output_tokens
                )),
                9
            )
        ),
    }

    eprintln!("{}", style::section("input"));
    // The agent input is always JSON; highlight it (terminal-gated inside
    // `style::json`, so a piped trace stays plain, valid JSON).
    println!("{}", style::json(&trace.input));
    eprintln!("{}", style::section("system"));
    println!("{}", trace.system);
    for message in &trace.messages {
        let label = match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        eprintln!("{}", style::section(label));
        println!("{}", render_content(&message.content));
    }
    if let Some(reply) = &trace.reply {
        eprintln!("{}", style::section("reply"));
        println!("{}", render_content(reply));
    }
    Ok(())
}

/// Render a turn's text or the final reply for display. On a terminal, if the
/// content is JSON — bare, or wrapped in a ```json fence the way agents reply —
/// it's pretty-printed and highlighted. Piped, or when it isn't JSON (a prose
/// prompt), it's the raw string verbatim, so a redirect keeps the exact bytes
/// the model sent.
fn render_content(content: &str) -> String {
    use std::io::IsTerminal;
    if std::io::stdout().is_terminal()
        && let Some(value) = as_json(content)
    {
        return style::json(&value);
    }
    content.to_string()
}

/// Parse `content` as JSON, trying it raw first and then with a surrounding
/// Markdown code fence stripped.
fn as_json(content: &str) -> Option<serde_json::Value> {
    serde_json::from_str(content.trim())
        .ok()
        .or_else(|| serde_json::from_str(strip_fence(content)).ok())
}

/// The inner text of one Markdown code fence (``` or ```json … ```), or the
/// trimmed input unchanged when it isn't fenced.
fn strip_fence(text: &str) -> &str {
    let trimmed = text.trim();
    let Some(after_open) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    // Drop the rest of the opening line (an optional language tag like `json`).
    let Some((_lang, body)) = after_open.split_once('\n') else {
        return trimmed;
    };
    body.trim_end().strip_suffix("```").unwrap_or(body).trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_json_parses_bare_and_fenced_objects() {
        assert!(as_json(r#"{"a": 1}"#).is_some());
        assert!(as_json("```json\n{\"a\": 1}\n```").is_some());
        assert!(as_json("```\n[1, 2, 3]\n```").is_some());
        assert!(as_json("just prose, not json").is_none());
    }

    #[test]
    fn render_content_is_verbatim_off_a_terminal() {
        // Tests aren't a terminal, so content passes through unchanged even
        // when it is JSON — the piped/redirected contract.
        assert_eq!(render_content(r#"{"a":1}"#), r#"{"a":1}"#);
        assert_eq!(render_content("plain prose"), "plain prose");
    }
}
