//! Per-paragraph provenance for a drafted cover letter: classify each body
//! paragraph by whether the claims it makes trace back to the candidate's
//! own recorded evidence, so an editing view can show where every paragraph
//! came from. The résumé has the same idea in
//! [`provenance`](crate::provenance) — this is the cover-letter analog, with
//! a shape purpose-built for prose.
//!
//! **Why this is model-graded, not word-matching.** An earlier version of
//! this module was pure deterministic code: it reduced a paragraph and the
//! evidence to normalized tokens and flagged any claim-bearing word the
//! evidence didn't literally contain. That structurally cannot handle
//! paraphrase. A résumé that says "billing" and a paragraph that says
//! "payments" describe the same experience, but they share no token, so the
//! word-matcher flags a true, backed claim as unrecorded. No amount of
//! stopword tuning fixes that — the two words are simply different words for
//! one concept, and natural language has endless such pairs. So the claim
//! dimension moved to a model that judges grounding by **meaning**: it reads
//! the paragraph and the evidence as text and decides whether the paragraph's
//! claims are supported, the same kind of judgment the adversarial reviewer
//! (see [`review`](crate::review)) makes about a résumé draft. Like that
//! reviewer, this agent only ever *classifies* the text it is handed; it has
//! no field in which to author or edit a paragraph, so it cannot become a
//! fabrication path.
//!
//! **Why the digit dimension stayed deterministic.** Whether a paragraph
//! states a number the evidence doesn't is an exact-match question with no
//! ambiguity a model would meaningfully improve — "63" is in the evidence or
//! it isn't. Handing it to the model would only add cost and latency, and
//! (worse) fold a hard, reliable check into a fuzzier judgment. So the number
//! check stays code-owned and runs alongside the model call: the digit set is
//! [`cover`](crate::cover)'s shipped `allowed_digits` guard, reused verbatim,
//! so it can never drift. A paragraph is flagged if *either* the model judges
//! its claims unrecorded *or* the digit check finds an unbacked number; the
//! two are reported separately so the reader sees which fired.
//!
//! Each paragraph lands in one of three buckets
//! ([`CoverParagraphStatus`]):
//! - **grounded** — the paragraph's claims trace to the evidence and it
//!   introduces no unbacked number. A paragraph that paraphrases the résumé,
//!   echoes the posting's language, or builds on something the candidate said
//!   in the interview brief lands here — including when it says the same thing
//!   in different words than the evidence uses.
//! - **unrecorded** — the paragraph asserts experience the evidence doesn't
//!   support (a skill, an employer, a technology, a domain the candidate
//!   never recorded), or states a number the évidence doesn't carry. This is
//!   the one an editing view surfaces. For a claim flag the model returns a
//!   plain-language description of *what* is unbacked ("claims
//!   payments-processing experience the résumé and posting don't mention"),
//!   which is more useful than naming a stray word.
//! - **exempt** — the paragraph makes no specific claim at all: pure
//!   first-person framing like "I'd welcome the opportunity to discuss this
//!   further." Not a flag, and not "grounded" either, because there is
//!   nothing to ground. A cover letter genuinely contains connective
//!   sentences that assert nothing, and calling those "unrecorded" would flag
//!   benign prose as if it were fabricated.
//!
//! **Informational, not enforcement.** Nothing here blocks a build or
//! rewrites a letter. The structural never-fabricate guard for cover letters
//! lives in [`cover::assemble`](crate::cover): a paragraph that states a
//! number the résumé and brief don't back is dropped there, before it can
//! reach a rendered letter, and the same deterministic guard re-runs on a
//! hand-edit ([`cover::guard_edited_paragraphs`](crate::cover)). This module
//! runs on top of what already passed that gate — for an editing view — and
//! reports; it never rejects. An `unrecorded` paragraph is not a violation:
//! never-fabricate governs what the *model* may claim, never what the
//! *candidate* may choose to write.
//!
//! Two deliberate scope decisions:
//! - The greeting and sign-off are never classified. They are filled by code
//!   from the posting's company and the résumé's contact block (see
//!   [`cover`](crate::cover)), never authored by the model, so they carry no
//!   provenance question — and they are not part of
//!   [`CoverLetter::paragraphs`](crate::cover::CoverLetter), so the loop below
//!   never sees them.
//! - The candidate's `voice_samples` are excluded from the evidence. They
//!   anchor tone during generation ("match this voice, do not reuse its
//!   content"), so treating them as evidence would license letter content to
//!   leak in from unrelated writing. They are not a parameter of
//!   [`check_cover_provenance`] at all, so a term that appears only in a voice
//!   sample can never ground a paragraph.

