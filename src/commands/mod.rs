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
pub mod cover;
pub mod dataset;
pub mod gap;
pub mod history;
pub mod ingest;
pub mod init;
pub mod jd;
pub mod key;
pub mod ping;
pub mod render;
pub mod roles;
pub mod skills;
pub mod tailor;
pub mod templates;
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
use crate::llm::{AnthropicClient, Auth, LlmError};
use crate::render::RenderError;
use crate::review::ReviewError;
use crate::secrets::{self, SecretsError};
use crate::style;
use crate::tailor::TailorError;
use crate::terminal::auto_user;
use crate::trace::TraceError;
use crate::user::{Answer, AskError, Question};
use crate::variant::{ClaimDivergence, VariantError};

/// Load the config, fetch the stored API key, and build the provider
/// client — the preamble every LLM-backed command starts with. Extracted
/// once the third consumer appeared (ping, ingest, jd parse); the model
/// to use comes from the returned config.
pub(crate) async fn configured_client() -> Result<(AnthropicClient, Config), CliError> {
    let config = Config::load()?;
    let provider = config.provider;

    // Headless path: a credential in the environment wins over the keychain,
    // so CI and containers (no keychain daemon, no interactive setup) just set
    // a var. OAuth takes precedence over an API key when both are present —
    // the same resolution the Anthropic SDK/CLI use. A `claude setup-token`
    // token goes in `ANTHROPIC_AUTH_TOKEN`.
    if let Some(token) = env_credential("ANTHROPIC_AUTH_TOKEN") {
        return Ok((AnthropicClient::with_auth(Auth::Oauth(token)), config));
    }
    if let Some(key) = env_credential("ANTHROPIC_API_KEY") {
        return Ok((AnthropicClient::with_auth(Auth::ApiKey(key)), config));
    }

    // Desktop path: which stored key to use — a one-off `AARG_KEY=<label>`
    // override wins (handy for scripts and the REPL without editing config),
    // else the configured active label.
    let override_label = std::env::var("AARG_KEY").ok();
    let label = override_label
        .as_deref()
        .unwrap_or_else(|| config.anthropic.active_label());

    // A CLI-delegated credential has no stored secret: fetch a fresh bearer
    // token from the official Anthropic CLI, which owns the OAuth refresh.
    if config.anthropic.kind_for(label) == crate::config::AuthKind::Cli {
        let token = fetch_cli_token()?;
        return Ok((AnthropicClient::with_auth(Auth::Oauth(token)), config));
    }

    let secret = match secrets::load_api_key(provider.name(), label).await? {
        Some(secret) => secret,
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
    // Send the secret the way its recorded kind expects (bearer for an OAuth
    // plan token, x-api-key otherwise). Legacy/untagged labels are API keys;
    // the `Cli` kind was handled above and never reaches here.
    let auth = match config.anthropic.kind_for(label) {
        crate::config::AuthKind::Oauth => Auth::Oauth(secret),
        crate::config::AuthKind::ApiKey | crate::config::AuthKind::Cli => Auth::ApiKey(secret),
    };
    Ok((AnthropicClient::with_auth(auth), config))
}

/// Fetch a fresh OAuth access token from the official Anthropic CLI for a
/// CLI-delegated credential. `ant auth print-credentials --access-token`
/// prints just the token and refreshes it if needed, so the official client
/// owns the OAuth + refresh — AARG only invokes it (no impersonation, no
/// stored secret). A missing `ant` or a logged-out profile is a clear,
/// actionable error rather than a failed request.
fn fetch_cli_token() -> Result<String, CliError> {
    let output = std::process::Command::new("ant")
        .args(["auth", "print-credentials", "--access-token"])
        .output()
        .map_err(CliError::CliTokenUnavailable)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(CliError::CliTokenFailed { stderr });
    }
    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if token.is_empty() {
        return Err(CliError::CliTokenFailed {
            stderr: "the CLI returned no token".to_string(),
        });
    }
    Ok(token)
}

