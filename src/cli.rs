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
    /// Maintain the skills in your dataset
    Skills {
        #[command(subcommand)]
        command: SkillsCommand,
    },
    /// Capture writing samples that anchor voice rewrites
    Voice {
        #[command(subcommand)]
        command: VoiceCommand,
    },
    /// Flesh out thin roles in your work history
    Roles {
        #[command(subcommand)]
        command: RolesCommand,
    },
    /// Inspect recorded agent runs
    Trace {
        #[command(subcommand)]
        command: TraceCommand,
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
    /// Open the dataset in $EDITOR, then re-validate and save
    Edit,
}

#[derive(Debug, Subcommand)]
pub enum SkillsCommand {
    /// Interview: back unverified skills with evidence (or remove them)
    Verify,
    /// Collapse redundant skills: auto-remove near-duplicates, then pick off the rest
    Dedup,
}

#[derive(Debug, Subcommand)]
pub enum VoiceCommand {
    /// Add a writing sample (read from stdin: pipe a file or type then Ctrl-D)
    Add {
        /// A short label for where it came from, e.g. "blog post"
        #[arg(long)]
        context: Option<String>,
    },
    /// List the captured writing samples
    List,
    /// Remove a sample by its id (see `aarg voice list`)
    Remove {
        /// The sample id, e.g. "sample-2"
        id: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum RolesCommand {
    /// Interview to add detail to thin roles (all of them, or one by id)
    Enrich {
        /// A specific role id, e.g. "role-3"; omit to cover every thin role
        id: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum TraceCommand {
    /// Show the most recent agent run
    Last,
    /// Show one run by its trace id (the filename also works)
    Show {
        /// Trace id, e.g. 2026-06-12T18-30-00_tailoring_v1_1a2b3
        id: String,
    },
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
        assert!(matches!(
            Cli::try_parse_from(["aarg", "dataset", "edit"])
                .unwrap()
                .command,
            Command::Dataset {
                command: DatasetCommand::Edit
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
    fn skills_verify_parses() {
        assert!(matches!(
            Cli::try_parse_from(["aarg", "skills", "verify"])
                .unwrap()
                .command,
            Command::Skills {
                command: SkillsCommand::Verify
            }
        ));
    }

    #[test]
    fn skills_dedup_parses() {
        assert!(matches!(
            Cli::try_parse_from(["aarg", "skills", "dedup"])
                .unwrap()
                .command,
            Command::Skills {
                command: SkillsCommand::Dedup
            }
        ));
    }

    #[test]
    fn voice_subcommands_parse() {
        assert!(matches!(
            Cli::try_parse_from(["aarg", "voice", "list"])
                .unwrap()
                .command,
            Command::Voice {
                command: VoiceCommand::List
            }
        ));
        match Cli::try_parse_from(["aarg", "voice", "add", "--context", "blog post"])
            .unwrap()
            .command
        {
            Command::Voice {
                command: VoiceCommand::Add { context },
            } => assert_eq!(context.as_deref(), Some("blog post")),
            other => panic!("expected voice add, got {other:?}"),
        }
        match Cli::try_parse_from(["aarg", "voice", "remove", "sample-2"])
            .unwrap()
            .command
        {
            Command::Voice {
                command: VoiceCommand::Remove { id },
            } => assert_eq!(id, "sample-2"),
            other => panic!("expected voice remove, got {other:?}"),
        }
        // remove needs an id.
        assert!(Cli::try_parse_from(["aarg", "voice", "remove"]).is_err());
    }

    #[test]
    fn roles_enrich_parses_with_and_without_an_id() {
        match Cli::try_parse_from(["aarg", "roles", "enrich"])
            .unwrap()
            .command
        {
            Command::Roles {
                command: RolesCommand::Enrich { id },
            } => assert_eq!(id, None),
            other => panic!("expected roles enrich, got {other:?}"),
        }
        match Cli::try_parse_from(["aarg", "roles", "enrich", "role-3"])
            .unwrap()
            .command
        {
            Command::Roles {
                command: RolesCommand::Enrich { id },
            } => assert_eq!(id.as_deref(), Some("role-3")),
            other => panic!("expected roles enrich role-3, got {other:?}"),
        }
    }

    #[test]
    fn trace_subcommands_parse() {
        assert!(matches!(
            Cli::try_parse_from(["aarg", "trace", "last"])
                .unwrap()
                .command,
            Command::Trace {
                command: TraceCommand::Last
            }
        ));
        match Cli::try_parse_from(["aarg", "trace", "show", "some-id"])
            .unwrap()
            .command
        {
            Command::Trace {
                command: TraceCommand::Show { id },
            } => assert_eq!(id, "some-id"),
            other => panic!("expected trace show, got {other:?}"),
        }
        assert!(Cli::try_parse_from(["aarg", "trace"]).is_err());
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