use std::collections::HashSet;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::agent::{Agent, AgentContext, ModelTier};
use crate::cover::{CoverLetter, allowed_digits};
use crate::cover_interview::CoverBrief;
use crate::jd::JobRequirements;
use crate::llm::LlmError;
use crate::tailor::{TailoredResume, digit_runs};

/// A cover-letter interview brief caps how many emphasis and constraint
/// items it shows the model (see [`cover`](crate::cover)'s `allowed_digits`
/// and the generation prompt). The evidence text mirrors that cap, so a
/// hand-edited or reused brief with a long list can't quietly widen what
/// counts as grounded past what the letter could actually have drawn on.
const BRIEF_LIST_CAP: usize = 8;

/// The claim classifier is a parse/match judgment — read a short paragraph,
/// decide whether the evidence supports it — so it runs on the cheap tier,
/// like the JD parser and the gap matcher.
const CLAIM_REPLY_BUDGET: u32 = 1024;

/// The three-way call [`check_cover_provenance`] makes on every paragraph.
/// See the module doc for what each one means, and — as important — what it
/// does not: this is not the never-fabricate gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverParagraphStatus {
    Grounded,
    Unrecorded,
    Exempt,
}

/// One classified paragraph: its text, the call, and — when the call is
/// `unrecorded` — why. Both `unbacked_claim` and `unbacked_digits` describe
/// the two independent reasons a paragraph can be flagged; a grounded or
/// exempt paragraph carries neither.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParagraphProvenance {
    pub text: String,
    pub status: CoverParagraphStatus,
    /// The model's plain-language account of the claim the evidence doesn't
    /// support, present only when the paragraph is `unrecorded` on content
    /// grounds ("claims payments-processing experience the résumé and posting
    /// don't mention"). `None` for grounded, exempt, or a paragraph flagged
    /// only for an unbacked number.
    pub unbacked_claim: Option<String>,
    /// Numbers the paragraph states that the evidence does not (sorted,
    /// deduped) — a percentage, a count, a team size nothing recorded
    /// mentions. Checked deterministically, independent of the model call.
    pub unbacked_digits: Vec<String>,
}

/// A whole letter's provenance, one entry per body paragraph in draft order —
/// nothing for the greeting or sign-off, which are code-filled and carry no
/// provenance question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverProvenanceReport {
    pub paragraphs: Vec<ParagraphProvenance>,
}

