//! `aarg dataset show|validate|edit` — inspect, check, and hand-edit
//! the local dataset.
//!
//! `show` and `validate` are read-only and entirely deterministic: no
//! LLM, no network, no prompt. `show` answers "what does aarg know
//! about me?"; `validate` answers "is that knowledge usable for
//! tailoring?" and exits nonzero when it isn't, so scripts and CI can
//! gate on it. `edit` opens a *draft copy* in `$EDITOR` and only saves
//! through the store (lock + backup + atomic write) once the edited
//! JSON parses.

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

pub async fn edit() -> Result<(), CliError> {
    let dataset = store::load()?;
    let draft_path = store::dir()?.join("dataset.edit.json");

    // A leftover draft means a previous edit didn't survive parsing —
    // resume it instead of clobbering the user's work with a fresh copy.
    if draft_path.exists() {
        eprintln!("resuming your previous draft at {}", draft_path.display());
    } else {
        let json = serde_json::to_vec_pretty(&dataset).map_err(CliError::OutputJson)?;
        std::fs::write(&draft_path, json).map_err(|source| CliError::ReadInput {
            path: draft_path.clone(),
            source,
        })?;
    }

    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .map_err(|_| CliError::NoEditor)?;
    // $EDITOR may carry arguments ("code --wait"): first token is the
    // program, the rest pass through.
    let mut parts = editor.split_whitespace();
    let program = parts.next().ok_or(CliError::NoEditor)?;
    let status = std::process::Command::new(program)
        .args(parts)
        .arg(&draft_path)
        .status()
        .map_err(|source| CliError::EditorLaunch {
            editor: editor.clone(),
            source,
        })?;
    if !status.success() {
        return Err(CliError::EditorAborted { status });
    }

    let text = std::fs::read_to_string(&draft_path).map_err(|source| CliError::ReadInput {
        path: draft_path.clone(),
        source,
    })?;
    let mut edited: crate::dataset::ResumeDataset =
        serde_json::from_str(&text).map_err(|source| CliError::EditedJsonInvalid {
            path: draft_path.clone(),
            source,
        })?;

    // Hand-editing schema_version could lock the user out of their own
    // dataset on the next load; quietly keep it sane.
    if edited.schema_version != crate::dataset::SCHEMA_VERSION {
        eprintln!(
            "note: schema_version reset to {} (it tracks the file format, not your edits)",
            crate::dataset::SCHEMA_VERSION
        );
        edited.schema_version = crate::dataset::SCHEMA_VERSION;
    }
    edited.metadata.updated_at = chrono::Utc::now();

    // Validation problems are reported, not blocking: the user may be
    // mid-cleanup, and their valid-JSON edits are theirs to keep.
    let report = validation::validate(&edited);
    for finding in &report.problems {
        println!("problem: {}", finding.message);
    }
    for finding in &report.notes {
        println!("note: {}", finding.message);
    }

    store::save(&edited)?;
    let _ = std::fs::remove_file(&draft_path);
    println!(
        "dataset saved{} (previous version backed up to dataset.json.bak)",
        if report.problems.is_empty() {
            String::new()
        } else {
            format!(" with {} validation problem(s)", report.problems.len())
        }
    );
    Ok(())
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
