//! Skill verification: the interviews that connect skills to evidence
//! (FR-3.1). Two flows share one per-skill interview:
//!
//! - `verify_unbacked` — recorded skills with an empty `evidence` list
//!   sit in limbo, barred from tailoring by the never-fabricate rule. A
//!   "yes" backs one with role evidence; a "no" removes it.
//! - `verify_unknown` — skills a job description wants that aren't in
//!   the dataset at all (the gap's "unknown" bucket). A "yes" *adds*
//!   the skill with evidence; a "no" leaves it absent. This is what
//!   turns "the job wants Data Engineering, you didn't record it" into
//!   recorded experience — but only if the person genuinely has it.
//!
//! A "yes" always means a real role plus, optionally, years and a
//! user-written sentence (words the user typed are the one source that
//! needs no further proof — they become a bullet). The interview never
//! invents experience; the person affirms it.
//!
//! Transactionality lives at the save boundary: these functions mutate
//! the in-memory dataset and return a summary; the caller saves once
//! through the store (lock + backup + atomic write) only on success.
//! An interview abandoned halfway — Esc, closed terminal — returns an
//! error and the file on disk never changes.

use chrono::Utc;

use crate::dataset::types::{
    Bullet, BulletId, EvidenceRef, Proficiency, ResumeDataset, Skill, SkillCategory, SkillId,
    Strength,
};
use crate::gap::GapReport;
use crate::user::{Answer, AskError, Question, UserHandle};

/// What an interview session accomplished.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct VerifyOutcome {
    pub verified: usize,
    pub removed: usize,
    pub skipped: usize,
    pub bullets_added: usize,
}

impl VerifyOutcome {
    /// Whether anything changed that is worth saving.
    pub fn changed(&self) -> bool {
        self.verified > 0 || self.removed > 0
    }
}

/// The user's verdict on one skill. `Have` carries the role they picked
/// and the optional details; the caller decides what to do with it
/// (back an existing skill, or create a new one).
enum Verdict {
    Have {
        role_index: usize,
        years: Option<f32>,
        sentence: Option<String>,
    },
    DontHave,
    Skip,
}

/// The shared per-skill interview: "do you have this, and where?".
/// `no_label` differs per flow ("remove it" vs "I haven't"), but the
/// questions and their order are identical.
// EXERCISE(EX-016)
async fn interview_skill(
    name: &str,
    no_label: &str,
    role_options: &[String],
    user: &dyn UserHandle,
) -> Result<Verdict, AskError> {
    let answer = user
        .ask(Question::Select {
            prompt: format!("Have you used {name} in any role?"),
            options: vec![
                "Yes, professionally".to_string(),
                "Yes, in a side project".to_string(),
                no_label.to_string(),
                "Skip for now".to_string(),
            ],
        })
        .await?;

    match answer {
        Answer::Choice(0) | Answer::Choice(1) => {
            let role_index = match user
                .ask(Question::Select {
                    prompt: format!("Which role best demonstrates {name}?"),
                    options: role_options.to_vec(),
                })
                .await?
            {
                Answer::Choice(index) if index < role_options.len() => index,
                _ => 0,
            };
            let years = match user
                .ask(Question::Text {
                    prompt: "Roughly how many years? (blank to skip)".to_string(),
                })
                .await?
            {
                Answer::Text(text) => parse_years(&text),
                _ => None,
            };
            let sentence = match user
                .ask(Question::Text {
                    prompt: format!("One sentence on what you did with {name} (blank to skip)"),
                })
                .await?
            {
                Answer::Text(text) => {
                    let text = text.trim().to_string();
                    (!text.is_empty()).then_some(text)
                }
                _ => None,
            };
            Ok(Verdict::Have {
                role_index,
                years,
                sentence,
            })
        }
        Answer::Choice(2) => Ok(Verdict::DontHave),
        _ => Ok(Verdict::Skip),
    }
}