/// Everything that can go wrong classifying a letter's paragraphs. The claim
/// judgment is a model call, so it can fail the way every agent call can.
#[derive(Debug, thiserror::Error)]
pub enum CoverProvenanceError {
    #[error(transparent)]
    Llm(#[from] LlmError),

    #[error("the paragraph classifier's reply was not the expected JSON (reply began {snippet:?})")]
    BadReply {
        snippet: String,
        #[source]
        source: serde_json::Error,
    },
}

/// Classify every body paragraph of `letter` against the evidence built from
/// the tailored `resume`, the `jd`, and the interview `brief` when one was
/// gathered. Two independent checks run per paragraph and combine:
/// - the **claim** check is a cheap-tier model call that judges, by meaning,
///   whether the paragraph's claims trace to the evidence text (see
///   [`corpus_text`] and [`CoverClaimAgent`]);
/// - the **digit** check is deterministic: any number the paragraph states
///   that the résumé and brief don't carry is unbacked (see the module doc on
///   why the posting's numbers are excluded, and why this stays code-owned).
///
/// A paragraph is `unrecorded` if either check fires, `exempt` if the model
/// found no claim and no number is unbacked, and `grounded` otherwise. The
/// model never sees the digit question and never authors text — it only
/// classifies — so nothing here can put a claim on the page.
///
/// A letter with no body paragraphs makes no model call and returns an empty
/// report.
pub async fn check_cover_provenance(
    ctx: &AgentContext<'_>,
    letter: &CoverLetter,
    resume: &TailoredResume,
    jd: &JobRequirements,
    brief: Option<&CoverBrief>,
) -> Result<CoverProvenanceReport, CoverProvenanceError> {
    if letter.paragraphs.is_empty() {
        return Ok(CoverProvenanceReport {
            paragraphs: Vec::new(),
        });
    }

    // The digit set is the résumé and brief only — a number in the posting
    // states what the *role* requires ("5+ years"), never what the *candidate*
    // has done, so it can never ground a personal-history claim. This is
    // `cover.rs`'s shipped guard, reused verbatim.
    let corpus_digits: HashSet<String> = allowed_digits(resume, brief);

    // The claim judgment: one cheap-tier call over the whole (short) letter,
    // returning a per-paragraph verdict aligned to `letter.paragraphs`.
    let input = CoverClaimInput {
        evidence: corpus_text(resume, jd, brief),
        paragraphs: letter.paragraphs.clone(),
    };
    let judgments = CoverClaimAgent.run(ctx, input).await?.output;

    let paragraphs = letter
        .paragraphs
        .iter()
        .enumerate()
        .map(|(i, paragraph)| combine(paragraph, judgments.get(i), &corpus_digits))
        .collect();

    Ok(CoverProvenanceReport { paragraphs })
}

/// Fold one paragraph's two checks into a single verdict. The claim verdict
/// comes from the model (or a conservative "couldn't verify" default when the
/// model returned fewer judgments than paragraphs); the digit list is computed
/// here, deterministically. A flag on either axis makes the paragraph
/// `unrecorded`, and the two reasons are reported in their own fields so the
/// reader sees which fired.
fn combine(
    paragraph: &str,
    judgment: Option<&ClaimJudgment>,
    corpus_digits: &HashSet<String>,
) -> ParagraphProvenance {
    let mut unbacked_digits: Vec<String> = digit_runs(paragraph)
        .into_iter()
        .filter(|d| !corpus_digits.contains(d))
        .collect();
    unbacked_digits.sort();

    // A missing judgment (the model under-returned) is treated as "couldn't
    // verify" — flagged for a look, never silently vouched for. This is the
    // safe direction to be wrong for an informational view.
    let claim_status = judgment.map_or(CoverParagraphStatus::Unrecorded, |j| j.status);
    let claim_flagged = claim_status == CoverParagraphStatus::Unrecorded;
    let digit_flagged = !unbacked_digits.is_empty();

    let status = if claim_flagged || digit_flagged {
        CoverParagraphStatus::Unrecorded
    } else if claim_status == CoverParagraphStatus::Exempt {
        CoverParagraphStatus::Exempt
    } else {
        CoverParagraphStatus::Grounded
    };

    // The claim description rides along only when the claim axis is the reason
    // (or part of it). A paragraph flagged solely for a number carries no claim
    // description — its `unbacked_digits` already say why.
    let unbacked_claim = if claim_flagged {
        Some(
            judgment
                .and_then(|j| j.unbacked.clone())
                .unwrap_or_else(|| DEFAULT_UNVERIFIED.to_string()),
        )
    } else {
        None
    };

    ParagraphProvenance {
        text: paragraph.to_string(),
        status,
        unbacked_claim,
        unbacked_digits,
    }
}

/// The description used when a paragraph is flagged on the claim axis but the
/// model gave no reason (or its reply was short a judgment): honest that it
/// could not be confirmed, without pretending to name a specific fault.
const DEFAULT_UNVERIFIED: &str =
    "mentions experience that couldn't be traced to your résumé or the posting";

/// Build the evidence text the claim classifier reads: the same material the
/// generation step draws on — the résumé (summary, target title, each role's
/// company/title/bullets, the skills), the posting (title, company, required
/// and preferred skill names, responsibilities), and the interview brief
/// (angle, emphasis, tone, motivation, constraints) when one is present —
/// gathered into labeled, readable sections rather than a token bag.
///
/// The posting belongs here: a cover letter legitimately echoes the posting's
/// own language — a required skill name, a phrase describing the role or
/// company — and a paragraph that does so is grounded, not inventing. The
/// posting's *numbers* are a separate matter and are handled by the digit
/// check, not this text (see the module doc).
fn corpus_text(
    resume: &TailoredResume,
    jd: &JobRequirements,
    brief: Option<&CoverBrief>,
) -> String {
    let mut text = String::new();

    text.push_str("RÉSUMÉ\n");
    text.push_str(&format!("Summary: {}\n", resume.summary));
    if let Some(title) = &resume.target_title {
        text.push_str(&format!("Target title: {title}\n"));
    }
    for role in &resume.roles {
        text.push_str(&format!("Role: {} at {}\n", role.title, role.company));
        for bullet in &role.bullets {
            text.push_str(&format!("  - {}\n", bullet.text));
        }
    }
    if !resume.skills_section.skills.is_empty() {
        text.push_str(&format!(
            "Skills: {}\n",
            resume.skills_section.skills.join(", ")
        ));
    }

    text.push_str("\nJOB POSTING\n");
    text.push_str(&format!("Title: {} at {}\n", jd.title, jd.company));
    let required: Vec<&str> = jd.required_skills.iter().map(|s| s.name.as_str()).collect();
    if !required.is_empty() {
        text.push_str(&format!("Required skills: {}\n", required.join(", ")));
    }
    let preferred: Vec<&str> = jd
        .preferred_skills
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    if !preferred.is_empty() {
        text.push_str(&format!("Preferred skills: {}\n", preferred.join(", ")));
    }
    for responsibility in &jd.responsibilities {
        text.push_str(&format!("  - {responsibility}\n"));
    }

    if let Some(brief) = brief {
        let mut block = String::new();
        if let Some(angle) = &brief.angle {
            block.push_str(&format!("Angle: {angle}\n"));
        }
        for item in brief.emphasis.iter().take(BRIEF_LIST_CAP) {
            block.push_str(&format!("Emphasize: {item}\n"));
        }
        if let Some(tone) = &brief.tone {
            block.push_str(&format!("Tone: {tone}\n"));
        }
        if let Some(motivation) = &brief.motivation {
            block.push_str(&format!("Motivation: {motivation}\n"));
        }
        for item in brief.constraints.iter().take(BRIEF_LIST_CAP) {
            block.push_str(&format!("Constraint: {item}\n"));
        }
        if !block.is_empty() {
            text.push_str("\nWHAT THE CANDIDATE TOLD US ABOUT THIS LETTER\n");
            text.push_str(&block);
        }
    }

    text
}

// ---------------------------------------------------------------------
// The claim classifier agent
// ---------------------------------------------------------------------

/// What the claim classifier works from: the evidence as readable text and the
/// body paragraphs to judge. Owned and `Serialize`, like every agent input.
#[derive(Serialize)]
pub struct CoverClaimInput {
    pub evidence: String,
    pub paragraphs: Vec<String>,
}

/// One paragraph's claim verdict from the model: the status and, when
/// `unrecorded`, a plain-language account of what isn't supported. The digit
/// axis is not the model's concern, so it never appears here. Public only
/// because it is the [`CoverClaimAgent`]'s `Output`; callers use
/// [`check_cover_provenance`], which folds it into `ParagraphProvenance`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimJudgment {
    status: CoverParagraphStatus,
    unbacked: Option<String>,
}

