//! `aarg tune [build]` — change a finished build's resume in plain words.
//!
//! The conversational counterpart to the objection menu inside `tailor`: load a
//! saved build's canonical draft and let the person edit it by describing what
//! they want ("drop the intern bullet", "make the summary read more
//! conversational"). A router maps each request onto a grounded operation, so
//! nothing here writes a claim: a removal only deletes, a tone change runs the
//! same fact-guarded voice rewrite tailoring uses. On any change the tuned draft
//! is saved as the build's new canonical and the PDFs are re-rendered from it.
//!
//! The build's stored review score is from the original tailoring; tuning
//! re-renders the page but does not re-run the reviewer, so a fresh `aarg
//! tailor` is the way to re-score.

use crate::agent::AgentContext;
use crate::builds;
use crate::commands::{CliError, configured_client};
use crate::dataset::store;
use crate::style;
use crate::tailor::TailoredResume;
use crate::terminal::auto_user;
use crate::tune;

pub async fn run(build: Option<String>) -> Result<(), CliError> {
    // Tuning re-renders on any change, so fail fast if typst isn't installed.
    crate::render::ensure_available()?;
    let build = match build {
        Some(id) => id,
        None => match super::pick_build("pick a build to tune", "aarg tune 029").await? {
            Some(id) => id,
            None => return Ok(()),
        },
    };

    // Tuning is a back-and-forth; without a terminal there's nothing to drive
    // it, so say so rather than silently doing nothing.
    let user = auto_user();
    if !user.is_interactive() {
        eprintln!(
            "{}",
            style::warn("tune needs an interactive terminal (it's a conversation)")
        );
        return Ok(());
    }

    let mut canonical: TailoredResume = crate::history::read_artifact(&build, "canonical.json")?;
    let dataset = store::load()?;
    let (client, config) = configured_client().await?;
    let tracer = super::default_tracer()?;
    let ctx = AgentContext {
        llm: &client,
        model: &config.anthropic,
        tracer: &tracer,
        sink: None,
    };

    eprintln!("{}", style::section(format!("tuning build {build}")));
    let samples: Vec<String> = dataset
        .voice_samples
        .iter()
        .map(|s| s.text.clone())
        .collect();
    let (changed, _usage) = tune::run_session(
        &ctx,
        &mut canonical,
        user.as_ref(),
        &samples,
        super::tailor::session_style(),
    )
    .await;

    if !changed {
        eprintln!("{}", style::dim("no changes made"));
        return Ok(());
    }

    // Strip AI-tell dashes the same way the tailor finalize does, save the tuned
    // draft as the build's new canonical, then re-render the PDFs from it
    // (`render` reloads canonical.json and runs the full projection + vetting).
    crate::tailor::scrub_resume_text(&mut canonical);
    let build_dir = builds::builds_root()?.join(&build);
    builds::write_json(&build_dir, "canonical.json", &canonical)?;
    eprintln!("{}", style::done("saved your changes; re-rendering"));
    super::render::run(Some(build), false, None).await
}
