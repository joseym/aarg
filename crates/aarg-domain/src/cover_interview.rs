//! The cover-letter interview — the honesty layer for cover-letter
//! generation, the same shape as bullet strengthening and role
//! enrichment. A cover letter says three things a résumé can't: the
//! angle the candidate wants to take, what from their background they
//! want to lead with, and why this specific role and company — genuine
//! motivation, not a résumé restated in paragraphs. A model asked to
//! just "write a cover letter" will happily invent all three: an angle
//! nobody chose, an emphasis nobody asked for, and a motivation that
//! reads as generic enthusiasm dressed up in facts. None of that traces
//! to the candidate.
//!
//! So this module only ever asks. [`CoverInterviewAgent`] poses one
//! grounded question at a time — specific to the actual posting and the
//! candidate's tailored résumé, never a generic prompt — and
//! [`run_cover_interview`] drives a short, adaptive session across a
//! handful of fixed topics: the letter's overall angle, what to
//! emphasize, its tone, why this role and company, and any constraints.
//! Which [`CoverBrief`] field an answer fills is decided entirely by
//! code — whichever topic is live when the user answers — never by
//! parsing the model's own words, so nothing the model writes can end up
//! recorded as a candidate fact. A skipped or partial interview degrades
//! to an empty or partial brief rather than blocking whatever generates
//! the letter later.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::agent::{Agent, AgentContext};
use crate::jd::JobRequirements;
use crate::llm::LlmError;
use crate::tailor::TailoredResume;
use crate::user::{Answer, AskError, Question, UserHandle};

/// A leading question is one short sentence.
const REPLY_BUDGET: u32 = 256;

/// Bounds the whole interview across every topic, so a model that never
/// signals "done" (or a run of thin answers) can't interrogate forever.
/// Five topics with at most one follow-up each is ten turns in the worst
/// case; six is enough for the common case — one opening question per
/// topic, with a follow-up on the occasional thin answer — without
/// letting a bad session run long. Any sane positive value works; this
/// exists only to guarantee termination.
const MAX_QUESTIONS: usize = 6;

#[derive(Debug, thiserror::Error)]
pub enum CoverInterviewError {
    #[error(transparent)]
    Llm(#[from] LlmError),

    #[error("the cover-interview reply was not the expected JSON (reply began {snippet:?})")]
    BadReply {
        snippet: String,
        #[source]
        source: serde_json::Error,
    },
}

/// What an interview about a cover letter recorded, to hand to whatever
/// later drafts the letter's text. Every field is optional or
/// empty-defaultable, so a partial or entirely skipped interview still
/// yields a usable (if empty) brief rather than blocking generation.
///
/// Every field is populated ONLY from the candidate's own typed answers.
/// [`CoverInterviewAgent`]'s `Output` is a question (or the empty-string
/// "done" signal) — never a fact — so there is no channel for the model
/// that asks the questions to write into this struct. [`run_cover_interview`]
/// is the only code that ever constructs a populated one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CoverBrief {
    /// The overall narrative the letter should take, in the candidate's
    /// own words (e.g. "position me as a builder who scales teams, not
    /// just an IC").
    pub angle: Option<String>,
    /// Specific things from their background the candidate wants the
    /// letter to lead with. Empty if they had nothing to add.
    pub emphasis: Vec<String>,
    /// How the letter should sound (e.g. "direct, a little informal").
    pub tone: Option<String>,
    /// Why this role, at this company — the one thing genuinely not on a
    /// résumé, and the reason this interview exists rather than letting a
    /// model invent enthusiasm.
    pub motivation: Option<String>,
    /// Anything the candidate wants the letter to avoid or must include,
    /// in their own words.
    pub constraints: Vec<String>,
}

// ---------------------------------------------------------------------
// The topics: code owns the question-to-field mapping
// ---------------------------------------------------------------------

