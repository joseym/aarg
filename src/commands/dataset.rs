//! `aarg dataset show|validate` — inspect and check the local dataset.
//!
//! Both commands are read-only and entirely deterministic: no LLM, no
//! network, no prompt. `show` answers "what does aarg know about me?";
//! `validate` answers "is that knowledge usable for tailoring?" and
//! exits nonzero when it isn't, so scripts and CI can gate on it.

use crate::commands::CliError;
use crate::dataset::store;
use crate::dataset::validate as validation;

pub async fn show() -> Result<(), CliError> {
    let dataset = store::load()?;

    println!(
        "dataset:  {} (schema v{})",
        store::dir()?.join("dataset.json").display(),
        dataset.schema_version
    );
    println!(
        "created:  {}  ·  updated: {}",
        dataset.metadata.created_at.format("%Y-%m-%d"),
        dataset.metadata.updated_at.format("%Y-%m-%d")
    );
    if !dataset.metadata.source_files.is_empty() {
        println!("sources:  {}", dataset.metadata.source_files.join(", "));
    }
    println!();

    let contact = &dataset.contact;
    let mut who = vec![contact.full_name.clone(), contact.email.clone()];
    if let Some(phone) = &contact.phone {
        who.push(phone.clone());
    }
    if let Some(location) = &contact.location {
        who.push(location.clone());
    }
    println!("{}", who.join(" · "));
    for link in &contact.links {
        println!("link:     {} {}", link.label, link.url);
    }
    if let Some(summary) = &dataset.summary {
        println!("summary:  {}", elide(summary, 100));
    }
    println!();

    println!("roles ({}):", dataset.roles.len());
    for role in &dataset.roles {
        let end = role
            .end
            .map_or_else(|| "present".to_string(), |ym| ym.to_string());
        println!(
            "  {} → {:7}  {} · {} ({} bullets)",
            role.start,
            end,
            role.title,
            role.company,
            role.bullets.len()
        );
    }
    println!();

    let verified = dataset.skills.skills.iter().filter(|s| s.verified).count();
    println!(
        "skills:   {} total · {} verified",
        dataset.skills.skills.len(),
        verified
    );
    println!(
        "also:     {} education · {} projects · {} certifications · {} achievements · \
         {} publications · {} languages · {} voice samples",
        dataset.education.len(),
        dataset.projects.len(),
        dataset.certifications.len(),
        dataset.achievements.len(),
        dataset.publications.len(),
        dataset.languages.len(),
        dataset.voice_samples.len()
    );
    Ok(())
}

pub async fn validate() -> Result<(), CliError> {
    let dataset = store::load()?;
    let report = validation::validate(&dataset);

    for finding in &report.problems {
        println!("problem: {}", finding.message);
    }
    for finding in &report.notes {
        println!("note: {}", finding.message);
    }

    if report.is_clean() {
        println!(
            "dataset is valid: {} skills, all evidence-backed",
            dataset.skills.skills.len()
        );
        Ok(())
    } else {
        println!(
            "{} problem(s), {} note(s)",
            report.problems.len(),
            report.notes.len()
        );
        Err(CliError::DatasetInvalid {
            problems: report.problems.len(),
        })
    }
}

/// First `max` characters of `text`, with an ellipsis when truncated.
fn elide(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max).collect();
        format!("{}…", cut.trim_end())
    }
}
