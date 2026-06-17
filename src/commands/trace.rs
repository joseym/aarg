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
    // pipes or greps, so it stays on stdout, unstyled and verbatim.
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
    println!(
        "{}",
        serde_json::to_string_pretty(&trace.input).map_err(CliError::OutputJson)?
    );
    eprintln!("{}", style::section("system"));
    println!("{}", trace.system);
    for message in &trace.messages {
        let label = match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        eprintln!("{}", style::section(label));
        println!("{}", message.content);
    }
    if let Some(reply) = &trace.reply {
        eprintln!("{}", style::section("reply"));
        println!("{reply}");
    }
    Ok(())
}
