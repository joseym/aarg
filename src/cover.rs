//! Cover-letter generation (stretch goal, post-v1): draft a letter for one
//! job from an already-tailored resume, holding the same never-fabricate
//! line the resume does.
//!
//! The letter draws on the canonical [`TailoredResume`] rather than the raw
//! dataset, so it inherits a body of claims that already traced to evidence.
//! The model writes only the body paragraphs; the greeting, sign-off, and
//! contact block are filled by code, so it never invents a recipient or a
//! name. A `digit_runs` guard (shared with tailoring and voice) then drops
//! any paragraph that introduces a number the resume does not state, the
//! same structural check that keeps a reworded bullet honest.
//!
//! Two entry points share this one agent: `aarg cover [build]` (reuse a
//! saved build) and `aarg tailor --cover` (write a letter alongside a fresh
//! run).

use std::collections::HashSet;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::agent::{Agent, AgentContext, ModelTier};
use crate::dataset::types::Contact;
use crate::jd::JobRequirements;
use crate::llm::{LlmError, TokenUsage};
use crate::tailor::{TailoredResume, digit_runs};

/// A cover letter is short; the body plus JSON overhead fits comfortably.
const REPLY_BUDGET: u32 = 1500;

/// Everything that can go wrong drafting a cover letter.
#[derive(Debug, thiserror::Error)]
pub enum CoverLetterError {
    #[error(transparent)]
    Llm(#[from] LlmError),

    #[error("the model's reply was not the expected cover-letter JSON (reply began {snippet:?})")]
    BadReply {
        snippet: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("the model produced no usable cover-letter text")]
    Empty,
}

/// Everything one cover-letter run works from: the canonical tailored
/// resume (already evidence-gated), the job it targets, and optional
/// writing samples to anchor tone. Owned, like every agent input, and
/// `Serialize` so the trace records it.
#[derive(Serialize)]
pub struct CoverLetterInput {
    pub resume: TailoredResume,
    pub jd: JobRequirements,
    pub voice_samples: Vec<String>,
}

/// The lenient shape the model replies in: just the body paragraphs. The
/// greeting, sign-off, recipient, and contact block are added by code, so
/// the model has no field in which to invent a name or an address.
#[derive(Debug, Deserialize)]
pub struct RawCoverLetter {
    #[serde(default)]
    paragraphs: Vec<String>,
}

/// A finished cover letter: a standard greeting and sign-off wrapped around
/// the model's body paragraphs, with the recipient and contact block drawn
/// from the JD and resume. This is the payload the Typst template renders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverLetter {
    pub contact: Contact,
    pub company: String,
    pub title: String,
    pub greeting: String,
    pub paragraphs: Vec<String>,
    pub signoff: String,
}

/// The cover-letter agent: the model writes the prose; the guards keep it
/// honest. Mid tier, no tools.
pub struct CoverLetterAgent;

#[async_trait]
impl Agent for CoverLetterAgent {
    type Input = CoverLetterInput;
    type Wire = RawCoverLetter;
    type Output = (CoverLetter, Vec<String>);
    type Error = CoverLetterError;

    fn id(&self) -> &'static str {
        "cover_letter_v1"
    }
    fn model_tier(&self) -> ModelTier {
        // Shaping a short letter from facts already on hand is prose work,
        // not the heaviest judgment, so the mid tier is the right fit.
        ModelTier::Mid
    }
    fn system_prompt(&self) -> &str {
        SYSTEM_PROMPT
    }
    fn reply_budget(&self) -> u32 {
        REPLY_BUDGET
    }
    fn user_message(&self, input: &CoverLetterInput) -> String {
        build_user_message(input)
    }
    fn bad_reply(&self, snippet: String, source: serde_json::Error) -> CoverLetterError {
        CoverLetterError::BadReply { snippet, source }
    }
    fn assemble(
        &self,
        wire: RawCoverLetter,
        input: CoverLetterInput,
    ) -> Result<(CoverLetter, Vec<String>), CoverLetterError> {
        assemble(wire, input)
    }
}

/// Draft a cover letter for `jd` from the already-tailored `resume`,
/// matching tone to the `voice_samples` when any are given. Returns the
/// letter, any never-fabricate warnings, and the tokens it cost.
pub async fn write_cover_letter(
    ctx: &AgentContext<'_>,
    resume: &TailoredResume,
    jd: &JobRequirements,
    voice_samples: &[String],
) -> Result<(CoverLetter, Vec<String>, TokenUsage), CoverLetterError> {
    let input = CoverLetterInput {
        resume: resume.clone(),
        jd: jd.clone(),
        voice_samples: voice_samples.to_vec(),
    };
    let run = CoverLetterAgent.run(ctx, input).await?;
    let (letter, warnings) = run.output;
    Ok((letter, warnings, run.usage))
}

/// The cover-letter contract. The never-fabricate rules here are the
/// prompt-level half; `assemble` enforces the structural half (no number
/// the resume doesn't state). Mirrors the tailoring prompt's discipline,
/// including the no-em-dash rule.
const SYSTEM_PROMPT: &str = r#"You write a concise, specific cover letter for a candidate applying to a job, using ONLY the facts in the resume provided.

