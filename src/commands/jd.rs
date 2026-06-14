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
use crate::trace::Tracer;

pub async fn parse(path: PathBuf, json: bool) -> Result<(), CliError> {
    let (client, config) = configured_client().await?;
    let tracer = Tracer::to_default_dir()?;
    let ctx = AgentContext {
        llm: &client,
        model: &config.anthropic,
        tracer: &tracer,
    };

    let requirements = load_requirements(&path, &ctx).await?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&requirements).map_err(CliError::OutputJson)?
        );
        return Ok(());
    }

    println!("company:   {}", or_unknown(&requirements.company));
    println!("title:     {}", or_unknown(&requirements.title));
    println!(
        "level:     {} · {} · {}",
        seniority_label(requirements.seniority),
        remote_label(requirements.remote),
        requirements
            .location
            .as_deref()
            .unwrap_or("location unstated")
    );
    if !requirements.domain_keywords.is_empty() {
        println!("domain:    {}", requirements.domain_keywords.join(", "));
    }

    print_skills("required skills", &requirements.required_skills);
    print_skills("preferred skills", &requirements.preferred_skills);

    if !requirements.responsibilities.is_empty() {
        println!(
            "\nresponsibilities ({}):",
            requirements.responsibilities.len()
        );
        for r in &requirements.responsibilities {
            println!("  - {r}");
        }
    }
    if !requirements.ats_phrases.is_empty() {
        println!("\nats phrases ({}):", requirements.ats_phrases.len());
        for p in &requirements.ats_phrases {
            println!("  \"{p}\"");
        }
    }
    Ok(())
}

fn print_skills(heading: &str, skills: &[JdSkill]) {
    if skills.is_empty() {
        return;
    }
    println!("\n{heading} ({}):", skills.len());
    for skill in skills {
        let mut line = format!(
            "  - {} ({}, {})",
            skill.name,
            category_label(skill.category),
            importance_label(skill.importance)
        );
        if let Some(quote) = skill.context_phrases.first() {
            line.push_str(&format!(" — \"{quote}\""));
        }
        println!("{line}");
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
