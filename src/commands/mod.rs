//! One module per CLI command, plus the error type that crosses the CLI
//! boundary.
//!
//! Commands return `Result<(), CliError>`; `main.rs` converts a
//! `CliError` into a `miette` diagnostic for display. This is the only
//! place in the codebase where module errors are gathered into one type —
//! everywhere below, errors stay module-specific.

pub mod config;
pub mod dataset;
pub mod gap;
pub mod ingest;
pub mod init;
pub mod jd;
pub mod ping;
pub mod tailor;

use std::path::{Path, PathBuf};

use crate::ats::AtsError;
use crate::builds::BuildError;
use crate::config::{Config, ConfigError};
use crate::dataset::DatasetError;
use crate::gap::GapError;
use crate::ingest::IngestError;
use crate::jd::JdError;
use crate::llm::{AnthropicClient, LlmError};
use crate::render::RenderError;
use crate::secrets::{self, SecretsError};
use crate::tailor::TailorError;

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
    #[diagnostic(help(
        "typst builds the PDF: install it with `cargo install typst-cli` or from https://github.com/typst/typst/releases; if it IS installed and compilation failed, the message above carries typst's own output"
    ))]
    Render(#[from] RenderError),

    #[error(transparent)]
    Ats(#[from] AtsError),

    #[error(transparent)]
    Build(#[from] BuildError),
}