/// Attach evidence for a skill that already exists in the dataset: a
/// role reference, optional years, and (if the user wrote one) a
/// bullet. Used by both flows once a skill id is in hand.
fn attach_evidence(
    dataset: &mut ResumeDataset,
    skill_id: &SkillId,
    role_index: usize,
    years: Option<f32>,
    sentence: Option<String>,
    outcome: &mut VerifyOutcome,
) {
    let role_id = dataset.roles[role_index].id.clone();
    if let Some(text) = sentence {
        let bullet_id = next_bullet_id(dataset);
        dataset.roles[role_index].bullets.push(Bullet {
            id: bullet_id,
            text,
            skill_ids: vec![skill_id.clone()],
            metric: None,
            theme: Vec::new(),
            strength: Strength::Medium,
            variants: Vec::new(),
        });
        if !dataset.roles[role_index].skill_ids.contains(skill_id) {
            dataset.roles[role_index].skill_ids.push(skill_id.clone());
        }
        outcome.bullets_added += 1;
    }
    if let Some(skill) = dataset.skills.skills.iter_mut().find(|s| s.id == *skill_id) {
        skill.evidence.push(EvidenceRef::Role(role_id));
        skill.verified = true;
        skill.verified_at = Some(Utc::now());
        if years.is_some() {
            skill.years = years;
        }
    }
}

/// Interview the user about every skill with no evidence behind it.
pub async fn verify_unbacked(
    dataset: &mut ResumeDataset,
    user: &dyn UserHandle,
) -> Result<VerifyOutcome, AskError> {
    let unbacked: Vec<SkillId> = dataset
        .skills
        .skills
        .iter()
        .filter(|skill| skill.evidence.is_empty())
        .map(|skill| skill.id.clone())
        .collect();

    let mut outcome = VerifyOutcome::default();
    if unbacked.is_empty() {
        user.notify("every recorded skill is already evidence-backed");
        return Ok(outcome);
    }
    if dataset.roles.is_empty() {
        user.notify("the dataset has no roles to attach evidence to — ingest a resume first");
        return Ok(outcome);
    }

    let role_options = role_options(dataset);

    for skill_id in unbacked {
        let Some(skill) = dataset.skills.skills.iter().find(|s| s.id == skill_id) else {
            continue; // removed earlier in this same session
        };
        let name = skill.canonical_name.clone();

        match interview_skill(
            &name,
            "No - remove it from the dataset",
            &role_options,
            user,
        )
        .await?
        {
            Verdict::Have {
                role_index,
                years,
                sentence,
            } => {
                attach_evidence(
                    dataset,
                    &skill_id,
                    role_index,
                    years,
                    sentence,
                    &mut outcome,
                );
                user.notify(&format!("added evidence for {name}"));
                outcome.verified += 1;
            }
            Verdict::DontHave => {
                remove_skill(dataset, &skill_id);
                user.notify(&format!("removed {name}"));
                outcome.removed += 1;
            }
            Verdict::Skip => {
                outcome.skipped += 1;
            }
        }
    }

    Ok(outcome)
}

/// Interview the user about JD requirements that aren't in the dataset
/// — the gap's "unknown" bucket — and add the ones they genuinely have.
/// A new skill is created only on a "yes" backed by a real role.
pub async fn verify_unknown(
    dataset: &mut ResumeDataset,
    gap: &GapReport,
    user: &dyn UserHandle,
) -> Result<VerifyOutcome, AskError> {
    let mut outcome = VerifyOutcome::default();
    if gap.unknown.is_empty() {
        return Ok(outcome);
    }
    if dataset.roles.is_empty() {
        user.notify("the dataset has no roles to attach evidence to — ingest a resume first");
        return Ok(outcome);
    }

    let role_options = role_options(dataset);

    for jd_skill in &gap.unknown {
        let name = jd_skill.name.clone();
        // Defensive: if a prior answer this session already recorded
        // this name, don't ask twice or create a duplicate.
        if dataset.skills.aliases.contains_key(&name.to_lowercase()) {
            continue;
        }

        match interview_skill(&name, "No, I haven't", &role_options, user).await? {
            Verdict::Have {
                role_index,
                years,
                sentence,
            } => {
                let skill_id = add_skill(dataset, &name, jd_skill.category);
                attach_evidence(
                    dataset,
                    &skill_id,
                    role_index,
                    years,
                    sentence,
                    &mut outcome,
                );
                user.notify(&format!("added {name}"));
                outcome.verified += 1;
            }
            // The skill isn't in the dataset, so "no" and "skip" both
            // simply leave it out — nothing to remove.
            Verdict::DontHave | Verdict::Skip => {
                outcome.skipped += 1;
            }
        }
    }

    Ok(outcome)
}

