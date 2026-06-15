//! One module per CLI command, plus the error type that crosses the CLI
//! boundary.
//!
//! Commands return `Result<(), CliError>`; `main.rs` converts a
//! `CliError` into a `miette` diagnostic for display. This is the only
//! place in the codebase where module errors are gathered into one type —
//! everywhere below, errors stay module-specific.

pub mod attack;
pub mod completions;
pub mod config;
pub mod dataset;
pub mod gap;
pub mod history;
pub mod ingest;
pub mod init;
pub mod jd;
pub mod key;
pub mod ping;
pub mod roles;
pub mod skills;
pub mod tailor;
pub mod trace;
pub mod voice;

use std::path::{Path, PathBuf};

use crate::agent::{AgentContext, ModelTier};
use crate::ats::AtsError;
use crate::builds::BuildError;
use crate::config::{Config, ConfigError};
use crate::dataset::DatasetError;
use crate::fetch::FetchError;
use crate::gap::GapError;
use crate::ingest::IngestError;
use crate::jd::{JdError, JobRequirements};
use crate::llm::{AnthropicClient, LlmError};
use crate::render::RenderError;
use crate::review::ReviewError;
use crate::secrets::{self, SecretsError};
use crate::tailor::TailorError;
use crate::trace::TraceError;
use crate::user::AskError;
use crate::variant::{ClaimDivergence, VariantError};

/// Load the config, fetch the stored API key, and build the provider
/// client — the preamble every LLM-backed command starts with. Extracted
/// once the third consumer appeared (ping, ingest, jd parse); the model
/// to use comes from the returned config.
pub(crate) async fn configured_client() -> Result<(AnthropicClient, Config), CliError> {
    let config = Config::load()?;
    let provider = config.provider;

    // Which stored key to use: a one-off `AARG_KEY=<label>` env override
    // wins (handy for scripts and the REPL without editing config), else the
    // configured active label.
    let override_label = std::env::var("AARG_KEY").ok();
    let label = override_label
        .as_deref()
        .unwrap_or_else(|| config.anthropic.active_label());

    let key = match secrets::load_api_key(provider.name(), label).await? {
        Some(key) => key,
        // No labeled key and none ever registered: a single key may still
        // live in the pre-labels bare slot. Read it so existing setups keep
        // working without a re-run of `aarg init`.
        None if config.anthropic.keys.is_empty() => secrets::load_legacy_key(provider.name())
            .await?
            .ok_or_else(|| LlmError::MissingApiKey {
                provider: provider.name().to_string(),
            })?,
        None => {
            return Err(LlmError::MissingApiKey {
                provider: provider.name().to_string(),
            }
            .into());
        }
    };
    Ok((AnthropicClient::new(key), config))
}

/// A tracer pointed at the active workspace's `traces/` directory. Replaces
/// the core `Tracer::to_default_dir()` at the command layer so traces land
/// in the workspace (local `.aarg/` or the home data dir) like every other
/// artifact — the runtime crate stays unaware of workspaces.
pub(crate) fn default_tracer() -> Result<crate::trace::Tracer, ConfigError> {
    crate::workspace::traces_dir()
        .map(crate::trace::Tracer::to_dir)
        .ok_or(ConfigError::NoHomeDir)
}

