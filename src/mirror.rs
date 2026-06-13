//! Evidence-gated phrase mirroring (PRD §2.2, FR-1.8). When a JD asks
//! for a keyword in wording the user's dataset expresses differently, and
//! a *recorded skill backs the concept*, surface the JD's phrasing on the
//! page so a literal ATS scan credits it — without ever inserting a claim
//! the user can't back.
//!
//! A **service**: pure code, no LLM. The gate is the whole point. A
//! phrase is mirrorable ONLY when its meaningful tokens are a subset of
//! some recorded skill's tokens — i.e. the user demonstrably has the
//! concept, the JD just words it differently ("AI-powered products" vs a
//! recorded "AI-Powered Product Development"). A phrase with no such
//! backing is never returned here: the coverage report reports it and
//! stops, exactly as the never-fabricate rule's known backdoor demands.
//! This module is the *only* path by which a JD phrase reaches the page
//! without being literally present in the dataset, so the subset gate is
//! load-bearing.

use crate::dataset::types::ResumeDataset;
use crate::jd::JobRequirements;
use crate::keywords::keyword_key;

/// A JD phrase a recorded skill backs, paired with the skill backing it —
/// the auditable unit of a mirror.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorMatch {
    pub phrase: String,
    pub dataset_skill: String,
}

/// JD ATS phrases worth mirroring onto the page: those whose normalized
/// tokens are a subset of some recorded skill's tokens. Excludes the role
/// title (a headline's job, not a skill's) and single-token phrases (too
/// loose to gate safely — "data" alone subsets half a dataset).
pub fn backed_phrases(jd: &JobRequirements, dataset: &ResumeDataset) -> Vec<MirrorMatch> {
    let title_key = keyword_key(&jd.title);
    // Each *evidence-backed* skill's token set, computed once. The
    // evidence filter is load-bearing: it makes the mirror gate exactly
    // as strict as the normal skill path (`assemble` only emits skills
    // with evidence), so a recorded-but-unsubstantiated skill can't
    // license its JD wording onto the page.
    let skills: Vec<(Vec<String>, &str)> = dataset
        .skills
        .skills
        .iter()
        .filter(|s| !s.evidence.is_empty())
        .map(|s| (keyword_key(&s.canonical_name), s.canonical_name.as_str()))
        .collect();

    let mut out: Vec<MirrorMatch> = Vec::new();
    let mut seen: Vec<Vec<String>> = Vec::new();
    for phrase in &jd.ats_phrases {
        let key = keyword_key(phrase);
        if key.len() < 2 || key == title_key || seen.contains(&key) {
            continue;
        }
        // The gate: every token of the phrase appears in the skill, so the
        // phrase is the same competency in the JD's words.
        if let Some((_, skill_name)) = skills
            .iter()
            .find(|(skill_key, _)| key.iter().all(|t| skill_key.contains(t)))
        {
            seen.push(key);
            out.push(MirrorMatch {
                phrase: phrase.clone(),
                dataset_skill: (*skill_name).to_string(),
            });
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::dataset::types::{
        Contact, EvidenceRef, Proficiency, RoleId, Skill, SkillCategory, SkillId,
    };
    use crate::jd::{RemotePolicy, Seniority};

    fn dataset_with_skill(name: &str) -> ResumeDataset {
        dataset_with(name, true)
    }

    fn dataset_with(name: &str, evidenced: bool) -> ResumeDataset {
        let mut d = ResumeDataset::new(Contact {
            full_name: "Ada".into(),
            email: "ada@example.com".into(),
            phone: None,
            location: None,
            links: Vec::new(),
        });
        d.skills.skills.push(Skill {
            id: SkillId("skill-1".into()),
            canonical_name: name.into(),
            aliases: Vec::new(),
            category: SkillCategory::Domain,
            proficiency: Proficiency::Working,
            years: None,
            last_used: None,
            evidence: if evidenced {
                vec![EvidenceRef::Role(RoleId("role-1".into()))]
            } else {
                Vec::new()
            },
            verified: true,
            verified_at: None,
        });
        d
    }

    fn jd(title: &str, phrases: &[&str]) -> JobRequirements {
        JobRequirements {
            company: "amplo".into(),
            title: title.into(),
            seniority: Seniority::Senior,
            location: None,
            remote: RemotePolicy::Unspecified,
            domain_keywords: Vec::new(),
            required_skills: Vec::new(),
            preferred_skills: Vec::new(),
            responsibilities: Vec::new(),
            ats_phrases: phrases.iter().map(|p| (*p).to_string()).collect(),
            raw_text: String::new(),
            source_url: None,
        }
    }

    #[test]
    fn a_wording_variant_of_a_recorded_skill_is_mirrored() {
        let dataset = dataset_with_skill("AI-Powered Product Development");
        let jd = jd("Staff Engineer", &["AI-powered products"]);

        let matches = backed_phrases(&jd, &dataset);

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].phrase, "AI-powered products");
        assert_eq!(matches[0].dataset_skill, "AI-Powered Product Development");
    }

    #[test]
    fn an_unbacked_phrase_is_never_mirrored_even_if_it_would_raise_coverage() {
        // The backdoor test: the user has NO matching skill, so the
        // phrase must not be surfaced no matter how much an ATS wants it.
        let dataset = dataset_with_skill("AI-Powered Product Development");
        let jd = jd("Staff Engineer", &["insurance underwriting"]);

        assert!(backed_phrases(&jd, &dataset).is_empty());
    }

    #[test]
    fn a_partial_token_overlap_is_not_enough() {
        // "data privacy" shares only "data" with "data engineering" — not
        // a subset, so not the same competency. Stays a reported miss.
        let dataset = dataset_with_skill("Data Engineering");
        let jd = jd("Staff Engineer", &["data privacy"]);

        assert!(backed_phrases(&jd, &dataset).is_empty());
    }

    #[test]
    fn a_single_token_phrase_is_too_loose_to_gate() {
        // "engineering" is one token; even though it subsets the skill,
        // single-token phrases match too readily to mirror safely.
        let dataset = dataset_with_skill("Data Engineering");
        let jd = jd("Staff Engineer", &["engineering"]);

        assert!(backed_phrases(&jd, &dataset).is_empty());
    }

    #[test]
    fn a_skill_with_no_evidence_cannot_back_a_mirror() {
        // The skill subsumes the phrase by tokens, but it has no evidence
        // — so it can't reach the page itself, and it can't license a
        // mirror either. Same gate as the normal skill path.
        let dataset = dataset_with("AI-Powered Product Development", false);
        let jd = jd("Staff Engineer", &["AI-powered products"]);

        assert!(backed_phrases(&jd, &dataset).is_empty());
    }

    #[test]
    fn the_role_title_is_never_mirrored_as_a_skill() {
        // Even if a title-shaped skill somehow exists, the JD title is a
        // headline concern, not a skill to surface.
        let dataset = dataset_with_skill("Senior Engineering Manager");
        let jd = jd(
            "Senior Engineering Manager",
            &["Senior Engineering Manager"],
        );

        assert!(backed_phrases(&jd, &dataset).is_empty());
    }
}
