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
    /// Inspect or check the local dataset
    Dataset {
        #[command(subcommand)]
        command: DatasetCommand,
    },
    /// Work with job descriptions
    Jd {
        #[command(subcommand)]
        command: JdCommand,
    },
    /// Compare your dataset against a job description's requirements
    Gap {
        /// JD text file, Greenhouse/Lever URL, `jd parse --json` output, or "-"
        jd: std::path::PathBuf,
        /// Print the report as JSON instead of a summary
        #[arg(long)]
        json: bool,
    },
    /// Tailor your resume to a job description and render the ATS PDF
    Tailor {
        /// JD text file, Greenhouse/Lever URL, `jd parse --json` output, or "-"
        jd: std::path::PathBuf,
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

#[derive(Debug, Subcommand)]
pub enum DatasetCommand {
    /// Summarize what the dataset contains and where it lives
    Show,
    /// Check integrity: unsupported skills, broken references (exits nonzero on problems)
    Validate,
}

#[derive(Debug, Subcommand)]
pub enum JdCommand {
    /// Parse a job description into structured requirements
    Parse {
        /// JD text file, Greenhouse/Lever posting URL, or "-" for stdin
        path: std::path::PathBuf,
        /// Print the parsed requirements as JSON instead of a summary
        #[arg(long)]
        json: bool,
    },
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
    fn dataset_subcommands_parse() {
        assert!(matches!(
            Cli::try_parse_from(["aarg", "dataset", "show"])
                .unwrap()
                .command,
            Command::Dataset {
                command: DatasetCommand::Show
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["aarg", "dataset", "validate"])
                .unwrap()
                .command,
            Command::Dataset {
                command: DatasetCommand::Validate
            }
        ));
        // Bare `aarg dataset` requires a subcommand.
        assert!(Cli::try_parse_from(["aarg", "dataset"]).is_err());
    }

    #[test]
    fn jd_parse_takes_a_path_and_an_optional_json_flag() {
        let cli = Cli::try_parse_from(["aarg", "jd", "parse", "jd.txt"]).unwrap();
        match cli.command {
            Command::Jd {
                command: JdCommand::Parse { path, json },
            } => {
                assert_eq!(path, std::path::PathBuf::from("jd.txt"));
                assert!(!json);
            }
            other => panic!("expected jd parse, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["aarg", "jd", "parse", "-", "--json"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Jd {
                command: JdCommand::Parse { json: true, .. }
            }
        ));
    }

    #[test]
    fn gap_takes_a_jd_path_and_an_optional_json_flag() {
        let cli = Cli::try_parse_from(["aarg", "gap", "jd.txt"]).unwrap();
        match cli.command {
            Command::Gap { jd, json } => {
                assert_eq!(jd, std::path::PathBuf::from("jd.txt"));
                assert!(!json);
            }
            other => panic!("expected gap, got {other:?}"),
        }
        assert!(matches!(
            Cli::try_parse_from(["aarg", "gap", "-", "--json"])
                .unwrap()
                .command,
            Command::Gap { json: true, .. }
        ));
        assert!(Cli::try_parse_from(["aarg", "gap"]).is_err());
    }

    #[test]
    fn tailor_takes_a_jd_path() {
        let cli = Cli::try_parse_from(["aarg", "tailor", "jd.txt"]).unwrap();
        match cli.command {
            Command::Tailor { jd } => assert_eq!(jd, std::path::PathBuf::from("jd.txt")),
            other => panic!("expected tailor, got {other:?}"),
        }
        assert!(Cli::try_parse_from(["aarg", "tailor"]).is_err());
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
