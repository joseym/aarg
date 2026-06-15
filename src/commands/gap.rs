//! `aarg gap <jd>` — the three-bucket comparison of a job description
//! against the dataset.
//!
//! Accepts JD text (file or stdin) and parses it first, or — when the
//! argument is a `.json` file — reuses the output of
//! `aarg jd parse --json` directly, skipping that LLM call. Thin glue,
//! as always: the analysis lives in `crate::gap`.

use std::path::PathBuf;

use crate::agent::AgentContext;
use crate::commands::{CliError, configured_client, load_requirements};
use crate::dataset::store;
use crate::gap::{GapReport, Weakness, analyze_gap};
use crate::jd::{Importance, JobRequirements};

pub async fn run(path: PathBuf, json: bool) -> Result<(), CliError> {
    let dataset = store::load()?;
    let (client, config) = configured_client().await?;
    let tracer = super::default_tracer()?;
    let ctx = AgentContext {
        llm: &client,
        model: &config.anthropic,
        tracer: &tracer,
        sink: None,
    };

    let requirements = load_requirements(&path, &ctx).await?;

    eprintln!(
        "comparing against {} recorded skills...",
        dataset.skills.skills.len()
    );
    let report = analyze_gap(&ctx, &requirements, &dataset).await?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(CliError::OutputJson)?
        );
        return Ok(());
    }

    print_report(&requirements, &report);
    Ok(())
}

fn print_report(requirements: &JobRequirements, report: &GapReport) {
    let title = non_empty(&requirements.title, "untitled role");
    let company = non_empty(&requirements.company, "unnamed company");
    println!("gap analysis: {title} @ {company}");

    println!("\nmatched with evidence  {}", report.matched.len());
    for m in &report.matched {
        if m.semantic {
            println!(
                "  + {} -> {} ({})",
                m.jd_skill.name,
                m.dataset_name,
                importance_label(m.jd_skill.importance)
            );
        } else {
            println!(
                "  + {} ({})",
                m.dataset_name,
                importance_label(m.jd_skill.importance)
            );
        }
    }

    println!("\nclaimed but weak       {}", report.weak.len());
    for w in &report.weak {
        println!(
            "  ! {} ({}) - {}",
            w.matched.dataset_name,
            importance_label(w.matched.jd_skill.importance),
            weakness_label(w.weakness)
        );
    }

    println!("\nunknown                {}", report.unknown.len());
    for u in &report.unknown {
        println!("  ? {} ({})", u.name, importance_label(u.importance));
    }

    if !report.unknown.is_empty() || !report.weak.is_empty() {
        println!(
            "\nweak and unknown skills stay out of tailored resumes until they're backed by evidence"
        );
    }
}

fn non_empty<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.is_empty() { fallback } else { value }
}

fn importance_label(i: Importance) -> &'static str {
    match i {
        Importance::Critical => "critical",
        Importance::Required => "required",
        Importance::Preferred => "preferred",
    }
}

fn weakness_label(w: Weakness) -> &'static str {
    match w {
        Weakness::NoEvidence => "no evidence recorded",
        Weakness::LowProficiency => "proficiency is only familiar",
    }
}
