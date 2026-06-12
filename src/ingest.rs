//! Resume ingestion: turn the raw text of an existing resume into a
//! `ResumeDataset` (FR-1.1).
//!
//! This is the first of the Phase 1 LLM features, and like the others it
//! is deliberately a plain `async fn` calling `LlmClient` directly — the
//! prompt is assembled by hand and the reply parsed by hand. The agent
//! abstraction is extracted *from* these functions in Phase 2, not
//! designed ahead of them.
//!
//! The split of responsibilities is the load-bearing decision:
//!
//! - The **LLM** only transcribes — it reports what the resume says, in a
//!   wire shape with no IDs (the prompt forbids inventing anything).
//! - **This code** does everything that must be deterministic: assigning
//!   IDs, resolving skill names to IDs, deriving evidence links, and
//!   collecting warnings about anything it had to drop.

use std::collections::HashMap;

use serde::Deserialize;

use crate::dataset::types::{
    Achievement, AchievementId, Bullet, BulletId, Certification, CertificationId, Contact,
    Education, EmploymentType, EvidenceRef, Fluency, HumanLanguage, Link, Metric, Proficiency,
    Project, ProjectId, Publication, ResumeDataset, Role, RoleId, Skill, SkillCategory, SkillId,
    Strength, Theme, YearMonth,
};
use crate::llm::{CompletionRequest, LlmClient, LlmError, Message};

/// Generous output budget: a long resume serializes to a lot of JSON.
const REPLY_BUDGET: u32 = 8192;

/// Everything that can go wrong while ingesting a resume.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error(transparent)]
    Llm(#[from] LlmError),

    #[error("the model's reply was not the expected resume JSON (reply began {snippet:?})")]
    BadReply {
        snippet: String,
        #[source]
        source: serde_json::Error,
    },
}

/// What ingestion produced: the dataset, plus anything the assembly step
/// had to drop or could not resolve. Warnings are for the user to review —
/// they signal data that needs hand-editing, not failure.
#[derive(Debug)]
pub struct IngestOutcome {
    pub dataset: ResumeDataset,
    pub warnings: Vec<String>,
}

/// Extract a dataset from resume text. The model transcribes; this code
/// assigns IDs, links evidence, and reports what didn't resolve.
pub async fn ingest_resume(
    client: &dyn LlmClient,
    model: &str,
    resume_text: &str,
) -> Result<IngestOutcome, IngestError> {
    let request = CompletionRequest {
        model: model.to_string(),
        max_tokens: REPLY_BUDGET,
        system: Some(SYSTEM_PROMPT.to_string()),
        messages: vec![Message::user(resume_text)],
        temperature: None,
    };
    let response = client.complete(request).await?;
    let json = strip_fences(&response.text);
    let raw: RawResume = serde_json::from_str(json).map_err(|source| IngestError::BadReply {
        snippet: json.chars().take(120).collect(),
        source,
    })?;
    Ok(assemble(raw))
}

/// The extraction contract sent as the system prompt. The never-invent
/// rule here is the prompt-level half of FR-1.7; the structural half is
/// that `Skill::evidence` must resolve to real entities, which `assemble`
/// and `dataset validate` enforce.
const SYSTEM_PROMPT: &str = r#"You extract structured data from resumes.

Rules — all of them matter:
- Extract only what the resume actually says. Never invent, infer, or embellish: no skill, employer, title, date, or metric may appear in your output unless the resume states it.
- Unknown or absent optional values are null. Unknown lists are [].
- Dates are "YYYY-MM" strings. If the resume gives only a year, use "YYYY-01". A current role has "end": null.
- Every skill name used in any "skills" list (in bullets, projects, or achievements) must also be an entry in the top-level "skills" array.
- "evidence_roles" lists 0-based indices into "roles" for the roles that demonstrate that skill; likewise "evidence_projects" and "evidence_certifications".
- Enum values: employment_type is one of "full_time"|"part_time"|"contract"|"founder"|"freelance"|"internship". category is one of "hard"|"soft"|"domain"|"tool"|"language"|"framework". proficiency is one of "familiar"|"working"|"proficient"|"expert" — choose the strongest level the resume's own wording supports, not more. strength rates the bullet as a resume line: "high"|"medium"|"low". fluency is one of "native"|"fluent"|"professional"|"conversational"|"basic".
- Reply with exactly one JSON object and nothing else — no markdown fences, no commentary.

