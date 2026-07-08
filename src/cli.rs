//! The command-line surface: what `aarg <args>` accepts.
//!
//! Pure declaration — no behavior lives here. `main.rs` parses with this
//! and dispatches to the `commands` module, which keeps the argument
//! grammar testable without running anything.

use clap::{Parser, Subcommand, ValueEnum};

/// AARG: the Adversarial Agentic Resume Generator.
#[derive(Debug, Parser)]
#[command(name = "aarg", version, about)]
pub struct Cli {
    /// `None` when invoked bare (`aarg`) — that drops into the interactive REPL.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Which resume PDF(s) `tailor` renders. Both variants are projections of
/// one canonical draft and are guaranteed to make the same claims.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum VariantArg {
    /// The parser-safe ATS PDF only.
    Ats,
    /// The designed human-reader PDF only.
    Human,
    /// Both (the default).
    Both,
}

impl VariantArg {
    /// The variants to render at finalize, in output order.
    pub fn variants(self) -> Vec<crate::variant::Variant> {
        use crate::variant::Variant;
        match self {
            VariantArg::Ats => vec![Variant::Ats],
            VariantArg::Human => vec![Variant::Human],
            VariantArg::Both => vec![Variant::Ats, Variant::Human],
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Set up aarg: create a workspace here and store an API key in the keychain
    Init {
        /// Use the global per-user config instead of a local `.aarg` workspace
        #[arg(long, conflicts_with = "dir")]
        global: bool,
        /// Create the workspace at this project directory instead of the current one
        #[arg(long, value_name = "PATH")]
        dir: Option<std::path::PathBuf>,
    },
    /// Show the current configuration and where it lives
    Config,
    /// Manage the API keys stored in the OS keychain (list, add, switch, remove)
    Key {
        #[command(subcommand)]
        command: KeyCommand,
    },
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
    /// Ask questions about a posting and how your background fits it
    Chat {
        /// JD text file, posting URL, or "-" for stdin; omit to pick a recent one
        path: Option<std::path::PathBuf>,
    },
    /// Compare your dataset against a job description's requirements
    Gap {
        /// JD file, Greenhouse/Lever/LinkedIn URL, `jd parse --json` output, or "-"; omit to pick a past one
        jd: Option<std::path::PathBuf>,
        /// Print the report as JSON instead of a summary
        #[arg(long)]
        json: bool,
    },
    /// Tailor your resume to a job description and render the PDF(s)
    Tailor {
        /// JD file, Greenhouse/Lever/LinkedIn URL, `jd parse --json` output, or "-"; omit to pick a past one
        jd: Option<std::path::PathBuf>,
        /// Which PDF(s) to render: ats, human, or both (default)
        #[arg(long, value_enum, default_value_t = VariantArg::Both)]
        variant: VariantArg,
        /// Render the human variant with your own Typst template (a `.typ`
        /// file reading the variant-payload JSON). The ATS layout stays the
        /// built-in parser-safe one.
        #[arg(long, value_name = "PATH")]
        template: Option<std::path::PathBuf>,
        /// Also draft a cover letter from the tailored resume
        #[arg(long)]
        cover: bool,
    },
    /// Draft a cover letter for a past build (reuses its resume and JD)
    Cover {
        /// Build id to write a letter for (e.g. 029); omit to pick one interactively
        build: Option<String>,
        /// Walk through a short interview (angle, emphasis, tone, motivation,
        /// constraints) before drafting; needs a terminal
        #[arg(long)]
        interactive: bool,
    },
    /// Copy a build's PDFs to a folder with friendly names (company.ats.pdf, ...)
    Export {
        /// Build id to export (e.g. 029); omit to pick one interactively
        build: Option<String>,
        /// Destination folder; overrides the configured export dir, defaults to the current directory
        #[arg(long, value_name = "DIR")]
        to: Option<std::path::PathBuf>,
    },
    /// Open a build's PDFs in your system viewer
    Open {
        /// Build id to open (e.g. 029); omit to pick one interactively
        build: Option<String>,
    },
    /// Re-render a past build's PDFs from its saved draft (skips the tailor loop)
    Render {
        /// Build id to re-render (e.g. 029); omit to pick one interactively
        build: Option<String>,
        /// Skip the model: re-render the saved payloads with the current templates (layout only)
        #[arg(long = "no-llm")]
        no_llm: bool,
        /// Render the human variant with this template (a built-in name like `technical`, or a `.typ` path)
        #[arg(long, value_name = "TEMPLATE")]
        template: Option<std::path::PathBuf>,
    },
    /// Change a past build's resume in plain words (remove a bullet, adjust tone), then re-render
    Tune {
        /// Build id to tune (e.g. 029); omit to pick one interactively
        build: Option<String>,
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
    /// Record projects, open-source, or other experience outside a job
    Experience {
        #[command(subcommand)]
        command: ExperienceCommand,
    },
    /// Re-review a saved build with the adversarial reviewer (no re-tailor)
    Attack {
        /// Build id to re-review (e.g. 021); omit to pick one interactively
        build: Option<String>,
    },
    /// List past builds (or `history rm <id>` to delete one)
    History {
        #[command(subcommand)]
        command: Option<HistoryCommand>,
    },
    /// Compare two builds field by field
    Diff {
        /// The earlier build id (e.g. 020)
        from: String,
        /// The later build id (e.g. 021)
        to: String,
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
    /// Print a shell completion script for tab-completion of aarg commands
    #[command(after_help = "\
To install, add the line for your shell to its startup file:
  bash   echo 'source <(aarg completions bash)' >> ~/.bashrc
  zsh    echo 'source <(aarg completions zsh)' >> ~/.zshrc
  fish   aarg completions fish > ~/.config/fish/completions/aarg.fish
  pwsh   aarg completions powershell >> $PROFILE
then restart your shell.")]
    Completions {
        /// Which shell to generate for
        shell: clap_complete::Shell,
    },
    /// List resume templates or set the default for a variant
    Templates {
        #[command(subcommand)]
        command: TemplatesCommand,
    },
    /// Run AARG as a Model Context Protocol server over stdio (for Claude Desktop, Claude Code, and other MCP clients)
    Mcp,
    /// Run the HTTP companion server a browser UI calls for the four things wasm can't do (key, typst, workspace, cross-origin fetch)
    Serve {
        /// Port to bind; defaults to 8787
        #[arg(long, value_name = "PORT")]
        port: Option<u16>,
        /// Serve a different web app build at `/` instead of the built-in one (development override)
        #[arg(long, value_name = "PATH")]
        dir: Option<std::path::PathBuf>,
        /// Bind address; defaults to 127.0.0.1 (localhost only). Pass 0.0.0.0 to reach the server from another device on your network (e.g. a phone) — this also exposes your dataset and the key-spending LLM proxy to that network, so use only a trusted one
        #[arg(long, value_name = "ADDR")]
        bind: Option<std::net::IpAddr>,
        /// Extra Host header values to accept (repeatable), used only when bound past loopback; this machine's own hostname is allowed automatically
        #[arg(long = "allow-host", value_name = "HOST")]
        allow_host: Vec<String>,
    },
}

// EXERCISE(EX-004)
#[derive(Debug, Subcommand)]
pub enum LlmCommand {
    /// Send a tiny request to verify the key, model, and connectivity
    Ping,
}

#[derive(Debug, Subcommand)]
pub enum TemplatesCommand {
    /// List the available templates, marking the active one per variant
    List,
    /// Make a template the default (its variant is inferred from the name)
    Use {
        /// Template name, e.g. `minimal` (ATS) or `technical` (human)
        name: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum KeyCommand {
    /// List the stored key labels, marking the active one
    List,
    /// Add a key under a label (prompts for the key; omit the label for `default`)
    Add {
        /// Label to file the key under (e.g. work, personal)
        label: Option<String>,
        /// Store a Claude-plan OAuth token (from `claude setup-token`) instead of an API key (experimental)
        #[arg(long)]
        oauth: bool,
        /// Delegate to the Anthropic CLI: fetch a fresh plan token via `ant` each run, store nothing (experimental)
        #[arg(long, conflicts_with = "oauth")]
        cli: bool,
    },
    /// Make a stored key the active one for new requests
    Use {
        /// Label of the key to activate
        label: String,
    },
    /// Remove a stored key from the keychain and config
    Remove {
        /// Label of the key to remove
        label: String,
    },
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
pub enum HistoryCommand {
    /// Delete builds and all their artifacts (no ids = pick from a list)
    Rm {
        /// Build ids to delete (e.g. 019 020); omit to choose interactively
        ids: Vec<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum SkillsCommand {
    /// Add a skill you have and back it with evidence (interview)
    Add {
        /// The skill name, e.g. "TypeScript"; omit to be prompted
        name: Option<String>,
        /// Category for a new skill: hard, soft, domain, tool, language, framework
        #[arg(long)]
        category: Option<String>,
    },
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
pub enum ExperienceCommand {
    /// Record a project / non-job experience and link the skills it demonstrates
    Add {
        /// The project or experience name; omit to be prompted
        name: Option<String>,
        /// One-line summary of what it was; omit to be prompted (or left blank in a script)
        #[arg(long)]
        summary: Option<String>,
        /// A link: repo, demo, or write-up
        #[arg(long)]
        url: Option<String>,
        /// A recorded skill this demonstrates (repeat for several); skips the picker
        #[arg(long = "skill", value_name = "NAME")]
        skills: Vec<String>,
    },
    /// List the recorded projects / experience
    List,
    /// Remove an entry by id (see `aarg experience list`)
    Remove {
        /// The project id, e.g. "project-2"
        id: String,
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
        /// JD text file, Greenhouse/Lever/LinkedIn posting URL, or "-" for stdin
        path: std::path::PathBuf,
        /// Print the parsed requirements as JSON instead of a summary
        #[arg(long)]
        json: bool,
    },
    /// Rate how well your profile fits a posting (a tight coverage score)
    Rate {
        /// JD file, Greenhouse/Lever/LinkedIn URL, `jd parse --json` output, or "-"; omit to pick a past one
        jd: Option<std::path::PathBuf>,
        /// Print the rating as JSON instead of a summary
        #[arg(long)]
        json: bool,
    },
    /// Forget remembered parsed JDs (pick from a checklist, or --all)
    Rm {
        /// Forget every remembered JD instead of picking from a list
        #[arg(long)]
        all: bool,
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
            Cli::try_parse_from(["aarg", "init"])
                .unwrap()
                .command
                .unwrap(),
            Command::Init { .. }
        ));
        assert!(matches!(
            Cli::try_parse_from(["aarg", "config"])
                .unwrap()
                .command
                .unwrap(),
            Command::Config
        ));
    }

    #[test]
    fn ingest_takes_a_path_and_requires_one() {
        let cli = Cli::try_parse_from(["aarg", "ingest", "resume.md"]).unwrap();
        match cli.command.unwrap() {
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
                .command
                .unwrap(),
            Command::Dataset {
                command: DatasetCommand::Show
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["aarg", "dataset", "validate"])
                .unwrap()
                .command
                .unwrap(),
            Command::Dataset {
                command: DatasetCommand::Validate
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["aarg", "dataset", "edit"])
                .unwrap()
                .command
                .unwrap(),
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
        match cli.command.unwrap() {
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
            cli.command.unwrap(),
            Command::Jd {
                command: JdCommand::Parse { json: true, .. }
            }
        ));
    }

    #[test]
    fn jd_rm_parses_bare_and_with_all() {
        assert!(matches!(
            Cli::try_parse_from(["aarg", "jd", "rm"])
                .unwrap()
                .command
                .unwrap(),
            Command::Jd {
                command: JdCommand::Rm { all: false }
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["aarg", "jd", "rm", "--all"])
                .unwrap()
                .command
                .unwrap(),
            Command::Jd {
                command: JdCommand::Rm { all: true }
            }
        ));
    }

    #[test]
    fn jd_rate_parses_with_an_optional_jd_and_json_flag() {
        let cli = Cli::try_parse_from(["aarg", "jd", "rate", "jd.txt"]).unwrap();
        match cli.command.unwrap() {
            Command::Jd {
                command: JdCommand::Rate { jd, json },
            } => {
                assert_eq!(jd, Some(std::path::PathBuf::from("jd.txt")));
                assert!(!json);
            }
            other => panic!("expected jd rate, got {other:?}"),
        }

        // Bare (the picker fallback) plus --json.
        assert!(matches!(
            Cli::try_parse_from(["aarg", "jd", "rate", "--json"])
                .unwrap()
                .command
                .unwrap(),
            Command::Jd {
                command: JdCommand::Rate {
                    jd: None,
                    json: true
                }
            }
        ));
    }

    #[test]
    fn gap_takes_a_jd_path_and_an_optional_json_flag() {
        let cli = Cli::try_parse_from(["aarg", "gap", "jd.txt"]).unwrap();
        match cli.command.unwrap() {
            Command::Gap { jd, json } => {
                assert_eq!(jd, Some(std::path::PathBuf::from("jd.txt")));
                assert!(!json);
            }
            other => panic!("expected gap, got {other:?}"),
        }
        assert!(matches!(
            Cli::try_parse_from(["aarg", "gap", "-", "--json"])
                .unwrap()
                .command
                .unwrap(),
            Command::Gap { json: true, .. }
        ));
        // The JD is now optional — bare `gap` means "pick one interactively".
        assert!(matches!(
            Cli::try_parse_from(["aarg", "gap"])
                .unwrap()
                .command
                .unwrap(),
            Command::Gap { jd: None, .. }
        ));
    }

    #[test]
    fn tailor_takes_a_jd_path_and_defaults_to_both_variants() {
        let cli = Cli::try_parse_from(["aarg", "tailor", "jd.txt"]).unwrap();
        match cli.command.unwrap() {
            Command::Tailor {
                jd,
                variant,
                template,
                cover,
            } => {
                assert_eq!(jd, Some(std::path::PathBuf::from("jd.txt")));
                assert_eq!(variant, VariantArg::Both);
                assert_eq!(template, None);
                assert!(!cover);
            }
            other => panic!("expected tailor, got {other:?}"),
        }
        // The JD is now optional — bare `tailor` means "pick one interactively".
        assert!(matches!(
            Cli::try_parse_from(["aarg", "tailor"])
                .unwrap()
                .command
                .unwrap(),
            Command::Tailor { jd: None, .. }
        ));
    }

    #[test]
    fn tailor_cover_flag_parses() {
        let cli = Cli::try_parse_from(["aarg", "tailor", "jd.txt", "--cover"]).unwrap();
        match cli.command.unwrap() {
            Command::Tailor { cover, .. } => assert!(cover),
            other => panic!("expected tailor, got {other:?}"),
        }
    }

    #[test]
    fn cover_parses_with_and_without_a_build_id() {
        match Cli::try_parse_from(["aarg", "cover", "029"])
            .unwrap()
            .command
            .unwrap()
        {
            Command::Cover { build, interactive } => {
                assert_eq!(build.as_deref(), Some("029"));
                assert!(!interactive);
            }
            other => panic!("expected cover, got {other:?}"),
        }
        // The build id is optional — omitting it means "pick interactively".
        match Cli::try_parse_from(["aarg", "cover"])
            .unwrap()
            .command
            .unwrap()
        {
            Command::Cover { build, .. } => assert_eq!(build, None),
            other => panic!("expected cover, got {other:?}"),
        }
    }

    #[test]
    fn cover_interactive_flag_parses() {
        let cli = Cli::try_parse_from(["aarg", "cover", "029", "--interactive"]).unwrap();
        match cli.command.unwrap() {
            Command::Cover { interactive, .. } => assert!(interactive),
            other => panic!("expected cover, got {other:?}"),
        }
    }

    #[test]
    fn export_parses_with_build_and_to() {
        // A build id and an explicit destination.
        match Cli::try_parse_from(["aarg", "export", "029", "--to", "/tmp/out"])
            .unwrap()
            .command
            .unwrap()
        {
            Command::Export { build, to } => {
                assert_eq!(build.as_deref(), Some("029"));
                assert_eq!(to, Some(std::path::PathBuf::from("/tmp/out")));
            }
            other => panic!("expected export, got {other:?}"),
        }
        // Both are optional: bare `export` picks a build and defaults the dir.
        match Cli::try_parse_from(["aarg", "export"])
            .unwrap()
            .command
            .unwrap()
        {
            Command::Export { build, to } => {
                assert_eq!(build, None);
                assert_eq!(to, None);
            }
            other => panic!("expected export, got {other:?}"),
        }
    }

    #[test]
    fn open_parses_with_and_without_a_build_id() {
        match Cli::try_parse_from(["aarg", "open", "029"])
            .unwrap()
            .command
            .unwrap()
        {
            Command::Open { build } => assert_eq!(build.as_deref(), Some("029")),
            other => panic!("expected open, got {other:?}"),
        }
        // The build id is optional — omitting it means "pick interactively".
        match Cli::try_parse_from(["aarg", "open"])
            .unwrap()
            .command
            .unwrap()
        {
            Command::Open { build } => assert_eq!(build, None),
            other => panic!("expected open, got {other:?}"),
        }
    }

    #[test]
    fn render_parses_with_a_build_and_the_no_llm_flag() {
        match Cli::try_parse_from(["aarg", "render", "029"])
            .unwrap()
            .command
            .unwrap()
        {
            Command::Render {
                build,
                no_llm,
                template,
            } => {
                assert_eq!(build.as_deref(), Some("029"));
                assert!(!no_llm);
                assert_eq!(template, None);
            }
            other => panic!("expected render, got {other:?}"),
        }
        // Build id optional (pick interactively); --no-llm and --template parse.
        match Cli::try_parse_from(["aarg", "render", "--no-llm", "--template", "technical"])
            .unwrap()
            .command
            .unwrap()
        {
            Command::Render {
                build,
                no_llm,
                template,
            } => {
                assert_eq!(build, None);
                assert!(no_llm);
                assert_eq!(template, Some(std::path::PathBuf::from("technical")));
            }
            other => panic!("expected render, got {other:?}"),
        }
    }

    #[test]
    fn tailor_template_flag_parses_a_path() {
        let cli =
            Cli::try_parse_from(["aarg", "tailor", "jd.txt", "--template", "my.typ"]).unwrap();
        match cli.command.unwrap() {
            Command::Tailor { template, .. } => {
                assert_eq!(template, Some(std::path::PathBuf::from("my.typ")));
            }
            other => panic!("expected tailor, got {other:?}"),
        }
    }

    #[test]
    fn tailor_variant_flag_parses() {
        let cli = Cli::try_parse_from(["aarg", "tailor", "jd.txt", "--variant", "human"]).unwrap();
        match cli.command.unwrap() {
            Command::Tailor { variant, .. } => assert_eq!(variant, VariantArg::Human),
            other => panic!("expected tailor, got {other:?}"),
        }
        // An unknown variant is rejected.
        assert!(Cli::try_parse_from(["aarg", "tailor", "jd.txt", "--variant", "fancy"]).is_err());
    }

    #[test]
    fn skills_verify_parses() {
        assert!(matches!(
            Cli::try_parse_from(["aarg", "skills", "verify"])
                .unwrap()
                .command
                .unwrap(),
            Command::Skills {
                command: SkillsCommand::Verify
            }
        ));
    }

    #[test]
    fn history_parses_bare_and_with_rm() {
        assert!(matches!(
            Cli::try_parse_from(["aarg", "history"])
                .unwrap()
                .command
                .unwrap(),
            Command::History { command: None }
        ));
        let cmd = Cli::try_parse_from(["aarg", "history", "rm", "019", "020"])
            .unwrap()
            .command
            .unwrap();
        match cmd {
            Command::History {
                command: Some(HistoryCommand::Rm { ids }),
            } => assert_eq!(ids, vec!["019", "020"]),
            other => panic!("expected history rm, got {other:?}"),
        }
        // `rm` with no ids is allowed — it means "pick interactively".
        assert!(matches!(
            Cli::try_parse_from(["aarg", "history", "rm"]).unwrap().command.unwrap(),
            Command::History {
                command: Some(HistoryCommand::Rm { ids }),
            } if ids.is_empty()
        ));
    }

    #[test]
    fn attack_parses_with_and_without_a_build_id() {
        match Cli::try_parse_from(["aarg", "attack", "021"])
            .unwrap()
            .command
            .unwrap()
        {
            Command::Attack { build } => assert_eq!(build.as_deref(), Some("021")),
            other => panic!("expected attack, got {other:?}"),
        }
        // The build id is optional — omitting it means "pick interactively".
        match Cli::try_parse_from(["aarg", "attack"])
            .unwrap()
            .command
            .unwrap()
        {
            Command::Attack { build } => assert_eq!(build, None),
            other => panic!("expected attack, got {other:?}"),
        }
    }

    #[test]
    fn diff_parses_two_build_ids() {
        let cmd = Cli::try_parse_from(["aarg", "diff", "020", "021"])
            .unwrap()
            .command
            .unwrap();
        match cmd {
            Command::Diff { from, to } => {
                assert_eq!(from, "020");
                assert_eq!(to, "021");
            }
            other => panic!("expected diff, got {other:?}"),
        }
    }

    #[test]
    fn skills_dedup_parses() {
        assert!(matches!(
            Cli::try_parse_from(["aarg", "skills", "dedup"])
                .unwrap()
                .command
                .unwrap(),
            Command::Skills {
                command: SkillsCommand::Dedup
            }
        ));
    }

    #[test]
    fn skills_add_parses_name_and_optional_category() {
        // Bare `skills add` is valid — the name is prompted for.
        assert!(matches!(
            Cli::try_parse_from(["aarg", "skills", "add"])
                .unwrap()
                .command
                .unwrap(),
            Command::Skills {
                command: SkillsCommand::Add {
                    name: None,
                    category: None
                }
            }
        ));
        match Cli::try_parse_from([
            "aarg",
            "skills",
            "add",
            "TypeScript",
            "--category",
            "language",
        ])
        .unwrap()
        .command
        .unwrap()
        {
            Command::Skills {
                command: SkillsCommand::Add { name, category },
            } => {
                assert_eq!(name.as_deref(), Some("TypeScript"));
                assert_eq!(category.as_deref(), Some("language"));
            }
            other => panic!("expected skills add, got {other:?}"),
        }
    }

    #[test]
    fn experience_add_parses_name_flags_and_repeated_skills() {
        // Bare `experience add` is valid — the name is prompted for.
        assert!(matches!(
            Cli::try_parse_from(["aarg", "experience", "add"])
                .unwrap()
                .command
                .unwrap(),
            Command::Experience {
                command: ExperienceCommand::Add { name: None, .. }
            }
        ));
        match Cli::try_parse_from([
            "aarg",
            "experience",
            "add",
            "aarg",
            "--summary",
            "a resume tailor",
            "--skill",
            "Rust",
            "--skill",
            "Typst",
        ])
        .unwrap()
        .command
        .unwrap()
        {
            Command::Experience {
                command:
                    ExperienceCommand::Add {
                        name,
                        summary,
                        url,
                        skills,
                    },
            } => {
                assert_eq!(name.as_deref(), Some("aarg"));
                assert_eq!(summary.as_deref(), Some("a resume tailor"));
                assert_eq!(url, None);
                assert_eq!(skills, vec!["Rust".to_string(), "Typst".to_string()]);
            }
            other => panic!("expected experience add, got {other:?}"),
        }
    }

    #[test]
    fn voice_subcommands_parse() {
        assert!(matches!(
            Cli::try_parse_from(["aarg", "voice", "list"])
                .unwrap()
                .command
                .unwrap(),
            Command::Voice {
                command: VoiceCommand::List
            }
        ));
        match Cli::try_parse_from(["aarg", "voice", "add", "--context", "blog post"])
            .unwrap()
            .command
            .unwrap()
        {
            Command::Voice {
                command: VoiceCommand::Add { context },
            } => assert_eq!(context.as_deref(), Some("blog post")),
            other => panic!("expected voice add, got {other:?}"),
        }
        match Cli::try_parse_from(["aarg", "voice", "remove", "sample-2"])
            .unwrap()
            .command
            .unwrap()
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
            .unwrap()
        {
            Command::Roles {
                command: RolesCommand::Enrich { id },
            } => assert_eq!(id, None),
            other => panic!("expected roles enrich, got {other:?}"),
        }
        match Cli::try_parse_from(["aarg", "roles", "enrich", "role-3"])
            .unwrap()
            .command
            .unwrap()
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
                .command
                .unwrap(),
            Command::Trace {
                command: TraceCommand::Last
            }
        ));
        match Cli::try_parse_from(["aarg", "trace", "show", "some-id"])
            .unwrap()
            .command
            .unwrap()
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
            cli.command.unwrap(),
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
    fn serve_parses_bare_and_with_port_and_dir() {
        // Bare `serve`: both optional, so the defaults (port 8787, no static) apply.
        match Cli::try_parse_from(["aarg", "serve"])
            .unwrap()
            .command
            .unwrap()
        {
            Command::Serve {
                port,
                dir,
                bind,
                allow_host,
            } => {
                assert_eq!(port, None);
                assert_eq!(dir, None);
                assert_eq!(bind, None);
                assert!(allow_host.is_empty());
            }
            other => panic!("expected serve, got {other:?}"),
        }
        match Cli::try_parse_from([
            "aarg",
            "serve",
            "--port",
            "9000",
            "--dir",
            "web/dist",
            "--bind",
            "0.0.0.0",
            "--allow-host",
            "mortm5.local",
        ])
        .unwrap()
        .command
        .unwrap()
        {
            Command::Serve {
                port,
                dir,
                bind,
                allow_host,
            } => {
                assert_eq!(port, Some(9000));
                assert_eq!(dir, Some(std::path::PathBuf::from("web/dist")));
                assert_eq!(bind, Some("0.0.0.0".parse().unwrap()));
                assert_eq!(allow_host, vec!["mortm5.local".to_string()]);
            }
            other => panic!("expected serve, got {other:?}"),
        }
        // A non-numeric port is rejected by clap.
        assert!(Cli::try_parse_from(["aarg", "serve", "--port", "notaport"]).is_err());
        // A malformed bind address is rejected by clap.
        assert!(Cli::try_parse_from(["aarg", "serve", "--bind", "not-an-ip"]).is_err());
    }

    #[test]
    fn unknown_commands_are_rejected() {
        assert!(Cli::try_parse_from(["aarg", "frobnicate"]).is_err());
        // Bare `aarg` is valid now — no subcommand means the interactive REPL.
        assert!(Cli::try_parse_from(["aarg"]).unwrap().command.is_none());
    }
}
