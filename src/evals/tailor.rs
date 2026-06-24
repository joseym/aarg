//! Eval fixtures for the tailoring agent (`tailoring_v1`) — the
//! never-fabricate guards, which is what this whole project is about.
//!
//! The model selects bullets by id and may rephrase them. The digit-runs
//! guard is the structural line: a rewrite may drop a number, but a number
//! it introduces that the source (or the source's recorded metric) doesn't
//! state reverts the bullet to its original wording. These three cases pin
//! all three sides of that rule.

use super::{Report, fixtures};
use crate::agent::{Agent, AgentContext};
use crate::dataset::types::ResumeDataset;
use crate::gap::GapReport;
use crate::llm::MockLlmClient;
use crate::tailor::{BuildId, JdId, TailorInput, TailoredResume, TailoringAgent};
use crate::trace::Tracer;

pub async fn eval(report: &mut Report) {
    report.check(
        "tailor",
        "reverts a rewrite that invents a number",
        invented_number_reverted().await,
    );
    report.check(
        "tailor",
        "keeps a number the source or its metric states",
        real_number_kept().await,
    );
    report.check(
        "tailor",
        "allows a rewrite that drops a number",
        dropping_a_number_ok().await,
    );
}

/// Run the tailoring agent against one scripted selection over `dataset`.
async fn run(dataset: ResumeDataset, reply: &str) -> Result<TailoredResume, String> {
    let mock = MockLlmClient::new();
    mock.enqueue(reply);
    let ctx = AgentContext {
        llm: &mock,
        model: &"eval-model",
        tracer: &Tracer::DISABLED,
        sink: None,
    };
    let input = TailorInput {
        build_id: BuildId("eval".into()),
        jd_id: JdId("globex".into()),
        jd: fixtures::jd(),
        dataset,
        gap: GapReport {
            matched: Vec::new(),
            weak: Vec::new(),
            unknown: Vec::new(),
        },
        revision: None,
    };
    TailoringAgent
        .run(&ctx, input)
        .await
        .map(|r| r.output.resume)
        .map_err(|e| format!("agent errored: {e}"))
}

/// The assembled text of the bullet with the given source id, if present.
fn bullet_text(resume: &TailoredResume, source_id: &str) -> Option<String> {
    resume
        .roles
        .iter()
        .flat_map(|r| &r.bullets)
        .find(|b| b.source_id.0 == source_id)
        .map(|b| b.text.clone())
}

/// A one-role dataset whose `bullet-1` is the line under test; `bullet-2`
/// is filler so the per-role floor has something of its own to keep.
fn dataset_with(bullet1: crate::dataset::types::Bullet) -> ResumeDataset {
    fixtures::dataset(
        Vec::new(),
        vec![fixtures::role(
            "role-1",
            vec![
                bullet1,
                fixtures::bullet("bullet-2", "Mentored the team", None),
            ],
        )],
    )
}

/// Pick only `bullet-1`, rephrased to `text`.
fn pick_bullet_1(text: &str) -> String {
    format!(
        r#"{{"summary":"Engineering leader.","roles":[{{"id":"role-1","bullets":[{{"source_id":"bullet-1","text":{}}}]}}],"skills":[],"projects":[]}}"#,
        serde_json::Value::String(text.to_string())
    )
}

async fn invented_number_reverted() -> Result<(), String> {
    // Source states no number; the rewrite invents "40%".
    let dataset = dataset_with(fixtures::bullet(
        "bullet-1",
        "Led the platform migration",
        None,
    ));
    let draft = run(
        dataset,
        &pick_bullet_1("Led the platform migration, cutting deploy time 40%"),
    )
    .await?;
    let text = bullet_text(&draft, "bullet-1").ok_or("bullet-1 missing from the draft")?;
    if text != "Led the platform migration" {
        return Err(format!(
            "bullet-1 = {text:?}, expected the source (invented number reverted)"
        ));
    }
    Ok(())
}

async fn real_number_kept() -> Result<(), String> {
    // The source's recorded metric states 40% — folding it in is allowed.
    let dataset = dataset_with(fixtures::bullet(
        "bullet-1",
        "Led the platform migration",
        Some("cut deploy time 40%"),
    ));
    let draft = run(
        dataset,
        &pick_bullet_1("Led the platform migration, cutting deploy time 40%"),
    )
    .await?;
    let text = bullet_text(&draft, "bullet-1").ok_or("bullet-1 missing")?;
    if !text.contains("40%") {
        return Err(format!(
            "bullet-1 = {text:?}, expected the recorded metric kept"
        ));
    }
    Ok(())
}

async fn dropping_a_number_ok() -> Result<(), String> {
    // Source has 45 and 8; the rewrite drops 45, keeps 8 — a subset, allowed.
    let dataset = dataset_with(fixtures::bullet(
        "bullet-1",
        "Cut deploy time from 45 to 8 minutes",
        None,
    ));
    let draft = run(dataset, &pick_bullet_1("Cut deploy time to 8 minutes")).await?;
    let text = bullet_text(&draft, "bullet-1").ok_or("bullet-1 missing")?;
    if text != "Cut deploy time to 8 minutes" {
        return Err(format!(
            "bullet-1 = {text:?}, expected the shortened rewrite kept"
        ));
    }
    Ok(())
}
