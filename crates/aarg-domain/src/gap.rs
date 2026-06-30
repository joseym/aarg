//! Gap analysis: compare a JD's requirements against the dataset and
//! sort every requested skill into one of three buckets (FR-1.5):
//! **matched** (recorded, with evidence), **weak** (recorded, but thin),
//! or **unknown** (not in the dataset at all).
//!
//! The third of the Phase 1 LLM features, same plain-`async fn` shape as
//! `ingest.rs` and `jd.rs` — but here the model's role is smallest of the
//! three. Matching is deterministic wherever possible:
//!
//! 1. **Alias map first (code).** A JD skill whose name resolves through
//!    `SkillGraph::aliases` is matched without any model involvement. If
//!    everything resolves, no LLM call happens at all.
//! 2. **Semantic pass (model), leftovers only.** The model sees the
//!    unresolved JD names and the recorded skill list, and may propose
//!    "this requirement is covered by that recorded skill".
//! 3. **Code is the gate.** Every proposal must itself resolve through
//!    the alias map; a proposed name that isn't actually recorded is
//!    discarded. The model can connect names — it cannot mint a match.
//!
//! Bucketing is pure code: evidence and proficiency live in the dataset,
//! and the never-fabricate invariant means a skill with no evidence can
//! only ever be "weak", whatever the model thinks of it.

use std::collections::HashSet;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::agent::{Agent, AgentContext, AgentRun, ModelTier, run_agent};
use crate::dataset::types::{Proficiency, ResumeDataset, Skill, SkillId};
use crate::jd::{JdSkill, JobRequirements};
use crate::keywords::{is_token_subset, keyword_key};
use crate::llm::{LlmError, TokenUsage};

/// The reply is a small name-to-name mapping.
const REPLY_BUDGET: u32 = 2048;

/// Everything that can go wrong while analyzing the gap.
#[derive(Debug, thiserror::Error)]
pub enum GapError {
    #[error(transparent)]
    Llm(#[from] LlmError),

    #[error("the model's reply was not the expected match JSON (reply began {snippet:?})")]
    BadReply {
        snippet: String,
        #[source]
        source: serde_json::Error,
    },
}

// ---------------------------------------------------------------------
// The report (PRD name used verbatim; persisted as gap_report.json later)
// ---------------------------------------------------------------------

/// The three-bucket comparison of a JD against the dataset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GapReport {
    /// Recorded skills with solid support behind them.
    pub matched: Vec<SkillMatch>,
    /// Recorded, but the support is thin — usable, worth shoring up.
    pub weak: Vec<WeakMatch>,
    /// Not in the dataset; candidates for skill verification (Phase 3).
    pub unknown: Vec<JdSkill>,
}

/// One JD requirement resolved to one recorded skill.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillMatch {
    pub jd_skill: JdSkill,
    pub skill_id: SkillId,
    /// The recorded skill's canonical name (may differ from the JD's).
    pub dataset_name: String,
    /// True when the match is by meaning — the model connected the names,
    /// or the JD phrase's tokens were a subset of the skill's — rather than
    /// an exact recorded name or alias.
    pub semantic: bool,
}

/// A match whose dataset side is thin, and how.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WeakMatch {
    pub matched: SkillMatch,
    pub weakness: Weakness,
}

/// Why a recorded skill counts as weak.
// EXERCISE(EX-010)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Weakness {
    /// No evidence references — excluded from tailoring until backed.
    NoEvidence,
    /// Recorded at `familiar` proficiency, the weakest level.
    LowProficiency,
}

/// What gap analysis works from: the parsed JD and the dataset to
/// compare it against.
#[derive(serde::Serialize)]
pub struct GapInput {
    pub jd: JobRequirements,
    pub dataset: ResumeDataset,
}

/// The gap analyzer as an agent — the one with nonstandard control
/// flow: it overrides `run` so a JD the alias map fully resolves never
/// reaches the model at all.
pub struct GapAnalyzerAgent;

#[async_trait]
impl Agent for GapAnalyzerAgent {
    type Input = GapInput;
    type Wire = RawMatches;
    type Output = GapReport;
    type Error = GapError;