The JSON object:
{
  "contact": {"full_name": "...", "email": "...", "phone": null, "location": null, "links": [{"label": "GitHub", "url": "..."}]},
  "summary": "the professional summary if present, else null",
  "roles": [{"company": "...", "title": "...", "start": "YYYY-MM", "end": null, "location": null, "employment_type": "full_time", "context": "1-2 sentences of company/team context if stated", "bullets": [{"text": "...", "skills": ["skill names used"], "metric": "the quantified result, if any", "themes": ["leadership"], "strength": "medium"}]}],
  "education": [{"institution": "...", "credential": "BSc Computer Science", "start": null, "end": null, "location": null}],
  "skills": [{"name": "...", "aliases": ["other names the resume uses for it"], "category": "tool", "proficiency": "working", "years": null, "last_used": null, "evidence_roles": [0], "evidence_projects": [], "evidence_certifications": []}],
  "projects": [{"name": "...", "summary": "...", "url": null, "skills": []}],
  "certifications": [{"name": "...", "issuer": "...", "issued": null, "expires": null}],
  "achievements": [{"text": "...", "skills": []}],
  "publications": [{"title": "...", "venue": null, "date": null, "url": null}],
  "languages": [{"name": "...", "fluency": "professional"}]
}"#;

/// Models often wrap JSON in ```fences``` despite instructions; strip one
/// outer fence pair (and its info string, e.g. ```json) if present.
fn strip_fences(text: &str) -> &str {
    let trimmed = text.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    let body = match rest.split_once('\n') {
        Some((_info_string, body)) => body,
        None => rest,
    };
    body.trim_end().strip_suffix("```").unwrap_or(body).trim()
}

// ---------------------------------------------------------------------
// The wire shape the model replies with
//
// Deliberately ID-free and lenient: every list defaults to empty and
// enums the model might omit get documented defaults, so one missing
// field doesn't sink the whole extraction. IDs and links are assigned by
// `assemble`, never by the model.
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RawResume {
    #[serde(default)]
    contact: RawContact,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    roles: Vec<RawRole>,
    #[serde(default)]
    education: Vec<RawEducation>,
    #[serde(default)]
    skills: Vec<RawSkill>,
    #[serde(default)]
    projects: Vec<RawProject>,
    #[serde(default)]
    certifications: Vec<RawCertification>,
    #[serde(default)]
    achievements: Vec<RawAchievement>,
    #[serde(default)]
    publications: Vec<Publication>,
    #[serde(default)]
    languages: Vec<RawLanguage>,
}

#[derive(Debug, Default, Deserialize)]
struct RawContact {
    full_name: Option<String>,
    email: Option<String>,
    phone: Option<String>,
    location: Option<String>,
    #[serde(default)]
    links: Vec<Link>,
}

#[derive(Debug, Deserialize)]
struct RawRole {
    company: String,
    title: String,
    /// Optional on the wire: a role the model found no start date for is
    /// dropped with a warning rather than failing the whole ingest.
    start: Option<YearMonth>,
    end: Option<YearMonth>,
    location: Option<String>,
    #[serde(default = "default_employment_type")]
    employment_type: EmploymentType,
    context: Option<String>,
    #[serde(default)]
    bullets: Vec<RawBullet>,
}

fn default_employment_type() -> EmploymentType {
    EmploymentType::FullTime
}

#[derive(Debug, Deserialize)]
struct RawBullet {
    text: String,
    #[serde(default)]
    skills: Vec<String>,
    metric: Option<String>,
    #[serde(default)]
    themes: Vec<String>,
    #[serde(default = "default_strength")]
    strength: Strength,
}

fn default_strength() -> Strength {
    Strength::Medium
}

#[derive(Debug, Deserialize)]
struct RawSkill {
    name: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default = "default_category")]
    category: SkillCategory,
    #[serde(default = "default_proficiency")]
    proficiency: Proficiency,
    years: Option<f32>,
    last_used: Option<YearMonth>,
    #[serde(default)]
    evidence_roles: Vec<usize>,
    #[serde(default)]
    evidence_projects: Vec<usize>,
    #[serde(default)]
    evidence_certifications: Vec<usize>,
}

fn default_category() -> SkillCategory {
    SkillCategory::Hard
}

fn default_proficiency() -> Proficiency {
    Proficiency::Working
}

