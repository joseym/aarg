//! Variant projections (FR-5.1): turning the one canonical `TailoredResume`
//! into the per-template payloads that get rendered to PDFs.
//!
//! The rule that governs the whole phase (CLAUDE.md non-negotiable #5): the
//! variants are *projections* of one canonical draft. Same facts, different
//! presentation, never different claims.
//!
//! Two projections, two costs:
//! - **ATS** is a deterministic, near-identity projection (`ats_payload`).
//!   The canonical draft is already keyword-dense and untrimmed, which is
//!   exactly what an applicant tracking system wants, so there is nothing to
//!   reword and no model call to make.
//! - **Human** is reshaped for a person to read (tighter prose, a designed
//!   layout), so it goes through `VariantAdapterAgent`. Because that is an
//!   LLM rewording, never-fabricate is held structurally in `project_human`:
//!   the role/company/date structure always comes from the canonical draft, a
//!   reworded line that introduces a number its source doesn't state reverts
//!   to the source (`tailor::digit_runs`, a second consumer of that guard),
//!   and the skills list can only be reordered, never added to. The
//!   claim-divergence lint (FR-5.3) is the independent backstop.

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::agent::{Agent, AgentContext, ModelTier};
use crate::dataset::types::{Certification, Contact, Education, ResumeDataset};
use crate::jd::JobRequirements;
use crate::llm::{LlmError, TokenUsage};
use crate::review::{
    AdversarialReviewerAgent, ObjectionKind, ObjectionTarget, ReviewError, ReviewInput,
};
use crate::tailor::{
    SkillsSection, TailoredBullet, TailoredProject, TailoredResume, TailoredRole, digit_runs,
};

/// Which presentation a payload is shaped for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Variant {
    Ats,
    Human,
}

impl Variant {
    /// The final PDF filename for this variant (PRD's `.ats.pdf`/`.human.pdf`).
    pub fn pdf_name(self) -> &'static str {
        match self {
            Variant::Ats => "resume.ats.pdf",
            Variant::Human => "resume.human.pdf",
        }
    }

    /// The staged payload JSON filename.
    pub fn payload_name(self) -> &'static str {
        match self {
            Variant::Ats => "ats_payload.json",
            Variant::Human => "human_payload.json",
        }
    }

    /// One-line guidance on what the file is for.
    pub fn purpose(self) -> &'static str {
        match self {
            Variant::Ats => "upload this to application portals",
            Variant::Human => "share this by email or in person",
        }
    }
}

/// The template a payload names (e.g. "ats/classic", "human/modern").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TemplateId(pub String);

/// Visual density a human template may honor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Density {
    Compact,
    Standard,
    Airy,
}

/// A `#rrggbb` accent color, kept as text for the template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HexColor(pub String);

/// Presentation knobs set by code per variant (never by the model), read by
/// the human template. The ATS template ignores them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutHints {
    pub sidebar: bool,
    pub accent_color: Option<HexColor>,
    pub density: Density,
    pub show_summary: bool,
    pub max_pages: u8,
}

/// A named group of skills for the human variant's display (e.g. "Leadership").
/// The label is presentation only, never a claim; the skills it holds are
/// always a subset of the canonical skills. The ATS variant leaves this empty
/// and renders the flat list instead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillGroup {
    pub label: String,
    pub skills: Vec<String>,
}

/// A variant-specific projection of the canonical draft, serialized to JSON
/// and handed to one Typst template. It carries the canonical's fields (so a
/// template reads the same names regardless of variant) plus the variant tag,
/// the template id, and the layout hints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VariantPayload {
    pub variant: Variant,
    pub template: TemplateId,
    pub contact: Contact,
    #[serde(default)]
    pub target_title: Option<String>,
    pub summary: String,
    pub roles: Vec<TailoredRole>,
    pub education: Vec<Education>,
    pub skills_section: SkillsSection,
    /// Human variant only: the curated skills organized into labeled display
    /// groups. Empty on the ATS variant and on older payloads (serde default),
    /// where templates fall back to the flat `skills_section`.
    #[serde(default)]
    pub skill_groups: Vec<SkillGroup>,
    pub projects: Vec<TailoredProject>,
    pub certifications: Vec<Certification>,
    pub layout_hints: LayoutHints,
}