    fn id(&self) -> &'static str {
        "gap_analyzer_v1"
    }
    fn model_tier(&self) -> ModelTier {
        // Matching dataset evidence against JD requirements is structured
        // comparison; the cheap tier keeps this cheap.
        ModelTier::Cheap
    }
    fn system_prompt(&self) -> &str {
        SYSTEM_PROMPT
    }
    fn reply_budget(&self) -> u32 {
        REPLY_BUDGET
    }
    /// Only the leftovers go to the model. `deterministic_pass` is pure
    /// and cheap, so recomputing it here (and again in `assemble`)
    /// keeps these methods stateless rather than threading a cache
    /// through the trait.
    fn user_message(&self, input: &GapInput) -> String {
        let (_, unresolved) = deterministic_pass(&input.jd, &input.dataset);
        semantic_message(&unresolved, &input.dataset)
    }
    fn bad_reply(&self, snippet: String, source: serde_json::Error) -> GapError {
        GapError::BadReply { snippet, source }
    }
    fn assemble(&self, wire: RawMatches, input: GapInput) -> Result<GapReport, GapError> {
        let (mut resolved, unresolved) = deterministic_pass(&input.jd, &input.dataset);
        let mut unknown = Vec::new();
        for jd_skill in unresolved {
            let proposal = wire
                .matches
                .iter()
                .find(|m| m.jd_skill.eq_ignore_ascii_case(&jd_skill.name))
                .and_then(|m| m.dataset_skill.as_deref());
            // The gate: a proposal only counts if the proposed name
            // really resolves to a recorded skill.
            match proposal.and_then(|name| lookup(&input.dataset, name)) {
                Some(id) => resolved.push((jd_skill, id, true)),
                None => unknown.push(jd_skill),
            }
        }
        Ok(bucket(resolved, unknown, &input.dataset))
    }

    /// The override: skip the model when there is nothing to ask it.
    async fn run(
        &self,
        ctx: &AgentContext<'_>,
        input: GapInput,
    ) -> Result<AgentRun<GapReport>, GapError> {
        let (resolved, unresolved) = deterministic_pass(&input.jd, &input.dataset);
        if unresolved.is_empty() || input.dataset.skills.skills.is_empty() {
            // Fully covered (or nothing to match against): zero tokens.
            return Ok(AgentRun {
                output: bucket(resolved, unresolved, &input.dataset),
                usage: TokenUsage::default(),
            });
        }
        run_agent(self, ctx, input).await
    }
}

/// Compare a JD against the dataset. Deterministic matching first; the
/// model only sees what the alias map could not resolve.
pub async fn analyze_gap(
    ctx: &AgentContext<'_>,
    jd: &JobRequirements,
    dataset: &ResumeDataset,
) -> Result<GapReport, GapError> {
    let input = GapInput {
        jd: jd.clone(),
        dataset: dataset.clone(),
    };
    Ok(GapAnalyzerAgent.run(ctx, input).await?.output)
}

/// A deterministic, model-free view of how the dataset covers a job
/// description: which JD skills the alias map / subset match already resolve,
/// and which they can't. This is the no-LLM half of [`analyze_gap`], exposed
/// for callers that want an instant coverage preview without a model call (a
/// wasm UI, say). The uncovered list is exactly what the model's semantic pass
/// would be handed; a portable build stops here.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DeterministicGap {
    /// JD skill names the dataset already covers.
    pub covered: Vec<String>,
    /// JD skill names the deterministic pass could not resolve.
    pub uncovered: Vec<String>,
}

/// Resolve a JD against a dataset using only code (no model call): the
/// deterministic half of [`analyze_gap`].
pub fn deterministic_gap(jd: &JobRequirements, dataset: &ResumeDataset) -> DeterministicGap {
    let (resolved, unresolved) = deterministic_pass(jd, dataset);
    DeterministicGap {
        covered: resolved
            .into_iter()
            .map(|(skill, _, _)| skill.name)
            .collect(),
        uncovered: unresolved.into_iter().map(|skill| skill.name).collect(),
    }
}

