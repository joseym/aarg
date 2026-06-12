//! The command-line surface: what `aarg <args>` accepts.
//!
//! Pure declaration — no behavior lives here. `main.rs` parses with this
//! and dispatches to the `commands` module, which keeps the argument
//! grammar testable without running anything.

use clap::{Parser, Subcommand};

/// AARG: the Adversarial Agentic Resume Generator.
#[derive(Debug, Parser)]
#[command(name = "aarg", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Set up aarg: pick a provider and store its API key in the OS keychain
    Init,
    /// Show the current configuration and where it lives
    Config,
    /// Build your dataset from an existing resume (text or Markdown)
    Ingest {
        /// Path to the resume file (for a PDF, extract its text first)
        path: std::path::PathBuf,
    },
    /// Talk to the configured LLM provider directly
    Llm {
        #[command(subcommand)]
        command: LlmCommand,
    },
}

// EXERCISE(EX-004)
#[derive(Debug, Subcommand)]
pub enum LlmCommand {
    /// Send a tiny request to verify the key, model, and connectivity
    Ping,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn top_level_commands_parse() {
        assert!(matches!(
            Cli::try_parse_from(["aarg", "init"]).unwrap().command,
            Command::Init
        ));
        assert!(matches!(
            Cli::try_parse_from(["aarg", "config"]).unwrap().command,
            Command::Config
        ));
    }

    #[test]
    fn ingest_takes_a_path_and_requires_one() {
        let cli = Cli::try_parse_from(["aarg", "ingest", "resume.md"]).unwrap();
        match cli.command {
            Command::Ingest { path } => {
                assert_eq!(path, std::path::PathBuf::from("resume.md"));
            }
            other => panic!("expected ingest, got {other:?}"),
        }
        assert!(Cli::try_parse_from(["aarg", "ingest"]).is_err());
    }

    #[test]
    fn llm_ping_parses_as_a_nested_subcommand() {
        let cli = Cli::try_parse_from(["aarg", "llm", "ping"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Llm {
                command: LlmCommand::Ping
            }
        ));
    }

    #[test]
    #[ignore = "exercise: llm ping always uses the configured model; add a --model flag that overrides it for a single run, then finish this test"]
    fn ex_004_ping_accepts_a_model_override() {
        // Once the flag exists: parse ["aarg", "llm", "ping", "--model",
        // "some-model"] and assert the value reaches the Ping variant.
        let model_flag_implemented = false;
        assert!(model_flag_implemented);
    }

    #[test]
    fn unknown_commands_are_rejected() {
        assert!(Cli::try_parse_from(["aarg", "frobnicate"]).is_err());
        assert!(Cli::try_parse_from(["aarg"]).is_err());
    }
}
