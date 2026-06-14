//! `aarg skills verify` — interview the user about unbacked skills and
//! write the evidence back.
//!
//! Thin glue around `crate::verify`: load, interview, save once. The
//! save only happens when the interview both finished and changed
//! something — an abandoned session leaves the file untouched, which
//! is the transactional half of the feature's promise.

use crate::agent::AgentContext;
use crate::commands::{CliError, configured_client};
use crate::dataset::store;
use crate::terminal::auto_user;
use crate::trace::Tracer;
use crate::verify::verify_unbacked;

pub async fn verify() -> Result<(), CliError> {
    let mut dataset = store::load()?;
    let user = auto_user();

    // The clarification guide is optional: offered when a provider is
    // configured, but the interview itself is keyless and deterministic.
    let provider = configured_client().await.ok();
    let tracer = Tracer::to_default_dir().ok();
    let ctx = match (&provider, &tracer) {
        (Some((client, config)), Some(tracer)) => Some(AgentContext {
            llm: client,
            model: &config.anthropic,
            tracer,
        }),
        _ => None,
    };

    let outcome = verify_unbacked(&mut dataset, user.as_ref(), ctx.as_ref()).await?;

    if outcome.changed() {
        dataset.metadata.updated_at = chrono::Utc::now();
        store::save(&dataset)?;
    }
    println!(
        "verified {} · removed {} · skipped {} · bullets added {}{}",
        outcome.verified,
        outcome.removed,
        outcome.skipped,
        outcome.bullets_added,
        if outcome.changed() {
            " · dataset saved (previous version backed up)"
        } else {
            " · dataset unchanged"
        }
    );
    Ok(())
}