/// Pass 1: dedup the JD's asks (required side wins) and resolve what
/// the alias map can. Pure, so the agent can recompute it freely.
fn deterministic_pass(
    jd: &JobRequirements,
    dataset: &ResumeDataset,
) -> (Vec<(JdSkill, SkillId, bool)>, Vec<JdSkill>) {
    let mut seen = HashSet::new();
    let mut resolved = Vec::new();
    let mut unresolved = Vec::new();
    for jd_skill in jd
        .required_skills
        .iter()
        .chain(jd.preferred_skills.iter())
        .filter(|s| seen.insert(s.name.to_lowercase()))
    {
        if let Some(id) = lookup(dataset, &jd_skill.name) {
            resolved.push((jd_skill.clone(), id, false));
        } else if let Some(id) = subset_match(dataset, &jd_skill.name) {
            // Same competency, different words: the JD phrase's tokens are
            // a subset of a recorded skill's ("engineering leadership"
            // inside "engineering team leadership and management"). Code,
            // not the model — but a meaning match, so it's flagged as one.
            resolved.push((jd_skill.clone(), id, true));
        } else {
            unresolved.push(jd_skill.clone());
        }
    }
    (resolved, unresolved)
}

/// A meaning match made in code: the JD phrase's normalized tokens are a
/// subset of an evidence-backed skill's, so it names the same competency
/// in fewer or reordered words. The evidence filter makes this gate
/// exactly as strict as `mirror.rs` — a fuzzy match is the riskier kind,
/// so it leans on a skill the user can actually back, never an unbacked
/// one. Single-token phrases are too loose to gate on ("engineering"
/// alone subsets half a dataset), so they fall through to the model. When
/// several skills qualify, the tightest (fewest extra tokens) wins.
fn subset_match(dataset: &ResumeDataset, name: &str) -> Option<SkillId> {
    let key = keyword_key(name);
    if key.len() < 2 {
        return None;
    }
    dataset
        .skills
        .skills
        .iter()
        .filter(|s| !s.evidence.is_empty())
        .map(|s| (keyword_key(&s.canonical_name), &s.id))
        .filter(|(skill_key, _)| is_token_subset(&key, skill_key))
        .min_by_key(|(skill_key, _)| skill_key.len())
        .map(|(_, id)| id.clone())
}

/// Bucketing: pure code over dataset facts.
fn bucket(
    resolved: Vec<(JdSkill, SkillId, bool)>,
    mut unknown: Vec<JdSkill>,
    dataset: &ResumeDataset,
) -> GapReport {
    let mut matched = Vec::new();
    let mut weak = Vec::new();
    for (jd_skill, skill_id, semantic) in resolved {
        let Some(skill) = dataset.skills.skills.iter().find(|s| s.id == skill_id) else {
            // A dangling alias-map entry; `dataset validate` reports
            // these, gap analysis just refuses to vouch for one.
            unknown.push(jd_skill);
            continue;
        };
        let entry = SkillMatch {
            jd_skill,
            skill_id,
            dataset_name: skill.canonical_name.clone(),
            semantic,
        };
        match weakness_of(skill) {
            None => matched.push(entry),
            Some(weakness) => weak.push(WeakMatch {
                matched: entry,
                weakness,
            }),
        }
    }
    GapReport {
        matched,
        weak,
        unknown,
    }
}

/// Case-insensitive lookup through the dataset's alias map.
fn lookup(dataset: &ResumeDataset, name: &str) -> Option<SkillId> {
    dataset.skills.aliases.get(&name.to_lowercase()).cloned()
}

/// The bucketing rule, in one place: no evidence is always weak (the
/// never-fabricate invariant — unbacked skills can't be presented as
/// matches), and bare familiarity is too thin to lean on in tailoring.
fn weakness_of(skill: &Skill) -> Option<Weakness> {
    if skill.evidence.is_empty() {
        Some(Weakness::NoEvidence)
    } else if skill.proficiency == Proficiency::Familiar {
        Some(Weakness::LowProficiency)
    } else {
        None
    }
}

const SYSTEM_PROMPT: &str = r#"You match job-description requirements against a candidate's recorded skills.

Rules — all of them matter:
- For each requirement, name the one recorded skill that genuinely covers it, or null if none does.
- Match on meaning: "container orchestration" is covered by a recorded "Kubernetes". Never stretch: "Java" does not cover "JavaScript", and a related-but-different tool is not a match.
- "dataset_skill" must be copied verbatim from the recorded-skills list. Never answer with a name that is not on that list.
- Role titles and recurring themes may be listed for context, to help you recognize that a requirement is a synonym of a recorded skill (a "Director of Engineering" makes clear that "engineering leadership" is the recorded "Engineering management"). They are context only: never answer with a role title or a theme.
- Include every requirement exactly once.
- Reply with exactly one JSON object and nothing else — no markdown fences, no commentary.

The JSON object:
{"matches": [{"jd_skill": "<name from the requirements list>", "dataset_skill": "<name from the recorded-skills list, or null>"}]}"#;

