//! Skill verification: the interview that turns unbacked skills into
//! evidence-backed ones (FR-3.1).
//!
//! Skills with an empty `evidence` list sit in limbo — recorded but
//! barred from tailoring by the never-fabricate rule. This interview
//! resolves each one the only way that's legitimate: by asking the
//! person. A "yes" gets role evidence (plus, optionally, years and a
//! user-written bullet — words the user typed are the one source that
//! needs no further proof); a "no" removes the skill and every
//! reference to it; "skip" leaves it for another day.
//!
//! Transactionality lives at the save boundary: this function mutates
//! the in-memory dataset and returns a summary; the caller saves once
//! through the store (lock + backup + atomic write) only on success.
//! An interview abandoned halfway — Esc, closed terminal — returns an
//! error and the file on disk never changes.

use chrono::Utc;

use crate::dataset::types::{Bullet, BulletId, EvidenceRef, ResumeDataset, SkillId, Strength};
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

/// Interview the user about every skill with no evidence behind it.
// EXERCISE(EX-016)
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

    let role_options: Vec<String> = dataset
        .roles
        .iter()
        .map(|role| format!("{} — {}", role.title, role.company))
        .collect();

    for skill_id in unbacked {
        let Some(skill) = dataset.skills.skills.iter().find(|s| s.id == skill_id) else {
            continue; // removed earlier in this same session
        };
        let name = skill.canonical_name.clone();

        let answer = user
            .ask(Question::Select {
                prompt: format!("Have you used {name} in any role?"),
                options: vec![
                    "Yes, professionally".to_string(),
                    "Yes, in a side project".to_string(),
                    "No - remove it from the dataset".to_string(),
                    "Skip for now".to_string(),
                ],
            })
            .await?;

        match answer {
            Answer::Choice(0) | Answer::Choice(1) => {
                let role_index = match user
                    .ask(Question::Select {
                        prompt: format!("Which role best demonstrates {name}?"),
                        options: role_options.clone(),
                    })
                    .await?
                {
                    Answer::Choice(index) if index < dataset.roles.len() => index,
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
                    if !dataset.roles[role_index].skill_ids.contains(&skill_id) {
                        dataset.roles[role_index].skill_ids.push(skill_id.clone());
                    }
                    outcome.bullets_added += 1;
                }
                if let Some(skill) = dataset.skills.skills.iter_mut().find(|s| s.id == skill_id) {
                    skill.evidence.push(EvidenceRef::Role(role_id));
                    skill.verified = true;
                    skill.verified_at = Some(Utc::now());
                    if years.is_some() {
                        skill.years = years;
                    }
                }
                user.notify(&format!("added evidence for {name}"));
                outcome.verified += 1;
            }
            Answer::Choice(2) => {
                remove_skill(dataset, &skill_id);
                user.notify(&format!("removed {name}"));
                outcome.removed += 1;
            }
            _ => {
                outcome.skipped += 1;
            }
        }
    }

    Ok(outcome)
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
