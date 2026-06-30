//! The resume dataset: the single local source of truth that every
//! tailored resume is built from (PRD §7.1).
//!
//! The shape of these types carries the project's core invariant: nothing
//! reaches an output document unless it traces back to something recorded
//! here. Most visibly, a `Skill` lists `EvidenceRef`s pointing at the
//! roles, projects, or certifications that back it — a skill with no
//! evidence fails validation and is excluded from tailoring.

use std::collections::HashMap;
use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The version of the on-disk JSON layout these types describe. Bumped on
/// any breaking change so an old binary refuses a newer file instead of
/// silently misreading it.
pub const SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------
// Identifiers
//
// Each entity gets its own ID type so they cannot be mixed up: a function
// wanting a `SkillId` will not compile when handed a `RoleId`, even though
// both are strings underneath. `#[serde(transparent)]` keeps the JSON a
// plain string.
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RoleId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BulletId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SkillId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProjectId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CertificationId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AchievementId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SampleId(pub String);

// ---------------------------------------------------------------------
// Dates
// ---------------------------------------------------------------------

/// A calendar month, the resolution resumes use for date ranges. Stored
/// in JSON as a `"YYYY-MM"` string.
// EXERCISE(EX-005)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct YearMonth {
    pub year: u16,
    pub month: u8,
}

/// The input did not look like `"YYYY-MM"` with a month of 01–12.
#[derive(Debug, thiserror::Error)]
#[error("expected a date like 2023-05 (YYYY-MM), got {0:?}")]
pub struct InvalidYearMonth(String);

impl TryFrom<String> for YearMonth {
    type Error = InvalidYearMonth;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let invalid = || InvalidYearMonth(value.clone());
        let (year, month) = value.split_once('-').ok_or_else(invalid)?;
        let year: u16 = year.parse().map_err(|_| invalid())?;
        let month: u8 = month.parse().map_err(|_| invalid())?;
        if !(1..=12).contains(&month) {
            return Err(invalid());
        }
        Ok(Self { year, month })
    }
}

impl From<YearMonth> for String {
    fn from(value: YearMonth) -> Self {
        value.to_string()
    }
}

impl fmt::Display for YearMonth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}", self.year, self.month)
    }
}

// ---------------------------------------------------------------------
// The dataset
// ---------------------------------------------------------------------

/// Everything aarg knows about the user's career. Persisted as
/// `dataset.json`; every tailored resume is assembled from this and only
/// this.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResumeDataset {
    pub schema_version: u32,
    pub contact: Contact,
    pub summary: Option<String>,
    /// Whether the user has confirmed `summary` as their own words (via the
    /// objection triage's summary refine). When true, tailoring and the human
    /// variant use it verbatim instead of regenerating or rewording it.
    /// `#[serde(default)]` so datasets written before this field load as false.
    #[serde(default)]
    pub summary_confirmed: bool,
    pub roles: Vec<Role>,
    pub education: Vec<Education>,
    pub skills: SkillGraph,
    pub projects: Vec<Project>,
    pub certifications: Vec<Certification>,
    /// Wins that are reusable across roles (awards, talks, open source).
    pub achievements: Vec<Achievement>,
    pub publications: Vec<Publication>,
    pub languages: Vec<HumanLanguage>,
    /// Writing in the user's own words; anchors voice rewrites in Phase 3.
    pub voice_samples: Vec<VoiceSample>,
    pub metadata: DatasetMetadata,
}

impl ResumeDataset {
    /// An empty dataset for the given person, stamped with the current
    /// schema version and creation time.
    pub fn new(contact: Contact) -> Self {
        let now = Utc::now();
        Self {
            schema_version: SCHEMA_VERSION,
            contact,
            summary: None,
            summary_confirmed: false,
            roles: Vec::new(),
            education: Vec::new(),
            skills: SkillGraph::default(),
            projects: Vec::new(),
            certifications: Vec::new(),
            achievements: Vec::new(),
            publications: Vec::new(),
            languages: Vec::new(),
            voice_samples: Vec::new(),
            metadata: DatasetMetadata {
                created_at: now,
                updated_at: now,
                source_files: Vec::new(),
                declined_skills: Vec::new(),
                dismissed_objections: Vec::new(),
            },
        }
    }

    /// The next `bullet-N` id, continuing the highest one already used
    /// across every role. Shared by the flows that add bullets
    /// (verification evidence, role enrichment) so ids never collide.
    pub fn next_bullet_id(&self) -> BulletId {
        let highest = self
            .roles
            .iter()
            .flat_map(|role| role.bullets.iter())
            .filter_map(|bullet| bullet.id.0.strip_prefix("bullet-")?.parse::<u32>().ok())
            .max()
            .unwrap_or(0);
        BulletId(format!("bullet-{}", highest + 1))
    }
}

