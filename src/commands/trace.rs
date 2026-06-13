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
    println!("trace:    {}", trace.trace_id.0);
    println!("agent:    {} · model {}", trace.agent, trace.model);
    println!(
        "started:  {} · took {} ms",
        trace.started_at.format("%Y-%m-%d %H:%M:%S UTC"),
        trace.duration_ms
    );
    match &trace.outcome {
        TraceOutcome::Succeeded => println!(
            "outcome:  succeeded · {} tokens in, {} out",
            trace.usage.input_tokens, trace.usage.output_tokens
        ),
        TraceOutcome::Failed { error } => println!(
            "outcome:  FAILED ({error}) · {} tokens in, {} out",
            trace.usage.input_tokens, trace.usage.output_tokens
        ),
    }

    println!("\n--- input ---");
    println!(
        "{}",
        serde_json::to_string_pretty(&trace.input).map_err(CliError::OutputJson)?
    );
    println!("\n--- system ---");
    println!("{}", trace.system);
    for message in &trace.messages {
        let label = match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        println!("\n--- {label} ---");
        println!("{}", message.content);
    }
    if let Some(reply) = &trace.reply {
        println!("\n--- reply ---");
        println!("{reply}");
    }
    Ok(())
}