Rules, all of them load-bearing:
- Draw every claim from the resume. Never introduce a skill, employer, job title, technology, metric, number, team size, scope, or outcome the resume does not state. If you use a number, it must already appear in the resume.
- Connect the candidate's real experience to what the role emphasizes, but never inflate it. If the resume itself names a gap (something the candidate does not have), it is fine to acknowledge that plainly and briefly. Do not invent a gap, and do not invent a strength.
- Write 3 to 4 short paragraphs of body text only. Lead with substance, not throat-clearing. Do NOT write a greeting, a sign-off, an address, a date, or the candidate's name; those are added separately.
- Sound like a real person, not a template. Do not open with or lean on phrases like "I am writing to express", "I am excited to apply", "passionate", "proven track record", "results-driven", "I am confident that", or "I believe my skills". Be direct and concrete instead.
- Do NOT use em-dashes ("—"). Join clauses with a comma or "and", or start a new sentence; a colon is fine where it genuinely fits.
- Match the tone of the writing samples if any are given; otherwise write plainly.
- Reply with exactly one JSON object and nothing else, no markdown fences:
{"paragraphs": ["first paragraph", "second paragraph", "..."]}"#;

/// Everything the model may draw from, in one message: who the candidate
/// is, the role, the resume's summary and work history (the evidenced
/// facts), the skills, what the JD emphasizes, and any voice samples.
fn build_user_message(input: &CoverLetterInput) -> String {
    let mut text = String::new();
    text.push_str(&format!("CANDIDATE: {}\n", input.resume.contact.full_name));
    text.push_str(&format!(
        "APPLYING FOR: {} at {}\n\n",
        input.jd.title, input.jd.company
    ));

    text.push_str("RESUME SUMMARY\n");
    text.push_str(&input.resume.summary);
    text.push_str("\n\nWORK HISTORY (draw only from these facts)\n");
    for role in &input.resume.roles {
        text.push_str(&format!("{} at {}\n", role.title, role.company));
        for bullet in &role.bullets {
            text.push_str(&format!("  - {}\n", bullet.text));
        }
    }

    if !input.resume.skills_section.skills.is_empty() {
        text.push_str("\nSKILLS\n");
        text.push_str(&input.resume.skills_section.skills.join(", "));
        text.push('\n');
    }

    if !input.jd.responsibilities.is_empty() {
        text.push_str("\nWHAT THE ROLE EMPHASIZES\n");
        for responsibility in input.jd.responsibilities.iter().take(8) {
            text.push_str(&format!("- {responsibility}\n"));
        }
    }

    if !input.voice_samples.is_empty() {
        text.push_str("\nWRITING SAMPLES (match this voice; do not reuse their content)\n");
        for (i, sample) in input.voice_samples.iter().enumerate().take(3) {
            text.push_str(&format!("Sample {}: {}\n", i + 1, sample));
        }
    }

    text.push_str("\nWrite the cover letter body now, as the JSON object specified.");
    text
}

/// Assemble the model's paragraphs into a finished letter, enforcing
/// never-fabricate structurally: a paragraph that introduces a number the
/// resume doesn't state is dropped (there is no source paragraph to revert
/// to, the way a bullet has), with a warning. Empty paragraphs are skipped;
/// a reply that leaves nothing usable is a typed error.
fn assemble(
    wire: RawCoverLetter,
    input: CoverLetterInput,
) -> Result<(CoverLetter, Vec<String>), CoverLetterError> {
    let allowed = allowed_digits(&input.resume);
    let mut warnings = Vec::new();
    let mut paragraphs = Vec::new();
    for para in wire.paragraphs {
        let para = para.trim().to_string();
        if para.is_empty() {
            continue;
        }
        if digit_runs(&para).is_subset(&allowed) {
            paragraphs.push(para);
        } else {
            warnings.push(format!(
                "dropped a paragraph that introduced a figure the resume doesn't state: {:?}",
                snippet(&para)
            ));
        }
    }
    if paragraphs.is_empty() {
        return Err(CoverLetterError::Empty);
    }

    let greeting = if input.jd.company.trim().is_empty() {
        "Dear hiring team,".to_string()
    } else {
        format!("Dear {} hiring team,", input.jd.company.trim())
    };

    let letter = CoverLetter {
        contact: input.resume.contact.clone(),
        company: input.jd.company.clone(),
        title: input.jd.title.clone(),
        greeting,
        paragraphs,
        signoff: input.resume.contact.full_name.clone(),
    };
    Ok((letter, warnings))
}

/// Every number the resume states, gathered from the summary, the role
/// titles and companies, the bullets, and the skills. A letter may only use
/// figures from this set.
fn allowed_digits(resume: &TailoredResume) -> HashSet<String> {
    let mut text = String::new();
    text.push_str(&resume.summary);
    if let Some(title) = &resume.target_title {
        text.push(' ');
        text.push_str(title);
    }
    for role in &resume.roles {
        text.push(' ');
        text.push_str(&role.company);
        text.push(' ');
        text.push_str(&role.title);
        for bullet in &role.bullets {
            text.push(' ');
            text.push_str(&bullet.text);
        }
    }
    for skill in &resume.skills_section.skills {
        text.push(' ');
        text.push_str(skill);
    }
    digit_runs(&text)
}

