//! Tailoring: select, order, and rephrase dataset material for one JD,
//! producing the canonical `TailoredResume` (FR-1.6).
//!
//! The most consequential of the Phase 1 LLM features, because its
//! output *is* the resume — so this is where never-fabricate (FR-1.7)
//! is enforced hardest. The split of powers:
//!
//! - The **model** chooses: which bullets, in what order, with wording
//!   mirrored to the JD. It speaks entirely in IDs from the dataset.
//! - **This code** disposes: a selected role/bullet/project must exist
//!   in the dataset (and the bullet must belong to the role it's cited
//!   under); a rephrased bullet may not contain any number that its
//!   source bullet doesn't; skills must resolve to evidence-backed
//!   entries. Violations are dropped or reverted with a warning — never
//!   silently accepted.
//! - Contact, education, and certifications are copied **verbatim from
//!   the dataset**; the model never even sees a place to alter them.
//!
//! The summary is the one stretch of free prose, held only by the
//! prompt until Phase 3's adversarial reviewer arrives.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::dataset::types::{
    Bullet, BulletId, Certification, Contact, Education, ProjectId, ResumeDataset, Role, RoleId,
    Strength, YearMonth,
};
use crate::gap::GapReport;
use crate::jd::JobRequirements;
use crate::mirror;
use async_trait::async_trait;

use crate::agent::{Agent, AgentContext};
use crate::llm::{LlmError, TokenUsage};

/// Selection output is compact (IDs + reworded lines), but resumes with
/// many roles need room.
const REPLY_BUDGET: u32 = 8192;

