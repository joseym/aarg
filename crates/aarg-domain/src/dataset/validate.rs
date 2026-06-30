//! Dataset integrity checks (FR-1.3): pure code, no LLM.
//!
//! This is a *service*, not an agent — determinism is the point. The
//! headline check is the structural half of never-fabricate: a skill
//! whose `evidence` vec is empty is reported here and excluded from
//! tailoring downstream. The rest are referential-integrity checks that
//! catch a hand-edited (or badly merged) dataset before it misleads an
//! LLM stage.
//!
//! Findings come in two grades: **problems** (the dataset is broken or a
//! claim is unsupported — `aarg dataset validate` exits nonzero) and
//! **notes** (worth a look, not blocking).

use std::collections::HashSet;

use crate::dataset::types::{EvidenceRef, ResumeDataset, SkillId};

/// What validation found, split by gravity. An empty `problems` vec
/// means the dataset is usable for tailoring.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ValidationReport {
    pub problems: Vec<Finding>,
    pub notes: Vec<Finding>,
}

impl ValidationReport {
    pub fn is_clean(&self) -> bool {
        self.problems.is_empty()
    }
}

/// One observation, tagged with what category of issue it is so tests
/// (and later, machine consumers) don't have to parse the message.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Finding {
    pub kind: FindingKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    /// A skill claims no evidence at all — unsupported, excluded from
    /// tailoring until verified.
    MissingEvidence,
    /// A role, bullet, project, or achievement references a skill ID
    /// that has no entry in the skill graph.
    DanglingSkillReference,
    /// A skill's evidence points at a role/project/certification that
    /// does not exist.
    DanglingEvidence,
    /// The alias map points at a skill ID that has no entry.
    BrokenAlias,
    /// Skills no bullet, project, or achievement mentions — legitimate,
    /// but nothing in the dataset demonstrates them.
    UnmentionedSkills,
}

// EXERCISE(EX-008)
pub fn validate(dataset: &ResumeDataset) -> ValidationReport {
    let mut problems = Vec::new();
    let mut notes = Vec::new();

    let skill_ids: HashSet<&SkillId> = dataset.skills.skills.iter().map(|s| &s.id).collect();
    let role_ids: HashSet<_> = dataset.roles.iter().map(|r| &r.id).collect();
    let project_ids: HashSet<_> = dataset.projects.iter().map(|p| &p.id).collect();
    let certification_ids: HashSet<_> = dataset.certifications.iter().map(|c| &c.id).collect();

    // Walk everything that references skills: collect who is mentioned,
    // and report references to skills that don't exist.
    let mut mentioned: HashSet<&SkillId> = HashSet::new();
    for role in &dataset.roles {
        collect_refs(
            format!("role {:?}", role.title),
            &role.skill_ids,
            &skill_ids,
            &mut mentioned,
            &mut problems,
        );
        for bullet in &role.bullets {
            collect_refs(
                format!("a bullet under {:?}", role.title),
                &bullet.skill_ids,
                &skill_ids,
                &mut mentioned,
                &mut problems,
            );
        }
    }
    for project in &dataset.projects {
        collect_refs(
            format!("project {:?}", project.name),
            &project.skill_ids,
            &skill_ids,
            &mut mentioned,
            &mut problems,
        );
    }
    for achievement in &dataset.achievements {
        collect_refs(
            format!("achievement {:?}", achievement.id.0),
            &achievement.skill_ids,
            &skill_ids,
            &mut mentioned,
            &mut problems,
        );
    }

    // The never-fabricate checks, per skill: evidence must exist and
    // must point at real entities.
    let mut unmentioned: Vec<&str> = Vec::new();
    for skill in &dataset.skills.skills {
        if skill.evidence.is_empty() {
            problems.push(Finding {
                kind: FindingKind::MissingEvidence,
                message: format!(
                    "skill {:?} has no evidence — it will be excluded from tailoring \
                     until something backs it",
                    skill.canonical_name
                ),
            });
        } else if !mentioned.contains(&skill.id) {
            unmentioned.push(&skill.canonical_name);
        }
        for evidence in &skill.evidence {
            let (exists, what) = match evidence {
                EvidenceRef::Role(id) => (role_ids.contains(id), "role"),
                EvidenceRef::Project(id) => (project_ids.contains(id), "project"),
                EvidenceRef::Certification(id) => (certification_ids.contains(id), "certification"),
            };
            if !exists {
                problems.push(Finding {
                    kind: FindingKind::DanglingEvidence,
                    message: format!(
                        "skill {:?} cites a {what} that does not exist in the dataset",
                        skill.canonical_name
                    ),
                });
            }
        }
    }

    for (alias, id) in &dataset.skills.aliases {
        if !skill_ids.contains(id) {
            problems.push(Finding {
                kind: FindingKind::BrokenAlias,
                message: format!(
                    "alias {alias:?} points at skill id {:?}, which has no entry",
                    id.0
                ),
            });
        }
    }

    if !unmentioned.is_empty() {
        unmentioned.sort_unstable();
        notes.push(Finding {
            kind: FindingKind::UnmentionedSkills,
            message: format!(
                "{} skill(s) have evidence but are never mentioned by a bullet, \
                 project, or achievement: {}",
                unmentioned.len(),
                unmentioned.join(", ")
            ),
        });
    }

    ValidationReport { problems, notes }
}

