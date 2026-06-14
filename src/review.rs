//! Adversarial review: a skeptical hiring manager reads the tailored
//! draft against the job description and files structured objections
//! (FR-3.4, PRD §7.3).
//!
//! The reviewer is the project's namesake — the "adversarial" half of
//! the loop. It produces an `AdversarialReport`: per-line objections
//! (each tagged with what it targets, how bad it is, what kind of flaw,
//! and whether it's a content or layout issue), an overall score, and a
//! one-line verdict. It only *flags*; it never edits the draft.
//!
//! Two never-fabricate guards bracket it. The reviewer reads the JD's
//! raw text as ground truth, and its prompt forbids suggesting any fact
//! the work history doesn't already contain — a critic that invents a
//! "missing metric" the candidate could add would be a fabrication
//! backdoor. And because its output is objections, not resume content,
//! it structurally cannot put a claim on the page; only the revision
//! step can, and that goes through tailoring's existing evidence
//! guards.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::agent::{Agent, AgentContext, ModelTier};
use crate::dataset::types::{BulletId, ResumeDataset};
use crate::jd::JobRequirements;
use crate::llm::LlmError;
use crate::tailor::TailoredResume;

/// Reviews are short — a list of objections, a score, a verdict.
const REPLY_BUDGET: u32 = 4096;

/// Everything that can go wrong while reviewing.
#[derive(Debug, thiserror::Error)]
pub enum ReviewError {
    #[error(transparent)]
    Llm(#[from] LlmError),

    #[error("the reviewer's reply was not the expected report JSON (reply began {snippet:?})")]
    BadReply {
        snippet: String,
        #[source]
        source: serde_json::Error,
    },
}

// ---------------------------------------------------------------------
// The report (PRD §7.3 names, used verbatim)
// ---------------------------------------------------------------------

/// A skeptical reviewer's verdict on one draft.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdversarialReport {
    pub objections: Vec<Objection>,
    /// 0.0 (would not interview) to 1.0 (strong yes).
    pub overall_score: f32,
    /// The reviewer's one-or-two-sentence overall take.
    pub persona_notes: String,
}

impl AdversarialReport {
    // EXERCISE(EX-017)
    /// Objections a *revision* should act on: content (canonical) only.
    /// Layout objections route to the variant adapter (Phase 5), which
    /// doesn't exist yet, so the loop ignores them for now.
    pub fn actionable(&self) -> impl Iterator<Item = &Objection> {
        self.objections
            .iter()
            .filter(|o| o.scope == ObjectionScope::Canonical)
    }

    /// Whether any objection is severe enough that the draft isn't done.
    pub fn has_blocking_or_major(&self) -> bool {
        self.actionable()
            .any(|o| matches!(o.severity, Severity::Blocking | Severity::Major))
    }
}

/// One structured complaint about the draft.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Objection {
    pub target: ObjectionTarget,
    pub severity: Severity,
    pub kind: ObjectionKind,
    pub scope: ObjectionScope,
    pub message: String,
    /// A concrete fix — but never one that adds an unsupported fact.
    pub suggestion: Option<String>,
}

/// What part of the draft an objection is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectionTarget {
    Summary,
    /// A specific bullet, by the dataset id it was selected from.
    Bullet(BulletId),
    SkillsSection,
    /// Presentation, not content — routes to the variant adapter.
    Layout,
    /// The draft as a whole.
    Overall,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Would reject the candidate over this.
    Blocking,
    /// Notably weak.
    Major,
    /// Nitpick.
    Minor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectionKind {
    /// A claim with no quantification.
    NoMetric,
    /// A weak or passive verb ("helped", "worked on").
    VagueVerb,
    /// Asserts more than the work history supports.
    UnsupportedClaim,
    /// Boilerplate that could be on anyone's resume.
    GenericPhrasing,
    /// Misses or underplays something the JD emphasizes.
    JdMismatch,
    /// Too dense to scan (a layout concern).
    LayoutDense,
    /// Anything else the reviewer flags.
    Other,
}

/// Whether an objection is about the shared content or one variant's
/// presentation. In Phase 3 there is only the ATS variant, so this is
/// almost always `Canonical`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectionScope {
    Canonical,
    VariantOnly(Variant),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Variant {
    Ats,
    Human,
}

