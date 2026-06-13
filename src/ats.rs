//! ATS keyword coverage: which of the JD's asks actually made it onto
//! the rendered page (FR-1.8). A **service** — pure code, no LLM (PRD
//! §9.3) — and deliberately checked against text extracted from the
//! *PDF*, not the payload: if a template bug drops a section, coverage
//! says so.
//!
//! The never-fabricate rule shows up here as the *suggestion gate*:
//! every miss carries its dataset-evidence status, and only backed
//! misses may ever turn into "mirror this phrase" suggestions. An
//! unbacked miss is reported, full stop — the coverage report must
//! never become the backdoor that inserts unbacked claims (PRD §2.2).

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::dataset::types::ResumeDataset;
use crate::gap::GapReport;
use crate::jd::JobRequirements;

/// Everything that can go wrong while checking coverage.
#[derive(Debug, thiserror::Error)]
pub enum AtsError {
    #[error("could not extract text from {path}")]
    Extract {
        path: std::path::PathBuf,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// The coverage half of the PRD's `AtsReport`. Round-trip fidelity and
/// layout checks join in later phases; starting with only the fields
/// that exist keeps every populated field honest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AtsReport {
    pub keyword_hits: Vec<KeywordHit>,
    pub keyword_misses: Vec<KeywordMiss>,
    /// Fraction of *required* JD skills present on the page.
    pub coverage: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeywordHit {
    pub phrase: String,
    pub kind: KeywordKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeywordMiss {
    pub phrase: String,
    pub kind: KeywordKind,
    /// Whether the dataset could back this phrase — the suggestion gate.
    pub evidence: EvidenceStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeywordKind {
    RequiredSkill,
    PreferredSkill,
    AtsPhrase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum EvidenceStatus {
    /// The gap report matched this to a recorded, usable skill — a
    /// revision could honestly mirror the phrase.
    Backed { dataset_skill: String },
    /// Nothing in the dataset supports it; report, never insert.
    Unbacked,
}

/// Pull the text layer out of the rendered PDF. Non-empty extraction is
/// itself a meaningful check: it proves the PDF has selectable text at
/// all (a scanned-image resume would extract nothing).
pub fn extract_pdf_text(path: &Path) -> Result<String, AtsError> {
    pdf_extract::extract_text(path).map_err(|source| AtsError::Extract {
        path: path.to_path_buf(),
        source: Box::new(source),
    })
}

/// Check every JD skill and ATS phrase against the page text.
// EXERCISE(EX-012)
pub fn keyword_coverage(
    jd: &JobRequirements,
    gap: &GapReport,
    dataset: &ResumeDataset,
    page_text: &str,
) -> AtsReport {
    let haystack = normalize(page_text);
    // What backs a JD name, for second-chance hits (the page says
    // "Kubernetes", the JD said "container orchestration") and for
    // evidence status on misses. Two sources, in order: the gap
    // analyzer's matches (which understand synonyms for JD *skills*),
    // then the dataset directly — because ATS *phrases* never go through
    // gap matching, so a phrase the user has recorded as a skill (e.g. a
    // verified "SOC 2 Type 2") would otherwise read as unbacked.
    let backing = |jd_name: &str| -> Option<String> {
        if let Some(matched) = gap
            .matched
            .iter()
            .find(|m| m.jd_skill.name.eq_ignore_ascii_case(jd_name))
        {
            return Some(matched.dataset_name.clone());
        }
        let lowered = jd_name.to_lowercase();
        if let Some(id) = dataset.skills.aliases.get(&lowered)
            && let Some(skill) = dataset.skills.skills.iter().find(|s| &s.id == id)
        {
            return Some(skill.canonical_name.clone());
        }
        dataset
            .skills
            .skills
            .iter()
            .find(|s| s.canonical_name.eq_ignore_ascii_case(jd_name))
            .map(|s| s.canonical_name.clone())
    };

    let mut hits = Vec::new();
    let mut misses = Vec::new();
    let mut check = |phrase: &str, kind: KeywordKind| {
        let matched_name = backing(phrase);
        let found = contains(&haystack, phrase)
            || matched_name
                .as_deref()
                .is_some_and(|name| contains(&haystack, name));
        if found {
            hits.push(KeywordHit {
                phrase: phrase.to_string(),
                kind,
            });
        } else {
            misses.push(KeywordMiss {
                phrase: phrase.to_string(),
                kind,
                evidence: match matched_name {
                    Some(dataset_skill) => EvidenceStatus::Backed { dataset_skill },
                    None => EvidenceStatus::Unbacked,
                },
            });
        }
    };

    for skill in &jd.required_skills {
        check(&skill.name, KeywordKind::RequiredSkill);
    }
    for skill in &jd.preferred_skills {
        check(&skill.name, KeywordKind::PreferredSkill);
    }
    for phrase in &jd.ats_phrases {
        check(phrase, KeywordKind::AtsPhrase);
    }

    let required_total = jd.required_skills.len();
    let required_hits = hits
        .iter()
        .filter(|h| h.kind == KeywordKind::RequiredSkill)
        .count();
    let coverage = if required_total == 0 {
        1.0
    } else {
        required_hits as f32 / required_total as f32
    };

    AtsReport {
        keyword_hits: hits,
        keyword_misses: misses,
        coverage,
    }
}

/// Lowercase and collapse all whitespace runs to single spaces, so a
/// phrase broken across a line wrap still matches.
fn normalize(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn contains(haystack: &str, phrase: &str) -> bool {
    haystack.contains(&normalize(phrase))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::dataset::types::{Contact, Proficiency, Skill, SkillCategory, SkillId};
    use crate::gap::SkillMatch;
    use crate::jd::{Importance, JdSkill, RemotePolicy, Seniority};

    fn empty_dataset() -> ResumeDataset {
        ResumeDataset::new(Contact {
            full_name: "Ada".into(),
            email: "ada@example.com".into(),
            phone: None,
            location: None,
            links: Vec::new(),
        })
    }

    fn jd_skill(name: &str, importance: Importance) -> JdSkill {
        JdSkill {
            name: name.into(),
            category: SkillCategory::Tool,
            importance,
            context_phrases: Vec::new(),
        }
    }

    fn jd_with(required: Vec<JdSkill>, phrases: Vec<&str>) -> JobRequirements {
        JobRequirements {
            company: "Acme".into(),
            title: "Engineer".into(),
            seniority: Seniority::Senior,
            location: None,
            remote: RemotePolicy::Remote,
            domain_keywords: Vec::new(),
            required_skills: required,
            preferred_skills: Vec::new(),
            responsibilities: Vec::new(),
            ats_phrases: phrases.into_iter().map(String::from).collect(),
            raw_text: "raw".into(),
            source_url: None,
        }
    }

    fn gap_matching(jd_name: &str, dataset_name: &str) -> GapReport {
        GapReport {
            matched: vec![SkillMatch {
                jd_skill: jd_skill(jd_name, Importance::Required),
                skill_id: SkillId("skill-1".into()),
                dataset_name: dataset_name.into(),
                semantic: true,
            }],
            weak: Vec::new(),
            unknown: Vec::new(),
        }
    }

    #[test]
    fn coverage_counts_required_skills_present_on_the_page() {
        let jd = jd_with(
            vec![
                jd_skill("Rust", Importance::Required),
                jd_skill("Kafka", Importance::Required),
            ],
            vec![],
        );
        let gap = GapReport {
            matched: Vec::new(),
            weak: Vec::new(),
            unknown: Vec::new(),
        };

        let report = keyword_coverage(
            &jd,
            &gap,
            &empty_dataset(),
            "Systems work in Rust and Typst.",
        );

        assert_eq!(report.keyword_hits.len(), 1);
        assert_eq!(report.keyword_misses.len(), 1);
        assert!((report.coverage - 0.5).abs() < f32::EPSILON);
        assert_eq!(report.keyword_misses[0].evidence, EvidenceStatus::Unbacked);
    }

    #[test]
    fn the_matched_dataset_name_counts_as_a_second_chance_hit() {
        // The JD says "container orchestration"; the page says
        // "Kubernetes"; the gap report connects them.
        let jd = jd_with(
            vec![jd_skill("container orchestration", Importance::Required)],
            vec![],
        );
        let gap = gap_matching("container orchestration", "Kubernetes");

        let report = keyword_coverage(
            &jd,
            &gap,
            &empty_dataset(),
            "Ran Kubernetes clusters in production.",
        );

        assert_eq!(report.keyword_hits.len(), 1);
        assert!((report.coverage - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn a_backed_miss_names_the_skill_that_could_cover_it() {
        let jd = jd_with(
            vec![jd_skill("container orchestration", Importance::Required)],
            vec![],
        );
        let gap = gap_matching("container orchestration", "Kubernetes");

        // Neither phrase nor dataset name on the page.
        let report = keyword_coverage(&jd, &gap, &empty_dataset(), "Wrote backend services.");

        assert_eq!(
            report.keyword_misses[0].evidence,
            EvidenceStatus::Backed {
                dataset_skill: "Kubernetes".into()
            }
        );
    }

    #[test]
    fn a_recorded_skill_backs_an_ats_phrase_miss() {
        // The user verified "SOC 2 Type 2" as a skill, but the tailored
        // page never printed the literal phrase. The miss must read as
        // backed-by-the-recorded-skill, not "no supporting evidence".
        let jd = jd_with(vec![], vec!["SOC 2 Type 2"]);
        let gap = GapReport {
            matched: Vec::new(),
            weak: Vec::new(),
            unknown: Vec::new(),
        };
        let mut dataset = empty_dataset();
        dataset.skills.skills.push(Skill {
            id: SkillId("skill-9".into()),
            canonical_name: "SOC 2 Type 2".into(),
            aliases: Vec::new(),
            category: SkillCategory::Domain,
            proficiency: Proficiency::Working,
            years: None,
            last_used: None,
            evidence: Vec::new(),
            verified: true,
            verified_at: None,
        });
        dataset
            .skills
            .aliases
            .insert("soc 2 type 2".into(), SkillId("skill-9".into()));

        let report = keyword_coverage(&jd, &gap, &dataset, "Led security and compliance work.");

        assert_eq!(
            report.keyword_misses[0].evidence,
            EvidenceStatus::Backed {
                dataset_skill: "SOC 2 Type 2".into()
            }
        );
    }

    #[test]
    fn phrases_match_across_line_wraps_and_case() {
        let jd = jd_with(vec![], vec!["engineering excellence"]);
        let gap = GapReport {
            matched: Vec::new(),
            weak: Vec::new(),
            unknown: Vec::new(),
        };

        let report = keyword_coverage(
            &jd,
            &gap,
            &empty_dataset(),
            "Drove Engineering\n   Excellence at scale.",
        );

        assert_eq!(report.keyword_hits.len(), 1);
        // No required skills at all: coverage is vacuously full.
        assert!((report.coverage - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    #[ignore = "exercise: matching is substring-based, so a JD asking for \"Java\" is satisfied by a page saying \"JavaScript\"; match on word boundaries instead, then finish this test"]
    fn ex_012_keywords_match_on_word_boundaries() {
        // Once boundary matching exists: a page containing only
        // "JavaScript" must NOT count as a hit for required skill
        // "Java", while "Java, TypeScript" still does.
        let boundaries_implemented = false;
        assert!(boundaries_implemented);
    }
}