/// A credential read from the environment, treating an empty value as absent
/// (an exported-but-empty var shouldn't authenticate with a blank secret).
fn env_credential(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|value| !value.is_empty())
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
    let requirements = if arg_str.starts_with("https://") || arg_str.starts_with("http://") {
        eprintln!("{}", style::dim(format!("fetching {arg_str}")));
        let text = crate::fetch::fetch_jd(&arg_str).await?;
        eprintln!(
            "{}",
            style::dim(format!(
                "parsing the posting with {}",
                ctx.model.resolve("jd_parser_v1", ModelTier::Cheap)
            ))
        );
        let mut requirements = crate::jd::parse_jd(ctx, &text).await?;
        requirements.source_url = Some(arg_str.into_owned());
        requirements
    } else if arg
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
    {
        let text = read_text_input(arg)?;
        serde_json::from_str(&text).map_err(|source| CliError::BadRequirementsJson {
            path: arg.to_path_buf(),
            source,
        })?
    } else {
        let text = read_text_input(arg)?;
        eprintln!(
            "{}",
            style::dim(format!(
                "parsing {} with {}",
                arg.display(),
                ctx.model.resolve("jd_parser_v1", ModelTier::Cheap)
            ))
        );
        crate::jd::parse_jd(ctx, &text).await?
    };
    // Remember every JD we resolve so the reuse picker can offer it later,
    // from any command — not just the ones that produced a build. Best-effort:
    // a cache write failure must not fail the command that's using the JD.
    let _ = crate::jdstore::remember(&requirements);
    Ok(requirements)
}

/// A job description from a past build, ready to reuse: the parsed
/// requirements plus a one-line label for the picker.
struct RecentJd {
    label: String,
    requirements: JobRequirements,
}

/// Resolve a job description interactively when a command was given none:
/// paste a new one as plain text, or reuse a JD from a past build (loaded
/// straight off disk — no model call). Returns `None` when there's no one to
/// ask (a piped/CI run) or the user backs out; in those cases it has printed
/// how to proceed, so the caller stops cleanly. The paste path parses the
/// text with the same cheap-tier call the file/URL paths use, which is why
/// this takes the agent context. Mirrors `attack`'s build picker, and keeps
/// scriptability intact: an agent always passes the JD explicitly.
pub(crate) async fn prompt_for_jd(
    ctx: &AgentContext<'_>,
) -> Result<Option<JobRequirements>, CliError> {
    let user = auto_user();
    // A piped/CI run can neither paste nor pick — point it at the explicit
    // forms rather than hanging on a prompt.
    if !user.is_interactive() {
        eprintln!(
            "{}",
            style::suggest(
                "pass a job description, e.g. `aarg tailor jd.txt` (a URL, `-` for stdin, or pasting one in a terminal all work)"
            )
        );
        return Ok(None);
    }

    // "Paste" is always the first option, so a fresh workspace with no builds
    // is still useful; the JDs of past builds follow, newest first.
    const PASTE: &str = "Paste a job description as plain text";
    let recent = recent_jds()?;
    let mut options = vec![PASTE.to_string()];
    options.extend(recent.iter().map(|jd| jd.label.clone()));

    let choice = match user
        .ask(Question::Select {
            prompt: "provide a job description".into(),
            options,
        })
        .await?
    {
        Answer::Choice(index) => index,
        _ => return Ok(None),
    };

    // Index 0 is the paste option; the rest map onto `recent`, shifted by one.
    let requirements = if choice == 0 {
        match paste_jd(ctx).await? {
            Some(requirements) => requirements,
            None => return Ok(None),
        }
    } else {
        match recent.into_iter().nth(choice - 1) {
            Some(jd) => jd.requirements,
            None => return Ok(None),
        }
    };
    // Remember the chosen JD: a fresh paste so it's offered next time, a
    // reused one to bump it back to the top (the store dedups, so this is
    // idempotent). Best-effort — a cache miss must not fail the command.
    let _ = crate::jdstore::remember(&requirements);
    Ok(Some(requirements))
}

/// The editor template a JD paste opens with; the comment block is stripped.
const JD_PASTE_TEMPLATE: &str = "\
# Paste the job description below, then save and quit.
# Lines in this leading block (starting with #) are ignored.

";