/// The user message for the semantic pass: the unresolved requirements
/// and the recorded skills (with aliases) to match them against.
fn semantic_message(unresolved: &[JdSkill], dataset: &ResumeDataset) -> String {
    let mut text = String::from("JD requirements:\n");
    for skill in unresolved {
        text.push_str(&format!("- {}\n", skill.name));
    }
    text.push_str("\nRecorded skills:\n");
    for skill in &dataset.skills.skills {
        if skill.aliases.is_empty() {
            text.push_str(&format!("- {}\n", skill.canonical_name));
        } else {
            text.push_str(&format!(
                "- {} (also known as: {})\n",
                skill.canonical_name,
                skill.aliases.join(", ")
            ));
        }
    }

    // Role titles and themes as *context* (the system prompt forbids
    // answering with them): they let the model see that "engineering
    // leadership" on a Director of Engineering maps to a recorded
    // management skill — the synonym a token match can't reach.
    if !dataset.roles.is_empty() {
        text.push_str("\nFor context only — the candidate's roles:\n");
        for role in &dataset.roles {
            text.push_str(&format!("- {} at {}\n", role.title, role.company));
        }
    }
    let mut themes: Vec<&str> = dataset
        .roles
        .iter()
        .flat_map(|role| role.bullets.iter())
        .flat_map(|bullet| bullet.theme.iter())
        .map(|theme| theme.0.as_str())
        .collect();
    themes.sort_unstable();
    themes.dedup();
    if !themes.is_empty() {
        text.push_str("\nFor context only — recurring themes in their work: ");
        text.push_str(&themes.join(", "));
        text.push('\n');
    }
    text
}

/// The wire shape of the model's reply.
#[derive(Debug, Deserialize)]
pub struct RawMatches {
    #[serde(default)]
    matches: Vec<RawMatch>,
}

#[derive(Debug, Deserialize)]
struct RawMatch {
    jd_skill: String,
    dataset_skill: Option<String>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::dataset::types::{Contact, EvidenceRef, RoleId, SkillCategory};
    use crate::jd::{Importance, RemotePolicy, Seniority};
    use crate::llm::MockLlmClient;