/// The fixed topics a cover-letter interview walks through, in order.
/// Code owns which [`CoverBrief`] field each topic fills; the model only
/// ever supplies the phrasing of the question asked for it — the loop
/// never parses a question's text to decide where an answer belongs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Angle,
    Emphasis,
    Tone,
    Motivation,
    Constraints,
}

impl Slot {
    const ALL: [Slot; 5] = [
        Slot::Angle,
        Slot::Emphasis,
        Slot::Tone,
        Slot::Motivation,
        Slot::Constraints,
    ];

    /// What this turn is about, handed to the agent so its question
    /// stays on topic.
    fn topic(self) -> &'static str {
        match self {
            Slot::Angle => "the overall angle or narrative the letter should take",
            Slot::Emphasis => "which parts of their background to emphasize",
            Slot::Tone => "the tone the letter should have",
            Slot::Motivation => {
                "why they want this specific role at this specific company - \
                 the one thing a resume can't say"
            }
            Slot::Constraints => "anything the letter should avoid or must include",
        }
    }

    /// A plain fallback question, used only when the agent can't be
    /// reached even for this topic's opening turn — mirrors
    /// [`strengthen`](crate::strengthen)'s `turn == 0` fallback so a
    /// transient error doesn't just skip the topic outright.
    fn fallback_question(self) -> &'static str {
        match self {
            Slot::Angle => "What's the one angle you want this letter to take?",
            Slot::Emphasis => {
                "What from your background do you most want this letter to highlight?"
            }
            Slot::Tone => "What tone should the letter have?",
            Slot::Motivation => "Why do you want this specific role, at this specific company?",
            Slot::Constraints => "Anything this letter should avoid, or must include?",
        }
    }

    /// Record a non-blank answer into the field this topic owns. The only
    /// place a `CoverBrief` field is ever written.
    fn record(self, brief: &mut CoverBrief, answer: String) {
        match self {
            Slot::Angle => brief.angle = Some(answer),
            Slot::Emphasis => brief.emphasis.push(answer),
            Slot::Tone => brief.tone = Some(answer),
            Slot::Motivation => brief.motivation = Some(answer),
            Slot::Constraints => brief.constraints.push(answer),
        }
    }
}

// ---------------------------------------------------------------------
// The agent: ask one grounded question at a time
// ---------------------------------------------------------------------

/// One exchange in the interview so far, scoped to the current topic.
#[derive(Debug, Clone, Serialize)]
pub struct QnA {
    pub question: String,
    pub answer: String,
}

/// What the question agent needs: the posting and the tailored résumé to
/// ground a specific question in, which topic this turn is about, and
/// the conversation so far on that topic (so it can judge whether to
/// follow up or stop).
#[derive(Serialize)]
pub struct CoverQuestionInput {
    pub jd_title: String,
    pub jd_company: String,
    pub jd_required_skills: Vec<String>,
    pub jd_responsibilities: Vec<String>,
    pub resume_target_title: Option<String>,
    pub resume_summary: String,
    pub resume_skills: Vec<String>,
    pub topic: String,
    pub transcript: Vec<QnA>,
}

/// Asks one grounded question at a time about a cover letter's angle,
/// emphasis, tone, motivation, or constraints — never proposing the
/// answer. Mirrors [`strengthen::StrengthenInterviewAgent`](crate::strengthen::StrengthenInterviewAgent):
/// the transcript drives follow-ups, and an empty question is the "I
/// have enough" sentinel.
pub struct CoverInterviewAgent;

#[async_trait]
impl Agent for CoverInterviewAgent {
    type Input = CoverQuestionInput;
    type Wire = RawQuestion;
    type Output = String;
    type Error = CoverInterviewError;