/// Capture a job description pasted as plain text (via an editor or stdin,
/// see `capture_free_text`) and parse it into requirements. An empty paste
/// is "never mind" (`None`), not an error.
async fn paste_jd(ctx: &AgentContext<'_>) -> Result<Option<JobRequirements>, CliError> {
    let text = capture_free_text(
        "jd.paste.txt",
        JD_PASTE_TEMPLATE,
        "Paste the job description, then press Ctrl-D on a blank line to finish:",
    )?;
    if text.is_empty() {
        eprintln!(
            "{}",
            style::warn("nothing pasted · run the command again to try once more")
        );
        return Ok(None);
    }
    eprintln!(
        "{}",
        style::dim(format!(
            "parsing the pasted job description with {}",
            ctx.model.resolve("jd_parser_v1", ModelTier::Cheap)
        ))
    );
    Ok(Some(crate::jd::parse_jd(ctx, &text).await?))
}

/// The distinct JDs to offer for reuse, newest first. Two sources, in order:
/// the JD store (everything entered in any command since it existed), then
/// any older builds' `jd.json` from before the store — so existing builds
/// stay pickable. Deduped by identity, so a JD entered and then tailored
/// shows once.
fn recent_jds() -> Result<Vec<RecentJd>, CliError> {
    // Source 1: the JD store. An unreadable store just contributes nothing —
    // it's a convenience cache, never a reason to fail the picker.
    let stored = crate::jdstore::recent()
        .unwrap_or_default()
        .into_iter()
        .map(|entry| RecentJd {
            label: format!(
                "{}  ·  {} (entered)",
                jd_role_label(&entry.requirements),
                entry.saved_at.format("%Y-%m-%d %H:%M")
            ),
            requirements: entry.requirements,
        });

    // Source 2: builds made before the store existed. `history::list` is
    // newest-first; skip builds whose `jd.json` is gone or unreadable.
    let from_builds = crate::history::list()?.into_iter().filter_map(|build| {
        let jd = crate::history::read_artifact::<JobRequirements>(&build.id, "jd.json").ok()?;
        Some(RecentJd {
            label: format!(
                "{}  ·  {} (build {})",
                jd_role_label(&jd),
                build.created_at,
                build.id
            ),
            requirements: jd,
        })
    });

    Ok(dedup_jds(stored.chain(from_builds)))
}

/// Collapse a newest-first stream of candidate JDs to the distinct ones,
/// keeping the first (newest) occurrence of each identity. Pure, so the
/// dedup is unit-testable without touching the store or the builds dir.
fn dedup_jds(items: impl IntoIterator<Item = RecentJd>) -> Vec<RecentJd> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for item in items {
        // The same posting re-entered or re-tailored is one entry; two
        // postings that merely share a title are two.
        if seen.insert(item.requirements.identity_key()) {
            out.push(item);
        }
    }
    out
}