/// The first stretch of a paragraph, for a warning message.
fn snippet(text: &str) -> String {
    text.chars().take(60).collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::dataset::types::{BulletId, RoleId, YearMonth};
    use crate::jd::{RemotePolicy, Seniority};
    use crate::llm::MockLlmClient;
    use crate::tailor::{
        BuildId, JdId, SkillsSection, TailoredBullet, TailoredResume, TailoredRole,
    };
    use chrono::Utc;

    fn test_ctx(mock: &MockLlmClient) -> AgentContext<'_> {
        AgentContext {
            llm: mock,
            model: &"test-model",
            tracer: &crate::trace::Tracer::DISABLED,
            sink: None,
        }
    }

    fn resume() -> TailoredResume {
        TailoredResume {
            build_id: BuildId("001".into()),
            jd_id: JdId("x".into()),
            generated_at: Utc::now(),
            contact: Contact {
                full_name: "Ada Lovelace".into(),
                email: "ada@example.com".into(),
                phone: None,
                location: None,
                links: Vec::new(),
            },
            target_title: Some("Engineer".into()),
            summary: "Built systems for 12 years.".into(),
            roles: vec![TailoredRole {
                id: RoleId("role-1".into()),
                company: "Analytical Engines".into(),
                title: "Director".into(),
                start: YearMonth {
                    year: 2020,
                    month: 1,
                },
                end: None,
                location: None,
                bullets: vec![TailoredBullet {
                    source_id: BulletId("b1".into()),
                    text: "Led a team of 12 engineers".into(),
                }],
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

    fn jd() -> JobRequirements {
        JobRequirements {
            company: "Acme".into(),
            title: "Staff Engineer".into(),
            seniority: Seniority::Unspecified,
            location: None,
            remote: RemotePolicy::Unspecified,
            domain_keywords: Vec::new(),
            required_skills: Vec::new(),
            preferred_skills: Vec::new(),
            responsibilities: vec!["Lead the platform team".into()],
            ats_phrases: Vec::new(),
            raw_text: String::new(),
            source_url: None,
        }
    }

    async fn run(reply: &str) -> Result<(CoverLetter, Vec<String>, TokenUsage), CoverLetterError> {
        let mock = MockLlmClient::default();
        mock.enqueue(reply);
        write_cover_letter(&test_ctx(&mock), &resume(), &jd(), &[]).await
    }

    #[tokio::test]
    async fn a_clean_reply_assembles_with_a_greeting_and_signoff() {
        let (letter, warnings, _usage) = run(r#"{"paragraphs": [
                "I led a team of 12 engineers at Analytical Engines.",
                "I would welcome a conversation."
            ]}"#)
        .await
        .unwrap();

        assert_eq!(letter.greeting, "Dear Acme hiring team,");
        assert_eq!(letter.signoff, "Ada Lovelace");
        assert_eq!(letter.company, "Acme");
        assert_eq!(letter.title, "Staff Engineer");
        assert_eq!(letter.paragraphs.len(), 2);
        // The contact block is the resume's, never the model's.
        assert_eq!(letter.contact.full_name, "Ada Lovelace");
        assert!(warnings.is_empty(), "got: {warnings:?}");
    }

    #[tokio::test]
    async fn a_paragraph_that_invents_a_number_is_dropped() {
        let (letter, warnings, _usage) = run(r#"{"paragraphs": [
                "I cut costs by 40% in my last role.",
                "I led a team of 12 engineers and would welcome a conversation."
            ]}"#)
        .await
        .unwrap();

        // "40" is in neither the resume nor the JD facts, so that paragraph
        // is dropped; the "12" paragraph (a resume figure) survives.
        assert_eq!(letter.paragraphs.len(), 1);
        assert!(letter.paragraphs[0].contains("12 engineers"));
        assert!(
            warnings.iter().any(|w| w.contains("introduced a figure")),
            "got: {warnings:?}"
        );
    }

    #[tokio::test]
    async fn a_reply_with_no_usable_text_is_a_typed_error() {
        let err = run(r#"{"paragraphs": ["   ", ""]}"#).await.unwrap_err();
        assert!(matches!(err, CoverLetterError::Empty));
    }

    #[tokio::test]
    async fn the_prompt_carries_the_candidate_role_and_work_history() {
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"paragraphs": ["A solid paragraph with no numbers at all."]}"#);
        write_cover_letter(&test_ctx(&mock), &resume(), &jd(), &[])
            .await
            .unwrap();

        let sent = &mock.requests()[0].messages[0].content;
        assert!(sent.contains("Ada Lovelace"));
        assert!(sent.contains("Staff Engineer at Acme"));
        assert!(sent.contains("Led a team of 12 engineers"));
        assert!(sent.contains("Lead the platform team"));
    }
}