/// The ATS projection: a faithful, deterministic copy of the canonical draft
/// plus plain ATS layout hints. No rewording, no model call — the canonical
/// draft is already what an ATS wants.
pub fn ats_payload(draft: &TailoredResume) -> VariantPayload {
    VariantPayload {
        variant: Variant::Ats,
        template: TemplateId("ats/classic".into()),
        contact: draft.contact.clone(),
        target_title: draft.target_title.clone(),
        summary: draft.summary.clone(),
        roles: draft.roles.clone(),
        education: draft.education.clone(),
        skills_section: draft.skills_section.clone(),
        // The ATS variant stays a flat keyword-dense list; no grouping.
        skill_groups: Vec::new(),
        projects: draft.projects.clone(),
        certifications: draft.certifications.clone(),
        layout_hints: LayoutHints {
            sidebar: false,
            accent_color: None,
            density: Density::Standard,
            show_summary: true,
            max_pages: 2,
        },
    }
}

/// What the variant adapter works from: the canonical draft and which
/// presentation to shape it into. Owned, like the other agents' inputs.
#[derive(Serialize)]
pub struct VariantInput {
    pub draft: TailoredResume,
    pub variant: Variant,
}

/// The human-variant adapter: the model reshapes presentation, the guards in
/// `project_human` keep the claims identical to the canonical draft.
pub struct VariantAdapterAgent;