/// Turn the JD argument every JD-consuming command accepts — a
/// Greenhouse/Lever URL, a `.json` file of already-parsed requirements,
/// a text file, or `-` for stdin — into `JobRequirements`. Extracted at
/// its third consumer (`jd parse`, `gap`, `tailor`).
pub(crate) async fn load_requirements(
    arg: &Path,
    ctx: &AgentContext<'_>,
) -> Result<JobRequirements, CliError> {
    let arg_str = arg.to_string_lossy();
    if arg_str.starts_with("https://") || arg_str.starts_with("http://") {
        eprintln!("fetching {arg_str}...");
        let text = crate::fetch::fetch_jd(&arg_str).await?;
        eprintln!(
            "parsing the posting with {}...",
            ctx.model.resolve("jd_parser_v1", ModelTier::Cheap)
        );
        let mut requirements = crate::jd::parse_jd(ctx, &text).await?;
        requirements.source_url = Some(arg_str.into_owned());
        Ok(requirements)
    } else if arg
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
    {
        let text = read_text_input(arg)?;
        serde_json::from_str(&text).map_err(|source| CliError::BadRequirementsJson {
            path: arg.to_path_buf(),
            source,
        })
    } else {
        let text = read_text_input(arg)?;
        eprintln!(
            "parsing {} with {}...",
            arg.display(),
            ctx.model.resolve("jd_parser_v1", ModelTier::Cheap)
        );
        Ok(crate::jd::parse_jd(ctx, &text).await?)
    }
}

/// Read a text argument that is either a file path or `-` for stdin.
/// Extracted at its second consumer (`jd parse`, `gap`).
pub(crate) fn read_text_input(path: &Path) -> Result<String, CliError> {
    use std::io::Read;

    if path == Path::new("-") {
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .map_err(|source| CliError::ReadInput {
                path: path.to_path_buf(),
                source,
            })?;
        Ok(buffer)
    } else {
        std::fs::read_to_string(path).map_err(|source| CliError::ReadInput {
            path: path.to_path_buf(),
            source,
        })
    }
}

/// Open `path` in the user's `$VISUAL`/`$EDITOR` and wait for it to
/// close. Shared by `dataset edit` and `voice add` — the two commands
/// that hand the user a file to write into. `$EDITOR` may carry
/// arguments ("code --wait"): the first token is the program, the rest
/// pass through.
pub(crate) fn launch_editor(path: &Path) -> Result<(), CliError> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .map_err(|_| CliError::NoEditor)?;
    let mut parts = editor.split_whitespace();
    let program = parts.next().ok_or(CliError::NoEditor)?;
    let status = std::process::Command::new(program)
        .args(parts)
        .arg(path)
        .status()
        .map_err(|source| CliError::EditorLaunch {
            editor: editor.clone(),
            source,
        })?;
    if !status.success() {
        return Err(CliError::EditorAborted { status });
    }
    Ok(())
}

/// Whether a `$VISUAL`/`$EDITOR` is configured — lets a command choose
/// the editor flow only when it would actually work.
pub(crate) fn editor_available() -> bool {
    std::env::var_os("VISUAL").is_some() || std::env::var_os("EDITOR").is_some()
}