// ---------------------------------------------------------------------
// The agent
// ---------------------------------------------------------------------

/// What the reviewer works from: the draft to criticize, the JD to
/// judge it against, and the dataset (so the reviewer knows what's
/// genuinely backed when it suspects an overclaim).
#[derive(Serialize)]
pub struct ReviewInput {
    pub draft: TailoredResume,
    pub jd: JobRequirements,
    pub dataset: ResumeDataset,
}

/// The skeptical-hiring-manager agent (PRD §6.3 `AdversarialReviewerAgent`).
pub struct AdversarialReviewerAgent;

#[async_trait]
impl Agent for AdversarialReviewerAgent {
    type Input = ReviewInput;
    type Wire = RawReport;
    type Output = AdversarialReport;
    type Error = ReviewError;

    fn id(&self) -> &'static str {
        "adversarial_reviewer_v1"
    }
    fn model_tier(&self) -> ModelTier {
        // The reviewer has to catch overstatement and unbacked claims a
        // weaker model would wave through — it runs on the smart tier.
        ModelTier::Smart
    }
    fn system_prompt(&self) -> &str {
        SYSTEM_PROMPT
    }
    fn reply_budget(&self) -> u32 {
        REPLY_BUDGET
    }
    fn user_message(&self, input: &ReviewInput) -> String {
        review_message(input)
    }
    fn bad_reply(&self, snippet: String, source: serde_json::Error) -> ReviewError {
        ReviewError::BadReply { snippet, source }
    }
    fn assemble(
        &self,
        wire: RawReport,
        input: ReviewInput,
    ) -> Result<AdversarialReport, ReviewError> {
        Ok(assemble(wire, &input.draft))
    }
}

/// Review a draft. Convenience wrapper over the agent.
pub async fn review_draft(
    ctx: &AgentContext<'_>,
    draft: TailoredResume,
    jd: JobRequirements,
    dataset: ResumeDataset,
) -> Result<AdversarialReport, ReviewError> {
    let input = ReviewInput { draft, jd, dataset };
    Ok(AdversarialReviewerAgent.run(ctx, input).await?.output)
}

const SYSTEM_PROMPT: &str = r#"You are a skeptical, experienced hiring manager screening a candidate's tailored resume for one specific role. You have read hundreds of resumes and you reject most of them. Be specific, fair, and a little harsh.

For each weak point in the draft, file one structured objection. Reference the exact bullet by its id (e.g. "bullet-3"), or "summary", "skills", or "overall".

Objection kinds:
- "no_metric": a claim with no quantification where one would land harder.
- "vague_verb": a weak or passive verb ("helped", "worked on", "responsible for").
- "unsupported_claim": asserts more seniority, scale, or impact than the line actually demonstrates.
- "generic_phrasing": boilerplate that could appear on anyone's resume.
- "jd_mismatch": misses or underplays something this job description emphasizes.
- "other": anything else worth flagging.

Severity: "blocking" (you would reject over this), "major" (notably weak), "minor" (nitpick).

Scope: "canonical" for everything about content (almost all objections). Only use a variant scope for pure layout issues, which are rare here.

Hard rule on suggestions: you may suggest rephrasing, reordering, sharpening a verb, or cutting a line. You may NEVER suggest adding a metric, technology, employer, scale, or outcome that is not already present in the candidate's work history — inventing accomplishments is the one thing worse than a weak resume. If a line is vague because the underlying work is thin, say so; do not paper over it with a fabricated number.

overall_score: 0.0 (would not interview) to 1.0 (strong yes). Score honestly; most drafts land between 0.5 and 0.8.
persona_notes: one or two sentences, your overall verdict.

Reply with exactly one JSON object and nothing else — no markdown fences, no commentary:
{"overall_score": 0.0, "persona_notes": "...", "objections": [{"target": "bullet-3", "severity": "major", "kind": "no_metric", "scope": "canonical", "message": "...", "suggestion": "..."}]}"#;

