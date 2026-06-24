//! Shared synthetic inputs for the eval cases.
//!
//! The harness is non-test code (it ships in a binary), so it can't borrow
//! the `#[cfg(test)]` builders the modules use. These are small, explicit
//! stand-ins: a person, a posting, a draft. They carry no real data, so the
//! committed harness leaks nothing personal and runs the same on any clone.

use crate::dataset::types::BulletId;
use crate::dataset::types::{
    Bullet, Contact, EmploymentType, EvidenceRef, Metric, Proficiency, ResumeDataset, Role, RoleId,
    Skill, SkillCategory, SkillId, Strength, YearMonth,
};
use crate::jd::{Importance, JdSkill, JobRequirements, RemotePolicy, Seniority};
use crate::tailor::{BuildId, JdId, SkillsSection, TailoredBullet, TailoredResume, TailoredRole};

/// A throwaway person for inputs that need contact details.
pub fn contact() -> Contact {
    Contact {
        full_name: "Ada Eval".into(),
        email: "ada@example.com".into(),
        phone: None,
        location: None,
        links: Vec::new(),
    }
}

/// An empty but valid dataset. Enough for agents whose assembly reads only
/// the draft or the JD (the reviewer serializes the dataset into its prompt
/// but never inspects it during assembly).
pub fn empty_dataset() -> ResumeDataset {
    ResumeDataset::new(contact())
}

/// One skill, optionally backed by a role's evidence. `backed = false`
/// leaves `evidence` empty — the type-level half of never-fabricate.
pub fn skill(
    id: &str,
    name: &str,
    aliases: &[&str],
    proficiency: Proficiency,
    backed: bool,
) -> Skill {
    Skill {
        id: SkillId(id.into()),
        canonical_name: name.into(),
        aliases: aliases.iter().map(|a| (*a).to_string()).collect(),
        category: SkillCategory::Tool,
        proficiency,
        years: None,
        last_used: None,
        evidence: if backed {
            vec![EvidenceRef::Role(RoleId("role-1".into()))]
        } else {
            Vec::new()
        },
        verified: false,
        verified_at: None,
    }
}

/// A dataset carrying `skills` (with the alias lookup populated the way the
/// store would) and `roles`. The gap matcher resolves a JD skill name
/// through that alias map before ever calling the model.
pub fn dataset(skills: Vec<Skill>, roles: Vec<Role>) -> ResumeDataset {
    let mut dataset = ResumeDataset::new(contact());
    for s in &skills {
        for name in std::iter::once(&s.canonical_name).chain(s.aliases.iter()) {
            dataset
                .skills
                .aliases
                .insert(name.to_lowercase(), s.id.clone());
        }
    }
    dataset.skills.skills = skills;
    dataset.roles = roles;
    dataset
}

/// One dataset bullet. `metric` is the verified quantified result, if any —
/// its digits, like the text's, are allowed to survive a rewrite.
pub fn bullet(id: &str, text: &str, metric: Option<&str>) -> Bullet {
    Bullet {
        id: BulletId(id.into()),
        text: text.into(),
        skill_ids: Vec::new(),
        metric: metric.map(|m| Metric(m.into())),
        theme: Vec::new(),
        strength: Strength::High,
        variants: Vec::new(),
    }
}

/// One role holding the given bullets.
pub fn role(id: &str, bullets: Vec<Bullet>) -> Role {
    Role {
        id: RoleId(id.into()),
        company: "Acme".into(),
        title: "Engineer".into(),
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

/// A small posting: one critical required skill, one preferred, some ats
/// phrases, and raw text the reviewer can read as ground truth.
pub fn jd() -> JobRequirements {
    JobRequirements {
        company: "Globex".into(),
        title: "Engineering Manager".into(),
        seniority: Seniority::Manager,
        location: None,
        remote: RemotePolicy::Remote,
        domain_keywords: vec!["fintech".into()],
        required_skills: vec![JdSkill {
            name: "Rust".into(),
            category: crate::dataset::types::SkillCategory::Language,
            importance: Importance::Critical,
            context_phrases: vec!["deep Rust expertise".into()],
        }],
        preferred_skills: vec![JdSkill {
            name: "Kubernetes".into(),
            category: crate::dataset::types::SkillCategory::Tool,
            importance: Importance::Preferred,
            context_phrases: Vec::new(),
        }],
        responsibilities: vec!["lead a platform team".into()],
        ats_phrases: vec!["Engineering Manager".into(), "platform team".into()],
        raw_text: "Globex seeks an Engineering Manager to lead a platform team. \
                   Deep Rust expertise required."
            .into(),
        source_url: None,
    }
}

/// A two-bullet draft: one weakly-worded line and one with a real metric,
/// with stable bullet ids (`bullet-1`, `bullet-2`) objections can target.
pub fn draft() -> TailoredResume {
    TailoredResume {
        build_id: BuildId("eval".into()),
        jd_id: JdId("globex".into()),
        generated_at: chrono::Utc::now(),
        contact: contact(),
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
        achievements: Vec::new(),
        certifications: Vec::new(),
    }
}