/// The lenient wire shape: one entry per paragraph, in order.
#[derive(Debug, Deserialize)]
pub struct RawCoverClaims {
    #[serde(default)]
    paragraphs: Vec<RawParagraphClaim>,
}

#[derive(Debug, Deserialize)]
struct RawParagraphClaim {
    #[serde(default)]
    status: String,
    #[serde(default)]
    unbacked: Option<String>,
}

/// Judges, by meaning, whether each cover-letter paragraph's claims trace to
/// the candidate's evidence (PRD never-fabricate discipline, cover-letter
/// analog). Cheap tier — this is a parse/match judgment, not the heavy writing
/// or review work. It only classifies; it has no field to author a paragraph,
/// so it can never introduce a claim.
pub struct CoverClaimAgent;

#[async_trait]
impl Agent for CoverClaimAgent {
    type Input = CoverClaimInput;
    type Wire = RawCoverClaims;
    type Output = Vec<ClaimJudgment>;
    type Error = CoverProvenanceError;

    fn id(&self) -> &'static str {
        "cover_claim_v1"
    }
    fn model_tier(&self) -> ModelTier {
        // Deciding whether a short paragraph's claims are supported by the
        // evidence is structured matching, not generation — the cheap tier.
        ModelTier::Cheap
    }
    fn system_prompt(&self) -> &str {
        SYSTEM_PROMPT
    }
    fn reply_budget(&self) -> u32 {
        CLAIM_REPLY_BUDGET
    }
    fn user_message(&self, input: &CoverClaimInput) -> String {
        build_claim_message(input)
    }
    fn bad_reply(&self, snippet: String, source: serde_json::Error) -> CoverProvenanceError {
        CoverProvenanceError::BadReply { snippet, source }
    }
    fn assemble(
        &self,
        wire: RawCoverClaims,
        input: CoverClaimInput,
    ) -> Result<Vec<ClaimJudgment>, CoverProvenanceError> {
        // Align to the input paragraphs by index. The prompt asks for exactly
        // one verdict per paragraph in order; if the model returns fewer,
        // `check_cover_provenance` treats the missing ones as "couldn't
        // verify" (flagged), never as grounded. Extra verdicts are ignored.
        let judgments = input
            .paragraphs
            .iter()
            .enumerate()
            .map(|(i, _)| match wire.paragraphs.get(i) {
                Some(raw) => ClaimJudgment {
                    status: parse_status(&raw.status),
                    unbacked: raw
                        .unbacked
                        .as_ref()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty()),
                },
                None => ClaimJudgment {
                    status: CoverParagraphStatus::Unrecorded,
                    unbacked: None,
                },
            })
            .collect();
        Ok(judgments)
    }
}