    fn id(&self) -> &'static str {
        "cover_interview_v1"
    }
    fn system_prompt(&self) -> &str {
        QUESTION_PROMPT
    }
    fn reply_budget(&self) -> u32 {
        REPLY_BUDGET
    }
    fn user_message(&self, input: &CoverQuestionInput) -> String {
        let mut text = format!("The posting: {} at {}\n", input.jd_title, input.jd_company);
        if !input.jd_required_skills.is_empty() {
            text.push_str(&format!(
                "Required skills: {}\n",
                input.jd_required_skills.join(", ")
            ));
        }
        if !input.jd_responsibilities.is_empty() {
            text.push_str("Responsibilities:\n");
            for duty in &input.jd_responsibilities {
                text.push_str(&format!("  - {duty}\n"));
            }
        }
        if let Some(title) = &input.resume_target_title {
            text.push_str(&format!("\nTheir tailored resume targets: {title}\n"));
        }
        if !input.resume_summary.is_empty() {
            text.push_str(&format!("Resume summary: {}\n", input.resume_summary));
        }
        if !input.resume_skills.is_empty() {
            text.push_str(&format!(
                "Resume skills: {}\n",
                input.resume_skills.join(", ")
            ));
        }
        text.push_str(&format!("\nThis turn's topic: {}\n\n", input.topic));
        if input.transcript.is_empty() {
            text.push_str("No questions asked yet on this topic. Ask your opening question.");
        } else {
            text.push_str("The conversation so far on this topic:\n");
            for qa in &input.transcript {
                text.push_str(&format!("Q: {}\nA: {}\n", qa.question, qa.answer));
            }
            text.push_str(
                "\nIf you now have enough for this topic, reply with an empty question. \
                 Otherwise ask the next one.",
            );
        }
        text
    }
    fn bad_reply(&self, snippet: String, source: serde_json::Error) -> CoverInterviewError {
        CoverInterviewError::BadReply { snippet, source }
    }
    fn assemble(
        &self,
        wire: RawQuestion,
        _input: CoverQuestionInput,
    ) -> Result<String, CoverInterviewError> {
        Ok(wire.question)
    }
}

const QUESTION_PROMPT: &str = r#"You interview a job candidate to prepare a brief for their cover letter. Each turn is about exactly ONE topic: the overall angle, what to emphasize, the tone, why they want this role and company, or constraints for the letter. You are given the topic for this turn, the job posting, the candidate's tailored resume, and the conversation so far on this topic.

How to run the interview:
- Ask your best question about the current topic, grounded in specifics from the posting and the resume ("the posting stresses reliability work - is that something you want to lead with, or a different angle?") rather than a generic prompt.
- After each answer, judge whether you now have enough for this topic. If yes, STOP by replying with an empty question (""). If the answer was thin or vague, ask ONE focused follow-up - never re-ask what they already told you.

Rules that always hold:
- NEVER propose, supply, or imply the answer - not an angle, not a reason, not a tone, not a claim about why they want the job. "The posting stresses reliability - want to lead with that, or is there something else?" is good; "You should lead with reliability, right?" is forbidden.
- Never invent a fact about the candidate or the company; you are only asking.
- One question, one sentence, warm and concrete.

Reply with exactly one JSON object and nothing else - no markdown fences. Use an empty string for the question when you have enough:
{"question": "your next question, or empty string if done"}"#;

#[derive(Debug, Deserialize)]
pub struct RawQuestion {
    #[serde(default)]
    question: String,
}

// ---------------------------------------------------------------------
// The interview loop
// ---------------------------------------------------------------------