#[derive(Debug, Deserialize)]
struct RawEducation {
    institution: String,
    credential: Option<String>,
    start: Option<YearMonth>,
    end: Option<YearMonth>,
    location: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawProject {
    name: String,
    #[serde(default)]
    summary: Option<String>,
    url: Option<String>,
    #[serde(default)]
    skills: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawCertification {
    name: String,
    #[serde(default)]
    issuer: Option<String>,
    issued: Option<YearMonth>,
    expires: Option<YearMonth>,
}

#[derive(Debug, Deserialize)]
struct RawAchievement {
    text: String,
    #[serde(default)]
    skills: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawLanguage {
    name: String,
    fluency: Option<Fluency>,
}

// ---------------------------------------------------------------------
// Assembly: wire shape -> dataset
// ---------------------------------------------------------------------

// EXERCISE(EX-007)
fn assemble(raw: RawResume) -> IngestOutcome {
    let mut warnings = Vec::new();

    let contact = Contact {
        full_name: raw.contact.full_name.unwrap_or_default(),
        email: raw.contact.email.unwrap_or_default(),
        phone: raw.contact.phone,
        location: raw.contact.location,
        links: raw.contact.links,
    };
    if contact.full_name.is_empty() {
        warnings.push("no name found in the resume — edit the dataset to add one".to_string());
    }
    if contact.email.is_empty() {
        warnings.push("no email found in the resume — edit the dataset to add one".to_string());
    }

    let mut dataset = ResumeDataset::new(contact);
    dataset.summary = raw.summary;

    // Skills first: bullets, projects, and achievements resolve names
    // against the alias map. Explicit evidence indices are stashed and
    // resolved after the referenced entities exist.
    let mut explicit_evidence = Vec::new();
    for (index, skill) in raw.skills.into_iter().enumerate() {
        let id = SkillId(format!("skill-{}", index + 1));
        for name in std::iter::once(&skill.name).chain(skill.aliases.iter()) {
            let key = name.to_lowercase();
            match dataset.skills.aliases.get(&key) {
                Some(existing) if *existing != id => warnings.push(format!(
                    "{name:?} names two different skills; keeping the first"
                )),
                _ => {
                    dataset.skills.aliases.insert(key, id.clone());
                }
            }
        }
        explicit_evidence.push((
            id.clone(),
            skill.evidence_roles,
            skill.evidence_projects,
            skill.evidence_certifications,
        ));
        dataset.skills.skills.push(Skill {
            id,
            canonical_name: skill.name,
            aliases: skill.aliases,
            category: skill.category,
            proficiency: skill.proficiency,
            years: skill.years,
            last_used: skill.last_used,
            evidence: Vec::new(),
            verified: false,
            verified_at: None,
        });
    }

    // Evidence accumulates from two directions: links the model stated
    // (`evidence_roles` indices) and links this code derives (a bullet
    // mentioning the skill is evidence of the role it sits in).
    let mut evidence: Vec<(SkillId, EvidenceRef)> = Vec::new();

    // Roles keep the ID their *wire position* implies even when earlier
    // roles are dropped, so the model's evidence indices stay meaningful.
    let mut kept_roles: HashMap<usize, RoleId> = HashMap::new();
    let mut bullet_count = 0;
    for (index, role) in raw.roles.into_iter().enumerate() {
        let id = RoleId(format!("role-{}", index + 1));
        let Some(start) = role.start else {
            warnings.push(format!(
                "dropped role {:?} at {:?}: no start date found",
                role.title, role.company
            ));
            continue;
        };
        kept_roles.insert(index, id.clone());

        let mut role_skill_ids: Vec<SkillId> = Vec::new();
        let mut bullets = Vec::new();
        for bullet in role.bullets {
            bullet_count += 1;
            let mut skill_ids = Vec::new();
            for name in &bullet.skills {
                match resolve_skill(&dataset, name) {
                    Some(skill_id) => {
                        evidence.push((skill_id.clone(), EvidenceRef::Role(id.clone())));
                        if !role_skill_ids.contains(&skill_id) {
                            role_skill_ids.push(skill_id.clone());
                        }
                        skill_ids.push(skill_id);
                    }
                    None => warnings.push(format!(
                        "a bullet under {:?} mentions {name:?}, which is not in the skills list",
                        role.title
                    )),
                }
            }
            bullets.push(Bullet {
                id: BulletId(format!("bullet-{bullet_count}")),
                text: bullet.text,
                skill_ids,
                metric: bullet.metric.map(Metric),
                theme: bullet.themes.into_iter().map(Theme).collect(),
                strength: bullet.strength,
                variants: Vec::new(),
            });
        }

        dataset.roles.push(Role {
            id,
            company: role.company,
            title: role.title,
            start,
            end: role.end,
            location: role.location,
            employment_type: role.employment_type,
            bullets,
            skill_ids: role_skill_ids,
            context: role.context,
        });
    }

    dataset.education = raw
        .education
        .into_iter()
        .map(|e| Education {
            institution: e.institution,
            credential: e.credential.unwrap_or_default(),
            start: e.start,
            end: e.end,
            location: e.location,
        })
        .collect();

    for (index, project) in raw.projects.into_iter().enumerate() {
        let id = ProjectId(format!("project-{}", index + 1));
        let mut skill_ids = Vec::new();
        for name in &project.skills {
            match resolve_skill(&dataset, name) {
                Some(skill_id) => {
                    evidence.push((skill_id.clone(), EvidenceRef::Project(id.clone())));
                    skill_ids.push(skill_id);
                }
                None => warnings.push(format!(
                    "project {:?} mentions {name:?}, which is not in the skills list",
                    project.name
                )),
            }
        }
        dataset.projects.push(Project {
            id,
            name: project.name,
            summary: project.summary.unwrap_or_default(),
            url: project.url,
            skill_ids,
        });
    }

    for (index, cert) in raw.certifications.into_iter().enumerate() {
        dataset.certifications.push(Certification {
            id: CertificationId(format!("certification-{}", index + 1)),
            name: cert.name,
            issuer: cert.issuer.unwrap_or_default(),
            issued: cert.issued,
            expires: cert.expires,
        });
    }

    for (index, achievement) in raw.achievements.into_iter().enumerate() {
        let mut skill_ids = Vec::new();
        for name in &achievement.skills {
            match resolve_skill(&dataset, name) {
                Some(skill_id) => skill_ids.push(skill_id),
                None => warnings.push(format!(
                    "an achievement mentions {name:?}, which is not in the skills list"
                )),
            }
        }
        dataset.achievements.push(Achievement {
            id: AchievementId(format!("achievement-{}", index + 1)),
            text: achievement.text,
            skill_ids,
        });
    }

    dataset.publications = raw.publications;
    dataset.languages = raw
        .languages
        .into_iter()
        .map(|l| HumanLanguage {
            name: l.name,
            fluency: l.fluency.unwrap_or(Fluency::Conversational),
        })
        .collect();

    // Resolve the model's explicit evidence indices now that all
    // referenced entities exist and dropped roles are known.
    for (skill_id, roles, projects, certifications) in explicit_evidence {
        for index in roles {
            match kept_roles.get(&index) {
                Some(role_id) => {
                    evidence.push((skill_id.clone(), EvidenceRef::Role(role_id.clone())))
                }
                None => warnings.push(format!(
                    "skill {} cites role index {index}, which does not exist",
                    skill_name(&dataset, &skill_id)
                )),
            }
        }
        for index in projects {
            if index < dataset.projects.len() {
                let id = dataset.projects[index].id.clone();
                evidence.push((skill_id.clone(), EvidenceRef::Project(id)));
            } else {
                warnings.push(format!(
                    "skill {} cites project index {index}, which does not exist",
                    skill_name(&dataset, &skill_id)
                ));
            }
        }
        for index in certifications {
            if index < dataset.certifications.len() {
                let id = dataset.certifications[index].id.clone();
                evidence.push((skill_id.clone(), EvidenceRef::Certification(id)));
            } else {
                warnings.push(format!(
                    "skill {} cites certification index {index}, which does not exist",
                    skill_name(&dataset, &skill_id)
                ));
            }
        }
    }

    for (skill_id, evidence_ref) in evidence {
        if let Some(skill) = dataset.skills.skills.iter_mut().find(|s| s.id == skill_id)
            && !skill.evidence.contains(&evidence_ref)
        {
            skill.evidence.push(evidence_ref);
        }
    }

    IngestOutcome { dataset, warnings }
}

/// Case-insensitive lookup of a skill name (or alias) to its ID.
fn resolve_skill(dataset: &ResumeDataset, name: &str) -> Option<SkillId> {
    dataset.skills.aliases.get(&name.to_lowercase()).cloned()
}

/// The canonical name for warning messages; falls back to the raw ID.
fn skill_name(dataset: &ResumeDataset, id: &SkillId) -> String {
    dataset
        .skills
        .skills
        .iter()
        .find(|s| s.id == *id)
        .map(|s| format!("{:?}", s.canonical_name))
        .unwrap_or_else(|| id.0.clone())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::llm::MockLlmClient;

    /// A reply exercising every linking path: two roles (one current),
    /// bullet-derived evidence, explicit evidence indices, a project, a
    /// certification, and an alias.
    const GOOD_REPLY: &str = r#"{
        "contact": {"full_name": "Ada Lovelace", "email": "ada@example.com",
                    "phone": null, "location": "London",
                    "links": [{"label": "GitHub", "url": "https://github.com/ada"}]},
        "summary": "Engineer of analytical engines.",
        "roles": [
            {"company": "Analytical Engines Ltd", "title": "Principal Engineer",
             "start": "1842-09", "end": null, "location": null,
             "employment_type": "full_time", "context": null,
             "bullets": [
                {"text": "Wrote the first published program",
                 "skills": ["Algorithm design"], "metric": null,
                 "themes": ["0-to-1"], "strength": "high"}
             ]},
            {"company": "Babbage & Co", "title": "Consultant",
             "start": "1840-01", "end": "1842-08", "location": null,
             "employment_type": "contract", "context": null, "bullets": []}
        ],
        "education": [{"institution": "Home tutoring", "credential": "Mathematics",
                       "start": null, "end": null, "location": null}],
        "skills": [
            {"name": "Algorithm design", "aliases": ["algorithms"],
             "category": "hard", "proficiency": "expert", "years": 10,
             "last_used": "1843-07",
             "evidence_roles": [1], "evidence_projects": [0],
             "evidence_certifications": [0]}
        ],
        "projects": [{"name": "Notes on the Analytical Engine",
                      "summary": "Annotated translation", "url": null,
                      "skills": ["algorithms"]}],
        "certifications": [{"name": "Royal Society endorsement",
                            "issuer": "Royal Society", "issued": null, "expires": null}],
        "achievements": [{"text": "First programmer", "skills": ["Algorithm design"]}],
        "publications": [{"title": "Sketch of the Analytical Engine",
                          "venue": null, "date": null, "url": null}],
        "languages": [{"name": "French", "fluency": "fluent"}]
    }"#;

    #[tokio::test]
    async fn a_full_reply_assembles_into_a_linked_dataset() {
        let mock = MockLlmClient::default();
        mock.enqueue(GOOD_REPLY);

        let outcome = ingest_resume(&mock, "test-model", "the resume text")
            .await
            .unwrap();
        let dataset = outcome.dataset;

        // Deterministic IDs, assigned by code.
        assert_eq!(dataset.roles[0].id, RoleId("role-1".into()));
        assert_eq!(dataset.roles[1].id, RoleId("role-2".into()));
        assert_eq!(dataset.skills.skills[0].id, SkillId("skill-1".into()));

        // The bullet's skill name resolved through the alias map, and the
        // role aggregated its bullets' skills.
        let skill_id = SkillId("skill-1".into());
        assert_eq!(
            dataset.roles[0].bullets[0].skill_ids,
            vec![skill_id.clone()]
        );
        assert_eq!(dataset.roles[0].skill_ids, vec![skill_id.clone()]);

        // Aliases map lowercased names (canonical and alternates) to the ID.
        assert_eq!(
            dataset.skills.aliases.get("algorithm design"),
            Some(&skill_id)
        );
        assert_eq!(dataset.skills.aliases.get("algorithms"), Some(&skill_id));

        // Evidence is the union of bullet-derived and explicit links:
        // role-1 (bullet mention), role-2 (evidence_roles: [1]),
        // project-1 (both directions, deduped), certification-1.
        let evidence = &dataset.skills.skills[0].evidence;
        assert!(evidence.contains(&EvidenceRef::Role(RoleId("role-1".into()))));
        assert!(evidence.contains(&EvidenceRef::Role(RoleId("role-2".into()))));
        assert!(evidence.contains(&EvidenceRef::Project(ProjectId("project-1".into()))));
        assert!(
            evidence.contains(&EvidenceRef::Certification(CertificationId(
                "certification-1".into()
            )))
        );
        assert_eq!(evidence.len(), 4, "duplicate evidence must be deduped");

        assert!(outcome.warnings.is_empty(), "got: {:?}", outcome.warnings);

        // The request carried our prompt and the resume text.
        let requests = mock.requests();
        assert_eq!(requests[0].model, "test-model");
        assert!(
            requests[0]
                .system
                .as_deref()
                .unwrap()
                .contains("Never invent")
        );
        assert_eq!(requests[0].messages[0].content, "the resume text");
    }

    #[tokio::test]
    async fn fenced_replies_are_unwrapped() {
        let mock = MockLlmClient::default();
        mock.enqueue(format!("```json\n{GOOD_REPLY}\n```"));
        let outcome = ingest_resume(&mock, "m", "text").await.unwrap();
        assert_eq!(outcome.dataset.roles.len(), 2);
    }

    #[tokio::test]
    async fn a_malformed_reply_is_a_typed_error_with_a_snippet() {
        let mock = MockLlmClient::default();
        mock.enqueue("Sure! Here is the JSON you asked for: {");
        let err = ingest_resume(&mock, "m", "text").await.unwrap_err();
        match err {
            IngestError::BadReply { snippet, .. } => {
                assert!(snippet.starts_with("Sure!"));
            }
            other => panic!("expected BadReply, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_skill_mentions_warn_instead_of_fabricating() {
        let mock = MockLlmClient::default();
        mock.enqueue(
            r#"{"contact": {"full_name": "A", "email": "a@b.c"},
                "roles": [{"company": "X", "title": "Dev", "start": "2020-01",
                           "bullets": [{"text": "Used Rust", "skills": ["Rust"]}]}],
                "skills": []}"#,
        );
        let outcome = ingest_resume(&mock, "m", "text").await.unwrap();
        // The mention is reported, not silently turned into a skill entry.
        assert!(outcome.dataset.skills.skills.is_empty());
        assert!(outcome.dataset.roles[0].bullets[0].skill_ids.is_empty());
        assert!(outcome.warnings.iter().any(|w| w.contains("\"Rust\"")));
    }

    #[tokio::test]
    async fn roles_without_a_start_date_are_dropped_with_a_warning() {
        let mock = MockLlmClient::default();
        mock.enqueue(
            r#"{"contact": {"full_name": "A", "email": "a@b.c"},
                "roles": [
                    {"company": "NoDates Inc", "title": "Ghost", "bullets": []},
                    {"company": "Real Corp", "title": "Dev", "start": "2021-03", "bullets": []}
                ],
                "skills": [{"name": "Rust", "evidence_roles": [1]}]}"#,
        );
        let outcome = ingest_resume(&mock, "m", "text").await.unwrap();
        // Only the dated role survives — but it keeps the ID its wire
        // position implies, so the model's evidence index still lands.
        assert_eq!(outcome.dataset.roles.len(), 1);
        assert_eq!(outcome.dataset.roles[0].id, RoleId("role-2".into()));
        assert_eq!(
            outcome.dataset.skills.skills[0].evidence,
            vec![EvidenceRef::Role(RoleId("role-2".into()))]
        );
        assert!(outcome.warnings.iter().any(|w| w.contains("NoDates")));
    }

    #[test]
    fn strip_fences_handles_the_common_shapes() {
        assert_eq!(strip_fences("{\"a\": 1}"), "{\"a\": 1}");
        assert_eq!(strip_fences("```json\n{\"a\": 1}\n```"), "{\"a\": 1}");
        assert_eq!(strip_fences("```\n{\"a\": 1}\n```"), "{\"a\": 1}");
        assert_eq!(strip_fences("  {\"a\": 1}  "), "{\"a\": 1}");
    }

    #[test]
    #[ignore = "exercise: the model sometimes lists the same skill twice (e.g. \"Rust\" and \"rust\"); merge duplicate skills in assemble — union their aliases and evidence, keep the first's category and proficiency — then finish this test"]
    fn ex_007_duplicate_skills_are_merged() {
        // Once merging exists: feed a RawResume whose skills list has two
        // entries with names differing only in case, and assert the
        // dataset has one skill whose aliases and evidence are the union.
        let merging_implemented = false;
        assert!(merging_implemented);
    }
}