/// Record which known skills `ids` mentions; report the unknown ones.
fn collect_refs<'a>(
    owner: String,
    ids: &'a [SkillId],
    known: &HashSet<&'a SkillId>,
    mentioned: &mut HashSet<&'a SkillId>,
    problems: &mut Vec<Finding>,
) {
    for id in ids {
        if known.contains(id) {
            mentioned.insert(id);
        } else {
            problems.push(Finding {
                kind: FindingKind::DanglingSkillReference,
                message: format!("{owner} references skill id {:?}, which has no entry", id.0),
            });
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::dataset::types::{
        Bullet, BulletId, Contact, EmploymentType, EvidenceRef, Proficiency, Role, RoleId, Skill,
        SkillCategory, SkillId, Strength, YearMonth,
    };

    fn base_dataset() -> ResumeDataset {
        ResumeDataset::new(Contact {
            full_name: "Ada".into(),
            email: "ada@example.com".into(),
            phone: None,
            location: None,
            links: Vec::new(),
        })
    }

    fn skill(id: &str, name: &str, evidence: Vec<EvidenceRef>) -> Skill {
        Skill {
            id: SkillId(id.into()),
            canonical_name: name.into(),
            aliases: Vec::new(),
            category: SkillCategory::Hard,
            proficiency: Proficiency::Working,
            years: None,
            last_used: None,
            evidence,
            verified: false,
            verified_at: None,
        }
    }

    fn role(id: &str, title: &str, skill_ids: Vec<SkillId>) -> Role {
        Role {
            id: RoleId(id.into()),
            company: "X".into(),
            title: title.into(),
            start: YearMonth {
                year: 2020,
                month: 1,
            },
            end: None,
            location: None,
            employment_type: EmploymentType::FullTime,
            bullets: Vec::new(),
            skill_ids,
            context: None,
        }
    }

    #[test]
    fn a_consistent_dataset_is_clean() {
        let mut dataset = base_dataset();
        dataset
            .roles
            .push(role("role-1", "Engineer", vec![SkillId("skill-1".into())]));
        dataset.skills.skills.push(skill(
            "skill-1",
            "Rust",
            vec![EvidenceRef::Role(RoleId("role-1".into()))],
        ));
        dataset
            .skills
            .aliases
            .insert("rust".into(), SkillId("skill-1".into()));

        let report = validate(&dataset);
        assert!(report.is_clean(), "got: {:?}", report.problems);
        assert!(report.notes.is_empty(), "got: {:?}", report.notes);
    }

    #[test]
    fn a_skill_without_evidence_is_a_problem() {
        let mut dataset = base_dataset();
        dataset
            .skills
            .skills
            .push(skill("skill-1", "TypeScript", Vec::new()));

        let report = validate(&dataset);
        assert!(!report.is_clean());
        assert_eq!(report.problems[0].kind, FindingKind::MissingEvidence);
        assert!(report.problems[0].message.contains("TypeScript"));
        // No "unmentioned" note on top — missing evidence is the root cause.
        assert!(report.notes.is_empty());
    }

    #[test]
    fn references_to_unknown_skills_are_problems() {
        let mut dataset = base_dataset();
        let mut r = role("role-1", "Engineer", vec![SkillId("ghost".into())]);
        r.bullets.push(Bullet {
            id: BulletId("bullet-1".into()),
            text: "Did things".into(),
            skill_ids: vec![SkillId("ghost".into())],
            metric: None,
            theme: Vec::new(),
            strength: Strength::Medium,
            variants: Vec::new(),
        });
        dataset.roles.push(r);

        let report = validate(&dataset);
        let dangling: Vec<_> = report
            .problems
            .iter()
            .filter(|f| f.kind == FindingKind::DanglingSkillReference)
            .collect();
        assert_eq!(dangling.len(), 2, "role list and bullet each report");
    }

    #[test]
    fn evidence_pointing_nowhere_is_a_problem() {
        let mut dataset = base_dataset();
        dataset.skills.skills.push(skill(
            "skill-1",
            "Rust",
            vec![EvidenceRef::Role(RoleId("role-99".into()))],
        ));

        let report = validate(&dataset);
        assert!(
            report
                .problems
                .iter()
                .any(|f| f.kind == FindingKind::DanglingEvidence)
        );
    }

    #[test]
    fn aliases_to_missing_skills_are_problems() {
        let mut dataset = base_dataset();
        dataset
            .skills
            .aliases
            .insert("rust".into(), SkillId("skill-99".into()));

        let report = validate(&dataset);
        assert!(
            report
                .problems
                .iter()
                .any(|f| f.kind == FindingKind::BrokenAlias)
        );
    }

    #[test]
    fn evidenced_but_unmentioned_skills_are_one_aggregated_note() {
        let mut dataset = base_dataset();
        dataset.roles.push(role("role-1", "Engineer", Vec::new()));
        for (id, name) in [("skill-1", "Rust"), ("skill-2", "Go")] {
            dataset.skills.skills.push(skill(
                id,
                name,
                vec![EvidenceRef::Role(RoleId("role-1".into()))],
            ));
        }

        let report = validate(&dataset);
        assert!(report.is_clean());
        assert_eq!(report.notes.len(), 1, "aggregated, not one note per skill");
        assert_eq!(report.notes[0].kind, FindingKind::UnmentionedSkills);
        assert!(report.notes[0].message.contains("Go, Rust"));
    }

    #[test]
    #[ignore = "exercise: validate does not check that ids are unique; report duplicate role, bullet, and skill ids as problems, then finish this test"]
    fn ex_008_duplicate_ids_are_reported() {
        // Once the check exists: build a dataset where two roles share an
        // id (and likewise two skills), and assert each duplicate is
        // reported as a problem.
        let uniqueness_checked = false;
        assert!(uniqueness_checked);
    }
}
