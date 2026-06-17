//! `aarg jd parse <path|->` — parse a job description into structured
//! requirements.
//!
//! Thin glue, like the other LLM commands: read the text (file or
//! stdin), call `crate::jd::parse_jd`, present the result. `--json`
//! prints the full `JobRequirements` for scripts; progress goes to
//! stderr so stdout stays clean either way.

use std::path::PathBuf;

use crate::agent::AgentContext;
use crate::commands::{CliError, configured_client, load_requirements};
use crate::dataset::types::SkillCategory;
use crate::jd::{Importance, JdSkill, RemotePolicy, Seniority};
use crate::style;

pub async fn parse(path: PathBuf, json: bool) -> Result<(), CliError> {
    let (client, config) = configured_client().await?;
    let tracer = super::default_tracer()?;
    let ctx = AgentContext {
        llm: &client,
        model: &config.anthropic,
        tracer: &tracer,
        sink: None,
    };

    let requirements = load_requirements(&path, &ctx).await?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&requirements).map_err(CliError::OutputJson)?
        );
        return Ok(());
    }

    // Human summary on stderr (the stream the color helpers detect on); the
    // `--json` form above is the machine output and stays on stdout.
    let title = or_unknown(&requirements.title);
    let company = or_unknown(&requirements.company);
    eprintln!("\n{}", style::bold(format!("{title} @ {company}")));

    let width = 8;
    eprintln!(
        "{}",
        style::kv(
            "level",
            format!(
                "{} · {} · {}",
                seniority_label(requirements.seniority),
                remote_label(requirements.remote),
                requirements
                    .location
                    .as_deref()
                    .unwrap_or("location unstated")
            ),
            width
        )
    );
    if !requirements.domain_keywords.is_empty() {
        eprintln!(
            "{}",
            style::kv("domain", requirements.domain_keywords.join(", "), width)
        );
    }

    print_skills("Required skills", &requirements.required_skills);
    print_skills("Preferred skills", &requirements.preferred_skills);

    if !requirements.responsibilities.is_empty() {
        eprintln!(
            "{}",
            style::section(format!(
                "Responsibilities ({})",
                requirements.responsibilities.len()
            ))
        );
        for r in &requirements.responsibilities {
            eprintln!("  {}", style::bullet(r));
        }
    }
    if !requirements.ats_phrases.is_empty() {
        eprintln!(
            "{}",
            style::section(format!("ATS phrases ({})", requirements.ats_phrases.len()))
        );
        for p in &requirements.ats_phrases {
            eprintln!("  {}", style::bullet(format!("\"{p}\"")));
        }
    }
    Ok(())
}

fn print_skills(heading: &str, skills: &[JdSkill]) {
    if skills.is_empty() {
        return;
    }
    eprintln!(
        "{}",
        style::section(format!("{heading} ({})", skills.len()))
    );
    for skill in skills {
        let meta = style::dim(format!(
            "({}, {})",
            category_label(skill.category),
            importance_label(skill.importance)
        ));
        let mut line = format!("{} {meta}", skill.name);
        if let Some(quote) = skill.context_phrases.first() {
            line.push_str(&format!(" {}", style::dim(format!("· \"{quote}\""))));
        }
        eprintln!("  {}", style::bullet(line));
    }
}

fn or_unknown(value: &str) -> &str {
    if value.is_empty() {
        "(not stated)"
    } else {
        value
    }
}

fn seniority_label(s: Seniority) -> &'static str {
    match s {
        Seniority::Junior => "junior",
        Seniority::Mid => "mid",
        Seniority::Senior => "senior",
        Seniority::Staff => "staff",
        Seniority::Principal => "principal",
        Seniority::Manager => "manager",
        Seniority::Director => "director",
        Seniority::Executive => "executive",
        Seniority::Unspecified => "seniority unstated",
    }
}

fn remote_label(r: RemotePolicy) -> &'static str {
    match r {
        RemotePolicy::Remote => "remote",
        RemotePolicy::Hybrid => "hybrid",
        RemotePolicy::OnSite => "on-site",
        RemotePolicy::Unspecified => "remote policy unstated",
    }
}

fn importance_label(i: Importance) -> &'static str {
    match i {
        Importance::Critical => "critical",
        Importance::Required => "required",
        Importance::Preferred => "preferred",
    }
}

fn category_label(c: SkillCategory) -> &'static str {
    match c {
        SkillCategory::Hard => "hard",
        SkillCategory::Soft => "soft",
        SkillCategory::Domain => "domain",
        SkillCategory::Tool => "tool",
        SkillCategory::Language => "language",
        SkillCategory::Framework => "framework",
    }
}
