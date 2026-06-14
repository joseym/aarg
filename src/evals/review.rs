//! Eval fixtures for the adversarial reviewer (`adversarial_reviewer_v1`).
//!
//! The reviewer's output is treated as untrusted: these cases check that
//! well-formed objections survive, an objection aimed at a line the draft
//! doesn't contain is dropped, and an out-of-range score is clamped.

use super::{Report, fixtures};
use crate::agent::{Agent, AgentContext};
use crate::llm::MockLlmClient;
use crate::review::{AdversarialReport, AdversarialReviewerAgent, ObjectionTarget, ReviewInput};
use crate::trace::Tracer;

pub async fn eval(report: &mut Report) {
    report.check("reviewer", "parses targeted objections", parses().await);
    report.check(
        "reviewer",
        "drops an objection that targets a missing bullet",
        drops_phantom().await,
    );
    report.check(
        "reviewer",
        "clamps an out-of-range score",
        clamps_score().await,
    );
}

/// Run the reviewer against one scripted report reply.
async fn run(reply: &str) -> Result<AdversarialReport, String> {
    let mock = MockLlmClient::new();
    mock.enqueue(reply);
    let ctx = AgentContext {
        llm: &mock,
        model: &"eval-model",
        tracer: &Tracer::DISABLED,
        sink: None,
    };
    AdversarialReviewerAgent
        .run(
            &ctx,
            ReviewInput {
                draft: fixtures::draft(),
                jd: fixtures::jd(),
                dataset: fixtures::empty_dataset(),
            },
        )
        .await
        .map(|r| r.output)
        .map_err(|e| format!("agent errored: {e}"))
}

async fn parses() -> Result<(), String> {
    let reply = r#"{
        "overall_score": 0.6,
        "persona_notes": "Thin in places.",
        "objections": [
            {"target": "bullet-1", "severity": "major", "kind": "vague_verb", "scope": "canonical", "message": "\"Helped\" hides what you did"},
            {"target": "summary", "severity": "minor", "kind": "generic_phrasing", "scope": "canonical", "message": "boilerplate"}
        ]
    }"#;
    let report = run(reply).await?;

    if report.objections.len() != 2 {
        return Err(format!(
            "{} objections, expected 2",
            report.objections.len()
        ));
    }
    match &report.objections[0].target {
        ObjectionTarget::Bullet(id) if id.0 == "bullet-1" => {}
        other => return Err(format!("first objection target = {other:?}")),
    }
    if !(0.0..=1.0).contains(&report.overall_score) {
        return Err(format!("score {} out of range", report.overall_score));
    }
    Ok(())
}

async fn drops_phantom() -> Result<(), String> {
    // bullet-1 is in the draft; bullet-99 is not — a hallucinated target.
    let reply = r#"{
        "overall_score": 0.5,
        "persona_notes": "...",
        "objections": [
            {"target": "bullet-99", "severity": "blocking", "kind": "unsupported_claim", "scope": "canonical", "message": "phantom line"},
            {"target": "bullet-1", "severity": "major", "kind": "vague_verb", "scope": "canonical", "message": "real line"}
        ]
    }"#;
    let report = run(reply).await?;

    if report.objections.len() != 1 {
        return Err(format!(
            "{} objections survived, expected the phantom dropped (1 left)",
            report.objections.len()
        ));
    }
    match &report.objections[0].target {
        ObjectionTarget::Bullet(id) if id.0 == "bullet-1" => Ok(()),
        other => Err(format!("survivor target = {other:?}, expected bullet-1")),
    }
}

async fn clamps_score() -> Result<(), String> {
    // A reviewer that returns 1.7 must not produce a score above 1.0.
    let report = run(r#"{"overall_score": 1.7, "persona_notes": "...", "objections": []}"#).await?;
    if (report.overall_score - 1.0).abs() > f32::EPSILON {
        return Err(format!(
            "score = {}, expected clamp to 1.0",
            report.overall_score
        ));
    }
    Ok(())
}