/// Render the draft and JD into the reviewer's user message. The draft
/// shows bullet ids so objections can target lines; the JD includes its
/// raw text as ground truth for spotting overclaims.
fn review_message(input: &ReviewInput) -> String {
    let draft = &input.draft;
    let mut text = String::from("TAILORED RESUME DRAFT\n\n");
    if !draft.summary.is_empty() {
        text.push_str(&format!("SUMMARY\n{}\n\n", draft.summary));
    }
    text.push_str("EXPERIENCE\n");
    for role in &draft.roles {
        let end = role
            .end
            .map_or_else(|| "present".to_string(), |ym| ym.to_string());
        text.push_str(&format!(
            "{} at {} ({} to {})\n",
            role.title, role.company, role.start, end
        ));
        for bullet in &role.bullets {
            text.push_str(&format!("  [{}] {}\n", bullet.source_id.0, bullet.text));
        }
    }
    text.push_str(&format!(
        "\nSKILLS\n{}\n",
        draft.skills_section.skills.join(", ")
    ));

    text.push_str(&format!(
        "\nTHE JOB: {} at {}\n",
        input.jd.title, input.jd.company
    ));
    let required: Vec<&str> = input
        .jd
        .required_skills
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    if !required.is_empty() {
        text.push_str(&format!("emphasized skills: {}\n", required.join(", ")));
    }
    text.push_str(&format!(
        "\nFULL POSTING (ground truth):\n{}\n",
        input.jd.raw_text
    ));
    text
}

// ---------------------------------------------------------------------
// Wire shape: lenient strings, parsed and validated in assembly
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct RawReport {
    #[serde(default)]
    overall_score: f32,
    #[serde(default)]
    persona_notes: String,
    #[serde(default)]
    objections: Vec<RawObjection>,
}

#[derive(Debug, Deserialize)]
struct RawObjection {
    #[serde(default)]
    target: String,
    #[serde(default)]
    severity: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    suggestion: Option<String>,
}

/// Validate the reviewer's objections against the draft. Objections
/// pointing at a bullet the draft doesn't contain are dropped — a
/// reviewer hallucinating a line is noise, not signal. The score is
/// clamped to its documented range.
fn assemble(wire: RawReport, draft: &TailoredResume) -> AdversarialReport {
    let bullet_ids: std::collections::HashSet<&str> = draft
        .roles
        .iter()
        .flat_map(|r| r.bullets.iter())
        .map(|b| b.source_id.0.as_str())
        .collect();

    let mut objections = Vec::new();
    for raw in wire.objections {
        let Some(target) = parse_target(&raw.target, &bullet_ids) else {
            continue; // targets a bullet that isn't in the draft
        };
        objections.push(Objection {
            target,
            severity: parse_severity(&raw.severity),
            kind: parse_kind(&raw.kind),
            scope: parse_scope(&raw.scope),
            message: raw.message,
            suggestion: raw.suggestion.filter(|s| !s.trim().is_empty()),
        });
    }

    AdversarialReport {
        objections,
        overall_score: wire.overall_score.clamp(0.0, 1.0),
        persona_notes: wire.persona_notes,
    }
}

fn parse_target(
    raw: &str,
    bullet_ids: &std::collections::HashSet<&str>,
) -> Option<ObjectionTarget> {
    let lower = raw.trim().to_lowercase();
    match lower.as_str() {
        "summary" => Some(ObjectionTarget::Summary),
        "skills" | "skills_section" | "skillssection" => Some(ObjectionTarget::SkillsSection),
        "layout" => Some(ObjectionTarget::Layout),
        "overall" | "" => Some(ObjectionTarget::Overall),
        _ => {
            // A bullet reference: keep it only if the draft has it.
            if bullet_ids.contains(raw.trim()) {
                Some(ObjectionTarget::Bullet(BulletId(raw.trim().to_string())))
            } else {
                None
            }
        }
    }
}

fn parse_severity(raw: &str) -> Severity {
    match raw.trim().to_lowercase().as_str() {
        "blocking" => Severity::Blocking,
        "major" => Severity::Major,
        _ => Severity::Minor,
    }
}