/// A JD's "Title @ Company" label, with gentle fallbacks for a posting whose
/// title or company didn't parse.
fn jd_role_label(jd: &JobRequirements) -> String {
    let title = if jd.title.is_empty() {
        "untitled role"
    } else {
        jd.title.as_str()
    };
    let company = if jd.company.is_empty() {
        "unnamed company"
    } else {
        jd.company.as_str()
    };
    format!("{title} @ {company}")
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

/// Resolve which saved build a command should act on when none was named:
/// list the builds and let a person pick one. Returns the chosen id, or
/// `None` when there's nothing to pick (no builds) or no one to ask (a
/// piped/CI run); in both cases it has printed how to proceed, so the caller
/// should stop cleanly. `prompt` titles the picker; `example` is the
/// explicit-form hint shown to a non-interactive run. Shared by `aarg attack`
/// and `aarg cover` — its second consumer is what pulled it out of `attack`.
pub(crate) async fn pick_build(prompt: &str, example: &str) -> Result<Option<String>, CliError> {
    let user = auto_user();
    let builds = crate::history::list()?;
    if builds.is_empty() {
        eprintln!(
            "{}",
            style::suggest("no builds yet · run `aarg tailor <jd>`")
        );
        return Ok(None);
    }
    // A piped/CI run can't answer a picker; point it at the explicit form.
    if !user.is_interactive() {
        eprintln!(
            "{}",
            style::suggest(format!("specify a build id, e.g. `{example}`"))
        );
        return Ok(None);
    }
    // One readable line per build, newest first (the order `list` returns).
    let options: Vec<String> = builds
        .iter()
        .map(|b| {
            format!(
                "{}  {:.2}  {}  {} · {} obj",
                b.id, b.score, b.target, b.created_at, b.objections
            )
        })
        .collect();
    match user
        .ask(Question::Select {
            prompt: prompt.to_string(),
            options,
        })
        .await?
    {
        Answer::Choice(i) => Ok(builds.get(i).map(|b| b.id.clone())),
        _ => Ok(None),
    }
}

/// Capture a block of free text from the user: an editor when one is
/// available (an interactive terminal with `$EDITOR`/`$VISUAL` set),
/// otherwise stdin — a piped file, or an interactive paste ended with
/// Ctrl-D. `scratch_name` is the throwaway file opened in the editor (under
/// the dataset dir), `editor_header` the instructional comment block it
/// opens with (stripped from the result), and `stdin_hint` the line shown
/// before an interactive stdin read. Returns the trimmed text. Extracted at
/// its second consumer: `voice add` and the JD paste flow both take a blob
/// of the user's own prose this way. The editor is preferred when present
/// because it handles a multi-line paste cleanly and, inside the REPL,
/// avoids reading the shared stdin out from under reedline.
pub(crate) fn capture_free_text(
    scratch_name: &str,
    editor_header: &str,
    stdin_hint: &str,
) -> Result<String, CliError> {
    use std::io::{IsTerminal, Read};

    let interactive = std::io::stdin().is_terminal();
    let raw = if interactive && editor_available() {
        let path = crate::dataset::store::dir()?.join(scratch_name);
        std::fs::write(&path, editor_header).map_err(|source| CliError::ReadInput {
            path: path.clone(),
            source,
        })?;
        launch_editor(&path)?;
        let raw = std::fs::read_to_string(&path).map_err(|source| CliError::ReadInput {
            path: path.clone(),
            source,
        })?;
        let _ = std::fs::remove_file(&path);
        strip_comment_header(&raw)
    } else {
        // The hint is the fix for "I pasted and nothing happened" — stdin
        // returns on EOF (Ctrl-D), not Enter.
        if interactive {
            eprintln!("{}", style::info(stdin_hint));
        }
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|source| CliError::ReadInput {
                path: "<stdin>".into(),
                source,
            })?;
        text
    };
    Ok(raw.trim().to_string())
}

/// Drop a leading block of `#` comment lines (an editor template) plus the
/// blank lines before the body, keeping the rest verbatim — including any
/// `#` lines inside the body itself. Pure, so it's unit-tested directly.
fn strip_comment_header(text: &str) -> String {
    match text
        .lines()
        .position(|line| !line.trim_start().starts_with('#') && !line.trim().is_empty())
    {
        Some(start) => text.lines().skip(start).collect::<Vec<_>>().join("\n"),
        None => String::new(), // nothing but the header / blanks
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
    Llm(LlmError),

    #[error(transparent)]
    #[diagnostic(help(
        "you're rate-limited, not misconfigured: wait a bit and retry, or switch to another credential with `aarg key use <label>` (a pay-as-you-go API key has separate capacity from a subscription)"
    ))]
    RateLimited(LlmError),

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
    #[diagnostic(help("the model's cover-letter output didn't parse; re-running usually helps"))]
    Cover(#[from] crate::cover::CoverLetterError),

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

    #[error(transparent)]
    Template(#[from] crate::templates::TemplateError),

    #[error("no template named {name:?}")]
    #[diagnostic(help("run `aarg templates list` to see the available templates"))]
    UnknownTemplate { name: String },

    #[error("could not run the Anthropic CLI (`ant`) to fetch a token")]
    #[diagnostic(help(
        "this credential is delegated to the official CLI: install it (https://github.com/anthropics/anthropic-cli) and run `ant auth login`"
    ))]
    CliTokenUnavailable(#[source] std::io::Error),

    #[error("the Anthropic CLI could not provide a token:\n{stderr}")]
    #[diagnostic(help("run `ant auth login` to sign in, then try again"))]
    CliTokenFailed { stderr: String },
}

