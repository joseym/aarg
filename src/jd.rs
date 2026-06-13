//! JD parsing: turn a job description's text into `JobRequirements`
//! (FR-1.4).
//!
//! The second of the three Phase 1 LLM features, and deliberately the
//! same shape as `ingest.rs`: a plain `async fn` against `LlmClient`,
//! hand-assembled prompt, hand-parsed reply, lenient wire types, and a
//! deterministic assembly step. The near-duplication between these
//! files is intentional — Phase 2's agent abstraction gets extracted
//! from whatever the three working functions genuinely share, and that
//! judgment needs honest material to work from.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::agent::{Agent, AgentContext};
use crate::dataset::types::SkillCategory;
use crate::llm::{LlmClient, LlmError};

/// JDs are shorter than resumes; the structured form is too.
const REPLY_BUDGET: u32 = 4096;

/// Everything that can go wrong while parsing a job description.
#[derive(Debug, thiserror::Error)]
pub enum JdError {
    #[error(transparent)]
    Llm(#[from] LlmError),

    #[error(
        "the model's reply was not the expected job-requirements JSON (reply began {snippet:?})"
    )]
    BadReply {
        snippet: String,
        #[source]
        source: serde_json::Error,
    },
}

// ---------------------------------------------------------------------
// The structured form of a JD (PRD names, used verbatim)
// ---------------------------------------------------------------------

/// What a job description asks for, in matchable form. This is the
/// JD-side counterpart of `ResumeDataset`: gap analysis compares the
/// two, and tailoring mirrors its language.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobRequirements {
    pub company: String,
    pub title: String,
    pub seniority: Seniority,
    pub location: Option<String>,
    pub remote: RemotePolicy,
    /// Industry and domain terms ("fintech", "digital assets").
    pub domain_keywords: Vec<String>,
    pub required_skills: Vec<JdSkill>,
    pub preferred_skills: Vec<JdSkill>,
    /// The role's duties, as stated.
    pub responsibilities: Vec<String>,
    /// Exact phrases from the JD worth mirroring verbatim for ATS scans.
    pub ats_phrases: Vec<String>,
    /// The JD as given — ground truth for review stages.
    pub raw_text: String,
    pub source_url: Option<String>,
}