/// Everything a command can fail with, unified for the CLI boundary.
/// `#[error(transparent)]` forwards the underlying error's message
/// unchanged — this type adds routing, not wording.
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum CliError {
    #[error(transparent)]
    Config(#[from] ConfigError),

    #[error(transparent)]
    Secrets(#[from] SecretsError),

    #[error(transparent)]
    #[diagnostic(help("check the stored key and model with `aarg config`"))]
    Llm(#[from] LlmError),

    #[error("could not read your answer")]
    #[diagnostic(help(
        "aarg init needs an interactive terminal; in scripts and CI, configure aarg ahead of time"
    ))]
    Prompt(#[from] inquire::InquireError),

    #[error(transparent)]
    Dataset(#[from] DatasetError),

    #[error(transparent)]
    #[diagnostic(help(
        "the model's output didn't parse; re-running usually helps, and a cleaner text export of the resume helps more"
    ))]
    Ingest(#[from] IngestError),

    #[error("could not read {path}")]
    ReadInput {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path} looks like a PDF — aarg ingests text, not PDF binaries")]
    #[diagnostic(help(
        "extract the text first (for example `pdftotext resume.pdf resume.txt`) and ingest that"
    ))]
    PdfInput { path: PathBuf },

    #[error("the dataset has {problems} problem(s)")]
    #[diagnostic(help(
        "review the problems above; skills without evidence stay out of tailored resumes until they're backed or removed"
    ))]
    DatasetInvalid { problems: usize },

    #[error("could not serialize the result to JSON")]
    OutputJson(#[source] serde_json::Error),

    #[error(transparent)]
    #[diagnostic(help(
        "the model's output didn't parse; re-running usually helps, and a plainer text version of the JD helps more"
    ))]
    Jd(#[from] JdError),

    #[error(transparent)]
    #[diagnostic(help("the model's output didn't parse; re-running usually helps"))]
    Gap(#[from] GapError),

    #[error("{path} is not a parsed-requirements JSON file")]
    #[diagnostic(help(
        "a .json argument must be the output of `aarg jd parse <jd> --json`; pass the JD text itself otherwise"
    ))]
    BadRequirementsJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error(transparent)]
    #[diagnostic(help(
        "the model's output didn't parse or selected nothing; re-running usually helps"
    ))]
    Tailor(#[from] TailorError),

    #[error(transparent)]
    #[diagnostic(help("the reviewer's output didn't parse; re-running usually helps"))]
    Review(#[from] ReviewError),

    #[error(transparent)]
    #[diagnostic(help(
        "typst builds the PDF: install it with `cargo install typst-cli` or from https://github.com/typst/typst/releases; if it IS installed and compilation failed, the message above carries typst's own output"
    ))]
    Render(#[from] RenderError),

    #[error(transparent)]
    Ats(#[from] AtsError),

    #[error(transparent)]
    Build(#[from] BuildError),

    #[error(transparent)]
    History(#[from] crate::history::HistoryError),

    #[error(transparent)]
    #[diagnostic(help("save the posting text to a file and pass that path (or pipe it with `-`)"))]
    Fetch(#[from] FetchError),

    #[error("no writing sample was provided")]
    #[diagnostic(help(
        "pipe a file (`aarg voice add < sample.txt`) or type the text and press Ctrl-D"
    ))]
    EmptyVoiceSample,

    #[error("no voice sample with id {id:?}")]
    #[diagnostic(help("run `aarg voice list` to see the ids you can remove"))]
    VoiceSampleNotFound { id: String },

    #[error("no role with id {id:?}")]
    #[diagnostic(help("run `aarg dataset show` to see your roles and their ids"))]
    RoleNotFound { id: String },

    #[error("no editor configured")]
    #[diagnostic(help(
        "set $EDITOR (or $VISUAL) to your editor, e.g. `export EDITOR=nano` or `export EDITOR=\"code --wait\"`"
    ))]
    NoEditor,

    #[error("could not launch your editor ({editor})")]
    EditorLaunch {
        editor: String,
        #[source]
        source: std::io::Error,
    },

    #[error("your editor exited with {status}; the dataset is unchanged")]
    EditorAborted { status: std::process::ExitStatus },

    #[error(transparent)]
    Trace(#[from] TraceError),

    #[error(transparent)]
    #[diagnostic(help(
        "this step needs a person; run it in a terminal, or pre-edit the dataset with `aarg dataset edit`"
    ))]
    Ask(#[from] AskError),

    #[error("the edited draft at {path} is not valid dataset JSON")]
    #[diagnostic(help(
        "your edits are preserved in that draft — run `aarg dataset edit` again to resume it (the dataset itself is unchanged)"
    ))]
    EditedJsonInvalid {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error(transparent)]
    Variant(#[from] VariantError),

    #[error(transparent)]
    #[diagnostic(help(
        "a variant projection made a claim the canonical draft doesn't; the build was refused to keep the two PDFs honest. This is a bug in the variant adapter, not your data."
    ))]
    ClaimDivergence(#[from] ClaimDivergence),

    #[error("--template customizes the human variant, but it isn't being rendered")]
    #[diagnostic(help(
        "drop --variant ats, or use --variant human / --variant both so the human PDF (the one your template renders) is produced"
    ))]
    TemplateWithoutHuman,

    #[error("{label:?} is not a usable key label")]
    #[diagnostic(help(
        "labels name a stored key (e.g. work, personal); they can't be empty or contain a colon"
    ))]
    InvalidKeyLabel { label: String },

    #[error("no stored key labeled {label:?}")]
    #[diagnostic(help(
        "run `aarg key list` to see your labels, or `aarg key add {label}` to add it"
    ))]
    NoSuchKey { label: String },

    #[error("could not determine the current directory")]
    #[diagnostic(help("run from an existing directory, or pass `aarg init --dir <path>`"))]
    CurrentDir(#[source] std::io::Error),
}

