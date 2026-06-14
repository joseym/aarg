//! Eval fixtures for the gap analyzer (`gap_analyzer_v1`).
//!
//! Three keyless cases pinning the hybrid matcher: a fully alias-matched JD
//! spends zero tokens, a thin recorded skill lands in the weak bucket
//! (also deterministic), and a leftover the model can't match stays unknown
//! (the one case that calls the model — and even then it can't mint a match).

use super::{Report, fixtures};
use crate::agent::AgentContext;
use crate::dataset::types::{Proficiency, SkillCategory};
use crate::gap::analyze_gap;
use crate::jd::{Importance, JdSkill, JobRequirements};
use crate::llm::MockLlmClient;
use crate::trace::Tracer;

pub async fn eval(report: &mut Report) {
    report.check(
        "gap",
        "a fully-backed JD spends zero tokens",
        fully_covered().await,
    );
    report.check("gap", "a thin recorded skill is weak", weak_bucket().await);
    report.check(
        "gap",
        "an unmatchable skill stays unknown",
        unknown_stays_unknown().await,
    );
}

/// A JD whose only requirements are the named skills.
fn jd_requiring(names: &[&str]) -> JobRequirements {
    let mut jd = fixtures::jd();
    jd.required_skills = names
        .iter()
        .map(|n| JdSkill {
            name: (*n).into(),
            category: SkillCategory::Tool,
            importance: Importance::Required,
            context_phrases: Vec::new(),
        })
        .collect();
    jd.preferred_skills = Vec::new();
    jd
}

async fn fully_covered() -> Result<(), String> {
    let dataset = fixtures::dataset(
        vec![fixtures::skill(
            "s-rust",
            "Rust",
            &["rust"],
            Proficiency::Expert,
            true,
        )],
        Vec::new(),
    );
    let jd = jd_requiring(&["Rust"]);
    let mock = MockLlmClient::new();
    let ctx = AgentContext {
        llm: &mock,
        model: &"eval-model",
        tracer: &Tracer::DISABLED,
        sink: None,
    };
    let report = analyze_gap(&ctx, &jd, &dataset)
        .await
        .map_err(|e| format!("gap errored: {e}"))?;

    if report.matched.len() != 1 {
        return Err(format!("{} matched, expected 1", report.matched.len()));
    }
    if !report.weak.is_empty() || !report.unknown.is_empty() {
        return Err("weak/unknown should be empty for a fully-backed JD".into());
    }
    // The decisive assertion: a fully alias-matched JD never calls the model.
    if !mock.requests().is_empty() {
        return Err(format!(
            "expected zero model calls, got {}",
            mock.requests().len()
        ));
    }
    Ok(())
}

async fn weak_bucket() -> Result<(), String> {
    // Recorded, but at the weakest proficiency — usable, worth shoring up.
    let dataset = fixtures::dataset(
        vec![fixtures::skill(
            "s-ts",
            "TypeScript",
            &["typescript"],
            Proficiency::Familiar,
            true,
        )],
        Vec::new(),
    );
    let jd = jd_requiring(&["TypeScript"]);
    let mock = MockLlmClient::new();
    let ctx = AgentContext {
        llm: &mock,
        model: &"eval-model",
        tracer: &Tracer::DISABLED,
        sink: None,
    };
    let report = analyze_gap(&ctx, &jd, &dataset)
        .await
        .map_err(|e| format!("gap errored: {e}"))?;

    if report.weak.len() != 1 {
        return Err(format!("{} weak, expected 1", report.weak.len()));
    }
    if !report.matched.is_empty() {
        return Err("a thin skill belongs in weak, not matched".into());
    }
    // Still alias-matched, so still no model call.
    if !mock.requests().is_empty() {
        return Err("a recorded-but-weak skill is resolved without the model".into());
    }
    Ok(())
}

async fn unknown_stays_unknown() -> Result<(), String> {
    let dataset = fixtures::dataset(
        vec![fixtures::skill(
            "s-rust",
            "Rust",
            &["rust"],
            Proficiency::Expert,
            true,
        )],
        Vec::new(),
    );
    // Rust alias-matches; Haskell is a leftover the model is asked about.
    let jd = jd_requiring(&["Rust", "Haskell"]);
    let mock = MockLlmClient::new();
    // The model finds no dataset skill for Haskell — it cannot mint one.
    mock.enqueue(r#"{"matches": [{"jd_skill": "Haskell", "dataset_skill": null}]}"#);
    let ctx = AgentContext {
        llm: &mock,
        model: &"eval-model",
        tracer: &Tracer::DISABLED,
        sink: None,
    };
    let report = analyze_gap(&ctx, &jd, &dataset)
        .await
        .map_err(|e| format!("gap errored: {e}"))?;

    if report.matched.len() != 1 || report.matched[0].dataset_name != "Rust" {
        return Err(format!("matched = {:?}", report.matched.len()));
    }
    let unknown: Vec<&str> = report.unknown.iter().map(|s| s.name.as_str()).collect();
    if unknown != ["Haskell"] {
        return Err(format!("unknown = {unknown:?}, expected [Haskell]"));
    }
    // Exactly one model call — for the leftover, not the alias hit.
    if mock.requests().len() != 1 {
        return Err(format!("{} model calls, expected 1", mock.requests().len()));
    }
    Ok(())
}