/// Everything that can go wrong while tailoring.
#[derive(Debug, thiserror::Error)]
pub enum TailorError {
    #[error(transparent)]
    Llm(#[from] LlmError),

    #[error("the model's reply was not the expected selection JSON (reply began {snippet:?})")]
    BadReply {
        snippet: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("the model selected nothing usable from the dataset")]
    EmptySelection,
}

// ---------------------------------------------------------------------
// The canonical output (PRD names, used verbatim)
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BuildId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JdId(pub String);

/// Canonical, variant-agnostic tailored output: one per build iteration.
/// In Phase 1 this is also exactly what the ATS template renders; the
/// variant projection layer arrives in Phase 5.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TailoredResume {
    pub build_id: BuildId,
    pub jd_id: JdId,
    pub generated_at: DateTime<Utc>,
    pub contact: Contact,
    /// The role being applied for, shown as a headline under the name.
    /// It's the JD's title, not a claim of having held it — the work
    /// history below still carries the real titles. `serde(default)`
    /// keeps older build artifacts deserializing.
    #[serde(default)]
    pub target_title: Option<String>,
    /// 2-3 sentences, the one free-prose field (prompt-held until the
    /// Phase 3 reviewer).
    pub summary: String,
    /// Selected roles in presentation order, each with selected bullets.
    pub roles: Vec<TailoredRole>,
    pub education: Vec<Education>,
    /// Evidence-backed skills, ordered by JD relevance.
    pub skills_section: SkillsSection,
    pub projects: Vec<TailoredProject>,
    pub certifications: Vec<Certification>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TailoredRole {
    pub id: RoleId,
    pub company: String,
    pub title: String,
    pub start: YearMonth,
    pub end: Option<YearMonth>,
    pub location: Option<String>,
    pub bullets: Vec<TailoredBullet>,
}

/// One selected (possibly reworded) resume line, traceable to its
/// source. `source_id` is the structural half of never-fabricate: every
/// line on the page points back at recorded material.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TailoredBullet {
    pub source_id: BulletId,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillsSection {
    pub skills: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TailoredProject {
    pub id: ProjectId,
    pub name: String,
    pub summary: String,
    pub url: Option<String>,
}

/// What tailoring produced: the canonical resume, anything the guards
/// had to drop or revert, and the tokens it cost.
#[derive(Debug)]
pub struct TailorOutcome {
    pub resume: TailoredResume,
    pub warnings: Vec<String>,
    pub usage: TokenUsage,
}

/// A revision pass's marching orders: the reviewer's objections,
/// pre-formatted into one human line each by the caller (so this module
/// needs no dependency on the reviewer's types).
#[derive(serde::Serialize)]
pub struct RevisionContext {
    pub objections: Vec<String>,
}

/// Everything one tailoring run works from. Owned: a run's input is a
/// value handed over whole, which is also what tracing will serialize.
#[derive(serde::Serialize)]
pub struct TailorInput {
    pub build_id: BuildId,
    pub jd_id: JdId,
    pub jd: JobRequirements,
    pub dataset: ResumeDataset,
    pub gap: GapReport,
    /// Present on a revision pass; absent on the first draft.
    pub revision: Option<RevisionContext>,
}

/// The tailoring agent: the model proposes; the guards dispose.
pub struct TailoringAgent;

#[async_trait]
impl Agent for TailoringAgent {
    type Input = TailorInput;
    type Wire = RawSelection;
    type Output = (TailoredResume, Vec<String>);
    type Error = TailorError;

    fn id(&self) -> &'static str {
        "tailoring_v1"
    }
    fn system_prompt(&self) -> &str {
        SYSTEM_PROMPT
    }
    fn reply_budget(&self) -> u32 {
        REPLY_BUDGET
    }
    fn user_message(&self, input: &TailorInput) -> String {
        let mut text = build_user_message(&input.jd, &input.dataset, &input.gap);
        if let Some(revision) = &input.revision {
            text.push_str(
                "\nREVISION — a skeptical reviewer flagged your previous draft. \
                 Address each objection by choosing a different bullet, rephrasing \
                 your selection, sharpening a weak verb, or cutting the line. NEVER \
                 invent a metric, technology, scale, or outcome the work history does \
                 not already state. Keep the lines that were strong.\n",
            );
            for line in &revision.objections {
                text.push_str(&format!("- {line}\n"));
            }
        }
        text
    }
    fn bad_reply(&self, snippet: String, source: serde_json::Error) -> TailorError {
        TailorError::BadReply { snippet, source }
    }
    fn assemble(
        &self,
        wire: RawSelection,
        input: TailorInput,
    ) -> Result<(TailoredResume, Vec<String>), TailorError> {
        assemble(
            wire,
            input.build_id,
            input.jd_id,
            &input.jd,
            &input.dataset,
            &input.gap,
        )
    }
}

/// Tailor the dataset to one JD. `revision` carries a prior draft's
/// objections on a revision pass, and is `None` for the first draft.
pub async fn tailor_resume(
    ctx: &AgentContext<'_>,
    build_id: BuildId,
    jd_id: JdId,
    jd: &JobRequirements,
    dataset: &ResumeDataset,
    gap: &GapReport,
    revision: Option<RevisionContext>,
) -> Result<TailorOutcome, TailorError> {
    let input = TailorInput {
        build_id,
        jd_id,
        jd: jd.clone(),
        dataset: dataset.clone(),
        gap: gap.clone(),
        revision,
    };
    let run = TailoringAgent.run(ctx, input).await?;
    let (resume, warnings) = run.output;
    Ok(TailorOutcome {
        resume,
        warnings,
        usage: run.usage,
    })
}

/// The selection contract. The never-fabricate rules here are the
/// prompt-level half of FR-1.7; `assemble` enforces the structural half.
const SYSTEM_PROMPT: &str = r#"You tailor a candidate's recorded work history to one job description. You select and rephrase ONLY from the provided material.

Rules — all of them matter:
- Include EVERY role, and taper rather than collapse: a resume where one role has six bullets and the rest have one looks lopsided. Budget by relevance — roughly 4-6 bullets for the most recent or most relevant role, about 3 for mid roles, and at least 2 for older or less relevant ones (use a single bullet only for a role that has just one recorded). An unexplained employment gap reads worse to a hiring manager than a lightly covered role.
- Keep roles in the order given (most recent first).
- You may rephrase a bullet to mirror the job description's vocabulary, but every fact, number, technology, and outcome must already be in the source bullet. Never add metrics, scale, team sizes, technologies, or results that the source does not state.
- Prefer mirroring the JD's exact phrases (the ats_phrases list) when the underlying fact honestly supports them.
- "summary": 2-3 sentences, factual, drawn only from the work history given. No superlatives the material doesn't earn.
- "skills": the usable skills ordered by relevance to this JD, spelled exactly as given in the usable-skills list. Include only skills from that list. Never mention anything from the do-not-claim list anywhere in your output.
- "projects": ids of projects that strengthen this application; may be empty.
- Reply with exactly one JSON object and nothing else — no markdown fences, no commentary.

The JSON object:
{"summary": "...", "roles": [{"id": "role-1", "bullets": [{"source_id": "bullet-2", "text": "the selected, possibly rephrased line"}]}], "skills": ["..."], "projects": ["project-1"]}"#;

/// Everything the model is allowed to work from, in one message: the
/// JD's asks, the work history with IDs, the usable (evidence-backed)
/// skills, and an explicit do-not-claim list from the gap report.
fn build_user_message(jd: &JobRequirements, dataset: &ResumeDataset, gap: &GapReport) -> String {
    let mut text = String::new();

    text.push_str(&format!(
        "THE JOB\ncompany: {}\ntitle: {}\n",
        jd.company, jd.title
    ));
    text.push_str("required skills: ");
    text.push_str(
        &jd.required_skills
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
    );
    text.push_str("\npreferred skills: ");
    text.push_str(
        &jd.preferred_skills
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
    );
    if !jd.ats_phrases.is_empty() {
        text.push_str(&format!("\nats_phrases: {}", jd.ats_phrases.join(" | ")));
    }
    if !jd.responsibilities.is_empty() {
        text.push_str("\nresponsibilities:\n");
        for r in jd.responsibilities.iter().take(10) {
            text.push_str(&format!("- {r}\n"));
        }
    }

    text.push_str("\nWORK HISTORY\n");
    for role in &dataset.roles {
        let end = role
            .end
            .map_or_else(|| "present".to_string(), |ym| ym.to_string());
        text.push_str(&format!(
            "{}: {} at {} ({} to {})\n",
            role.id.0, role.title, role.company, role.start, end
        ));
        if let Some(context) = &role.context {
            text.push_str(&format!("  context: {context}\n"));
        }
        for bullet in &role.bullets {
            text.push_str(&format!("  {}: {}\n", bullet.id.0, bullet.text));
        }
    }

    text.push_str("\nUSABLE SKILLS (evidence-backed)\n");
    let jd_coverage: HashMap<&str, &str> = gap
        .matched
        .iter()
        .map(|m| (m.dataset_name.as_str(), m.jd_skill.name.as_str()))
        .collect();
    for skill in &dataset.skills.skills {
        if skill.evidence.is_empty() {
            continue;
        }
        match jd_coverage.get(skill.canonical_name.as_str()) {
            Some(jd_name) if *jd_name != skill.canonical_name => text.push_str(&format!(
                "- {} (covers the JD's {:?})\n",
                skill.canonical_name, jd_name
            )),
            _ => text.push_str(&format!("- {}\n", skill.canonical_name)),
        }
    }

    if !dataset.projects.is_empty() {
        text.push_str("\nPROJECTS\n");
        for project in &dataset.projects {
            text.push_str(&format!(
                "{}: {} — {}\n",
                project.id.0, project.name, project.summary
            ));
        }
    }

    let mut do_not_claim: Vec<&str> = gap.unknown.iter().map(|s| s.name.as_str()).collect();
    for weak in &gap.weak {
        if weak.weakness == crate::gap::Weakness::NoEvidence {
            do_not_claim.push(weak.matched.dataset_name.as_str());
        }
    }
    if !do_not_claim.is_empty() {
        text.push_str(&format!(
            "\nDO NOT CLAIM (no evidence in the dataset): {}\n",
            do_not_claim.join(", ")
        ));
    }

    text
}

// ---------------------------------------------------------------------
// The wire shape the model replies with
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct RawSelection {
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    roles: Vec<RawRoleSelection>,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    projects: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawRoleSelection {
    id: String,
    #[serde(default)]
    bullets: Vec<RawBulletSelection>,
}

#[derive(Debug, Deserialize)]
struct RawBulletSelection {
    source_id: String,
    text: String,
}

// ---------------------------------------------------------------------
// Assembly: every claim checked against the dataset
// ---------------------------------------------------------------------

// EXERCISE(EX-011)
fn assemble(
    raw: RawSelection,
    build_id: BuildId,
    jd_id: JdId,
    jd: &JobRequirements,
    dataset: &ResumeDataset,
    gap: &GapReport,
) -> Result<(TailoredResume, Vec<String>), TailorError> {
    let mut warnings = Vec::new();

    // Index the model's picks by role ID, then walk the DATASET's roles
    // in order. Two guarantees fall out: chronology is the dataset's
    // (the model cannot reorder a work history), and every role appears
    // on the page — an unexplained employment gap invites worse
    // questions from a hiring manager than a lightly covered role.
    let mut picks: HashMap<String, RawRoleSelection> = raw
        .roles
        .into_iter()
        .map(|selection| (selection.id.clone(), selection))
        .collect();

    let mut selected_any = false;
    let mut roles = Vec::new();
    for role in &dataset.roles {
        let mut bullets: Vec<TailoredBullet> = Vec::new();
        // Tracks which of the role's bullets are already on the page, so
        // the floor top-up below never duplicates one.
        let mut used: HashSet<String> = HashSet::new();
        match picks.remove(role.id.0.as_str()) {
            Some(selection) => {
                selected_any = true;
                let bullets_by_id: HashMap<&str, &Bullet> =
                    role.bullets.iter().map(|b| (b.id.0.as_str(), b)).collect();
                for picked in selection.bullets {
                    let Some(source) = bullets_by_id.get(picked.source_id.as_str()) else {
                        warnings.push(format!(
                            "the model cited bullet {:?} under {}, but that bullet is not in that role; dropped",
                            picked.source_id, role.id.0
                        ));
                        continue;
                    };
                    if !used.insert(picked.source_id.clone()) {
                        continue; // same source selected twice; keep the first
                    }
                    let text = if digit_runs(&picked.text).is_subset(&digit_runs(&source.text)) {
                        picked.text
                    } else {
                        warnings.push(format!(
                            "a rewrite of {} added numbers its source doesn't state; kept the original wording",
                            picked.source_id
                        ));
                        source.text.clone()
                    };
                    bullets.push(TailoredBullet {
                        source_id: source.id.clone(),
                        text,
                    });
                }
                if bullets.is_empty() {
                    warnings.push(format!(
                        "none of the model's picks for {} were usable; kept its own strongest instead",
                        role.id.0
                    ));
                }
            }
            None => {
                // The model omitted this role entirely; the floor below
                // keeps it on the page so the work history stays continuous.
                warnings.push(format!(
                    "the model omitted {} ({}); kept it to avoid an employment gap",
                    role.id.0, role.company
                ));
            }
        }
        // Per-role floor: top up to MIN_BULLETS_PER_ROLE from the role's
        // strongest unused bullets. This is what stops the resume going
        // lopsided when the model lavishes the recent role and leaves the
        // older ones a single line each.
        top_up(role, &mut bullets, &mut used, MIN_BULLETS_PER_ROLE);
        roles.push(TailoredRole {
            id: role.id.clone(),
            company: role.company.clone(),
            title: role.title.clone(),
            start: role.start,
            end: role.end,
            location: role.location.clone(),
            bullets,
        });
    }
    // Whatever's left in the map cited roles that don't exist.
    for id in picks.into_keys() {
        warnings.push(format!(
            "the model selected role {id:?}, which is not in the dataset; dropped"
        ));
    }
    // The continuity guarantee can fill bullets, but it must not paper
    // over a reply that selected nothing real — that's a failed run.
    if !selected_any || roles.is_empty() {
        return Err(TailorError::EmptySelection);
    }

    // Skills: resolve each proposed name; only evidence-backed entries
    // survive, under their canonical spelling. An empty result falls
    // back to the gap report's matches — deterministic and backed.
    let mut skills = Vec::new();
    let mut seen = HashSet::new();
    for name in &raw.skills {
        let resolved = dataset
            .skills
            .aliases
            .get(&name.to_lowercase())
            .and_then(|id| dataset.skills.skills.iter().find(|s| s.id == *id));
        match resolved {
            Some(skill) if !skill.evidence.is_empty() => {
                if seen.insert(skill.canonical_name.clone()) {
                    skills.push(skill.canonical_name.clone());
                }
            }
            Some(skill) => warnings.push(format!(
                "the model listed {:?}, which has no evidence; dropped",
                skill.canonical_name
            )),
            None => warnings.push(format!(
                "the model listed {name:?}, which is not a recorded skill; dropped"
            )),
        }
    }
    if skills.is_empty() {
        warnings.push("the model proposed no usable skills; using the gap report's matches".into());
        for m in &gap.matched {
            if seen.insert(m.dataset_name.clone()) {
                skills.push(m.dataset_name.clone());
            }
        }
    }

    // Evidence-gated phrase mirroring: add the JD's wording for any
    // keyword a recorded skill already backs, so a literal ATS scan
    // credits a concept the user genuinely has but words differently.
    // `mirror::backed_phrases` is the gate — it returns only phrases
    // subsumed by a recorded skill, never an unbacked one — so this is
    // the single sanctioned path for a JD phrase to reach the page
    // without being literally in the dataset.
    for matched in mirror::backed_phrases(jd, dataset) {
        if seen.insert(matched.phrase.clone()) {
            skills.push(matched.phrase.clone());
            warnings.push(format!(
                "mirrored {:?} into skills — backed by your {:?}",
                matched.phrase, matched.dataset_skill
            ));
        }
    }

    let mut projects = Vec::new();
    for id in &raw.projects {
        match dataset.projects.iter().find(|p| p.id.0 == *id) {
            Some(project) => projects.push(TailoredProject {
                id: project.id.clone(),
                name: project.name.clone(),
                summary: project.summary.clone(),
                url: project.url.clone(),
            }),
            None => warnings.push(format!(
                "the model selected project {id:?}, which is not in the dataset; dropped"
            )),
        }
    }

    let summary = match raw.summary {
        Some(s) if !s.trim().is_empty() => s,
        _ => {
            warnings.push("the model wrote no summary; using the dataset's own summary".into());
            dataset.summary.clone().unwrap_or_default()
        }
    };

    Ok((
        TailoredResume {
            build_id,
            jd_id,
            generated_at: Utc::now(),
            contact: dataset.contact.clone(),
            target_title: Some(jd.title.clone()),
            summary,
            roles,
            education: dataset.education.clone(),
            skills_section: SkillsSection { skills },
            projects,
            certifications: dataset.certifications.clone(),
        },
        warnings,
    ))
}

/// The fewest bullets any included role should carry, so the resume
/// tapers instead of collapsing — one role with six lines and the rest
/// with one reads lopsided. Capped at what a role actually has.
const MIN_BULLETS_PER_ROLE: usize = 2;

/// Top a role's bullets up to `floor` (capped at what it has) from its
/// strongest *unused* bullets. This is the deterministic half of keeping
/// the resume balanced: whatever the model selected (or omitted), no role
/// on the page drops below the floor while it still has lines to show.
/// Strongest first; ties keep dataset order (`sort_by_key` is stable). A
/// role with no bullets at all renders title-only, which still beats a
/// gap. The topped-up lines are the user's verbatim source text — no
/// rewrite, so nothing to fabricate.
fn top_up(
    role: &Role,
    bullets: &mut Vec<TailoredBullet>,
    used: &mut HashSet<String>,
    floor: usize,
) {
    if bullets.len() >= floor {
        return;
    }
    let mut unused: Vec<&Bullet> = role
        .bullets
        .iter()
        .filter(|b| !used.contains(&b.id.0))
        .collect();
    unused.sort_by_key(|b| match b.strength {
        Strength::High => 0u8,
        Strength::Medium => 1,
        Strength::Low => 2,
    });
    for bullet in unused {
        if bullets.len() >= floor {
            break;
        }
        used.insert(bullet.id.0.clone());
        bullets.push(TailoredBullet {
            source_id: bullet.id.clone(),
            text: bullet.text.clone(),
        });
    }
}

/// The maximal runs of consecutive digits in a string — "cut p99 by 40%"
/// yields {"99", "40"}. The fabrication guard compares these sets: a
/// rewrite may drop or repeat numbers, but never introduce one. Shared
/// with the voice rewriter, which applies the same no-invented-numbers
/// check to its phrasing changes.
pub(crate) fn digit_runs(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_ascii_digit())
        .filter(|run| !run.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::dataset::types::{
        Bullet, Contact, EmploymentType, EvidenceRef, Metric, Proficiency, Skill, SkillCategory,
        SkillId, Strength,
    };
    use crate::gap::{SkillMatch, WeakMatch, Weakness};
    use crate::jd::{Importance, JdSkill, RemotePolicy, Seniority};
    use crate::llm::MockLlmClient;

    fn test_ctx(mock: &MockLlmClient) -> AgentContext<'_> {
        AgentContext {
            llm: mock,
            model: "test-model",
            tracer: &crate::trace::Tracer::DISABLED,
        }
    }

    fn bullet(id: &str, text: &str) -> Bullet {
        Bullet {
            id: BulletId(id.into()),
            text: text.into(),
            skill_ids: Vec::new(),
            metric: Some(Metric("placeholder".into())),
            theme: Vec::new(),
            strength: Strength::Medium,
            variants: Vec::new(),
        }
    }

    fn sample_dataset() -> ResumeDataset {
        let mut dataset = ResumeDataset::new(Contact {
            full_name: "Ada Lovelace".into(),
            email: "ada@example.com".into(),
            phone: None,
            location: Some("London".into()),
            links: Vec::new(),
        });
        dataset.roles.push(Role {
            id: RoleId("role-1".into()),
            company: "Analytical Engines Ltd".into(),
            title: "Director of Engineering".into(),
            start: YearMonth {
                year: 2020,
                month: 3,
            },
            end: None,
            location: None,
            employment_type: EmploymentType::FullTime,
            bullets: vec![
                bullet("bullet-1", "Led a team of 12 engineers across 3 squads"),
                bullet("bullet-2", "Cut deploy time from 45 minutes to 8"),
            ],
            skill_ids: Vec::new(),
            context: None,
        });
        dataset.roles.push(Role {
            id: RoleId("role-2".into()),
            company: "Babbage & Co".into(),
            title: "Engineer".into(),
            start: YearMonth {
                year: 2016,
                month: 1,
            },
            end: Some(YearMonth {
                year: 2020,
                month: 2,
            }),
            location: None,
            employment_type: EmploymentType::FullTime,
            bullets: vec![bullet("bullet-3", "Built the settlement pipeline")],
            skill_ids: Vec::new(),
            context: None,
        });
        for (id, name, evidenced) in [
            ("skill-1", "Engineering management", true),
            ("skill-2", "Rust", true),
            ("skill-3", "TypeScript", false),
        ] {
            dataset.skills.skills.push(Skill {
                id: SkillId(id.into()),
                canonical_name: name.into(),
                aliases: Vec::new(),
                category: SkillCategory::Hard,
                proficiency: Proficiency::Proficient,
                years: None,
                last_used: None,
                evidence: if evidenced {
                    vec![EvidenceRef::Role(RoleId("role-1".into()))]
                } else {
                    Vec::new()
                },
                verified: false,
                verified_at: None,
            });
            dataset
                .skills
                .aliases
                .insert(name.to_lowercase(), SkillId(id.into()));
        }
        dataset
    }

    fn sample_jd() -> JobRequirements {
        JobRequirements {
            company: "amplo".into(),
            title: "Senior Engineering Manager".into(),
            seniority: Seniority::Senior,
            location: None,
            remote: RemotePolicy::Remote,
            domain_keywords: Vec::new(),
            required_skills: vec![JdSkill {
                name: "Engineering Management".into(),
                category: SkillCategory::Soft,
                importance: Importance::Critical,
                context_phrases: Vec::new(),
            }],
            preferred_skills: Vec::new(),
            responsibilities: vec!["Own delivery".into()],
            ats_phrases: vec!["engineering excellence".into()],
            raw_text: "raw".into(),
            source_url: None,
        }
    }

    fn sample_gap() -> GapReport {
        GapReport {
            matched: vec![SkillMatch {
                jd_skill: sample_jd().required_skills[0].clone(),
                skill_id: SkillId("skill-1".into()),
                dataset_name: "Engineering management".into(),
                semantic: false,
            }],
            weak: vec![WeakMatch {
                matched: SkillMatch {
                    jd_skill: JdSkill {
                        name: "TypeScript".into(),
                        category: SkillCategory::Language,
                        importance: Importance::Required,
                        context_phrases: Vec::new(),
                    },
                    skill_id: SkillId("skill-3".into()),
                    dataset_name: "TypeScript".into(),
                    semantic: false,
                },
                weakness: Weakness::NoEvidence,
            }],
            unknown: vec![JdSkill {
                name: "Kafka".into(),
                category: SkillCategory::Tool,
                importance: Importance::Required,
                context_phrases: Vec::new(),
            }],
        }
    }

    async fn run_tailor(reply: &str) -> Result<TailorOutcome, TailorError> {
        let mock = MockLlmClient::default();
        mock.enqueue(reply);
        tailor_resume(
            &test_ctx(&mock),
            BuildId("001".into()),
            JdId("amplo-senior-engineering-manager".into()),
            &sample_jd(),
            &sample_dataset(),
            &sample_gap(),
            None,
        )
        .await
    }

    #[tokio::test]
    async fn a_clean_selection_assembles_with_dataset_facts_intact() {
        let outcome = run_tailor(
            r#"{"summary": "Engineering leader with delivery focus.",
                "roles": [
                  {"id": "role-1", "bullets": [
                    {"source_id": "bullet-2", "text": "Drove engineering excellence, cutting deploy time from 45 minutes to 8"},
                    {"source_id": "bullet-1", "text": "Led a team of 12 engineers across 3 squads"}
                  ]},
                  {"id": "role-2", "bullets": [
                    {"source_id": "bullet-3", "text": "Built the settlement pipeline"}
                  ]}
                ],
                "skills": ["Engineering management", "Rust"],
                "projects": []}"#,
        )
        .await
        .unwrap();

        let resume = outcome.resume;
        assert_eq!(resume.roles.len(), 2);
        // The model's ordering of bullets is preserved.
        assert_eq!(
            resume.roles[0].bullets[0].source_id,
            BulletId("bullet-2".into())
        );
        // Rewording that mirrors the JD but adds no numbers survives.
        assert!(
            resume.roles[0].bullets[0]
                .text
                .starts_with("Drove engineering excellence")
        );
        // Contact and education come from the dataset, not the model.
        assert_eq!(resume.contact.full_name, "Ada Lovelace");
        // The headline is the JD's title, derived — not a hardcoded or
        // model-chosen value.
        assert_eq!(
            resume.target_title.as_deref(),
            Some(sample_jd().title.as_str())
        );
        assert_eq!(
            resume.skills_section.skills,
            vec!["Engineering management", "Rust"]
        );
        assert!(outcome.warnings.is_empty(), "got: {:?}", outcome.warnings);
    }

