//! `aarg skills add [name]` — record one skill the user names and back it
//! with evidence. `aarg skills verify` — interview the user about unbacked
//! skills and write the evidence back. `aarg skills dedup` — collapse
//! redundant skills the dataset accumulated.
//!
//! Thin glue around `crate::verify`: load, interview, save once. The
//! save only happens when the interview both finished and changed
//! something — an abandoned session leaves the file untouched, which
//! is the transactional half of the feature's promise.

use crate::agent::AgentContext;
use crate::commands::{CliError, configured_client};
use crate::dataset::store;
use crate::dataset::types::{SkillCategory, SkillId};
use crate::style;
use crate::terminal::auto_user;
use crate::user::{Answer, Question, UserHandle};
use crate::verify::{add_one_skill, dedup_skills, remove_skills, verify_unbacked};

/// `aarg skills add [name]` — record a skill you have and back it with
/// evidence in one interview. The name comes from the argument or a prompt;
/// a brand-new skill takes a category (from `--category`, an interactive
/// pick, or a neutral default), while a name that already resolves gains the
/// new evidence rather than a duplicate. Either way you point it at a real
/// role and write one line about what you did — polished into resume wording
/// when a provider is configured, never inflated. Saves once, only when the
/// interview added something; an abandoned session leaves the file untouched.
pub async fn add(name: Option<String>, category: Option<String>) -> Result<(), CliError> {
    let mut dataset = store::load()?;
    let user = auto_user();

    // The name: the argument, or a prompt. A blank prompt cancels.
    let name = match name {
        Some(n) if !n.trim().is_empty() => n.trim().to_string(),
        _ => match user
            .ask(Question::Text {
                prompt: "skill to add (e.g. \"TypeScript\")".into(),
            })
            .await?
        {
            Answer::Text(t) if !t.trim().is_empty() => t.trim().to_string(),
            _ => {
                eprintln!("{}", style::dim("no skill named - nothing added"));
                return Ok(());
            }
        },
    };

    // A brand-new skill needs a category; an existing one keeps its own, so
    // the value is only consulted when the name doesn't already resolve.
    let exists = dataset.skills.aliases.contains_key(&name.to_lowercase())
        || dataset
            .skills
            .skills
            .iter()
            .any(|s| s.canonical_name.eq_ignore_ascii_case(&name));
    let category = if exists {
        SkillCategory::Hard // unused: add_one_skill reuses the existing skill
    } else {
        resolve_category(category, user.as_ref()).await?
    };

    // The polish guide is optional: offered when a provider is configured,
    // but the interview itself is keyless and deterministic.
    let provider = configured_client().await.ok();
    let tracer = super::default_tracer().ok();
    let ctx = match (&provider, &tracer) {
        (Some((client, config)), Some(tracer)) => Some(AgentContext {
            llm: &**client,
            model: config.active_resolver(),
            tracer,
            sink: None,
        }),
        _ => None,
    };

    let outcome = add_one_skill(&mut dataset, &name, category, user.as_ref(), ctx.as_ref()).await?;

    if outcome.changed() {
        dataset.metadata.updated_at = chrono::Utc::now();
        store::save(&dataset)?;
        eprintln!(
            "{}",
            style::success(format!(
                "recorded {name} {}",
                style::dim("· dataset saved (previous version backed up)")
            ))
        );
    } else {
        eprintln!("{}", style::info("nothing recorded · dataset unchanged"));
    }
    Ok(())
}

/// Decide a new skill's category: the `--category` flag if given (parsed
/// leniently), an interactive pick when a person is driving, or a neutral
/// default for a piped/CI run that named neither.
async fn resolve_category(
    flag: Option<String>,
    user: &dyn UserHandle,
) -> Result<SkillCategory, CliError> {
    if let Some(raw) = flag {
        if let Some(category) = parse_category(&raw) {
            return Ok(category);
        }
        eprintln!(
            "{}",
            style::warn(format!(
                "unknown category {raw:?} - valid: hard, soft, domain, tool, language, framework"
            ))
        );
    }
    if !user.is_interactive() {
        return Ok(SkillCategory::Hard);
    }
    let labels = ["hard", "soft", "domain", "tool", "language", "framework"];
    let choice = match user
        .ask(Question::Select {
            prompt: "what kind of skill is it?".into(),
            options: labels.iter().map(|s| (*s).to_string()).collect(),
        })
        .await?
    {
        Answer::Choice(i) if i < labels.len() => i,
        _ => 0,
    };
    Ok(parse_category(labels[choice]).unwrap_or(SkillCategory::Hard))
}

/// Map a category word to its `SkillCategory`, case-insensitively. `None`
/// for anything unrecognized, so the caller can warn and fall back.
fn parse_category(raw: &str) -> Option<SkillCategory> {
    match raw.trim().to_lowercase().as_str() {
        "hard" => Some(SkillCategory::Hard),
        "soft" => Some(SkillCategory::Soft),
        "domain" => Some(SkillCategory::Domain),
        "tool" => Some(SkillCategory::Tool),
        "language" => Some(SkillCategory::Language),
        "framework" => Some(SkillCategory::Framework),
        _ => None,
    }
}

pub async fn verify() -> Result<(), CliError> {
    let mut dataset = store::load()?;
    let user = auto_user();

    // The clarification guide is optional: offered when a provider is
    // configured, but the interview itself is keyless and deterministic.
    let provider = configured_client().await.ok();
    let tracer = super::default_tracer().ok();
    let ctx = match (&provider, &tracer) {
        (Some((client, config)), Some(tracer)) => Some(AgentContext {
            llm: &**client,
            model: config.active_resolver(),
            tracer,
            sink: None,
        }),
        _ => None,
    };

    let outcome = verify_unbacked(&mut dataset, user.as_ref(), ctx.as_ref()).await?;

    if outcome.changed() {
        dataset.metadata.updated_at = chrono::Utc::now();
        store::save(&dataset)?;
    }
    let tail = if outcome.changed() {
        style::dim("· dataset saved (previous version backed up)")
    } else {
        style::dim("· dataset unchanged")
    };
    eprintln!(
        "{}",
        style::success(format!(
            "verified {} · removed {} · skipped {} · bullets added {} {tail}",
            outcome.verified, outcome.removed, outcome.skipped, outcome.bullets_added,
        ))
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
    if !pruned.is_empty() {
        eprintln!("{}", style::section(format!("Pruned ({})", pruned.len())));
        for p in &pruned {
            eprintln!(
                "  {}",
                style::bullet(format!(
                    "removed {:?} {}",
                    p.removed,
                    style::dim(format!("· covered by {:?}", p.kept))
                ))
            );
        }
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
        eprintln!(
            "{}",
            style::success(format!(
                "removed {total} skill(s) {}",
                style::dim("· dataset saved (previous version backed up)")
            ))
        );
    } else {
        eprintln!(
            "{}",
            style::info("no redundant skills found · dataset unchanged")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_words_parse_case_insensitively() {
        assert_eq!(parse_category("language"), Some(SkillCategory::Language));
        assert_eq!(
            parse_category("  Framework "),
            Some(SkillCategory::Framework)
        );
        assert_eq!(parse_category("TOOL"), Some(SkillCategory::Tool));
        assert_eq!(parse_category("hard"), Some(SkillCategory::Hard));
        // Anything unrecognized is None, so the caller warns and defaults.
        assert_eq!(parse_category("wizardry"), None);
        assert_eq!(parse_category(""), None);
    }
}