/// Reject labels that are empty or carry the `:` that separates provider
/// from label in a keychain slot — both would make for ambiguous or
/// unreachable entries. Shared by `init` and the `key` command.
pub(crate) fn validate_key_label(label: &str) -> Result<&str, CliError> {
    let label = label.trim();
    if label.is_empty() || label.contains(':') {
        return Err(CliError::InvalidKeyLabel {
            label: label.to_string(),
        });
    }
    Ok(label)
}

/// Run one parsed command. Extracted so the binary's `main` and the
/// interactive REPL go through the exact same dispatch — the REPL is a
/// wrapper over this, not a parallel implementation.
pub async fn dispatch(command: crate::cli::Command) -> Result<(), CliError> {
    use crate::cli::{
        Command, DatasetCommand, HistoryCommand, JdCommand, KeyCommand, LlmCommand, RolesCommand,
        SkillsCommand, TraceCommand, VoiceCommand,
    };
    match command {
        Command::Init { global, dir } => init::run(global, dir).await?,
        Command::Config => config::run().await?,
        Command::Key {
            command: KeyCommand::List,
        } => key::list().await?,
        Command::Key {
            command: KeyCommand::Add { label },
        } => key::add(label).await?,
        Command::Key {
            command: KeyCommand::Use { label },
        } => key::use_key(label).await?,
        Command::Key {
            command: KeyCommand::Remove { label },
        } => key::remove(label).await?,
        Command::Ingest { path } => ingest::run(path).await?,
        Command::Dataset {
            command: DatasetCommand::Show,
        } => dataset::show().await?,
        Command::Dataset {
            command: DatasetCommand::Validate,
        } => dataset::validate().await?,
        Command::Dataset {
            command: DatasetCommand::Edit,
        } => dataset::edit().await?,
        Command::Jd {
            command: JdCommand::Parse { path, json },
        } => jd::parse(path, json).await?,
        Command::Gap { jd, json } => gap::run(jd, json).await?,
        Command::Tailor {
            jd,
            variant,
            template,
        } => tailor::run(jd, variant.variants(), template).await?,
        Command::Attack { build } => attack::run(build).await?,
        Command::History { command: None } => history::list()?,
        Command::History {
            command: Some(HistoryCommand::Rm { ids }),
        } => history::remove(ids).await?,
        Command::Diff { from, to } => history::diff(from, to)?,
        Command::Skills {
            command: SkillsCommand::Verify,
        } => skills::verify().await?,
        Command::Skills {
            command: SkillsCommand::Dedup,
        } => skills::dedup().await?,
        Command::Voice {
            command: VoiceCommand::Add { context },
        } => voice::add(context).await?,
        Command::Voice {
            command: VoiceCommand::List,
        } => voice::list().await?,
        Command::Voice {
            command: VoiceCommand::Remove { id },
        } => voice::remove(id).await?,
        Command::Roles {
            command: RolesCommand::Enrich { id },
        } => roles::enrich(id).await?,
        Command::Trace {
            command: TraceCommand::Last,
        } => trace::last().await?,
        Command::Trace {
            command: TraceCommand::Show { id },
        } => trace::show(id).await?,
        Command::Llm {
            command: LlmCommand::Ping,
        } => ping::run().await?,
        Command::Completions { shell } => completions::run(shell)?,
    }
    Ok(())
}
