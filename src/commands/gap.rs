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
use crate::style::{self, Spinner};

pub async fn run(jd: Option<PathBuf>, json: bool) -> Result<(), CliError> {
    let dataset = store::load()?;
    let (client, config) = configured_client().await?;
    let tracer = super::default_tracer()?;
    let ctx = AgentContext {
        llm: &*client,
        model: config.active_resolver(),
        tracer: &tracer,
        sink: None,
    };

    // A JD argument is parsed (file/URL/stdin); with none, offer the JDs from
    // past builds to reuse, loaded off disk. A picker that returns nothing (no
    // past builds, or a piped/CI run) has already said how to proceed.
    let requirements = match &jd {
        Some(path) => load_requirements(path, &ctx).await?,
        None => match super::prompt_for_jd(&ctx).await? {
            Some(requirements) => requirements,
            None => return Ok(()),
        },
    };

    let sp = Spinner::start(format!(
        "comparing against {} recorded skills",
        dataset.skills.skills.len()
    ));
    let report = analyze_gap(&ctx, &requirements, &dataset).await?;
    // Clear the spinner without a line of its own — the report below opens
    // with its own header.
    sp.clear();

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
    // Human report on stderr (the stream the color helpers detect on); the
    // `--json` form above is the machine output and stays on stdout.
    eprintln!(
        "\n{}",
        style::bold(format!("Gap analysis · {title} @ {company}"))
    );

    // ✓ matched: a JD skill the dataset can back. A semantic match shows how
    // the JD's wording maps to your recorded skill.
    eprintln!(
        "{}",
        style::section(format!("Matched with evidence ({})", report.matched.len()))
    );
    for m in &report.matched {
        let imp = style::dim(format!("({})", importance_label(m.jd_skill.importance)));
        if m.semantic {
            eprintln!(
                "  {}",
                style::success(format!("{} → {}  {imp}", m.jd_skill.name, m.dataset_name))
            );
        } else {
            eprintln!("  {}", style::success(format!("{}  {imp}", m.dataset_name)));
        }
    }

    // ⚠ weak: claimed, but the evidence is thin — present, just not strong.
    eprintln!(
        "{}",
        style::section(format!("Claimed but weak ({})", report.weak.len()))
    );
    for w in &report.weak {
        let imp = style::dim(format!(
            "({})",
            importance_label(w.matched.jd_skill.importance)
        ));
        let why = style::dim(format!("· {}", weakness_label(w.weakness)));
        eprintln!(
            "  {}",
            style::warn(format!("{} {why}  {imp}", w.matched.dataset_name))
        );
    }

    // ✗ unknown: a JD skill with nothing behind it in the dataset — a real gap.
    eprintln!(
        "{}",
        style::section(format!("Unknown ({})", report.unknown.len()))
    );
    for u in &report.unknown {
        let imp = style::dim(format!("({})", importance_label(u.importance)));
        eprintln!("  {}", style::fail(format!("{}  {imp}", u.name)));
    }

    if !report.unknown.is_empty() || !report.weak.is_empty() {
        eprintln!(
            "\n{}",
            style::suggest(
                "weak and unknown skills stay out of tailored resumes until they're backed by evidence"
            )
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