/// Map the model's status string to the enum. Only "grounded" and "exempt"
/// are affirmative; anything else — "unrecorded", an unknown word, an empty
/// field — is treated as `unrecorded`, the conservative call for a check that
/// must never vouch for a claim it isn't sure about.
fn parse_status(raw: &str) -> CoverParagraphStatus {
    match raw.trim().to_lowercase().as_str() {
        "grounded" => CoverParagraphStatus::Grounded,
        "exempt" => CoverParagraphStatus::Exempt,
        _ => CoverParagraphStatus::Unrecorded,
    }
}

const SYSTEM_PROMPT: &str = r#"You check a cover letter for honesty, one paragraph at a time. You are given the candidate's EVIDENCE (their tailored résumé, the job posting, and any notes they gave about this letter) and a numbered list of the letter's body paragraphs. For each paragraph, decide whether the specific claims it makes about the candidate are supported by the evidence.

Judge by MEANING, not by matching words. The evidence and the letter often say the same thing in different words: "billing" in the résumé supports "payments" in a paragraph; "led a team" supports "managed engineers"; "reliability" supports "keeping the platform up". A paragraph that paraphrases a recorded fact, or echoes the posting's own language about the role, is supported. Only flag a paragraph when it asserts experience the evidence genuinely does not support — a skill, employer, technology, industry, or scope of work the candidate never recorded.

Classify each paragraph as exactly one of:
- "grounded": every specific claim it makes is supported by the evidence (possibly in different words).
- "unrecorded": it asserts experience the evidence does not support.
- "exempt": it makes no specific claim to check — pure connecting or framing language, like "I'd welcome the chance to discuss this further" or "Thank you for your consideration".

Do NOT judge numbers. Ignore any figures, percentages, dates, or counts entirely — those are checked separately by other code. Never flag a paragraph just because of a number.

You only classify. You never rewrite, improve, or add to any paragraph.

For an "unrecorded" paragraph, set "unbacked" to a short, plain sentence naming what isn't supported (for example: "claims payments-processing experience the résumé and posting don't mention"). For "grounded" and "exempt", set "unbacked" to "".

Reply with exactly one JSON object and nothing else — no markdown fences, no commentary — with one entry per paragraph, in the same order:
{"paragraphs": [{"status": "grounded", "unbacked": ""}, {"status": "unrecorded", "unbacked": "..."}]}"#;

