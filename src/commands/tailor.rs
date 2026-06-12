//! `aarg tailor <jd>` — the whole Phase 1 pipeline in one command:
//! parse the JD (or reuse `jd parse --json` output), analyze the gap,
//! tailor the canonical resume, render the ATS PDF, and report keyword
//! coverage. Every intermediate artifact lands in a numbered build
//! directory, so a run can be inspected and reproduced.
//!
//! This function is the embryo of the PRD's Orchestrator: in Phase 1 it
//! is honest sequential glue; the adversarial loop (Phase 3) grows
//! around exactly this sequence.

use std::path::PathBuf;

use chrono::Utc;

use crate::ats::{self, EvidenceStatus, KeywordKind};
use crate::builds::{self, BuildMeta};
use crate::commands::{CliError, configured_client, load_requirements};
use crate::dataset::store;
use crate::gap::analyze_gap;
use crate::render;
use crate::tailor::{JdId, tailor_resume};

pub async fn run(path: PathBuf) -> Result<(), CliError> {
    let dataset = store::load()?;
    let (client, config) = configured_client().await?;
    let model = &config.anthropic.model;

    let requirements = load_requirements(&path, &client, model).await?;

    eprintln!("analyzing the gap...");
    let gap = analyze_gap(&client, model, &requirements, &dataset).await?;

    let build = builds::create_next()?;
    let jd_id = JdId(slug(&requirements.company, &requirements.title));
    eprintln!(
        "build {}: tailoring for {} @ {} with {model}...",
        build.id.0, requirements.title, requirements.company
    );
    let outcome = tailor_resume(
        &client,
        model,
        build.id.clone(),
        jd_id,
        &requirements,
        &dataset,
        &gap,
    )
    .await?;
    for warning in &outcome.warnings {
        eprintln!("warning: {warning}");
    }

    builds::write_json(&build.dir, "jd.json", &requirements)?;
    builds::write_json(&build.dir, "gap_report.json", &gap)?;
    builds::write_json(&build.dir, "canonical.json", &outcome.resume)?;

    eprintln!("rendering with typst...");
    let pdf = render::render_ats(&build.dir, &outcome.resume)?;

    let page_text = ats::extract_pdf_text(&pdf)?;
    let report = ats::keyword_coverage(&requirements, &gap, &page_text);
    builds::write_json(&build.dir, "ats_report.json", &report)?;
    builds::write_json(
        &build.dir,
        "meta.json",
        &BuildMeta {
            created_at: Utc::now(),
            model: model.clone(),
            template: "ats/classic".into(),
            tailor_usage: outcome.usage,
        },
    )?;

    print_coverage(&report);
    println!("\nsaved build {}:", build.id.0);
    println!("  {}", pdf.display());
    println!(
        "tokens: {} in, {} out",
        outcome.usage.input_tokens, outcome.usage.output_tokens
    );
    Ok(())
}

fn print_coverage(report: &crate::ats::AtsReport) {
    let required_total = report
        .keyword_hits
        .iter()
        .map(|h| h.kind)
        .chain(report.keyword_misses.iter().map(|m| m.kind))
        .filter(|k| *k == KeywordKind::RequiredSkill)
        .count();
    let required_hits = report
        .keyword_hits
        .iter()
        .filter(|h| h.kind == KeywordKind::RequiredSkill)
        .count();

    println!(
        "\nkeyword coverage: {required_hits}/{required_total} required ({:.0}%)",
        report.coverage * 100.0
    );
    for miss in &report.keyword_misses {
        match &miss.evidence {
            EvidenceStatus::Backed { dataset_skill } => println!(
                "  miss: {:?} ({}) - backed by {:?}; a revision could mirror it",
                miss.phrase,
                kind_label(miss.kind),
                dataset_skill
            ),
            EvidenceStatus::Unbacked => println!(
                "  miss: {:?} ({}) - no supporting evidence in the dataset; not inserted",
                miss.phrase,
                kind_label(miss.kind)
            ),
        }
    }
}

fn kind_label(kind: KeywordKind) -> &'static str {
    match kind {
        KeywordKind::RequiredSkill => "required skill",
        KeywordKind::PreferredSkill => "preferred skill",
        KeywordKind::AtsPhrase => "ats phrase",
    }
}

/// "amplo" + "Senior Engineering Manager" -> "amplo-senior-engineering-manager"
fn slug(company: &str, title: &str) -> String {
    let mut out = String::new();
    for c in format!("{company} {title}").chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_lowercase_hyphenated_and_trimmed() {
        assert_eq!(
            slug("amplo", "Senior Engineering Manager"),
            "amplo-senior-engineering-manager"
        );
        assert_eq!(
            slug("Acme, Inc.", "Staff Engineer (L6)!"),
            "acme-inc-staff-engineer-l6"
        );
        assert_eq!(slug("", ""), "");
    }
}