fn parse_kind(raw: &str) -> ObjectionKind {
    match raw.trim().to_lowercase().as_str() {
        "no_metric" | "nometric" => ObjectionKind::NoMetric,
        "vague_verb" | "vagueverb" => ObjectionKind::VagueVerb,
        "unsupported_claim" | "unsupportedclaim" => ObjectionKind::UnsupportedClaim,
        "generic_phrasing" | "genericphrasing" => ObjectionKind::GenericPhrasing,
        "jd_mismatch" | "jdmismatch" => ObjectionKind::JdMismatch,
        "layout_dense" | "layoutdense" => ObjectionKind::LayoutDense,
        _ => ObjectionKind::Other,
    }
}

fn parse_scope(raw: &str) -> ObjectionScope {
    match raw.trim().to_lowercase().as_str() {
        "ats" | "variant:ats" => ObjectionScope::VariantOnly(Variant::Ats),
        "human" | "variant:human" => ObjectionScope::VariantOnly(Variant::Human),
        _ => ObjectionScope::Canonical,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::dataset::types::{Contact, YearMonth};
    use crate::jd::{Importance, JdSkill, RemotePolicy, Seniority};
    use crate::llm::MockLlmClient;
    use crate::tailor::{
        BuildId, JdId, SkillsSection, TailoredBullet, TailoredResume, TailoredRole,
    };
    use crate::trace::Tracer;

    fn sample_draft() -> TailoredResume {
        TailoredResume {
            build_id: BuildId("001".into()),
            jd_id: JdId("acme".into()),
            generated_at: chrono::Utc::now(),
            contact: Contact {
                full_name: "Ada".into(),
                email: "ada@example.com".into(),
                phone: None,
                location: None,
                links: Vec::new(),
            },
            target_title: Some("Engineering Manager".into()),
            summary: "Engineering leader.".into(),
            roles: vec![TailoredRole {
                id: crate::dataset::types::RoleId("role-1".into()),
                company: "Acme".into(),
                title: "Engineer".into(),
                start: YearMonth {
                    year: 2020,
                    month: 1,
                },
                end: None,
                location: None,
                bullets: vec![
                    TailoredBullet {
                        source_id: BulletId("bullet-1".into()),
                        text: "Helped with the platform".into(),
                    },
                    TailoredBullet {
                        source_id: BulletId("bullet-2".into()),
                        text: "Cut deploy time from 45 to 8 minutes".into(),
                    },
                ],
            }],
            education: Vec::new(),
            skills_section: SkillsSection {
                skills: vec!["Rust".into()],
            },
            projects: Vec::new(),
            certifications: Vec::new(),
        }
    }

    fn sample_jd() -> JobRequirements {
        JobRequirements {
            company: "Acme".into(),
            title: "Staff Engineer".into(),
            seniority: Seniority::Staff,
            location: None,
            remote: RemotePolicy::Remote,
            domain_keywords: Vec::new(),
            required_skills: vec![JdSkill {
                name: "Rust".into(),
                category: crate::dataset::types::SkillCategory::Language,
                importance: Importance::Critical,
                context_phrases: Vec::new(),
            }],
            preferred_skills: Vec::new(),
            responsibilities: Vec::new(),
            ats_phrases: Vec::new(),
            raw_text: "We want a Staff Engineer strong in Rust.".into(),
            source_url: None,
        }
    }

    fn dataset() -> ResumeDataset {
        ResumeDataset::new(Contact {
            full_name: "Ada".into(),
            email: "ada@example.com".into(),
            phone: None,
            location: None,
            links: Vec::new(),
        })
    }

    async fn run_review(reply: &str) -> AdversarialReport {
        let mock = MockLlmClient::default();
        mock.enqueue(reply);
        let ctx = AgentContext {
            llm: &mock,
            model: &"m",
            tracer: &Tracer::DISABLED,
        };
        review_draft(&ctx, sample_draft(), sample_jd(), dataset())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_full_report_parses_into_typed_objections() {
        let report = run_review(
            r#"{"overall_score": 0.65,
                "persona_notes": "Solid but the first bullet is weak.",
                "objections": [
                  {"target": "bullet-1", "severity": "major", "kind": "vague_verb",
                   "scope": "canonical", "message": "\"Helped\" hides what you did.",
                   "suggestion": "Lead with the action you actually took."},
                  {"target": "summary", "severity": "minor", "kind": "generic_phrasing",
                   "scope": "canonical", "message": "Generic.", "suggestion": null}
                ]}"#,
        )
        .await;

        assert_eq!(report.overall_score, 0.65);
        assert_eq!(report.objections.len(), 2);
        assert_eq!(
            report.objections[0].target,
            ObjectionTarget::Bullet(BulletId("bullet-1".into()))
        );
        assert_eq!(report.objections[0].kind, ObjectionKind::VagueVerb);
        assert_eq!(report.objections[1].target, ObjectionTarget::Summary);
        // Empty/null suggestions become None.
        assert!(report.objections[1].suggestion.is_none());
        assert!(report.has_blocking_or_major());
    }

    #[tokio::test]
    async fn objections_targeting_phantom_bullets_are_dropped() {
        let report = run_review(
            r#"{"overall_score": 0.5, "persona_notes": "x",
                "objections": [
                  {"target": "bullet-9", "severity": "blocking", "kind": "no_metric",
                   "scope": "canonical", "message": "no such bullet"},
                  {"target": "bullet-2", "severity": "minor", "kind": "no_metric",
                   "scope": "canonical", "message": "real bullet"}
                ]}"#,
        )
        .await;

        // bullet-9 isn't in the draft; only the real objection survives.
        assert_eq!(report.objections.len(), 1);
        assert_eq!(
            report.objections[0].target,
            ObjectionTarget::Bullet(BulletId("bullet-2".into()))
        );
    }

    #[tokio::test]
    async fn the_score_is_clamped_and_unknown_enums_default() {
        let report = run_review(
            r#"{"overall_score": 1.7, "persona_notes": "x",
                "objections": [
                  {"target": "overall", "severity": "catastrophic", "kind": "vibes",
                   "scope": "sideways", "message": "m"}
                ]}"#,
        )
        .await;

        assert_eq!(report.overall_score, 1.0);
        let o = &report.objections[0];
        assert_eq!(o.target, ObjectionTarget::Overall);
        assert_eq!(o.severity, Severity::Minor); // unknown -> nitpick
        assert_eq!(o.kind, ObjectionKind::Other);
        assert_eq!(o.scope, ObjectionScope::Canonical); // unknown -> content
    }

    #[tokio::test]
    async fn layout_objections_are_not_actionable_content() {
        let report = run_review(
            r#"{"overall_score": 0.8, "persona_notes": "x",
                "objections": [
                  {"target": "layout", "severity": "major", "kind": "layout_dense",
                   "scope": "ats", "message": "too dense"},
                  {"target": "bullet-1", "severity": "major", "kind": "vague_verb",
                   "scope": "canonical", "message": "vague"}
                ]}"#,
        )
        .await;

        // The variant-scoped layout objection is not content the revision
        // loop should act on; only the canonical one is actionable.
        assert_eq!(report.objections.len(), 2);
        assert_eq!(report.actionable().count(), 1);
    }

    #[tokio::test]
    #[ignore = "exercise: a reviewer can file Blocking objections and still report a high overall_score; add effective_score() to AdversarialReport that caps the score when an actionable objection is Blocking, then finish this test"]
    async fn ex_017_blocking_objections_cap_the_score() {
        // Once effective_score() exists: a report with a Blocking
        // canonical objection and overall_score 0.9 should yield 0.5;
        // a report with only Minor objections passes through unchanged.
        let capping_implemented = false;
        assert!(capping_implemented);
    }

    #[tokio::test]
    async fn the_prompt_forbids_inventing_facts_and_carries_the_jd_text() {
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"overall_score": 0.7, "persona_notes": "ok", "objections": []}"#);
        let ctx = AgentContext {
            llm: &mock,
            model: &"m",
            tracer: &Tracer::DISABLED,
        };
        review_draft(&ctx, sample_draft(), sample_jd(), dataset())
            .await
            .unwrap();

        let req = &mock.requests()[0];
        // The reviewer is told never to suggest inventing accomplishments.
        assert!(
            req.system
                .as_deref()
                .unwrap()
                .contains("NEVER suggest adding")
        );
        // The draft's bullet ids and the JD's raw text both reach it.
        let msg = &req.messages[0].content;
        assert!(msg.contains("[bullet-1]"));
        assert!(msg.contains("strong in Rust"));
    }
}