/// Render the evidence and the numbered paragraphs into the classifier's user
/// message. Numbering makes the "one verdict per paragraph, in order" contract
/// concrete for the model.
fn build_claim_message(input: &CoverClaimInput) -> String {
    let mut text = String::from("EVIDENCE\n");
    text.push_str(&input.evidence);
    text.push_str("\n\nLETTER PARAGRAPHS\n");
    for (i, paragraph) in input.paragraphs.iter().enumerate() {
        text.push_str(&format!("{}. {}\n", i + 1, paragraph));
    }
    text.push_str(&format!(
        "\nClassify all {} paragraph(s) now, as the JSON object specified, one entry per paragraph in order.",
        input.paragraphs.len()
    ));
    text
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::cover::CoverLetter;
    use crate::dataset::types::{Contact, SkillCategory};
    use crate::jd::{Importance, JdSkill, RemotePolicy, Seniority};
    use crate::llm::MockLlmClient;
    use crate::tailor::{
        BuildId, JdId, SkillsSection, TailoredBullet, TailoredResume, TailoredRole,
    };
    use crate::trace::Tracer;
    use chrono::Utc;

    // --- fixtures ---------------------------------------------------------

    fn test_ctx(mock: &MockLlmClient) -> AgentContext<'_> {
        AgentContext {
            llm: mock,
            model: &"test-model",
            tracer: &Tracer::DISABLED,
            sink: None,
        }
    }

    fn contact() -> Contact {
        Contact {
            full_name: "Ada Lovelace".into(),
            email: "ada@example.com".into(),
            phone: None,
            location: None,
            links: Vec::new(),
        }
    }

    /// A résumé whose one bullet names distinctive, checkable content:
    /// Contoso, a billing platform, reliability work, and the figure 12.
    fn resume() -> TailoredResume {
        TailoredResume {
            build_id: BuildId("b1".into()),
            jd_id: JdId("jd1".into()),
            generated_at: Utc::now(),
            contact: contact(),
            target_title: Some("Staff Engineer".into()),
            summary: "Engineering leader focused on billing-platform reliability.".into(),
            roles: vec![TailoredRole {
                id: crate::dataset::types::RoleId("role-1".into()),
                company: "Contoso".into(),
                title: "Director of Engineering".into(),
                start: crate::dataset::types::YearMonth {
                    year: 2020,
                    month: 3,
                },
                end: None,
                location: None,
                bullets: vec![TailoredBullet {
                    source_id: crate::dataset::types::BulletId("bullet-1".into()),
                    text: "Rebuilt the billing pipeline and led reliability work for 12 services"
                        .into(),
                }],
            }],
            education: Vec::new(),
            skills_section: SkillsSection {
                skills: vec!["Distributed systems".into(), "Incident response".into()],
            },
            projects: Vec::new(),
            achievements: Vec::new(),
            certifications: Vec::new(),
        }
    }

    fn jd() -> JobRequirements {
        JobRequirements {
            company: "Acme".into(),
            title: "Platform Engineer".into(),
            seniority: Seniority::Senior,
            location: None,
            remote: RemotePolicy::Remote,
            domain_keywords: Vec::new(),
            required_skills: vec![JdSkill {
                name: "Kubernetes".into(),
                category: SkillCategory::Hard,
                importance: Importance::Critical,
                context_phrases: Vec::new(),
            }],
            preferred_skills: Vec::new(),
            responsibilities: vec!["Own platform reliability at scale".into()],
            ats_phrases: Vec::new(),
            raw_text: String::new(),
            source_url: None,
        }
    }

    /// A letter carrying exactly the paragraphs handed in — greeting and
    /// sign-off are fixed code-filled values a real assembly would use, and
    /// are never part of `paragraphs`.
    fn letter(paragraphs: &[&str]) -> CoverLetter {
        CoverLetter {
            contact: contact(),
            company: "Acme".into(),
            title: "Platform Engineer".into(),
            greeting: "Dear Acme hiring team,".into(),
            paragraphs: paragraphs.iter().map(|p| p.to_string()).collect(),
            signoff: "Ada Lovelace".into(),
        }
    }

    /// Build a mock reply for `check_cover_provenance`: one `{status, unbacked}`
    /// per verdict, in order.
    fn claims_reply(verdicts: &[(&str, &str)]) -> String {
        let entries: Vec<String> = verdicts
            .iter()
            .map(|(status, unbacked)| {
                format!("{{\"status\": \"{status}\", \"unbacked\": \"{unbacked}\"}}")
            })
            .collect();
        format!("{{\"paragraphs\": [{}]}}", entries.join(", "))
    }

    // --- the paraphrase case (the reason this became model-graded) --------

    #[tokio::test]
    async fn a_paraphrase_by_meaning_is_grounded_even_with_no_shared_word() {
        // The motivating case. The résumé says "billing"; the paragraph says
        // "payments" — same concept, no shared word, so the old word-matcher
        // flagged it. The model judges by meaning and calls it grounded; this
        // test proves the plumbing carries the paragraph and the evidence to
        // the model and reads the verdict back correctly.
        let mock = MockLlmClient::default();
        mock.enqueue(claims_reply(&[("grounded", "")]));
        let letter = letter(&["I owned the payments platform end to end at Contoso."]);

        let report = check_cover_provenance(&test_ctx(&mock), &letter, &resume(), &jd(), None)
            .await
            .unwrap();

        assert_eq!(report.paragraphs[0].status, CoverParagraphStatus::Grounded);
        assert!(report.paragraphs[0].unbacked_claim.is_none());
        assert!(report.paragraphs[0].unbacked_digits.is_empty());

        // The evidence the model saw actually carried the paraphrased fact and
        // both paragraphs reached it — the plumbing, not the model, is what
        // this asserts.
        let sent = &mock.requests()[0].messages[0].content;
        assert!(
            sent.contains("billing"),
            "evidence must carry the résumé fact"
        );
        assert!(
            sent.contains("payments platform"),
            "paragraph must reach the model"
        );
    }

    #[tokio::test]
    async fn a_fabricated_claim_is_surfaced_with_the_models_reason() {
        // A genuinely invented employer/technology. The mock stands in for the
        // model's judgment; the test proves an "unrecorded" verdict and its
        // reason are surfaced verbatim into the report.
        let mock = MockLlmClient::default();
        mock.enqueue(claims_reply(&[(
            "unrecorded",
            "claims settlement-systems work at Globex the evidence never mentions",
        )]));
        let letter = letter(&["I built the settlement system at Globex."]);

        let report = check_cover_provenance(&test_ctx(&mock), &letter, &resume(), &jd(), None)
            .await
            .unwrap();

        let p = &report.paragraphs[0];
        assert_eq!(p.status, CoverParagraphStatus::Unrecorded);
        assert_eq!(
            p.unbacked_claim.as_deref(),
            Some("claims settlement-systems work at Globex the evidence never mentions")
        );
        assert!(p.unbacked_digits.is_empty());
    }

    #[tokio::test]
    async fn a_connective_paragraph_is_exempt() {
        let mock = MockLlmClient::default();
        mock.enqueue(claims_reply(&[("exempt", "")]));
        let letter = letter(&["I'd welcome the chance to discuss this further."]);

        let report = check_cover_provenance(&test_ctx(&mock), &letter, &resume(), &jd(), None)
            .await
            .unwrap();

        let p = &report.paragraphs[0];
        assert_eq!(p.status, CoverParagraphStatus::Exempt);
        assert!(p.unbacked_claim.is_none());
        assert!(p.unbacked_digits.is_empty());
    }

    // --- the digit guard: independent of, and not overridden by, the model -

    #[tokio::test]
    async fn a_fabricated_number_flags_even_when_the_model_says_grounded() {
        // The digit guard is deterministic and independent of the claim call.
        // Even with the model scripted to call the claim grounded, the invented
        // figure 63 (nowhere in the résumé) still flags the paragraph, and it
        // is reported as a digit, not a claim.
        let mock = MockLlmClient::default();
        mock.enqueue(claims_reply(&[("grounded", "")]));
        let letter = letter(&["I cut incidents by 63 percent across the 12 services at Contoso."]);

        let report = check_cover_provenance(&test_ctx(&mock), &letter, &resume(), &jd(), None)
            .await
            .unwrap();

        let p = &report.paragraphs[0];
        assert_eq!(
            p.status,
            CoverParagraphStatus::Unrecorded,
            "an unbacked number must flag regardless of the claim verdict"
        );
        assert_eq!(p.unbacked_digits, vec!["63".to_string()]);
        // 12 is a résumé figure, so it is not flagged.
        assert!(!p.unbacked_digits.contains(&"12".to_string()));
        // The paragraph was flagged purely on the number, so there is no claim
        // description — the digit list already says why.
        assert!(p.unbacked_claim.is_none());
    }

    #[tokio::test]
    async fn a_jd_requirement_number_does_not_ground_a_personal_history_claim() {
        // The digit corpus excludes the posting's numbers: "5+ years" in the
        // posting states what the role requires, never what the candidate did,
        // so a paragraph asserting "5 years" as personal history still flags on
        // the digit axis even when the model calls the wording grounded.
        let mock = MockLlmClient::default();
        mock.enqueue(claims_reply(&[("grounded", "")]));
        let mut jd = jd();
        jd.responsibilities = vec!["Bring 5+ years of platform engineering experience".into()];
        let letter = letter(&["I have 5 years of experience with Kubernetes."]);

        let report = check_cover_provenance(&test_ctx(&mock), &letter, &resume(), &jd, None)
            .await
            .unwrap();

        let p = &report.paragraphs[0];
        assert_eq!(p.status, CoverParagraphStatus::Unrecorded);
        assert_eq!(p.unbacked_digits, vec!["5".to_string()]);
    }

    #[tokio::test]
    async fn a_number_recorded_in_the_brief_is_not_flagged() {
        // A figure the candidate recorded in the interview brief is not
        // invented, so it must widen the allowed digit set exactly as
        // generation's guard does.
        let mock = MockLlmClient::default();
        mock.enqueue(claims_reply(&[("grounded", "")]));
        let brief = CoverBrief {
            emphasis: vec!["a 25% cut in incident response time".into()],
            ..CoverBrief::default()
        };
        let letter = letter(&["I drove a 25% cut in incident response time."]);

        let report =
            check_cover_provenance(&test_ctx(&mock), &letter, &resume(), &jd(), Some(&brief))
                .await
                .unwrap();

        let p = &report.paragraphs[0];
        assert_eq!(p.status, CoverParagraphStatus::Grounded);
        assert!(p.unbacked_digits.is_empty());
    }

    // --- plumbing: the prompt, the corpus, alignment ----------------------

    #[tokio::test]
    async fn the_evidence_carries_resume_posting_and_brief_but_not_voice_samples() {
        let mock = MockLlmClient::default();
        mock.enqueue(claims_reply(&[("grounded", "")]));
        let brief = CoverBrief {
            emphasis: vec!["my side project ChessCoach".into()],
            ..CoverBrief::default()
        };
        let letter = letter(&["A paragraph."]);

        check_cover_provenance(&test_ctx(&mock), &letter, &resume(), &jd(), Some(&brief))
            .await
            .unwrap();

        let sent = &mock.requests()[0].messages[0].content;
        // Résumé, posting, and brief all reach the model as evidence.
        assert!(
            sent.contains("Contoso"),
            "résumé role missing from evidence"
        );
        assert!(
            sent.contains("Kubernetes"),
            "posting skill missing from evidence"
        );
        assert!(
            sent.contains("Own platform reliability"),
            "posting responsibility missing from evidence"
        );
        assert!(
            sent.contains("ChessCoach"),
            "brief emphasis missing from evidence"
        );
        // Voice samples are not even a parameter, so nothing from them can be
        // in the evidence — the corpus builder never sees them.
        assert!(!sent.to_lowercase().contains("kitesurfing"));
    }

    #[tokio::test]
    async fn only_body_paragraphs_are_classified_and_numbered_in_order() {
        let mock = MockLlmClient::default();
        mock.enqueue(claims_reply(&[("grounded", ""), ("exempt", "")]));
        let letter = letter(&["First body paragraph.", "Second body paragraph."]);

        let report = check_cover_provenance(&test_ctx(&mock), &letter, &resume(), &jd(), None)
            .await
            .unwrap();

        assert_eq!(report.paragraphs.len(), 2);
        // The greeting and sign-off are code-filled and never sent.
        let sent = &mock.requests()[0].messages[0].content;
        assert!(!sent.contains("Dear Acme"));
        assert!(!sent.contains("Ada Lovelace"));
        // Paragraphs are numbered so the per-index alignment is well defined.
        assert!(sent.contains("1. First body paragraph."));
        assert!(sent.contains("2. Second body paragraph."));
    }

    #[tokio::test]
    async fn a_short_reply_flags_the_unjudged_paragraphs_rather_than_vouching() {
        // The model returned one verdict for a two-paragraph letter. The
        // missing verdict must be treated as "couldn't verify" (flagged), never
        // silently grounded — the safe direction for an informational check.
        let mock = MockLlmClient::default();
        mock.enqueue(claims_reply(&[("grounded", "")]));
        let letter = letter(&["A backed paragraph.", "An unjudged paragraph."]);

        let report = check_cover_provenance(&test_ctx(&mock), &letter, &resume(), &jd(), None)
            .await
            .unwrap();

        assert_eq!(report.paragraphs[0].status, CoverParagraphStatus::Grounded);
        assert_eq!(
            report.paragraphs[1].status,
            CoverParagraphStatus::Unrecorded,
            "an unjudged paragraph must be flagged, not vouched for"
        );
        assert!(report.paragraphs[1].unbacked_claim.is_some());
    }

    #[tokio::test]
    async fn an_empty_letter_makes_no_model_call() {
        let mock = MockLlmClient::default();
        let letter = letter(&[]);

        let report = check_cover_provenance(&test_ctx(&mock), &letter, &resume(), &jd(), None)
            .await
            .unwrap();

        assert!(report.paragraphs.is_empty());
        assert!(
            mock.requests().is_empty(),
            "a letter with no body paragraphs must not call the model"
        );
    }

    #[tokio::test]
    async fn an_unknown_status_word_is_treated_as_unrecorded() {
        // A malformed status string must never read as grounded — the parser
        // defaults anything it doesn't recognize to the conservative flag.
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"paragraphs": [{"status": "maybe?", "unbacked": ""}]}"#);
        let letter = letter(&["A paragraph."]);

        let report = check_cover_provenance(&test_ctx(&mock), &letter, &resume(), &jd(), None)
            .await
            .unwrap();

        assert_eq!(
            report.paragraphs[0].status,
            CoverParagraphStatus::Unrecorded
        );
    }

    #[tokio::test]
    async fn the_report_round_trips_through_json() {
        let mock = MockLlmClient::default();
        mock.enqueue(claims_reply(&[("unrecorded", "claims something unbacked")]));
        let letter = letter(&["I used Zig on the settlement rail."]);

        let report = check_cover_provenance(&test_ctx(&mock), &letter, &resume(), &jd(), None)
            .await
            .unwrap();

        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"unrecorded\""));
        let back: CoverProvenanceReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back, report);
    }

    #[tokio::test]
    async fn a_non_json_reply_is_a_typed_error_not_a_panic() {
        let mock = MockLlmClient::default();
        // Both the first attempt and the one retry are unparseable.
        mock.enqueue("I cannot classify this today.");
        mock.enqueue("Still not JSON.");
        let letter = letter(&["A paragraph."]);

        let err = check_cover_provenance(&test_ctx(&mock), &letter, &resume(), &jd(), None)
            .await
            .unwrap_err();
        assert!(matches!(err, CoverProvenanceError::BadReply { .. }));
    }
}
