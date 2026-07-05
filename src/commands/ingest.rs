//! `aarg ingest <path>` — build the dataset from an existing resume.
//!
//! Reads the file, hands the text to `ingest::ingest_resume`, and saves
//! the result as the dataset. This module is deliberately thin: all the
//! extraction and assembly logic lives in `crate::ingest`, where the mock
//! client can test it.

use std::path::PathBuf;

use crate::agent::{AgentContext, ModelTier};
use crate::commands::{CliError, configured_client};
use crate::dataset::store;
use crate::ingest::ingest_resume;
use crate::style;

pub async fn run(path: PathBuf) -> Result<(), CliError> {
    // The client comes first because reading the input may need it: a `.txt`,
    // a text-layer `.pdf`, or `-` for stdin read deterministically, but an
    // image or a scanned PDF is transcribed by the model via `read_input`.
    let (client, config) = configured_client().await?;
    let tracer = super::default_tracer()?;
    let ctx = AgentContext {
        llm: &*client,
        model: config.active_resolver(),
        tracer: &tracer,
        sink: None,
    };
    let text = super::read_input(&path, &ctx).await?;

    eprintln!(
        "{}",
        style::info(format!(
            "ingesting {} with {}",
            path.display(),
            ctx.model.resolve("ingest_resume_v1", ModelTier::Cheap)
        ))
    );
    let mut outcome = ingest_resume(&ctx, &text).await?;
    outcome.dataset.metadata.source_files = vec![path.display().to_string()];

    let dataset_path = store::dir()?.join("dataset.json");
    let replacing = dataset_path.exists();
    store::save(&outcome.dataset)?;

    let d = &outcome.dataset;
    eprintln!("{}", style::section("Extracted"));
    eprintln!("  {}", style::bullet(format!("{} roles", d.roles.len())));
    eprintln!(
        "  {}",
        style::bullet(format!("{} education entries", d.education.len()))
    );
    eprintln!(
        "  {}",
        style::bullet(format!("{} skills", d.skills.skills.len()))
    );
    eprintln!(
        "  {}",
        style::bullet(format!("{} projects", d.projects.len()))
    );
    for warning in &outcome.warnings {
        eprintln!("{}", style::warn(warning));
    }
    eprintln!(
        "{}",
        style::success(format!("saved to {}", dataset_path.display()))
    );
    if replacing {
        eprintln!(
            "{}",
            style::info("the previous dataset was backed up to dataset.json.bak")
        );
    }

    // Onboarding: offer to capture a writing sample now so voice rewrites
    // have an anchor from the first build. The dataset is already saved
    // above, so a declined or interrupted capture never loses the ingest;
    // we persist again only if a sample was actually added. Interactive
    // only — a piped or CI run skips this silently.
    if super::voice::offer_onboarding_sample(&mut outcome.dataset)? {
        store::save(&outcome.dataset)?;
    }
    Ok(())
}
