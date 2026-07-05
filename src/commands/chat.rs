//! `aarg chat [jd]` - an interactive Q/A about a job posting and how your
//! recorded background fits it.
//!
//! Thin wiring, like the other LLM commands: resolve the JD (a path/URL/stdin,
//! or a recent one picked interactively), load the dataset, and hand both to
//! the read-only chat loop in `crate::jdchat`. Interactive-only: a piped/CI run
//! is pointed elsewhere rather than left hanging on a prompt.

use std::path::PathBuf;

use crate::agent::AgentContext;
use crate::commands::{CliError, configured_client, load_requirements};
use crate::style;

pub async fn run(path: Option<PathBuf>) -> Result<(), CliError> {
    let (client, config) = configured_client().await?;
    let tracer = super::default_tracer()?;
    let ctx = AgentContext {
        llm: &*client,
        model: config.active_resolver(),
        tracer: &tracer,
        sink: None,
    };

    let user = crate::terminal::auto_user();
    if !user.is_interactive() {
        eprintln!(
            "{}",
            style::suggest("`aarg chat` is interactive; run it in a terminal")
        );
        return Ok(());
    }

    // A path is parsed (file/URL/stdin); with none, offer a recent JD to pick.
    let requirements = match path {
        Some(p) => load_requirements(&p, &ctx).await?,
        None => match super::prompt_for_jd(&ctx).await? {
            Some(requirements) => requirements,
            None => return Ok(()), // prompt_for_jd already said how to proceed
        },
    };

    let dataset = crate::dataset::store::load()?;
    crate::jdchat::chat(&requirements, &dataset, user.as_ref(), &ctx).await?;
    Ok(())
}