    fn test_ctx(mock: &MockLlmClient) -> AgentContext<'_> {
        AgentContext {
            llm: mock,
            model: &"test-model",
            tracer: &crate::trace::Tracer::DISABLED,
            sink: None,
        }
    }

    fn skill(
        id: &str,
        name: &str,
        aliases: &[&str],
        proficiency: Proficiency,
        evidence: bool,
    ) -> Skill {
        Skill {
            id: SkillId(id.into()),
            canonical_name: name.into(),
            aliases: aliases.iter().map(|a| (*a).to_string()).collect(),
            category: SkillCategory::Tool,
            proficiency,
            years: None,
            last_used: None,
            evidence: if evidence {
                vec![EvidenceRef::Role(RoleId("role-1".into()))]
            } else {
                Vec::new()
            },
            verified: false,
            verified_at: None,
        }
    }

    fn dataset_with(skills: Vec<Skill>) -> ResumeDataset {
        let mut dataset = ResumeDataset::new(Contact {
            full_name: "Ada".into(),
            email: "ada@example.com".into(),
            phone: None,
            location: None,
            links: Vec::new(),
        });
        for s in &skills {
            for name in std::iter::once(&s.canonical_name).chain(s.aliases.iter()) {
                dataset
                    .skills
                    .aliases
                    .insert(name.to_lowercase(), s.id.clone());
            }
        }
        dataset.skills.skills = skills;
        dataset
    }

    fn jd_skill(name: &str, importance: Importance) -> JdSkill {
        JdSkill {
            name: name.into(),
            category: SkillCategory::Tool,
            importance,
            context_phrases: Vec::new(),
        }
    }

    fn jd_with(required: Vec<JdSkill>, preferred: Vec<JdSkill>) -> JobRequirements {
        JobRequirements {
            company: "Acme".into(),
            title: "Engineer".into(),
            seniority: Seniority::Senior,
            location: None,
            remote: RemotePolicy::Remote,
            domain_keywords: Vec::new(),
            required_skills: required,
            preferred_skills: preferred,
            responsibilities: Vec::new(),
            ats_phrases: Vec::new(),
            raw_text: "raw".into(),
            source_url: None,
        }
    }

    #[tokio::test]
    async fn fully_alias_matched_jds_never_call_the_model() {
        let mock = MockLlmClient::default();
        let dataset = dataset_with(vec![skill(
            "skill-1",
            "Kubernetes",
            &["k8s"],
            Proficiency::Proficient,
            true,
        )]);
        let jd = jd_with(vec![jd_skill("k8s", Importance::Required)], vec![]);

        let report = analyze_gap(&test_ctx(&mock), &jd, &dataset).await.unwrap();

        assert_eq!(report.matched.len(), 1);
        assert_eq!(report.matched[0].dataset_name, "Kubernetes");
        assert!(!report.matched[0].semantic);
        assert!(report.weak.is_empty() && report.unknown.is_empty());
        // The decisive assertion: zero tokens were spent.
        assert!(mock.requests().is_empty());
    }

    #[tokio::test]
    async fn a_reworded_requirement_matches_a_recorded_skill_in_code() {
        let mock = MockLlmClient::default();
        // "Engineering leadership" is not a recorded name or alias, but its
        // tokens are a subset of this evidence-backed skill's.
        let dataset = dataset_with(vec![skill(
            "skill-1",
            "Engineering team leadership and management",
            &[],
            Proficiency::Expert,
            true,
        )]);
        let jd = jd_with(
            vec![jd_skill("Engineering leadership", Importance::Required)],
            vec![],
        );

        let report = analyze_gap(&test_ctx(&mock), &jd, &dataset).await.unwrap();

        assert_eq!(report.matched.len(), 1);
        assert_eq!(
            report.matched[0].dataset_name,
            "Engineering team leadership and management"
        );
        assert!(
            report.matched[0].semantic,
            "a reworded match is a meaning match"
        );
        // Code resolved it; the model was never asked.
        assert!(mock.requests().is_empty());
    }

    #[tokio::test]
    async fn a_subset_match_requires_an_evidence_backed_skill() {
        let mock = MockLlmClient::default();
        mock.enqueue(
            r#"{"matches": [{"jd_skill": "Engineering leadership", "dataset_skill": null}]}"#,
        );
        // Same token subset, but the skill has no evidence: the code gate
        // declines it (a fuzzy match leans only on backed skills), so it
        // falls through to the model — which here also declines.
        let dataset = dataset_with(vec![skill(
            "skill-1",
            "Engineering team leadership and management",
            &[],
            Proficiency::Expert,
            false,
        )]);
        let jd = jd_with(
            vec![jd_skill("Engineering leadership", Importance::Required)],
            vec![],
        );

        let report = analyze_gap(&test_ctx(&mock), &jd, &dataset).await.unwrap();

        assert!(report.matched.is_empty());
        assert_eq!(report.unknown.len(), 1);
        assert_eq!(mock.requests().len(), 1, "fell through to the model");
    }

    #[tokio::test]
    async fn leftovers_go_through_the_model_and_its_proposals_are_gated() {
        let mock = MockLlmClient::default();
        mock.enqueue(
            r#"{"matches": [
                {"jd_skill": "container orchestration", "dataset_skill": "Kubernetes"},
                {"jd_skill": "Haskell", "dataset_skill": "Hadoop"},
                {"jd_skill": "PCI-DSS", "dataset_skill": null}
            ]}"#,
        );
        let dataset = dataset_with(vec![
            skill(
                "skill-1",
                "Kubernetes",
                &["k8s"],
                Proficiency::Proficient,
                true,
            ),
            skill("skill-2", "Rust", &[], Proficiency::Expert, true),
        ]);
        let jd = jd_with(
            vec![
                jd_skill("Rust", Importance::Critical),
                jd_skill("container orchestration", Importance::Required),
                jd_skill("Haskell", Importance::Required),
                jd_skill("PCI-DSS", Importance::Required),
            ],
            vec![],
        );

        let report = analyze_gap(&test_ctx(&mock), &jd, &dataset).await.unwrap();

        // Rust matched by alias; container orchestration by the model.
        assert_eq!(report.matched.len(), 2);
        let semantic = report.matched.iter().find(|m| m.semantic).unwrap();
        assert_eq!(semantic.jd_skill.name, "container orchestration");
        assert_eq!(semantic.dataset_name, "Kubernetes");

        // "Hadoop" is not a recorded skill: the proposal is discarded and
        // Haskell stays unknown — the model cannot mint a match.
        let unknown_names: Vec<&str> = report.unknown.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(unknown_names, vec!["Haskell", "PCI-DSS"]);

        // The model only ever saw the leftovers, not the alias hits.
        let requests = mock.requests();
        assert_eq!(requests.len(), 1);
        let sent = &requests[0].messages[0].content;
        let (asked, recorded) = sent.split_once("Recorded skills:").unwrap();
        assert!(asked.contains("container orchestration"));
        assert!(!asked.contains("Rust"), "alias hits must not be re-asked");
        assert!(recorded.contains("(also known as: k8s)"));
    }

    #[tokio::test]
    async fn thin_skills_land_in_the_weak_bucket_with_a_reason() {
        let mock = MockLlmClient::default();
        let dataset = dataset_with(vec![
            skill("skill-1", "TypeScript", &[], Proficiency::Proficient, false),
            skill("skill-2", "GraphQL", &[], Proficiency::Familiar, true),
            skill("skill-3", "Rust", &[], Proficiency::Expert, true),
        ]);
        let jd = jd_with(
            vec![
                jd_skill("TypeScript", Importance::Required),
                jd_skill("GraphQL", Importance::Required),
                jd_skill("Rust", Importance::Required),
            ],
            vec![],
        );

        let report = analyze_gap(&test_ctx(&mock), &jd, &dataset).await.unwrap();

        assert_eq!(report.matched.len(), 1);
        assert_eq!(report.matched[0].dataset_name, "Rust");
        let weakness_of = |name: &str| {
            report
                .weak
                .iter()
                .find(|w| w.matched.dataset_name == name)
                .unwrap()
                .weakness
        };
        assert_eq!(weakness_of("TypeScript"), Weakness::NoEvidence);
        assert_eq!(weakness_of("GraphQL"), Weakness::LowProficiency);
    }

    #[tokio::test]
    async fn a_skill_listed_in_both_jd_sections_is_analyzed_once_as_required() {
        let mock = MockLlmClient::default();
        let dataset = dataset_with(vec![skill(
            "skill-1",
            "Rust",
            &[],
            Proficiency::Expert,
            true,
        )]);
        let jd = jd_with(
            vec![jd_skill("Rust", Importance::Critical)],
            vec![jd_skill("rust", Importance::Preferred)],
        );

        let report = analyze_gap(&test_ctx(&mock), &jd, &dataset).await.unwrap();

        assert_eq!(report.matched.len(), 1);
        assert_eq!(report.matched[0].jd_skill.importance, Importance::Critical);
    }

    #[tokio::test]
    async fn an_empty_dataset_skips_the_model_and_reports_all_unknown() {
        let mock = MockLlmClient::default();
        let dataset = dataset_with(Vec::new());
        let jd = jd_with(vec![jd_skill("Rust", Importance::Required)], vec![]);

        let report = analyze_gap(&test_ctx(&mock), &jd, &dataset).await.unwrap();

        assert_eq!(report.unknown.len(), 1);
        assert!(mock.requests().is_empty(), "nothing to match against");
    }

    #[tokio::test]
    async fn a_malformed_reply_is_a_typed_error_with_a_snippet() {
        let mock = MockLlmClient::default();
        // Two bad replies: the spine's validation-retry consumes one.
        mock.enqueue("The best match would probably be...");
        mock.enqueue("The best match would probably be...");
        let dataset = dataset_with(vec![skill(
            "skill-1",
            "Rust",
            &[],
            Proficiency::Expert,
            true,
        )]);
        let jd = jd_with(vec![jd_skill("Kafka", Importance::Required)], vec![]);

        let err = analyze_gap(&test_ctx(&mock), &jd, &dataset)
            .await
            .unwrap_err();
        match err {
            GapError::BadReply { snippet, .. } => assert!(snippet.starts_with("The best")),
            other => panic!("expected BadReply, got {other:?}"),
        }
    }

    #[test]
    #[ignore = "exercise: a skill last used years ago is also weak; add a Stale variant to Weakness (last_used older than ~3 years counts), wire it into weakness_of, then finish this test"]
    fn ex_010_long_unused_skills_are_weak() {
        // Once Stale exists: build a skill with evidence, proficiency
        // proficient, and last_used around 2019; assert it lands in the
        // weak bucket with Weakness::Stale, and that a recently-used
        // skill does not.
        let staleness_implemented = false;
        assert!(staleness_implemented);
    }
}