/// Route a transport error to the right diagnostic: an HTTP 429 is a rate
/// limit (wait, or switch credentials), everything else is the generic LLM
/// path (check the key/model). Hand-written instead of `#[from]` so the
/// boundary can branch on the kind, while `?` on an `LlmError` still works.
impl From<LlmError> for CliError {
    fn from(error: LlmError) -> Self {
        if error.is_rate_limited() {
            CliError::RateLimited(error)
        } else {
            CliError::Llm(error)
        }
    }
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
        SkillsCommand, TemplatesCommand, TraceCommand, VoiceCommand,
    };
    match command {
        Command::Init { global, dir } => init::run(global, dir).await?,
        Command::Config => config::run().await?,
        Command::Key {
            command: KeyCommand::List,
        } => key::list().await?,
        Command::Key {
            command: KeyCommand::Add { label, oauth, cli },
        } => key::add(label, oauth, cli).await?,
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
            cover,
        } => tailor::run(jd, variant.variants(), template, cover).await?,
        Command::Cover { build } => cover::run(build).await?,
        Command::Render {
            build,
            no_llm,
            template,
        } => render::run(build, no_llm, template).await?,
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
        Command::Templates {
            command: TemplatesCommand::List,
        } => templates::list().await?,
        Command::Templates {
            command: TemplatesCommand::Use { name },
        } => templates::use_template(name).await?,
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::jd::{RemotePolicy, Seniority};

    /// A minimal JD; only the fields the picker keys and labels on matter.
    fn req(company: &str, title: &str, source: Option<&str>) -> JobRequirements {
        JobRequirements {
            company: company.to_string(),
            title: title.to_string(),
            seniority: Seniority::Unspecified,
            location: None,
            remote: RemotePolicy::Unspecified,
            domain_keywords: Vec::new(),
            required_skills: Vec::new(),
            preferred_skills: Vec::new(),
            responsibilities: Vec::new(),
            ats_phrases: Vec::new(),
            raw_text: String::new(),
            source_url: source.map(str::to_string),
        }
    }

    /// A candidate as it reaches `dedup_jds`: the label is opaque to dedup
    /// (identity is keyed off the requirements), so the test labels are just
    /// markers to assert which copy survived.
    fn candidate(label: &str, company: &str, title: &str, source: Option<&str>) -> RecentJd {
        RecentJd {
            label: label.to_string(),
            requirements: req(company, title, source),
        }
    }

    #[test]
    fn dedup_keeps_the_newest_copy_of_each_distinct_jd() {
        // Newest-first. The older Acme entry is the same JD as the newest, so
        // it's dropped and the newest copy's label survives.
        let items = vec![
            candidate("acme-newest", "Acme", "Staff Engineer", None),
            candidate("globex", "Globex", "Eng Manager", None),
            candidate("acme-older", "Acme", "Staff Engineer", None),
        ];
        let out = dedup_jds(items);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].label, "acme-newest");
        assert_eq!(out[1].label, "globex");
    }

    #[test]
    fn same_role_from_different_postings_stays_distinct() {
        // Same company and title, but two different source URLs: two real
        // postings, so both are offered.
        let items = vec![
            candidate("a2", "Acme", "Engineer", Some("https://acme/2")),
            candidate("a1", "Acme", "Engineer", Some("https://acme/1")),
        ];
        assert_eq!(dedup_jds(items).len(), 2);
    }

    #[test]
    fn an_empty_history_yields_no_jds() {
        assert!(dedup_jds(Vec::new()).is_empty());
    }

    #[test]
    fn strip_comment_header_drops_the_template_but_keeps_body_hashes() {
        let raw = "# instructions\n# more\n\nThe real text.\n# a heading I wrote\nmore text";
        assert_eq!(
            strip_comment_header(raw),
            "The real text.\n# a heading I wrote\nmore text"
        );
        // A buffer that is only the header (or blanks) yields nothing.
        assert_eq!(strip_comment_header("# only\n# comments\n\n"), "");
    }

    #[test]
    fn a_rate_limit_routes_to_its_own_diagnostic() {
        let limited: CliError = LlmError::Api {
            status: 429,
            kind: "rate_limit_error".into(),
            message: "slow down".into(),
        }
        .into();
        assert!(matches!(limited, CliError::RateLimited(_)));
        // Anything else stays on the generic key/model path.
        let other: CliError = LlmError::Api {
            status: 401,
            kind: "authentication_error".into(),
            message: "bad key".into(),
        }
        .into();
        assert!(matches!(other, CliError::Llm(_)));
    }
}
