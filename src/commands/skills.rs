//! `aarg skills verify` — interview the user about unbacked skills and
//! write the evidence back. `aarg skills dedup` — collapse redundant
//! skills the dataset accumulated.
//!
//! Thin glue around `crate::verify`: load, interview, save once. The
//! save only happens when the interview both finished and changed
//! something — an abandoned session leaves the file untouched, which
//! is the transactional half of the feature's promise.

use crate::agent::AgentContext;
use crate::commands::{CliError, configured_client};
use crate::dataset::store;
use crate::dataset::types::SkillId;
use crate::terminal::auto_user;
use crate::trace::Tracer;
use crate::user::{Answer, Question};
use crate::verify::{dedup_skills, remove_skills, verify_unbacked};

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

/// `aarg skills dedup` — collapse redundant skills. A deterministic pass
/// removes exact and token-subset duplicates ("remote-first" under
/// "Remote-First Communication"); then, when a person is driving, a manual
/// pass lets them pick off the synonym near-duplicates the automatic pass
/// can't safely judge. One save on success (the store backs up the prior
/// version), so an abandoned manual pass after auto-removals still records
/// the safe part.
pub async fn dedup() -> Result<(), CliError> {
    let mut dataset = store::load()?;

    let pruned = dedup_skills(&mut dataset);
    for p in &pruned {
        println!("removed {:?} — covered by {:?}", p.removed, p.kept);
    }

    // Manual pass: the synonym clusters (e.g. "operational excellence" vs
    // "engineering excellence") aren't token-subsets, so only the user can
    // say which to drop. Offered only interactively; a piped/CI run keeps
    // the deterministic result and nothing more.
    let mut manual = 0;
    let user = auto_user();
    if user.is_interactive() && !dataset.skills.skills.is_empty() {
        let names: Vec<String> = dataset
            .skills
            .skills
            .iter()
            .map(|s| s.canonical_name.clone())
            .collect();
        let answer = user
            .ask(Question::MultiSelect {
                prompt: "select any further redundant skills to remove (space toggles, enter confirms; the previous dataset is backed up)".into(),
                options: names,
            })
            .await?;
        if let Answer::Choices(indexes) = answer {
            // Resolve indexes to ids up front — removal shifts positions.
            let ids: Vec<SkillId> = indexes
                .iter()
                .filter_map(|&i| dataset.skills.skills.get(i).map(|s| s.id.clone()))
                .collect();
            manual = ids.len();
            remove_skills(&mut dataset, &ids);
        }
    }

    let total = pruned.len() + manual;
    if total > 0 {
        dataset.metadata.updated_at = chrono::Utc::now();
        store::save(&dataset)?;
        println!("removed {total} skill(s) · dataset saved (previous version backed up)");
    } else {
        println!("no redundant skills found · dataset unchanged");
    }
    Ok(())
}
