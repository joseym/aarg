//! One module per CLI command, plus the error type that crosses the CLI
//! boundary.
//!
//! Commands return `Result<(), CliError>`; `main.rs` converts a
//! `CliError` into a `miette` diagnostic for display. This is the only
//! place in the codebase where module errors are gathered into one type —
//! everywhere below, errors stay module-specific.

pub mod attack;
pub mod config;
pub mod dataset;
pub mod gap;
pub mod history;
pub mod ingest;
pub mod init;
pub mod jd;
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

/// Load the config, fetch the stored API key, and build the provider
/// client — the preamble every LLM-backed command starts with. Extracted
/// once the third consumer appeared (ping, ingest, jd parse); the model
/// to use comes from the returned config.
pub(crate) async fn configured_client() -> Result<(AnthropicClient, Config), CliError> {
    let config = Config::load()?;
    let provider = config.provider;
    let key = secrets::load_api_key(provider.name())
        .await?
        .ok_or_else(|| LlmError::MissingApiKey {
            provider: provider.name().to_string(),
        })?;
    Ok((AnthropicClient::new(key), config))
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
}