/// One skill the JD asks for, with where and how hard it asks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JdSkill {
    pub name: String,
    pub category: SkillCategory,
    pub importance: Importance,
    /// Short quotes from the JD where this skill appears.
    pub context_phrases: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Seniority {
    Junior,
    Mid,
    Senior,
    Staff,
    Principal,
    Manager,
    Director,
    Executive,
    /// The JD doesn't say (or says something unmappable).
    Unspecified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemotePolicy {
    Remote,
    Hybrid,
    OnSite,
    Unspecified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Importance {
    /// The JD treats it as make-or-break ("must have", "extensive").
    Critical,
    /// Listed among requirements without special emphasis.
    Required,
    /// Nice-to-have, bonus, "a plus".
    Preferred,
}

/// The JD parser as an agent: the model classifies; assembly fills in
/// what only code can know (the raw text, defaults for omitted fields).
pub struct JdParserAgent;

#[async_trait]
impl Agent for JdParserAgent {
    type Input = String;
    type Wire = RawJd;
    type Output = JobRequirements;
    type Error = JdError;

    fn system_prompt(&self) -> &str {
        SYSTEM_PROMPT
    }
    fn reply_budget(&self) -> u32 {
        REPLY_BUDGET
    }
    fn user_message(&self, input: &String) -> String {
        input.clone()
    }
    fn bad_reply(&self, snippet: String, source: serde_json::Error) -> JdError {
        JdError::BadReply { snippet, source }
    }
    fn assemble(&self, wire: RawJd, input: String) -> Result<JobRequirements, JdError> {
        Ok(assemble(wire, &input))
    }
}

/// Parse a job description.
// EXERCISE(EX-009)
pub async fn parse_jd(
    client: &dyn LlmClient,
    model: &str,
    jd_text: &str,
) -> Result<JobRequirements, JdError> {
    let ctx = AgentContext { llm: client, model };
    Ok(JdParserAgent.run(&ctx, jd_text.to_string()).await?.output)
}

/// The extraction contract. Same discipline as the resume prompt:
/// classify only what the text says, never embellish.
const SYSTEM_PROMPT: &str = r#"You extract structured requirements from job descriptions.

Rules — all of them matter:
- Extract only what the job description actually says. Never invent or embellish requirements that are not in the text.
- Unknown or absent optional values are null. Unknown lists are [].
- required_skills are things the JD demands; preferred_skills are nice-to-haves ("bonus", "a plus", "ideally"). Mark a required skill's importance "critical" only when the JD emphasizes it ("must have", "extensive experience", "deep expertise"); otherwise "required". Preferred skills get importance "preferred".
- context_phrases are short verbatim quotes from the JD where that skill appears.
- ats_phrases are the exact multi-word phrases from the JD that a resume should mirror word-for-word (role title, key technologies, recurring domain terms) — typically 5 to 15.
- domain_keywords are industry/domain terms, not technologies.
- Enum values: seniority is one of "junior"|"mid"|"senior"|"staff"|"principal"|"manager"|"director"|"executive"|"unspecified". remote is one of "remote"|"hybrid"|"on_site"|"unspecified". category is one of "hard"|"soft"|"domain"|"tool"|"language"|"framework".
- Reply with exactly one JSON object and nothing else — no markdown fences, no commentary.

The JSON object:
{
  "company": "...",
  "title": "...",
  "seniority": "director",
  "location": null,
  "remote": "hybrid",
  "domain_keywords": ["..."],
  "required_skills": [{"name": "...", "category": "soft", "importance": "critical", "context_phrases": ["..."]}],
  "preferred_skills": [{"name": "...", "category": "tool", "importance": "preferred", "context_phrases": ["..."]}],
  "responsibilities": ["..."],
  "ats_phrases": ["..."]
}"#;

// ---------------------------------------------------------------------
// The wire shape the model replies with: lenient, no raw_text
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct RawJd {
    #[serde(default)]
    company: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default = "default_seniority")]
    seniority: Seniority,
    location: Option<String>,
    #[serde(default = "default_remote")]
    remote: RemotePolicy,
    #[serde(default)]
    domain_keywords: Vec<String>,
    #[serde(default)]
    required_skills: Vec<RawJdSkill>,
    #[serde(default)]
    preferred_skills: Vec<RawJdSkill>,
    #[serde(default)]
    responsibilities: Vec<String>,
    #[serde(default)]
    ats_phrases: Vec<String>,
}

fn default_seniority() -> Seniority {
    Seniority::Unspecified
}

fn default_remote() -> RemotePolicy {
    RemotePolicy::Unspecified
}

#[derive(Debug, Deserialize)]
struct RawJdSkill {
    name: String,
    #[serde(default = "default_category")]
    category: SkillCategory,
    /// Optional on the wire: the list a skill sits in implies a default.
    importance: Option<Importance>,
    #[serde(default)]
    context_phrases: Vec<String>,
}

fn default_category() -> SkillCategory {
    SkillCategory::Hard
}