/// Walk the candidate through a short, adaptive interview about the
/// cover letter's angle, emphasis, tone, motivation, and constraints, and
/// return what they said as a [`CoverBrief`]. Every field comes from the
/// user's own typed answers — the agent only ever asks (see
/// [`CoverBrief`]'s doc comment for how that's enforced structurally).
///
/// Degrades rather than blocking: a non-interactive user, or any `ask`
/// failure partway through, returns whatever partial brief was gathered
/// so far (possibly empty) instead of propagating an error — a skipped
/// interview means "no brief," never a blocked build.
pub async fn run_cover_interview(
    resume: &TailoredResume,
    jd: &JobRequirements,
    user: &dyn UserHandle,
    ctx: &AgentContext<'_>,
) -> Result<CoverBrief, AskError> {
    let mut brief = CoverBrief::default();
    if !user.is_interactive() {
        return Ok(brief);
    }

    let mut asked = 0usize;
    for slot in Slot::ALL {
        if asked >= MAX_QUESTIONS {
            break;
        }
        let mut transcript: Vec<QnA> = Vec::new();
        // At most two turns per topic: an opening question and, only when
        // the model judges the first answer too thin, one follow-up.
        for turn in 0..2 {
            if asked >= MAX_QUESTIONS {
                break;
            }
            let input = CoverQuestionInput {
                jd_title: jd.title.clone(),
                jd_company: jd.company.clone(),
                jd_required_skills: jd.required_skills.iter().map(|s| s.name.clone()).collect(),
                jd_responsibilities: jd.responsibilities.clone(),
                resume_target_title: resume.target_title.clone(),
                resume_summary: resume.summary.clone(),
                resume_skills: resume.skills_section.skills.clone(),
                topic: slot.topic().to_string(),
                transcript: transcript.clone(),
            };
            let question = match CoverInterviewAgent.run(ctx, input).await {
                Ok(run) => run.output.trim().to_string(),
                // On failure, ask one generic opening for this topic; if
                // even that can't be had, move to the next topic rather
                // than looping on errors.
                Err(_) if turn == 0 => slot.fallback_question().to_string(),
                Err(_) => break,
            };
            if question.is_empty() {
                break; // the interviewer has enough for this topic
            }
            asked += 1;
            let answer = match user
                .ask(Question::Text {
                    prompt: question.clone(),
                })
                .await
            {
                Ok(Answer::Text(t)) if !t.trim().is_empty() => t.trim().to_string(),
                Ok(_) => break,             // blank: leave this topic empty, move on
                Err(_) => return Ok(brief), // abandoned interview: keep what's gathered
            };
            slot.record(&mut brief, answer.clone());
            transcript.push(QnA { question, answer });
        }
    }

    Ok(brief)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::dataset::types::Contact;
    use crate::dataset::types::SkillCategory;
    use crate::jd::{Importance, JdSkill, RemotePolicy, Seniority};
    use crate::llm::MockLlmClient;
    use crate::tailor::{BuildId, JdId, SkillsSection, TailoredResume};
    use crate::trace::Tracer;
    use crate::user::ScriptedUser;

    fn ctx<'a>(mock: &'a MockLlmClient) -> AgentContext<'a> {
        AgentContext {
            llm: mock,
            model: &"m",
            tracer: &Tracer::DISABLED,
            sink: None,
        }
    }

    fn sample_jd() -> JobRequirements {
        JobRequirements {
            company: "Acme".into(),
            title: "Staff Engineer".into(),
            seniority: Seniority::Senior,
            location: None,
            remote: RemotePolicy::Remote,
            domain_keywords: vec!["fintech".into()],
            required_skills: vec![JdSkill {
                name: "Distributed systems".into(),
                category: SkillCategory::Hard,
                importance: Importance::Critical,
                context_phrases: Vec::new(),
            }],
            preferred_skills: Vec::new(),
            responsibilities: vec!["Own platform reliability".into()],
            ats_phrases: Vec::new(),
            raw_text: "Staff engineer, reliability at scale.".into(),
            source_url: None,
        }
    }

    fn sample_resume() -> TailoredResume {
        TailoredResume {
            build_id: BuildId("build-1".into()),
            jd_id: JdId("jd-1".into()),
            generated_at: chrono::Utc::now(),
            contact: Contact {
                full_name: "Ada".into(),
                email: "ada@example.com".into(),
                phone: None,
                location: None,
                links: Vec::new(),
            },
            target_title: Some("Staff Engineer".into()),
            summary: "Reliability-focused engineering leader.".into(),
            roles: Vec::new(),
            education: Vec::new(),
            skills_section: SkillsSection {
                skills: vec!["Distributed systems".into()],
            },
            projects: Vec::new(),
            achievements: Vec::new(),
            certifications: Vec::new(),
        }
    }

    /// A `UserHandle` standing in for a non-interactive run (CI, a piped
    /// command): never interactive, and any `ask` would fail — matching
    /// the real `NonInteractiveUser` in the binary crate, which this
    /// portable crate cannot depend on.
    #[derive(Default)]
    struct NonInteractiveStub;

    #[async_trait]
    impl UserHandle for NonInteractiveStub {
        async fn ask(&self, question: Question) -> Result<Answer, AskError> {
            Err(AskError::NotInteractive {
                what: question.prompt().to_string(),
            })
        }
        async fn confirm(&self, _prompt: &str, default: bool) -> Result<bool, AskError> {
            Ok(default)
        }
        fn notify(&self, _message: &str) {}
    }

    #[test]
    fn the_system_prompt_forbids_proposing_answers() {
        assert!(QUESTION_PROMPT.contains("NEVER propose, supply, or imply the answer"));
    }

    #[tokio::test]
    async fn a_full_interview_records_only_the_users_answers() {
        let mock = MockLlmClient::default();
        // One real question per topic, then an empty "done" reply, for
        // each of the five topics in order.
        mock.enqueue(r#"{"question": "Lead with scale, or with reliability?"}"#);
        mock.enqueue(r#"{"question": ""}"#);
        mock.enqueue(r#"{"question": "What should this letter highlight?"}"#);
        mock.enqueue(r#"{"question": ""}"#);
        mock.enqueue(r#"{"question": "Formal or conversational?"}"#);
        mock.enqueue(r#"{"question": ""}"#);
        mock.enqueue(r#"{"question": "Why this company specifically?"}"#);
        mock.enqueue(r#"{"question": ""}"#);
        mock.enqueue(r#"{"question": "Anything to avoid mentioning?"}"#);
        mock.enqueue(r#"{"question": ""}"#);

        let user = ScriptedUser::new();
        user.answer(Answer::Text("lead with the reliability angle".into()));
        user.answer(Answer::Text("the incident response work".into()));
        user.answer(Answer::Text("direct and a little informal".into()));
        user.answer(Answer::Text("I used their product for years".into()));
        user.answer(Answer::Text("don't mention my current employer".into()));

        let brief = run_cover_interview(&sample_resume(), &sample_jd(), &user, &ctx(&mock))
            .await
            .unwrap();

        assert_eq!(
            brief.angle.as_deref(),
            Some("lead with the reliability angle")
        );
        assert_eq!(
            brief.emphasis,
            vec!["the incident response work".to_string()]
        );
        assert_eq!(brief.tone.as_deref(), Some("direct and a little informal"));
        assert_eq!(
            brief.motivation.as_deref(),
            Some("I used their product for years")
        );
        assert_eq!(
            brief.constraints,
            vec!["don't mention my current employer".to_string()]
        );
        // Nothing from the mock's own question text ever lands in the brief.
        assert!(!brief.angle.as_deref().unwrap().contains("Lead with scale"));
        assert!(!brief.emphasis[0].contains("should this letter highlight"));
    }

    #[tokio::test]
    async fn a_blank_answer_leaves_its_slot_empty_without_crashing() {
        let mock = MockLlmClient::default();
        for _ in 0..5 {
            mock.enqueue(r#"{"question": "Tell me more?"}"#);
        }
        let user = ScriptedUser::new();
        for _ in 0..5 {
            user.answer(Answer::Text("   ".into())); // blank every topic
        }

        let brief = run_cover_interview(&sample_resume(), &sample_jd(), &user, &ctx(&mock))
            .await
            .unwrap();

        assert_eq!(brief.angle, None);
        assert!(brief.emphasis.is_empty());
        assert_eq!(brief.tone, None);
        assert_eq!(brief.motivation, None);
        assert!(brief.constraints.is_empty());
    }

    #[tokio::test]
    async fn the_max_questions_cap_terminates_the_loop() {
        let mock = MockLlmClient::default();
        // The interviewer never says "done" and the user never goes
        // blank, so without a cap this would run all five topics to
        // their two-turn-each ceiling (ten questions).
        for i in 0..10 {
            mock.enqueue(format!(r#"{{"question": "question number {i}?"}}"#));
        }
        let user = ScriptedUser::new();
        for i in 0..10 {
            user.answer(Answer::Text(format!("answer number {i}")));
        }

        let brief = run_cover_interview(&sample_resume(), &sample_jd(), &user, &ctx(&mock))
            .await
            .unwrap();

        // Capped at MAX_QUESTIONS: exactly that many questions were
        // actually asked (and answered), not the ten the topics allow.
        assert_eq!(mock.requests().len(), MAX_QUESTIONS);
        // The first three topics (angle, emphasis, tone) each got their
        // full two turns before the cap bit; motivation and constraints
        // never got a chance to record anything.
        assert_eq!(brief.motivation, None);
        assert!(brief.constraints.is_empty());
    }

    #[tokio::test]
    async fn a_non_interactive_user_degrades_to_an_empty_brief() {
        let mock = MockLlmClient::default();
        let user = NonInteractiveStub;

        let brief = run_cover_interview(&sample_resume(), &sample_jd(), &user, &ctx(&mock))
            .await
            .unwrap();

        assert_eq!(brief, CoverBrief::default());
        assert!(
            mock.requests().is_empty(),
            "no agent call for a skipped interview"
        );
    }

    #[tokio::test]
    async fn an_ask_failure_partway_through_keeps_the_partial_brief() {
        // Only one scripted reply and one scripted answer: the angle
        // topic completes normally, its follow-up turn finds the mock
        // exhausted (so it just moves on), and the emphasis topic's
        // fallback question then finds no answer queued at all - the
        // `ask` fails, and the loop must return what it already
        // gathered rather than erroring the whole flow.
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"question": "Lead with scale, or with reliability?"}"#);
        let user = ScriptedUser::new();
        user.answer(Answer::Text("lead with the reliability angle".into()));
        // No second answer queued: ScriptedUser::ask fails NotInteractive.

        let brief = run_cover_interview(&sample_resume(), &sample_jd(), &user, &ctx(&mock))
            .await
            .unwrap();

        assert_eq!(
            brief.angle.as_deref(),
            Some("lead with the reliability angle")
        );
        assert!(brief.emphasis.is_empty());
    }

    #[test]
    fn cover_brief_round_trips_through_json() {
        let brief = CoverBrief {
            angle: Some("lead with reliability".into()),
            emphasis: vec!["incident response".into()],
            tone: Some("direct".into()),
            motivation: Some("used their product for years".into()),
            constraints: vec!["skip my current employer".into()],
        };
        let json = serde_json::to_string(&brief).unwrap();
        let back: CoverBrief = serde_json::from_str(&json).unwrap();
        assert_eq!(brief.angle, back.angle);
        assert_eq!(brief.emphasis, back.emphasis);
        assert_eq!(brief.tone, back.tone);
        assert_eq!(brief.motivation, back.motivation);
        assert_eq!(brief.constraints, back.constraints);
    }

    #[test]
    fn an_empty_cover_brief_also_round_trips() {
        let brief = CoverBrief::default();
        let json = serde_json::to_string(&brief).unwrap();
        let back: CoverBrief = serde_json::from_str(&json).unwrap();
        assert_eq!(back.angle, None);
        assert!(back.emphasis.is_empty());
    }
}