/// How to reach the person; rendered at the top of every resume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    pub full_name: String,
    pub email: String,
    pub phone: Option<String>,
    pub location: Option<String>,
    /// Personal site, GitHub, LinkedIn, ... in display order.
    pub links: Vec<Link>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    /// Display label, e.g. "GitHub".
    pub label: String,
    pub url: String,
}

/// One job held: the unit work history is organized around.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Role {
    pub id: RoleId,
    pub company: String,
    pub title: String,
    pub start: YearMonth,
    /// `None` means the role is current.
    pub end: Option<YearMonth>,
    pub location: Option<String>,
    pub employment_type: EmploymentType,
    pub bullets: Vec<Bullet>,
    pub skill_ids: Vec<SkillId>,
    /// 1–2 sentences of company/team context the JD matcher can use.
    pub context: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmploymentType {
    FullTime,
    PartTime,
    Contract,
    Founder,
    Freelance,
    Internship,
}

/// One resume line: a single accomplishment or responsibility.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bullet {
    pub id: BulletId,
    pub text: String,
    pub skill_ids: Vec<SkillId>,
    /// The quantified result inside the text, if there is one.
    pub metric: Option<Metric>,
    /// What the bullet demonstrates, e.g. "leadership", "0-to-1".
    pub theme: Vec<Theme>,
    /// How strong this bullet is, used when selecting which to include.
    pub strength: Strength,
    /// Alternate phrasings of the same fact (library UX, post-v1).
    pub variants: Vec<String>,
}

/// A quantified claim ("cut p99 latency 40%"). Kept as text for now;
/// structure (value, unit, baseline) waits until selection logic needs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Metric(pub String);

/// A topical tag bullets are grouped and selected by.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Theme(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Strength {
    High,
    Medium,
    Low,
}