fn assemble(raw: RawJd, jd_text: &str) -> JobRequirements {
    let finish = |skills: Vec<RawJdSkill>, fallback: Importance| {
        skills
            .into_iter()
            .map(|s| JdSkill {
                name: s.name,
                category: s.category,
                importance: s.importance.unwrap_or(fallback),
                context_phrases: s.context_phrases,
            })
            .collect()
    };

    JobRequirements {
        company: raw.company.unwrap_or_default(),
        title: raw.title.unwrap_or_default(),
        seniority: raw.seniority,
        location: raw.location,
        remote: raw.remote,
        domain_keywords: raw.domain_keywords,
        required_skills: finish(raw.required_skills, Importance::Required),
        preferred_skills: finish(raw.preferred_skills, Importance::Preferred),
        responsibilities: raw.responsibilities,
        ats_phrases: raw.ats_phrases,
        raw_text: jd_text.to_string(),
        source_url: None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::llm::MockLlmClient;

    const GOOD_REPLY: &str = r#"{
        "company": "Acme Corp",
        "title": "Director of Engineering",
        "seniority": "director",
        "location": "New York, NY",
        "remote": "hybrid",
        "domain_keywords": ["fintech", "payments"],
        "required_skills": [
            {"name": "Engineering management", "category": "soft",
             "importance": "critical",
             "context_phrases": ["7+ years leading engineering teams"]},
            {"name": "Node.js", "category": "framework",
             "context_phrases": ["our stack is Node.js and TypeScript"]}
        ],
        "preferred_skills": [
            {"name": "Rust", "category": "language", "context_phrases": []}
        ],
        "responsibilities": ["Own delivery across four teams"],
        "ats_phrases": ["Director of Engineering", "engineering management"]
    }"#;

    #[tokio::test]
    async fn a_full_reply_becomes_job_requirements() {
        let mock = MockLlmClient::default();
        mock.enqueue(GOOD_REPLY);

        let jd = parse_jd(&mock, "test-model", "the jd text").await.unwrap();

        assert_eq!(jd.company, "Acme Corp");
        assert_eq!(jd.seniority, Seniority::Director);
        assert_eq!(jd.remote, RemotePolicy::Hybrid);
        assert_eq!(jd.required_skills.len(), 2);
        assert_eq!(jd.preferred_skills.len(), 1);
        // Ground truth travels with the structure.
        assert_eq!(jd.raw_text, "the jd text");
        assert_eq!(jd.source_url, None);

        // The request carried our prompt and the JD text.
        let requests = mock.requests();
        assert_eq!(requests[0].model, "test-model");
        assert!(
            requests[0]
                .system
                .as_deref()
                .unwrap()
                .contains("Never invent")
        );
        assert_eq!(requests[0].messages[0].content, "the jd text");
    }

    #[tokio::test]
    async fn omitted_importance_defaults_by_list() {
        let mock = MockLlmClient::default();
        mock.enqueue(GOOD_REPLY);

        let jd = parse_jd(&mock, "m", "text").await.unwrap();

        // Stated importance survives; omitted importance takes the
        // default its list implies.
        assert_eq!(jd.required_skills[0].importance, Importance::Critical);
        assert_eq!(jd.required_skills[1].importance, Importance::Required);
        assert_eq!(jd.preferred_skills[0].importance, Importance::Preferred);
    }

    #[tokio::test]
    async fn fenced_replies_are_unwrapped() {
        let mock = MockLlmClient::default();
        mock.enqueue(format!("```json\n{GOOD_REPLY}\n```"));
        let jd = parse_jd(&mock, "m", "text").await.unwrap();
        assert_eq!(jd.title, "Director of Engineering");
    }

    #[tokio::test]
    async fn a_malformed_reply_is_a_typed_error_with_a_snippet() {
        let mock = MockLlmClient::default();
        // Two bad replies: the spine's validation-retry consumes one.
        mock.enqueue("I'd be happy to help! The job requires...");
        mock.enqueue("I'd be happy to help! The job requires...");
        let err = parse_jd(&mock, "m", "text").await.unwrap_err();
        match err {
            JdError::BadReply { snippet, .. } => assert!(snippet.starts_with("I'd")),
            other => panic!("expected BadReply, got {other:?}"),
        }
    }

    #[test]
    fn job_requirements_round_trip_through_json() {
        // jd.json gets persisted into build directories later; the type
        // must survive serialization both ways.
        let mock_raw: RawJd = serde_json::from_str(GOOD_REPLY).unwrap();
        let jd = assemble(mock_raw, "raw");
        let json = serde_json::to_string(&jd).unwrap();
        let back: JobRequirements = serde_json::from_str(&json).unwrap();
        assert_eq!(back, jd);
    }

    #[test]
    #[ignore = "exercise: parse_jd happily sends empty or whitespace-only input to the API; reject it with a typed error before spending tokens, then finish this test"]
    fn ex_009_empty_input_is_rejected_before_the_api_call() {
        // Once the guard exists: call parse_jd with "" and with "   \n",
        // assert the new error variant comes back, and assert the mock
        // recorded zero requests — the check must happen before the
        // network, not after.
        let guard_implemented = false;
        assert!(guard_implemented);
    }
}