    #[tokio::test]
    async fn a_backed_jd_phrase_is_mirrored_but_an_unbacked_one_is_not() {
        let mut jd = sample_jd();
        // Neutral title so the title guard doesn't filter the test phrase
        // (the default title also normalizes to "engineering management").
        jd.title = "Staff Architect".into();
        // "managing engineering" is the recorded "Engineering management"
        // skill in the JD's words; "blockchain custody" backs to nothing.
        jd.ats_phrases = vec!["managing engineering".into(), "blockchain custody".into()];
        let mock = MockLlmClient::default();
        mock.enqueue(
            r#"{"summary":"Lead.",
                "roles":[{"id":"role-1","bullets":[
                  {"source_id":"bullet-1","text":"Led a team of 12 engineers across 3 squads"}
                ]}],
                "skills":["Engineering management"],
                "projects":[]}"#,
        );

        let outcome = tailor_resume(
            &test_ctx(&mock),
            BuildId("001".into()),
            JdId("x".into()),
            &jd,
            &sample_dataset(),
            &sample_gap(),
            None,
        )
        .await
        .unwrap();

        let skills = &outcome.resume.skills_section.skills;
        // The backed wording variant is surfaced for the ATS scan...
        assert!(
            skills.iter().any(|s| s == "managing engineering"),
            "got {skills:?}"
        );
        // ...and the unbacked phrase never reaches the page (backdoor shut).
        assert!(!skills.iter().any(|s| s == "blockchain custody"));
        // The mirror is recorded so the build is auditable.
        assert!(
            outcome
                .warnings
                .iter()
                .any(|w| w.contains("mirrored") && w.contains("managing engineering"))
        );
    }

    #[tokio::test]
    async fn rewrites_that_invent_numbers_revert_to_the_source_text() {
        let outcome = run_tailor(
            r#"{"summary": "s",
                "roles": [{"id": "role-1", "bullets": [
                  {"source_id": "bullet-1", "text": "Led a team of 20 engineers across 3 squads"}
                ]}],
                "skills": ["Rust"], "projects": []}"#,
        )
        .await
        .unwrap();

        // "20" is not in the source ("12 engineers, 3 squads") — revert.
        assert_eq!(
            outcome.resume.roles[0].bullets[0].text,
            "Led a team of 12 engineers across 3 squads"
        );
        assert!(outcome.warnings.iter().any(|w| w.contains("added numbers")));
    }

    #[tokio::test]
    async fn unbacked_and_unknown_skills_are_dropped_from_the_section() {
        let outcome = run_tailor(
            r#"{"summary": "s",
                "roles": [{"id": "role-1", "bullets": [
                  {"source_id": "bullet-1", "text": "Led a team of 12 engineers across 3 squads"}
                ]}],
                "skills": ["TypeScript", "Kafka", "Rust"],
                "projects": []}"#,
        )
        .await
        .unwrap();

        // TypeScript exists but has no evidence; Kafka isn't recorded.
        assert_eq!(outcome.resume.skills_section.skills, vec!["Rust"]);
        assert!(outcome.warnings.iter().any(|w| w.contains("no evidence")));
        assert!(
            outcome
                .warnings
                .iter()
                .any(|w| w.contains("not a recorded skill"))
        );
    }

    #[tokio::test]
    async fn bullets_cited_under_the_wrong_role_are_dropped() {
        let outcome = run_tailor(
            r#"{"summary": "s",
                "roles": [
                  {"id": "role-1", "bullets": [
                    {"source_id": "bullet-3", "text": "Built the settlement pipeline"},
                    {"source_id": "bullet-1", "text": "Led a team of 12 engineers across 3 squads"}
                  ]},
                  {"id": "role-9", "bullets": [
                    {"source_id": "bullet-1", "text": "x"}
                  ]}
                ],
                "skills": ["Rust"], "projects": ["project-7"]}"#,
        )
        .await
        .unwrap();

        // bullet-3 belongs to role-2, role-9 and project-7 don't exist —
        // and role-2, which the model skipped, is kept for continuity.
        assert_eq!(outcome.resume.roles.len(), 2);
        // role-1 kept its one valid pick (bullet-1) and the floor topped
        // it up with the role's other bullet (bullet-2) — never the
        // wrong-role bullet-3.
        let role1_ids: Vec<&str> = outcome.resume.roles[0]
            .bullets
            .iter()
            .map(|b| b.source_id.0.as_str())
            .collect();
        assert_eq!(role1_ids, vec!["bullet-1", "bullet-2"]);
        assert_eq!(
            outcome.resume.roles[1].bullets[0].source_id,
            BulletId("bullet-3".into())
        );
        assert!(outcome.resume.projects.is_empty());
        assert_eq!(outcome.warnings.len(), 4, "got: {:?}", outcome.warnings);
    }

    #[tokio::test]
    async fn omitted_roles_are_kept_to_avoid_employment_gaps() {
        let outcome = run_tailor(
            r#"{"summary": "s",
                "roles": [{"id": "role-1", "bullets": [
                  {"source_id": "bullet-1", "text": "Led a team of 12 engineers across 3 squads"}
                ]}],
                "skills": ["Rust"], "projects": []}"#,
        )
        .await
        .unwrap();

        // The model only selected role-1; role-2 still appears, in
        // dataset (chronological) order, carrying its strongest bullet.
        let resume = outcome.resume;
        assert_eq!(resume.roles.len(), 2);
        // role-1 was given a single bullet but has two — the floor tops
        // it up so it doesn't sit at one line.
        assert_eq!(resume.roles[0].bullets.len(), 2);
        assert_eq!(resume.roles[1].id, RoleId("role-2".into()));
        // role-2 has only one recorded bullet, so the floor can't pad it
        // past what it has.
        assert_eq!(resume.roles[1].bullets.len(), 1);
        assert_eq!(
            resume.roles[1].bullets[0].source_id,
            BulletId("bullet-3".into())
        );
        assert!(
            outcome
                .warnings
                .iter()
                .any(|w| w.contains("employment gap")),
            "got: {:?}",
            outcome.warnings
        );
    }

    fn role_of(bullets: Vec<Bullet>) -> Role {
        Role {
            id: RoleId("role-1".into()),
            company: "Acme".into(),
            title: "Eng".into(),
            start: YearMonth {
                year: 2020,
                month: 1,
            },
            end: None,
            location: None,
            employment_type: EmploymentType::FullTime,
            bullets,
            skill_ids: Vec::new(),
            context: None,
        }
    }

    fn graded(id: &str, strength: Strength) -> Bullet {
        Bullet {
            strength,
            ..bullet(id, "text")
        }
    }

    #[test]
    fn top_up_fills_to_the_floor_strongest_first_without_reusing() {
        let role = role_of(vec![
            graded("b-weak", Strength::Low),
            graded("b-strong", Strength::High),
            graded("b-mid", Strength::Medium),
        ]);
        // b-strong is already on the page; the floor adds exactly one
        // more — the strongest of what's left (b-mid over b-weak).
        let mut bullets = vec![TailoredBullet {
            source_id: BulletId("b-strong".into()),
            text: "x".into(),
        }];
        let mut used: std::collections::HashSet<String> =
            ["b-strong".to_string()].into_iter().collect();

        top_up(&role, &mut bullets, &mut used, 2);

        let ids: Vec<&str> = bullets.iter().map(|b| b.source_id.0.as_str()).collect();
        assert_eq!(ids, vec!["b-strong", "b-mid"]);
    }

    #[test]
    fn top_up_caps_at_what_the_role_actually_has() {
        let role = role_of(vec![graded("only", Strength::Medium)]);
        let mut bullets = Vec::new();
        let mut used = std::collections::HashSet::new();

        // Floor of 2, but the role has one bullet — it can't be padded.
        top_up(&role, &mut bullets, &mut used, 2);

        assert_eq!(bullets.len(), 1);
        assert_eq!(bullets[0].source_id, BulletId("only".into()));
    }

    #[test]
    fn top_up_leaves_a_role_already_at_the_floor_untouched() {
        let role = role_of(vec![
            graded("a", Strength::High),
            graded("b", Strength::High),
        ]);
        let mut bullets = vec![TailoredBullet {
            source_id: BulletId("a".into()),
            text: "x".into(),
        }];
        let mut used: std::collections::HashSet<String> = ["a".to_string()].into_iter().collect();

        top_up(&role, &mut bullets, &mut used, 1); // already at floor 1

        assert_eq!(bullets.len(), 1); // unchanged
    }

    #[tokio::test]
    async fn selecting_nothing_usable_is_a_typed_error() {
        let err = run_tailor(r#"{"summary": "s", "roles": [], "skills": [], "projects": []}"#)
            .await
            .unwrap_err();
        assert!(matches!(err, TailorError::EmptySelection));
    }

    #[tokio::test]
    async fn the_prompt_carries_the_do_not_claim_list_and_omits_unbacked_skills() {
        let mock = MockLlmClient::default();
        mock.enqueue(
            r#"{"summary": "s",
                "roles": [{"id": "role-1", "bullets": [
                  {"source_id": "bullet-1", "text": "Led a team of 12 engineers across 3 squads"}
                ]}],
                "skills": ["Rust"], "projects": []}"#,
        );
        tailor_resume(
            &test_ctx(&mock),
            BuildId("001".into()),
            JdId("jd".into()),
            &sample_jd(),
            &sample_dataset(),
            &sample_gap(),
            None,
        )
        .await
        .unwrap();

        let sent = &mock.requests()[0].messages[0].content;
        let (usable, after) = sent.split_once("DO NOT CLAIM").unwrap();
        // Unknown JD skills and unbacked dataset skills are barred...
        assert!(after.contains("Kafka"));
        assert!(after.contains("TypeScript"));
        // ...and the usable list never offered TypeScript in the first
        // place (it has no evidence).
        let usable_section = usable.split_once("USABLE SKILLS").unwrap().1;
        assert!(!usable_section.contains("TypeScript"));
        assert!(usable_section.contains("Engineering management"));
    }

    #[tokio::test]
    async fn a_malformed_reply_is_a_typed_error_with_a_snippet() {
        // Two bad replies: the spine's validation-retry consumes one.
        let mock = MockLlmClient::default();
        mock.enqueue("Here's a great resume for you!");
        mock.enqueue("Here's a great resume for you!");
        let err = tailor_resume(
            &test_ctx(&mock),
            BuildId("001".into()),
            JdId("jd".into()),
            &sample_jd(),
            &sample_dataset(),
            &sample_gap(),
            None,
        )
        .await
        .unwrap_err();
        match err {
            TailorError::BadReply { snippet, .. } => assert!(snippet.starts_with("Here's")),
            other => panic!("expected BadReply, got {other:?}"),
        }
    }

    #[test]
    fn digit_runs_extracts_maximal_runs() {
        let runs = digit_runs("cut p99 latency 40% in 2024");
        assert_eq!(
            runs,
            ["99", "40", "2024"].iter().map(|s| s.to_string()).collect()
        );
    }

    #[test]
    #[ignore = "exercise: the prompt asks for 3-5 bullets per role but nothing enforces it; cap each role at 5 bullets in assemble, preferring the dataset's strength ratings, then finish this test"]
    fn ex_011_role_bullets_are_capped_at_five() {
        // Once the cap exists: build a role with 8 bullets of mixed
        // Strength, have the model select all 8, and assert the five
        // that survive are the strongest (ties broken by the model's
        // ordering), with a warning naming the dropped count.
        let cap_implemented = false;
        assert!(cap_implemented);
    }
}
