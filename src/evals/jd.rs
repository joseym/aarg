//! Eval fixtures for the JD parser (`jd_parser_v1`).
//!
//! Three keyless cases: a full posting parses cleanly, a sparse reply is
//! tolerated (the lenient wire fills defaults), and a skill with no stated
//! importance defaults by the list it sits in.

use super::Report;
use crate::agent::{Agent, AgentContext};
use crate::jd::{Importance, JdParserAgent, JobRequirements};
use crate::llm::MockLlmClient;
use crate::trace::Tracer;

pub async fn eval(report: &mut Report) {
    report.check("jd_parser", "parses a full posting", standard().await);
    report.check("jd_parser", "tolerates a sparse reply", sparse().await);
    report.check(
        "jd_parser",
        "defaults skill importance by its list",
        importance_fallback().await,
    );
}

/// Run the parser against one scripted reply, returning the assembled
/// requirements or a reason the run failed.
async fn run(reply: &str, jd_text: &str) -> Result<JobRequirements, String> {
    let mock = MockLlmClient::new();
    mock.enqueue(reply);
    let ctx = AgentContext {
        llm: &mock,
        model: &"eval-model",
        tracer: &Tracer::DISABLED,
        sink: None,
    };
    JdParserAgent::default()
        .run(&ctx, jd_text.to_string())
        .await
        .map(|r| r.output)
        .map_err(|e| format!("agent errored: {e}"))
}

async fn standard() -> Result<(), String> {
    let reply = r#"{
        "company": "Globex",
        "title": "Staff Engineer",
        "seniority": "staff",
        "remote": "remote",
        "domain_keywords": ["distributed systems"],
        "required_skills": [
            {"name": "Rust", "category": "language", "importance": "critical", "context_phrases": ["deep Rust expertise"]}
        ],
        "preferred_skills": [{"name": "Kubernetes", "category": "tool"}],
        "responsibilities": ["own the platform"],
        "ats_phrases": ["Staff Engineer", "distributed systems"]
    }"#;
    let jd = run(
        reply,
        "Globex is hiring a Staff Engineer to own the platform.",
    )
    .await?;

    if jd.company != "Globex" {
        return Err(format!("company = {:?}", jd.company));
    }
    if jd.title != "Staff Engineer" {
        return Err(format!("title = {:?}", jd.title));
    }
    if jd.required_skills.len() != 1 {
        return Err(format!(
            "{} required skills, expected 1",
            jd.required_skills.len()
        ));
    }
    if jd.preferred_skills.len() != 1 {
        return Err(format!(
            "{} preferred skills, expected 1",
            jd.preferred_skills.len()
        ));
    }
    // The raw text is preserved verbatim — the reviewer reads it as ground truth.
    if !jd.raw_text.contains("own the platform") {
        return Err("raw_text was not preserved".into());
    }
    Ok(())
}

async fn sparse() -> Result<(), String> {
    // The model returns only what it found; every other field defaults.
    let jd = run(
        r#"{"company": "Initech", "title": "Engineer"}"#,
        "Initech hiring",
    )
    .await?;

    if jd.company != "Initech" || jd.title != "Engineer" {
        return Err(format!("parsed {:?} / {:?}", jd.company, jd.title));
    }
    if !jd.required_skills.is_empty() || !jd.preferred_skills.is_empty() {
        return Err("missing skill lists should default to empty".into());
    }
    if !jd.ats_phrases.is_empty() {
        return Err("missing ats_phrases should default to empty".into());
    }
    Ok(())
}

async fn importance_fallback() -> Result<(), String> {
    // Neither skill states an importance; the list it sits in decides.
    let reply = r#"{
        "company": "Acme",
        "title": "Engineer",
        "required_skills": [{"name": "Go"}],
        "preferred_skills": [{"name": "AWS"}]
    }"#;
    let jd = run(reply, "Acme hiring an engineer").await?;

    let req = jd
        .required_skills
        .first()
        .ok_or("no required skill parsed")?;
    if req.importance != Importance::Required {
        return Err(format!("required skill importance = {:?}", req.importance));
    }
    let pref = jd
        .preferred_skills
        .first()
        .ok_or("no preferred skill parsed")?;
    if pref.importance != Importance::Preferred {
        return Err(format!(
            "preferred skill importance = {:?}",
            pref.importance
        ));
    }
    Ok(())
}
