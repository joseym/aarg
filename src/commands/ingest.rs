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
use crate::trace::Tracer;

pub async fn run(path: PathBuf) -> Result<(), CliError> {
    if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
    {
        return Err(CliError::PdfInput { path });
    }
    let text = std::fs::read_to_string(&path).map_err(|source| CliError::ReadInput {
        path: path.clone(),
        source,
    })?;

    let (client, config) = configured_client().await?;
    let tracer = Tracer::to_default_dir()?;
    let ctx = AgentContext {
        llm: &client,
        model: &config.anthropic,
        tracer: &tracer,
        sink: None,
    };

    println!(
        "ingesting {} with {}...",
        path.display(),
        ctx.model.resolve("ingest_resume_v1", ModelTier::Cheap)
    );
    let mut outcome = ingest_resume(&ctx, &text).await?;
    outcome.dataset.metadata.source_files = vec![path.display().to_string()];

    let dataset_path = store::dir()?.join("dataset.json");
    let replacing = dataset_path.exists();
    store::save(&outcome.dataset)?;

    let d = &outcome.dataset;
    println!(
        "extracted {} roles, {} education entries, {} skills, {} projects",
        d.roles.len(),
        d.education.len(),
        d.skills.skills.len(),
        d.projects.len()
    );
    for warning in &outcome.warnings {
        println!("warning: {warning}");
    }
    println!("saved to {}", dataset_path.display());
    if replacing {
        println!("(the previous dataset was backed up to dataset.json.bak)");
    }
    Ok(())
}