/// "Title — Company" for each role, the menu the interview offers.
fn role_options(dataset: &ResumeDataset) -> Vec<String> {
    dataset
        .roles
        .iter()
        .map(|role| format!("{} — {}", role.title, role.company))
        .collect()
}

/// Create a fresh, verified skill (evidence attached separately). The
/// proficiency defaults to working — a sensible middle for something
/// the user just affirmed; it's a self-assessment, not a claim about
/// specific evidence.
fn add_skill(dataset: &mut ResumeDataset, name: &str, category: SkillCategory) -> SkillId {
    let id = next_skill_id(dataset);
    dataset.skills.skills.push(Skill {
        id: id.clone(),
        canonical_name: name.to_string(),
        aliases: Vec::new(),
        category,
        proficiency: Proficiency::Working,
        years: None,
        last_used: None,
        evidence: Vec::new(),
        verified: true,
        verified_at: Some(Utc::now()),
    });
    dataset
        .skills
        .aliases
        .insert(name.to_lowercase(), id.clone());
    id
}

/// New skill IDs continue the `skill-N` sequence.
fn next_skill_id(dataset: &ResumeDataset) -> SkillId {
    let highest = dataset
        .skills
        .skills
        .iter()
        .filter_map(|s| s.id.0.strip_prefix("skill-")?.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    SkillId(format!("skill-{}", highest + 1))
}

/// Pull a leading number out of free text: "2", "2.5 years", " 3 ".
fn parse_years(text: &str) -> Option<f32> {
    // split_whitespace already ignores leading whitespace.
    text.split_whitespace()
        .next()
        .and_then(|word| word.parse().ok())
}

/// New bullet IDs continue the existing `bullet-N` sequence.
fn next_bullet_id(dataset: &ResumeDataset) -> BulletId {
    let highest = dataset
        .roles
        .iter()
        .flat_map(|role| role.bullets.iter())
        .filter_map(|bullet| bullet.id.0.strip_prefix("bullet-")?.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    BulletId(format!("bullet-{}", highest + 1))
}

/// Remove a skill and every reference to it — the alias map and any
/// bullet/role/project/achievement that lists it. Leaving references
/// dangling would just hand `dataset validate` a new problem.
fn remove_skill(dataset: &mut ResumeDataset, id: &SkillId) {
    dataset.skills.skills.retain(|skill| skill.id != *id);
    dataset.skills.aliases.retain(|_, mapped| mapped != id);
    for role in &mut dataset.roles {
        role.skill_ids.retain(|s| s != id);
        for bullet in &mut role.bullets {
            bullet.skill_ids.retain(|s| s != id);
        }
    }
    for project in &mut dataset.projects {
        project.skill_ids.retain(|s| s != id);
    }
    for achievement in &mut dataset.achievements {
        achievement.skill_ids.retain(|s| s != id);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::dataset::types::{
        Contact, EmploymentType, Proficiency, Role, RoleId, Skill, SkillCategory, YearMonth,
    };
    use crate::user::ScriptedUser;

    fn dataset_with_unbacked(name: &str) -> ResumeDataset {
        let mut dataset = ResumeDataset::new(Contact {
            full_name: "Ada".into(),
            email: "ada@example.com".into(),
            phone: None,
            location: None,
            links: Vec::new(),
        });
        dataset.roles.push(Role {
            id: RoleId("role-1".into()),
            company: "Acme".into(),
            title: "Director".into(),
            start: YearMonth {
                year: 2019,
                month: 4,
            },
            end: None,
            location: None,
            employment_type: EmploymentType::FullTime,
            bullets: vec![Bullet {
                id: BulletId("bullet-7".into()),
                text: "Did things".into(),
                skill_ids: Vec::new(),
                metric: None,
                theme: Vec::new(),
                strength: Strength::Medium,
                variants: Vec::new(),
            }],
            skill_ids: Vec::new(),
            context: None,
        });
        dataset.skills.skills.push(Skill {
            id: SkillId("skill-1".into()),
            canonical_name: name.into(),
            aliases: vec!["ts".into()],
            category: SkillCategory::Language,
            proficiency: Proficiency::Proficient,
            years: None,
            last_used: None,
            evidence: Vec::new(),
            verified: false,
            verified_at: None,
        });
        dataset
            .skills
            .aliases
            .insert(name.to_lowercase(), SkillId("skill-1".into()));
        dataset
            .skills
            .aliases
            .insert("ts".into(), SkillId("skill-1".into()));
        dataset
    }

    #[tokio::test]
    async fn a_yes_adds_evidence_years_and_a_user_written_bullet() {
        let mut dataset = dataset_with_unbacked("TypeScript");
        let user = ScriptedUser::new();
        user.answer(Answer::Choice(0)); // yes, professionally
        user.answer(Answer::Choice(0)); // role-1
        user.answer(Answer::Text("4 years".into()));
        user.answer(Answer::Text("Built the trading UI in TypeScript".into()));

        let outcome = verify_unbacked(&mut dataset, &user).await.unwrap();

        assert_eq!(outcome.verified, 1);
        assert_eq!(outcome.bullets_added, 1);
        assert!(outcome.changed());
        let skill = &dataset.skills.skills[0];
        assert_eq!(
            skill.evidence,
            vec![EvidenceRef::Role(RoleId("role-1".into()))]
        );
        assert!(skill.verified);
        assert!(skill.verified_at.is_some());
        assert_eq!(skill.years, Some(4.0));
        // The sentence became a real, skill-linked bullet with the next
        // ID in sequence (existing bullet-7 -> new bullet-8).
        let bullet = dataset.roles[0].bullets.last().unwrap();
        assert_eq!(bullet.id, BulletId("bullet-8".into()));
        assert_eq!(bullet.text, "Built the trading UI in TypeScript");
        assert_eq!(bullet.skill_ids, vec![SkillId("skill-1".into())]);
        assert!(
            dataset.roles[0]
                .skill_ids
                .contains(&SkillId("skill-1".into()))
        );
    }

    #[tokio::test]
    async fn a_no_removes_the_skill_and_every_reference() {
        let mut dataset = dataset_with_unbacked("TypeScript");
        // Seed references that must not dangle afterwards.
        dataset.roles[0].skill_ids.push(SkillId("skill-1".into()));
        dataset.roles[0].bullets[0]
            .skill_ids
            .push(SkillId("skill-1".into()));
        let user = ScriptedUser::new();
        user.answer(Answer::Choice(2)); // no - remove

        let outcome = verify_unbacked(&mut dataset, &user).await.unwrap();

        assert_eq!(outcome.removed, 1);
        assert!(dataset.skills.skills.is_empty());
        assert!(dataset.skills.aliases.is_empty());
        assert!(dataset.roles[0].skill_ids.is_empty());
        assert!(dataset.roles[0].bullets[0].skill_ids.is_empty());
    }

    #[tokio::test]
    async fn skips_change_nothing() {
        let mut dataset = dataset_with_unbacked("TypeScript");
        let before = dataset.clone();
        let user = ScriptedUser::new();
        user.answer(Answer::Choice(3)); // skip

        let outcome = verify_unbacked(&mut dataset, &user).await.unwrap();

        assert_eq!(outcome.skipped, 1);
        assert!(!outcome.changed());
        assert_eq!(dataset, before);
    }

    #[tokio::test]
    async fn an_aborted_interview_is_an_error_the_caller_must_not_save() {
        let mut dataset = dataset_with_unbacked("TypeScript");
        let user = ScriptedUser::new();
        user.answer(Answer::Choice(0)); // yes...
        // ...but the script ends before "which role" — like Esc mid-flow.

        let result = verify_unbacked(&mut dataset, &user).await;
        assert!(matches!(result, Err(AskError::NotInteractive { .. })));
    }

    #[tokio::test]
    async fn fully_backed_datasets_are_left_alone() {
        let mut dataset = dataset_with_unbacked("TypeScript");
        dataset.skills.skills[0]
            .evidence
            .push(EvidenceRef::Role(RoleId("role-1".into())));
        let user = ScriptedUser::new();

        let outcome = verify_unbacked(&mut dataset, &user).await.unwrap();

        assert_eq!(outcome, VerifyOutcome::default());
        assert!(
            user.notices()
                .iter()
                .any(|n| n.contains("already evidence-backed"))
        );
    }

    fn gap_wanting(name: &str) -> GapReport {
        use crate::jd::{Importance, JdSkill};
        GapReport {
            matched: Vec::new(),
            weak: Vec::new(),
            unknown: vec![JdSkill {
                name: name.into(),
                category: SkillCategory::Hard,
                importance: Importance::Required,
                context_phrases: Vec::new(),
            }],
        }
    }

    #[tokio::test]
    async fn a_yes_on_an_unknown_jd_skill_adds_it_with_evidence() {
        let mut dataset = dataset_with_unbacked("TypeScript");
        let before_skills = dataset.skills.skills.len();
        let gap = gap_wanting("Data Engineering");
        let user = ScriptedUser::new();
        user.answer(Answer::Choice(0)); // yes, professionally
        user.answer(Answer::Choice(0)); // role-1
        user.answer(Answer::Text("3".into()));
        user.answer(Answer::Text(
            "Built the trade-settlement data pipeline".into(),
        ));

        let outcome = verify_unknown(&mut dataset, &gap, &user).await.unwrap();

        assert_eq!(outcome.verified, 1);
        assert!(outcome.changed());
        // A brand-new skill exists, verified, with role evidence and the
        // sentence recorded as a bullet.
        assert_eq!(dataset.skills.skills.len(), before_skills + 1);
        let added = dataset
            .skills
            .skills
            .iter()
            .find(|s| s.canonical_name == "Data Engineering")
            .unwrap();
        assert!(added.verified);
        assert_eq!(
            added.evidence,
            vec![EvidenceRef::Role(RoleId("role-1".into()))]
        );
        assert_eq!(
            dataset.skills.aliases.get("data engineering"),
            Some(&added.id)
        );
        assert!(
            dataset.roles[0]
                .bullets
                .iter()
                .any(|b| b.text.contains("data pipeline"))
        );
    }

    #[tokio::test]
    async fn a_no_on_an_unknown_skill_leaves_it_absent() {
        let mut dataset = dataset_with_unbacked("TypeScript");
        let before = dataset.clone();
        let gap = gap_wanting("Kafka");
        let user = ScriptedUser::new();
        user.answer(Answer::Choice(2)); // no, I haven't

        let outcome = verify_unknown(&mut dataset, &gap, &user).await.unwrap();

        assert_eq!(outcome.verified, 0);
        assert!(!outcome.changed());
        // Nothing added; the skill the job wanted stays out of the dataset.
        assert_eq!(dataset, before);
        assert!(!dataset.skills.aliases.contains_key("kafka"));
    }

    #[tokio::test]
    async fn an_empty_unknown_bucket_changes_nothing() {
        let mut dataset = dataset_with_unbacked("TypeScript");
        let gap = GapReport {
            matched: Vec::new(),
            weak: Vec::new(),
            unknown: Vec::new(),
        };
        let user = ScriptedUser::new();

        let outcome = verify_unknown(&mut dataset, &gap, &user).await.unwrap();

        assert_eq!(outcome, VerifyOutcome::default());
    }

    #[tokio::test]
    #[ignore = "exercise: a verified skill takes evidence from exactly one role; let the user keep adding roles (confirm-driven loop, no duplicate evidence), then finish this test"]
    async fn ex_016_multiple_roles_can_back_one_skill() {
        // Once the loop exists: dataset with two roles, script a yes,
        // the first role, a confirmed second pick, the other role, then
        // years/sentence; assert two distinct Role evidence entries.
        let multi_role_implemented = false;
        assert!(multi_role_implemented);
    }

    #[test]
    fn years_parse_leniently() {
        assert_eq!(parse_years("4"), Some(4.0));
        assert_eq!(parse_years(" 2.5 years "), Some(2.5));
        assert_eq!(parse_years(""), None);
        assert_eq!(parse_years("a while"), None);
    }
}
