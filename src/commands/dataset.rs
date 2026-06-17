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
use crate::style;

pub async fn show() -> Result<(), CliError> {
    let dataset = store::load()?;

    // Human view on stderr (the stream the color helpers detect on); this
    // command has no machine-readable mode, so nothing is reserved for stdout.
    let width = 9;
    eprintln!("{}", style::section("Dataset"));
    eprintln!(
        "{}",
        style::kv(
            "file",
            format!(
                "{} {}",
                store::dir()?.join("dataset.json").display(),
                style::dim(format!("(schema v{})", dataset.schema_version))
            ),
            width
        )
    );
    eprintln!(
        "{}",
        style::kv(
            "created",
            format!(
                "{}  ·  updated {}",
                dataset.metadata.created_at.format("%Y-%m-%d"),
                dataset.metadata.updated_at.format("%Y-%m-%d")
            ),
            width
        )
    );
    if !dataset.metadata.source_files.is_empty() {
        eprintln!(
            "{}",
            style::kv("sources", dataset.metadata.source_files.join(", "), width)
        );
    }

    let contact = &dataset.contact;
    let mut who = vec![contact.full_name.clone(), contact.email.clone()];
    if let Some(phone) = &contact.phone {
        who.push(phone.clone());
    }
    if let Some(location) = &contact.location {
        who.push(location.clone());
    }
    eprintln!("{}", style::section("Contact"));
    eprintln!("  {}", style::bullet(who.join(" · ")));
    for link in &contact.links {
        eprintln!(
            "  {}",
            style::bullet(format!("{} {}", link.label, style::dim(&link.url)))
        );
    }
    if let Some(summary) = &dataset.summary {
        eprintln!("  {}", style::bullet(elide(summary, 100)));
    }

    eprintln!(
        "{}",
        style::section(format!("Roles ({})", dataset.roles.len()))
    );
    let width = style::term_width();
    for role in &dataset.roles {
        let end = role
            .end
            .map_or_else(|| "present".to_string(), |ym| ym.to_string());
        let span = style::dim(format!("{} → {:7}", role.start, end));
        let count = style::dim(format!("({} bullets)", role.bullets.len()));
        let who = format!("{} · {}", role.title, role.company);
        // Width-aware like `aarg history`: dates, title, and bullet count on
        // one line when it fits, else fold the title onto its own indented
        // line so a long title never wraps mid-word past the count.
        let one_line = format!("  {}", style::bullet(format!("{span}  {who} {count}")));
        if style::display_width(&one_line) <= width {
            eprintln!("{one_line}");
        } else {
            eprintln!("  {}", style::bullet(format!("{span}  {count}")));
            eprintln!("      {who}");
        }
    }

    let verified = dataset.skills.skills.iter().filter(|s| s.verified).count();
    eprintln!("{}", style::section("Counts"));
    eprintln!(
        "{}",
        style::kv(
            "skills",
            format!(
                "{} total · {} verified",
                dataset.skills.skills.len(),
                verified
            ),
            width
        )
    );
    eprintln!(
        "{}",
        style::kv(
            "also",
            format!(
                "{} education · {} projects · {} certifications · {} achievements · \
                 {} publications · {} languages · {} voice samples",
                dataset.education.len(),
                dataset.projects.len(),
                dataset.certifications.len(),
                dataset.achievements.len(),
                dataset.publications.len(),
                dataset.languages.len(),
                dataset.voice_samples.len()
            ),
            width
        )
    );
    Ok(())
}

pub async fn validate() -> Result<(), CliError> {
    let dataset = store::load()?;
    let report = validation::validate(&dataset);

    // Findings stay on stdout (one line each, script-friendly); the verdict
    // is the exit code. Restyled in place, stream unchanged.
    for finding in &report.problems {
        println!("{}", style::fail(format!("problem: {}", finding.message)));
    }
    for finding in &report.notes {
        println!("{}", style::info(format!("note: {}", finding.message)));
    }

    if report.is_clean() {
        println!(
            "{}",
            style::success(format!(
                "dataset is valid: {} skills, all evidence-backed",
                dataset.skills.skills.len()
            ))
        );
        Ok(())
    } else {
        println!(
            "{}",
            style::warn(format!(
                "{} problem(s), {} note(s)",
                report.problems.len(),
                report.notes.len()
            ))
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
        eprintln!(
            "{}",
            style::info(format!(
                "resuming your previous draft at {}",
                draft_path.display()
            ))
        );
    } else {
        let json = serde_json::to_vec_pretty(&dataset).map_err(CliError::OutputJson)?;
        std::fs::write(&draft_path, json).map_err(|source| CliError::ReadInput {
            path: draft_path.clone(),
            source,
        })?;
    }

    crate::commands::launch_editor(&draft_path)?;

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
            "{}",
            style::info(format!(
                "schema_version reset to {} (it tracks the file format, not your edits)",
                crate::dataset::SCHEMA_VERSION
            ))
        );
        edited.schema_version = crate::dataset::SCHEMA_VERSION;
    }
    edited.metadata.updated_at = chrono::Utc::now();

    // Validation problems are reported, not blocking: the user may be
    // mid-cleanup, and their valid-JSON edits are theirs to keep.
    let report = validation::validate(&edited);
    for finding in &report.problems {
        println!("{}", style::fail(format!("problem: {}", finding.message)));
    }
    for finding in &report.notes {
        println!("{}", style::info(format!("note: {}", finding.message)));
    }

    store::save(&edited)?;
    let _ = std::fs::remove_file(&draft_path);
    let qualifier = if report.problems.is_empty() {
        String::new()
    } else {
        format!(" with {} validation problem(s)", report.problems.len())
    };
    eprintln!(
        "{}",
        style::success(format!(
            "dataset saved{qualifier} {}",
            style::dim("(previous version backed up to dataset.json.bak)")
        ))
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