/// One skill and — critically — the evidence backing it. An empty
/// `evidence` vec fails `aarg dataset validate` and excludes the skill
/// from tailoring: this field is the type-level half of never-fabricate
/// (FR-1.7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Skill {
    pub id: SkillId,
    pub canonical_name: String,
    /// Other names JDs use for the same thing ("k8s" for "Kubernetes").
    pub aliases: Vec<String>,
    pub category: SkillCategory,
    pub proficiency: Proficiency,
    pub years: Option<f32>,
    pub last_used: Option<YearMonth>,
    pub evidence: Vec<EvidenceRef>,
    /// Whether the user has confirmed this skill interactively (Phase 3).
    pub verified: bool,
    pub verified_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillCategory {
    Hard,
    Soft,
    Domain,
    Tool,
    Language,
    Framework,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Proficiency {
    Familiar,
    Working,
    Proficient,
    Expert,
}

/// Where a skill claim comes from. Stored in JSON as
/// `{"type": "role", "id": "..."}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "id", rename_all = "snake_case")]
pub enum EvidenceRef {
    Role(RoleId),
    Project(ProjectId),
    Certification(CertificationId),
}

/// All skills plus a lookup from any known alias to its canonical skill.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SkillGraph {
    pub skills: Vec<Skill>,
    pub aliases: HashMap<String, SkillId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Education {
    pub institution: String,
    /// e.g. "BSc Computer Science".
    pub credential: String,
    pub start: Option<YearMonth>,
    pub end: Option<YearMonth>,
    pub location: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub summary: String,
    pub url: Option<String>,
    pub skill_ids: Vec<SkillId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Certification {
    pub id: CertificationId,
    pub name: String,
    pub issuer: String,
    pub issued: Option<YearMonth>,
    pub expires: Option<YearMonth>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Achievement {
    pub id: AchievementId,
    pub text: String,
    pub skill_ids: Vec<SkillId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Publication {
    pub title: String,
    pub venue: Option<String>,
    pub date: Option<YearMonth>,
    pub url: Option<String>,
}

/// A spoken language (the `skills` graph covers programming languages).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HumanLanguage {
    pub name: String,
    pub fluency: Fluency,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fluency {
    Native,
    Fluent,
    Professional,
    Conversational,
    Basic,
}

/// A snippet of the user's own writing, used to keep rewrites in their
/// voice instead of LLM house style.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VoiceSample {
    pub id: SampleId,
    pub text: String,
    pub captured_at: DateTime<Utc>,
    /// Where it came from: "Slack message", "blog post", ...
    pub context: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatasetMetadata {
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The files this dataset was ingested from, for provenance.
    pub source_files: Vec<String>,
    /// Lowercased names of skills a job wanted that the user said they
    /// don't have. Remembered so verification never re-asks a settled
    /// "no". `#[serde(default)]` keeps older dataset files loading.
    #[serde(default)]
    pub declined_skills: Vec<String>,
    /// Reviewer objections the user has accepted as intentional, so the
    /// adversarial loop stops re-litigating them across runs. The same
    /// "remember a settled decision" idea as `declined_skills`, one rung up.
    #[serde(default)]
    pub dismissed_objections: Vec<DismissedObjection>,
}

/// A reviewer objection the user accepted as intentional. Stored as a
/// stable `(target, kind)` signature, not the objection's free-text
/// message — the wording varies run to run, but *what it's about* doesn't.
/// Kept as plain strings so the data model stays decoupled from the review
/// taxonomy; `review.rs` computes the signature from an `Objection`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DismissedObjection {
    /// What the objection targets: `"bullet:<id>"`, `"summary"`,
    /// `"skills"`, `"layout"`, or `"overall"`.
    pub target: String,
    /// The objection kind's stable tag, e.g. `"vague_verb"`.
    pub kind: String,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn year_month_parses_and_displays() {
        let ym = YearMonth::try_from("2023-05".to_string()).unwrap();
        assert_eq!(
            ym,
            YearMonth {
                year: 2023,
                month: 5
            }
        );
        assert_eq!(ym.to_string(), "2023-05");
    }

    #[test]
    fn year_month_rejects_garbage() {
        for bad in ["2023", "202305", "2023-13", "2023-00", "20x3-05", "2023-1x"] {
            assert!(
                YearMonth::try_from(bad.to_string()).is_err(),
                "{bad:?} should not parse"
            );
        }
    }

    #[test]
    #[ignore = "exercise: YearMonth values cannot be compared yet; implement Ord (year first, then month) so date ranges can be sorted newest-first, then finish this test"]
    fn ex_005_year_months_order_chronologically() {
        // Once YearMonth is Ord: build a few out-of-order values, sort
        // them, and assert chronological order — including two dates in
        // the same year.
        let ordering_implemented = false;
        assert!(ordering_implemented);
    }

    #[test]
    fn evidence_ref_uses_a_tagged_wire_shape() {
        let role = EvidenceRef::Role(RoleId("r-1".into()));
        let json = serde_json::to_value(&role).unwrap();
        assert_eq!(json, serde_json::json!({"type": "role", "id": "r-1"}));

        let back: EvidenceRef =
            serde_json::from_value(serde_json::json!({"type": "certification", "id": "c-9"}))
                .unwrap();
        assert_eq!(
            back,
            EvidenceRef::Certification(CertificationId("c-9".into()))
        );
    }

    #[test]
    fn dataset_round_trips_through_json() {
        let mut dataset = ResumeDataset::new(Contact {
            full_name: "Ada Lovelace".into(),
            email: "ada@example.com".into(),
            phone: None,
            location: Some("London".into()),
            links: vec![Link {
                label: "GitHub".into(),
                url: "https://github.com/ada".into(),
            }],
        });
        dataset.roles.push(Role {
            id: RoleId("r-1".into()),
            company: "Analytical Engines Ltd".into(),
            title: "Principal Engineer".into(),
            start: YearMonth {
                year: 1842,
                month: 9,
            },
            end: None,
            location: None,
            employment_type: EmploymentType::FullTime,
            bullets: vec![Bullet {
                id: BulletId("b-1".into()),
                text: "Wrote the first published computer program".into(),
                skill_ids: vec![SkillId("s-1".into())],
                metric: None,
                theme: vec![Theme("0-to-1".into())],
                strength: Strength::High,
                variants: Vec::new(),
            }],
            skill_ids: vec![SkillId("s-1".into())],
            context: None,
        });
        dataset.skills.skills.push(Skill {
            id: SkillId("s-1".into()),
            canonical_name: "Algorithm design".into(),
            aliases: vec!["algorithms".into()],
            category: SkillCategory::Hard,
            proficiency: Proficiency::Expert,
            years: Some(10.0),
            last_used: Some(YearMonth {
                year: 1843,
                month: 7,
            }),
            evidence: vec![EvidenceRef::Role(RoleId("r-1".into()))],
            verified: false,
            verified_at: None,
        });

        let json = serde_json::to_string_pretty(&dataset).unwrap();
        let back: ResumeDataset = serde_json::from_str(&json).unwrap();
        assert_eq!(back, dataset);
    }

    #[test]
    fn unknown_enum_values_fail_loudly() {
        // The dataset is the source of truth — a typo'd category must be
        // a parse error, never silently dropped or defaulted.
        let result = serde_json::from_value::<SkillCategory>(serde_json::json!("hardware"));
        assert!(result.is_err());
    }
}