#[derive(Debug, thiserror::Error)]
pub enum VariantError {
    #[error(transparent)]
    Llm(#[from] LlmError),

    #[error("the model's reply was not the expected variant JSON (reply began {snippet:?})")]
    BadReply {
        snippet: String,
        #[source]
        source: serde_json::Error,
    },

    /// The re-review pass (`vet_human`) failed; surfaced as the reviewer's error.
    #[error(transparent)]
    Review(#[from] ReviewError),
}

/// The model's reply: a reworded summary, per-bullet rewordings keyed by
/// `source_id`, and a (reordered) skills list. Lenient — anything missing
/// falls back to the canonical text in assembly.
#[derive(Debug, Deserialize)]
pub struct RawVariant {
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    roles: Vec<RawVariantRole>,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    skill_groups: Vec<RawSkillGroup>,
}

#[derive(Debug, Deserialize)]
struct RawSkillGroup {
    #[serde(default)]
    label: String,
    #[serde(default)]
    skills: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawVariantRole {
    #[serde(default)]
    id: String,
    #[serde(default)]
    bullets: Vec<RawVariantBullet>,
}

#[derive(Debug, Deserialize)]
struct RawVariantBullet {
    source_id: String,
    text: String,
}

#[async_trait]
impl Agent for VariantAdapterAgent {
    type Input = VariantInput;
    type Wire = RawVariant;
    type Output = VariantPayload;
    type Error = VariantError;

    fn id(&self) -> &'static str {
        "variant_adapter_v1"
    }
    fn model_tier(&self) -> ModelTier {
        // Rewording for register and scannability without changing the claim
        // is a moderate judgment task, not the heaviest one — the mid tier.
        ModelTier::Mid
    }
    fn system_prompt(&self) -> &str {
        HUMAN_SYSTEM_PROMPT
    }
    fn reply_budget(&self) -> u32 {
        4096
    }
    fn user_message(&self, input: &VariantInput) -> String {
        build_user_message(&input.draft)
    }
    fn bad_reply(&self, snippet: String, source: serde_json::Error) -> VariantError {
        VariantError::BadReply { snippet, source }
    }
    fn assemble(
        &self,
        wire: RawVariant,
        input: VariantInput,
    ) -> Result<VariantPayload, VariantError> {
        Ok(project_human(wire, input.draft))
    }
}

const HUMAN_SYSTEM_PROMPT: &str = r#"You reshape an already-tailored resume for a HUMAN reader: a recruiter skimming it in seconds, then a hiring manager reading it before an interview. The facts have already been selected and verified. Your job is presentation only.

Rules — all of them matter:
- Reword the summary and the bullet lines to be tighter and more scannable: lead with the action and the outcome, prefer precise concrete verbs over vague ones, cut filler. Aim for one line per bullet.
- NEVER add or change a fact. No new metric, number, technology, employer, scale, scope, or seniority. You may only restate what a line already says. Strengthen the wording, never the claim. If a line is thin because the work was thin, leave it thin — do not pad it with anything invented.
- Refer to every bullet by its source_id. Return every role and every bullet you were given; you may rephrase a line, never drop or invent one.
- skills: this list is keyword-dense for machine scanning, so it carries near-duplicates and verbose phrases that read as keyword-stuffing to a person. For the HUMAN reader, CURATE and GROUP it. First choose a tight, non-redundant subset (about 8 to 12 total): keep the single cleanest phrasing where several entries overlap (e.g. one of "Engineering management" / "engineering leadership experience" / "managing senior technical leaders") and drop vague catch-alls and full-sentence entries. Then organize what remains into 2 to 4 short, sensibly-labeled groups (for example "Leadership", "Platform & Architecture", "Delivery & Process"). Use ONLY skills from the list given: never add one, drop the redundant ones, and put each kept skill in exactly one group.
- Reply with exactly one JSON object and nothing else — no markdown fences, no commentary:
{"summary": "...", "roles": [{"id": "role-1", "bullets": [{"source_id": "bullet-1", "text": "..."}]}], "skill_groups": [{"label": "Leadership", "skills": ["..."]}]}"#;

/// Render the canonical draft for the adapter to reword, showing bullet
/// `source_id`s so rewordings can be matched back.
fn build_user_message(draft: &TailoredResume) -> String {
    let mut text = String::from("CANONICAL RESUME (reshape its presentation, keep its claims)\n\n");
    text.push_str(&format!("SUMMARY\n{}\n\nEXPERIENCE\n", draft.summary));
    for role in &draft.roles {
        text.push_str(&format!(
            "[{}] {} at {}\n",
            role.id.0, role.title, role.company
        ));
        for bullet in &role.bullets {
            text.push_str(&format!("  ({}) {}\n", bullet.source_id.0, bullet.text));
        }
    }
    text.push_str(&format!(
        "\nSKILLS (curate a tight subset and GROUP it for a human; pick only from these, never add)\n{}\n",
        draft.skills_section.skills.join(", ")
    ));
    text
}

/// Build the human payload from the model's reply and the canonical draft,
/// holding every claim to the canonical. This is where never-fabricate lives
/// for the variant layer.
fn project_human(wire: RawVariant, draft: TailoredResume) -> VariantPayload {
    // Index the model's rewordings: role id -> (source_id -> new text).
    let mut rewrites: HashMap<String, HashMap<String, String>> = HashMap::new();
    for role in wire.roles {
        let bullets = role
            .bullets
            .into_iter()
            .map(|b| (b.source_id, b.text))
            .collect();
        rewrites.insert(role.id, bullets);
    }

    // Walk the CANONICAL roles in order: structure (company, title, dates,
    // which bullets exist) is never the model's to change. Reword a bullet
    // only when the model offered a rewrite for that source_id AND it adds no
    // number the source lacks; otherwise keep the canonical text.
    let roles = draft
        .roles
        .iter()
        .map(|role| {
            let role_rewrites = rewrites.get(&role.id.0);
            let bullets = role
                .bullets
                .iter()
                .map(|bullet| {
                    let text = role_rewrites
                        .and_then(|m| m.get(&bullet.source_id.0))
                        .filter(|new| {
                            !new.trim().is_empty()
                                && digit_runs(new).is_subset(&digit_runs(&bullet.text))
                        })
                        .cloned()
                        .unwrap_or_else(|| bullet.text.clone());
                    TailoredBullet {
                        source_id: bullet.source_id.clone(),
                        text,
                    }
                })
                .collect();
            TailoredRole {
                bullets,
                ..role.clone()
            }
        })
        .collect();

    // Summary: a reworded summary is accepted only if it adds no number the
    // canonical summary doesn't state.
    let summary = wire
        .summary
        .filter(|s| !s.trim().is_empty() && digit_runs(s).is_subset(&digit_runs(&draft.summary)))
        .unwrap_or_else(|| draft.summary.clone());

    // Skills: the model curates and GROUPS the list for a human reader. Every
    // grouped skill is validated against the canonical set (never minted),
    // empty/unlabeled groups are dropped, and each skill lands in just one
    // group. The flat `skills` list is the flatten of the groups — it is what
    // the lint checks and what a template without group support renders.
    let canonical_skills: HashSet<&str> = draft
        .skills_section
        .skills
        .iter()
        .map(String::as_str)
        .collect();
    let mut seen: HashSet<String> = HashSet::new();
    let mut skill_groups: Vec<SkillGroup> = Vec::new();
    for group in wire.skill_groups {
        let label = group.label.trim().to_string();
        if label.is_empty() {
            continue;
        }
        let skills: Vec<String> = group
            .skills
            .into_iter()
            .filter(|s| canonical_skills.contains(s.as_str()) && seen.insert(s.clone()))
            .collect();
        if !skills.is_empty() {
            skill_groups.push(SkillGroup { label, skills });
        }
    }
    let mut skills: Vec<String> = skill_groups
        .iter()
        .flat_map(|g| g.skills.iter().cloned())
        .collect();
    // No usable groups (the model returned a flat list, or none): fall back to
    // the curated flat skills, then to the canonical order.
    if skills.is_empty() {
        skills = wire
            .skills
            .into_iter()
            .filter(|s| canonical_skills.contains(s.as_str()))
            .collect();
    }
    if skills.is_empty() {
        skills = draft.skills_section.skills.clone();
    }

    VariantPayload {
        variant: Variant::Human,
        template: TemplateId("human/modern".into()),
        contact: draft.contact,
        target_title: draft.target_title,
        summary,
        roles,
        education: draft.education,
        skills_section: SkillsSection { skills },
        skill_groups,
        projects: draft.projects,
        certifications: draft.certifications,
        layout_hints: LayoutHints {
            sidebar: true,
            accent_color: Some(HexColor("#1f6feb".into())),
            density: Density::Standard,
            show_summary: true,
            max_pages: 2,
        },
    }
}

// ---------------------------------------------------------------------
// Claim-divergence lint (FR-5.3): the independent guarantee
// ---------------------------------------------------------------------

/// A variant said something the canonical draft doesn't. The build is
/// refused rather than shipping two resumes that make different claims.
#[derive(Debug, thiserror::Error)]
#[error("the {variant:?} variant diverged from the canonical draft:\n  - {}", divergences.join("\n  - "))]
pub struct ClaimDivergence {
    pub variant: Variant,
    pub divergences: Vec<String>,
}

/// Assert a payload makes no claim the canonical draft doesn't. Presentation
/// may differ freely (reorder, reword, even omit — a shorter variant is not a
/// new claim); claims may not. This is the structural guarantee behind the
/// LLM adapter: `project_human` already *prevents* divergence by reverting,
/// and this runs on every build as the independent check that hard-fails if
/// anything slipped through.
///
/// What counts as a divergent claim:
/// - a skill not in the canonical skills;
/// - a bullet whose `source_id` isn't in the canonical, or that states a
///   number the source bullet doesn't;
/// - a role not in the canonical, or with a changed company/title/dates;
/// - a summary stating a number the canonical summary doesn't.
pub fn check_claims(
    canonical: &TailoredResume,
    payload: &VariantPayload,
) -> Result<(), ClaimDivergence> {
    let mut divergences = Vec::new();

    let canonical_skills: HashSet<&str> = canonical
        .skills_section
        .skills
        .iter()
        .map(String::as_str)
        .collect();
    for skill in &payload.skills_section.skills {
        if !canonical_skills.contains(skill.as_str()) {
            divergences.push(format!("skill not in the canonical draft: {skill:?}"));
        }
    }
    // Grouped skills (human variant) are claims too; every one must be
    // canonical, even though the flat list above is their flatten.
    for group in &payload.skill_groups {
        for skill in &group.skills {
            if !canonical_skills.contains(skill.as_str()) {
                divergences.push(format!(
                    "grouped skill not in the canonical draft: {skill:?}"
                ));
            }
        }
    }

    let canonical_roles: HashMap<&str, &TailoredRole> = canonical
        .roles
        .iter()
        .map(|r| (r.id.0.as_str(), r))
        .collect();
    for role in &payload.roles {
        let Some(source) = canonical_roles.get(role.id.0.as_str()) else {
            divergences.push(format!("role not in the canonical draft: {}", role.id.0));
            continue;
        };
        if role.company != source.company
            || role.title != source.title
            || role.start != source.start
            || role.end != source.end
        {
            divergences.push(format!(
                "role {} changed its company, title, or dates",
                role.id.0
            ));
        }
        let canonical_bullets: HashMap<&str, &TailoredBullet> = source
            .bullets
            .iter()
            .map(|b| (b.source_id.0.as_str(), b))
            .collect();
        for bullet in &role.bullets {
            let Some(src) = canonical_bullets.get(bullet.source_id.0.as_str()) else {
                divergences.push(format!(
                    "bullet not in the canonical draft: {}",
                    bullet.source_id.0
                ));
                continue;
            };
            if !digit_runs(&bullet.text).is_subset(&digit_runs(&src.text)) {
                divergences.push(format!(
                    "bullet {} states a number its source doesn't",
                    bullet.source_id.0
                ));
            }
        }
    }

    if !digit_runs(&payload.summary).is_subset(&digit_runs(&canonical.summary)) {
        divergences.push("summary states a number the canonical summary doesn't".into());
    }

    if divergences.is_empty() {
        Ok(())
    } else {
        Err(ClaimDivergence {
            variant: payload.variant,
            divergences,
        })
    }
}

// ---------------------------------------------------------------------
// Re-review (FR-5.3, the non-numeric backstop): vet the human reword
// ---------------------------------------------------------------------

/// Run the adversarial reviewer over the reworded human variant and revert
/// any line it flags as an overclaim (`UnsupportedClaim`) to the canonical
/// text. `project_human`'s digit guard and `check_claims` catch fabricated
/// numbers, skills, employers, and dates structurally; this is the backstop
/// for the one thing they can't see — a reword that inflates scope or
/// seniority in prose without adding a number. It mirrors the voice pass,
/// which re-reviews its output for exactly the same reason. Returns the
/// vetted payload and the review's token usage.
pub async fn vet_human(
    ctx: &AgentContext<'_>,
    canonical: &TailoredResume,
    mut human: VariantPayload,
    jd: &JobRequirements,
    dataset: &ResumeDataset,
) -> Result<(VariantPayload, TokenUsage), VariantError> {
    // Present the reworded variant as a draft the reviewer can read.
    let draft = human_as_draft(canonical, &human);
    let run = AdversarialReviewerAgent
        .run(
            ctx,
            ReviewInput {
                draft,
                jd: jd.clone(),
                dataset: dataset.clone(),
            },
        )
        .await?;

    // The lines the reviewer judged overstated.
    let mut flagged_bullets: HashSet<&str> = HashSet::new();
    let mut summary_flagged = false;
    for objection in &run.output.objections {
        if objection.kind != ObjectionKind::UnsupportedClaim {
            continue;
        }
        match &objection.target {
            ObjectionTarget::Bullet(id) => {
                flagged_bullets.insert(id.0.as_str());
            }
            ObjectionTarget::Summary => summary_flagged = true,
            _ => {}
        }
    }

    if !flagged_bullets.is_empty() {
        let canonical_text: HashMap<&str, &str> = canonical
            .roles
            .iter()
            .flat_map(|r| &r.bullets)
            .map(|b| (b.source_id.0.as_str(), b.text.as_str()))
            .collect();
        for role in &mut human.roles {
            for bullet in &mut role.bullets {
                if flagged_bullets.contains(bullet.source_id.0.as_str())
                    && let Some(original) = canonical_text.get(bullet.source_id.0.as_str())
                {
                    bullet.text = (*original).to_string();
                }
            }
        }
    }
    if summary_flagged {
        human.summary = canonical.summary.clone();
    }

    Ok((human, run.usage))
}

/// A `TailoredResume` view of the reworded human payload, carrying the
/// canonical's build identity so the reviewer reads it like any draft.
fn human_as_draft(canonical: &TailoredResume, human: &VariantPayload) -> TailoredResume {
    TailoredResume {
        build_id: canonical.build_id.clone(),
        jd_id: canonical.jd_id.clone(),
        generated_at: canonical.generated_at,
        contact: human.contact.clone(),
        target_title: human.target_title.clone(),
        summary: human.summary.clone(),
        roles: human.roles.clone(),
        education: human.education.clone(),
        skills_section: human.skills_section.clone(),
        projects: human.projects.clone(),
        certifications: human.certifications.clone(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::agent::AgentContext;
    use crate::dataset::types::{BulletId, RoleId, YearMonth};
    use crate::llm::MockLlmClient;
    use crate::tailor::{BuildId, JdId};
    use crate::trace::Tracer;

    fn draft() -> TailoredResume {
        TailoredResume {
            build_id: BuildId("eval".into()),
            jd_id: JdId("globex".into()),
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
                id: RoleId("role-1".into()),
                company: "Acme".into(),
                title: "Engineer".into(),
                start: YearMonth {
                    year: 2020,
                    month: 1,
                },
                end: None,
                location: None,
                bullets: vec![TailoredBullet {
                    source_id: BulletId("bullet-1".into()),
                    text: "Led the platform migration".into(),
                }],
            }],
            education: Vec::new(),
            skills_section: SkillsSection {
                skills: vec!["Rust".into(), "Kubernetes".into()],
            },
            projects: Vec::new(),
            certifications: Vec::new(),
        }
    }

    async fn run_human(reply: &str) -> VariantPayload {
        let mock = MockLlmClient::new();
        mock.enqueue(reply);
        let ctx = AgentContext {
            llm: &mock,
            model: &"m",
            tracer: &Tracer::DISABLED,
            sink: None,
        };
        VariantAdapterAgent
            .run(
                &ctx,
                VariantInput {
                    draft: draft(),
                    variant: Variant::Human,
                },
            )
            .await
            .unwrap()
            .output
    }

    fn bullet_text(payload: &VariantPayload, source_id: &str) -> Option<String> {
        payload
            .roles
            .iter()
            .flat_map(|r| &r.bullets)
            .find(|b| b.source_id.0 == source_id)
            .map(|b| b.text.clone())
    }

    #[test]
    fn the_ats_payload_is_a_faithful_copy() {
        let d = draft();
        let p = ats_payload(&d);
        assert_eq!(p.variant, Variant::Ats);
        assert_eq!(p.summary, d.summary);
        assert_eq!(p.skills_section.skills, d.skills_section.skills);
        assert_eq!(
            bullet_text(&p, "bullet-1").unwrap(),
            "Led the platform migration"
        );
    }

    #[tokio::test]
    async fn a_faithful_human_rewrite_is_kept() {
        let p = run_human(
            r#"{"summary":"Engineering leader who ships.","roles":[{"id":"role-1","bullets":[{"source_id":"bullet-1","text":"Drove the platform migration"}]}],"skills":["Kubernetes","Rust"]}"#,
        )
        .await;
        assert_eq!(p.variant, Variant::Human);
        assert_eq!(
            bullet_text(&p, "bullet-1").unwrap(),
            "Drove the platform migration"
        );
        // Skills reordered, same set.
        assert_eq!(p.skills_section.skills, vec!["Kubernetes", "Rust"]);
    }

    #[tokio::test]
    async fn an_invented_number_reverts_to_the_source() {
        let p = run_human(
            r#"{"summary":"Engineering leader.","roles":[{"id":"role-1","bullets":[{"source_id":"bullet-1","text":"Led the platform migration, cutting costs 30%"}]}],"skills":[]}"#,
        )
        .await;
        assert_eq!(
            bullet_text(&p, "bullet-1").unwrap(),
            "Led the platform migration"
        );
    }

    #[tokio::test]
    async fn the_human_variant_curates_skills_to_a_subset() {
        // The adapter drops a redundant skill for the human reader, returning
        // fewer than the canonical set. A subset is legal (omission is not a
        // new claim), so it survives assembly and passes the lint.
        let p = run_human(
            r#"{"summary":"Engineering leader.","roles":[{"id":"role-1","bullets":[{"source_id":"bullet-1","text":"Led the platform migration"}]}],"skills":["Rust"]}"#,
        )
        .await;
        assert_eq!(p.skills_section.skills, vec!["Rust"]);
        assert!(check_claims(&draft(), &p).is_ok());
    }

    #[tokio::test]
    async fn the_human_variant_groups_skills_and_drops_minted_ones() {
        // The model returns labeled groups drawn from the canonical set, with
        // one minted skill ("Haskell") that must be dropped. The payload
        // carries the groups, the flat list is their flatten, and the lint
        // passes.
        let p = run_human(
            r#"{"summary":"Engineering leader.","roles":[],"skill_groups":[{"label":"Languages","skills":["Rust","Haskell"]},{"label":"Ops","skills":["Kubernetes"]}]}"#,
        )
        .await;
        assert_eq!(p.skill_groups.len(), 2);
        assert_eq!(p.skill_groups[0].label, "Languages");
        // "Haskell" isn't canonical, so it's dropped from the group.
        assert_eq!(p.skill_groups[0].skills, vec!["Rust"]);
        assert_eq!(p.skill_groups[1].skills, vec!["Kubernetes"]);
        // The flat list is the flatten of the groups, and the lint is happy.
        assert_eq!(p.skills_section.skills, vec!["Rust", "Kubernetes"]);
        assert!(check_claims(&draft(), &p).is_ok());
    }

    #[tokio::test]
    async fn a_minted_skill_is_dropped() {
        let p = run_human(
            r#"{"summary":"Engineering leader.","roles":[],"skills":["Rust","Go","Kubernetes"]}"#,
        )
        .await;
        // "Go" was not in the canonical set; it must not appear.
        assert!(!p.skills_section.skills.iter().any(|s| s == "Go"));
        assert_eq!(p.skills_section.skills, vec!["Rust", "Kubernetes"]);
    }

    #[tokio::test]
    async fn a_phantom_bullet_rewrite_is_ignored() {
        // The model rewords a source_id the canonical role doesn't have; the
        // real bullet keeps its canonical text and no phantom line appears.
        let p = run_human(
            r#"{"summary":"Engineering leader.","roles":[{"id":"role-1","bullets":[{"source_id":"bullet-99","text":"Invented line"}]}],"skills":[]}"#,
        )
        .await;
        assert_eq!(
            bullet_text(&p, "bullet-1").unwrap(),
            "Led the platform migration"
        );
        assert!(bullet_text(&p, "bullet-99").is_none());
        assert_eq!(p.roles[0].bullets.len(), 1);
    }

    #[test]
    fn the_ats_payload_passes_the_lint() {
        let d = draft();
        assert!(check_claims(&d, &ats_payload(&d)).is_ok());
    }

    #[tokio::test]
    async fn a_faithful_human_payload_passes_the_lint() {
        let d = draft();
        let p = run_human(
            r#"{"summary":"Engineering leader who ships.","roles":[{"id":"role-1","bullets":[{"source_id":"bullet-1","text":"Drove the platform migration"}]}],"skills":["Kubernetes","Rust"]}"#,
        )
        .await;
        assert!(check_claims(&d, &p).is_ok());
    }

    #[test]
    fn the_lint_catches_an_invented_number() {
        let d = draft();
        // A hand-built divergent payload (bypassing the adapter's revert).
        let mut p = ats_payload(&d);
        p.variant = Variant::Human;
        p.roles[0].bullets[0].text = "Led the platform migration, cutting costs 30%".into();
        let err = check_claims(&d, &p).unwrap_err();
        assert!(err.divergences.iter().any(|x| x.contains("number")));
    }

    #[test]
    fn the_lint_catches_a_minted_skill() {
        let d = draft();
        let mut p = ats_payload(&d);
        p.skills_section.skills.push("Haskell".into());
        let err = check_claims(&d, &p).unwrap_err();
        assert!(err.divergences.iter().any(|x| x.contains("Haskell")));
    }

    #[test]
    fn the_lint_catches_a_phantom_role() {
        let d = draft();
        let mut p = ats_payload(&d);
        p.roles[0].id = RoleId("role-99".into());
        let err = check_claims(&d, &p).unwrap_err();
        assert!(!err.divergences.is_empty());
    }

    fn sample_jd() -> JobRequirements {
        JobRequirements {
            company: "Globex".into(),
            title: "Engineering Manager".into(),
            seniority: crate::jd::Seniority::Unspecified,
            location: None,
            remote: crate::jd::RemotePolicy::Unspecified,
            domain_keywords: Vec::new(),
            required_skills: Vec::new(),
            preferred_skills: Vec::new(),
            responsibilities: Vec::new(),
            ats_phrases: Vec::new(),
            raw_text: "Globex hiring".into(),
            source_url: None,
        }
    }

    #[tokio::test]
    async fn vet_human_reverts_an_overclaim_to_the_canonical() {
        let canonical = draft();
        let mut human = ats_payload(&canonical);
        human.variant = Variant::Human;
        // A non-numeric scope inflation the digit guard can't see.
        human.roles[0].bullets[0].text =
            "Single-handedly led the company-wide platform migration".into();

        let mock = MockLlmClient::new();
        mock.enqueue(
            r#"{"overall_score":0.6,"persona_notes":"...","objections":[{"target":"bullet-1","severity":"major","kind":"unsupported_claim","scope":"canonical","message":"overstated"}]}"#,
        );
        let ctx = AgentContext {
            llm: &mock,
            model: &"m",
            tracer: &Tracer::DISABLED,
            sink: None,
        };
        let dataset = ResumeDataset::new(canonical.contact.clone());

        let (vetted, _usage) = vet_human(&ctx, &canonical, human, &sample_jd(), &dataset)
            .await
            .unwrap();

        // The reviewer's flag reverted the inflated line to the canonical text.
        assert_eq!(
            bullet_text(&vetted, "bullet-1").unwrap(),
            "Led the platform migration"
        );
    }

    #[tokio::test]
    async fn vet_human_keeps_a_clean_reword() {
        let canonical = draft();
        let mut human = ats_payload(&canonical);
        human.variant = Variant::Human;
        human.roles[0].bullets[0].text = "Drove the platform migration".into();

        let mock = MockLlmClient::new();
        mock.enqueue(r#"{"overall_score":0.8,"persona_notes":"solid","objections":[]}"#);
        let ctx = AgentContext {
            llm: &mock,
            model: &"m",
            tracer: &Tracer::DISABLED,
            sink: None,
        };
        let dataset = ResumeDataset::new(canonical.contact.clone());

        let (vetted, _usage) = vet_human(&ctx, &canonical, human, &sample_jd(), &dataset)
            .await
            .unwrap();

        assert_eq!(
            bullet_text(&vetted, "bullet-1").unwrap(),
            "Drove the platform migration"
        );
    }
}
