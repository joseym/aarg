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
//! So this module only ever asks. [`run_cover_interview`] runs draft-first,
//! silently, before anything is put in front of the candidate: it generates a
//! naive preliminary letter from the résumé and JD alone (reusing
//! [`write_cover_letter`](crate::cover::write_cover_letter) with no brief).
//! Only once the candidate has answered the interview's leading open
//! question does it read the draft's own honesty check
//! ([`check_cover_provenance`](crate::cover_provenance)) to see which
//! paragraphs make a claim the candidate's evidence doesn't back — run at
//! that point, not sooner, so a claim the leading answer already grounded
//! doesn't get flagged as a gap. Those flagged paragraphs are the interview: for each one,
//! [`CoverGapQuestionAgent`] phrases a single, specific question that puts the
//! claim back to the candidate to confirm, expand, or drop — a better use of
//! their attention than a fixed list of generic questions asked before anyone
//! knows what the draft actually needs. When the preliminary draft has no such
//! gaps (every paragraph grounded or exempt), or when the draft or its check
//! can't be produced, the session falls back to the original fixed walk-through
//! across a handful of topics: the letter's overall angle, what to emphasize,
//! its tone, why this role and company, and any constraints, each
//! [`CoverInterviewAgent`] question grounded in the posting and the résumé.
//! Either way, which [`CoverBrief`] field an answer fills is decided entirely
//! by code — a gap answer always lands in `emphasis`, and a fixed-topic answer
//! in whichever topic is live — never by parsing the model's own words, so
//! nothing the model writes can end up recorded as a candidate fact. The
//! preliminary draft is scratch: it is never shown as final and never
//! persisted; the caller always re-drafts the real letter from the finished
//! brief. A skipped or partial interview degrades to an empty or partial brief
//! rather than blocking whatever generates the letter later.
//!
//! For four of the five topics, the interview also offers a guarded,
//! evidence-grounded suggestion first — the same "propose one draft, the
//! candidate disposes" mechanism [`strengthen`](crate::strengthen) uses
//! for weak bullets, [`CoverSuggestAgent`] draws only on the JD and the
//! candidate's own tailored résumé, and [`cover_suggest_flow`] is the
//! guard that keeps a decline or an unreachable model from ever reaching
//! the user as a menu. `motivation` gets the strictest reading of that
//! prompt: it may surface an evidence-based fit observation, never a
//! first-person claim of enthusiasm. `constraints` skips the mechanism
//! entirely and goes straight to the plain question — there is no honest
//! way to suggest a restriction the candidate never raised.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::agent::{Agent, AgentContext};
use crate::cover::{CoverLetter, write_cover_letter};
use crate::cover_provenance::{CoverParagraphStatus, check_cover_provenance};
use crate::jd::JobRequirements;
use crate::llm::LlmError;
use crate::tailor::{TailoredResume, within_evidence};
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
///
/// Counts everything actually put in front of the user — an interview
/// question or a suggestion menu — the same way a model's empty "I have
/// enough" reply doesn't count today. A suggestion that's declined, or a
/// tweak the guard rejects, is never shown, so it costs nothing against
/// this cap either.
const MAX_QUESTIONS: usize = 6;

/// How many times a suggested answer can be tweaked before "Tweak it"
/// stops being offered. Mirrors `strengthen`'s `InterviewLimits::revises`,
/// kept as a plain constant here since this module takes no limits
/// argument; any sane positive value works, this only guarantees the
/// tweak loop terminates.
const SUGGEST_REVISES: usize = 2;

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
/// `#[serde(default)]` on the struct is what makes that true even for
/// hand-written or older JSON that omits fields entirely, not just for
/// values this module itself produced.
///
/// Every field is populated ONLY from the candidate's own typed answers.
/// [`CoverInterviewAgent`]'s `Output` is a question (or the empty-string
/// "done" signal), and [`CoverSuggestAgent`]'s `Output` is a proposed
/// answer the candidate must explicitly accept — neither is ever written
/// into this struct directly. [`run_cover_interview`] is the only code
/// that ever constructs a populated one, and [`Slot::record`] is the only
/// place that ever writes a field.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CoverBrief {
    /// The overall narrative the letter should take, in the candidate's
    /// own words (e.g. "position me as a builder who scales teams, not
    /// just an IC"). A follow-up answer is appended to, never overwrites,
    /// whatever the candidate already said for this topic.
    pub angle: Option<String>,
    /// Specific things from their background the candidate wants the
    /// letter to lead with. Empty if they had nothing to add. Its first
    /// entry may be whatever the candidate volunteered to the interview's
    /// leading open question, before any of the guided topics below — an
    /// unstructured note (a personal project, a detail the résumé doesn't
    /// carry) rather than a résumé accomplishment specifically, but it
    /// belongs here for the same reason: it's the candidate's own words on
    /// something they want foregrounded.
    pub emphasis: Vec<String>,
    /// How the letter should sound (e.g. "direct, a little informal").
    /// Appended to, not overwritten, on a follow-up answer.
    pub tone: Option<String>,
    /// Why this role, at this company — the one thing genuinely not on a
    /// résumé, and the reason this interview exists rather than letting a
    /// model invent enthusiasm. Appended to, not overwritten, on a
    /// follow-up answer.
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

    /// A short, stable name for this slot — used as the machine-readable
    /// tag the suggestion agent is told which topic it's on (alongside
    /// `topic`'s full sentence), and in the suggestion menu shown to the
    /// user.
    fn key(self) -> &'static str {
        match self {
            Slot::Angle => "angle",
            Slot::Emphasis => "emphasis",
            Slot::Tone => "tone",
            Slot::Motivation => "motivation",
            Slot::Constraints => "constraints",
        }
    }

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

    /// Whether this slot gets a suggestion attempt before the plain
    /// interview question. True for every slot except `Constraints`: a
    /// short free-form "anything to avoid" field where a suggestion
    /// doesn't save effort and risks putting words in the candidate's
    /// mouth about a restriction they never raised.
    fn suggestible(self) -> bool {
        !matches!(self, Slot::Constraints)
    }

    /// Record a non-blank answer into the field this topic owns. The only
    /// place a `CoverBrief` field is ever written — whether the answer
    /// came from a typed reply or an accepted suggestion, it arrives here
    /// the same way.
    fn record(self, brief: &mut CoverBrief, answer: String) {
        match self {
            Slot::Angle => append_scalar(&mut brief.angle, answer),
            Slot::Emphasis => brief.emphasis.push(answer),
            Slot::Tone => append_scalar(&mut brief.tone, answer),
            Slot::Motivation => append_scalar(&mut brief.motivation, answer),
            Slot::Constraints => brief.constraints.push(answer),
        }
    }
}

/// Merge a second answer for the same scalar slot instead of discarding
/// the first. A follow-up turn produces a second, genuinely additional
/// answer about the same topic — not a correction of the first — so both
/// must survive verbatim in the final brief; joined on their own line so
/// they stay readable and distinguishable downstream.
fn append_scalar(field: &mut Option<String>, answer: String) {
    match field {
        Some(existing) => {
            existing.push('\n');
            existing.push_str(&answer);
        }
        None => *field = Some(answer),
    }
}

// ---------------------------------------------------------------------
// Agent 1: ask one grounded question at a time
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
// Agent 2: suggest a starting point from the JD and tailored résumé
// ---------------------------------------------------------------------

/// What the suggestion agent needs: the same JD/résumé grounding the
/// question agent sees, which slot this turn concerns (its short key and
/// its full topic sentence), and any revision notes from an earlier
/// "tweak it" round.
#[derive(Serialize)]
pub struct CoverSuggestInput {
    pub slot: String,
    pub topic: String,
    pub jd_title: String,
    pub jd_company: String,
    pub jd_required_skills: Vec<String>,
    pub jd_responsibilities: Vec<String>,
    pub resume_target_title: Option<String>,
    pub resume_summary: String,
    pub resume_skills: Vec<String>,
    /// Every recorded bullet across the tailored résumé's selected roles,
    /// labeled by role — the material `emphasis` picks from and `angle`
    /// or `motivation` may point to, never invent beyond.
    pub resume_bullets: Vec<String>,
    pub notes: Vec<String>,
}

/// Drafts one proposed answer for a suggestible slot as a *starting
/// point*, grounded only in the JD and the candidate's own tailored
/// résumé. Mirrors [`strengthen::StrengthenSuggestAgent`](crate::strengthen::StrengthenSuggestAgent):
/// it proposes, the candidate disposes (use / tweak / own words / skip),
/// and [`run_cover_suggest`]'s guard plus the user-as-final-gate menu keep
/// it from ever silently becoming a recorded fact.
pub struct CoverSuggestAgent;

#[async_trait]
impl Agent for CoverSuggestAgent {
    type Input = CoverSuggestInput;
    type Wire = RawSuggestion;
    type Output = String;
    type Error = CoverInterviewError;

    fn id(&self) -> &'static str {
        "cover_suggest_v1"
    }
    fn system_prompt(&self) -> &str {
        SUGGEST_PROMPT
    }
    fn reply_budget(&self) -> u32 {
        REPLY_BUDGET
    }
    fn user_message(&self, input: &CoverSuggestInput) -> String {
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
        if !input.resume_bullets.is_empty() {
            text.push_str("\nRecorded accomplishments (facts you may draw on):\n");
            for bullet in &input.resume_bullets {
                text.push_str(&format!("  - {bullet}\n"));
            }
        }
        text.push_str(&format!(
            "\nThis turn's topic ({}): {}\n",
            input.slot, input.topic
        ));
        if !input.notes.is_empty() {
            text.push_str("\nThe candidate asked you to revise your previous suggestion:\n");
            for note in &input.notes {
                text.push_str(&format!("- {note}\n"));
            }
        }
        text.push_str("\nPropose one suggestion for this topic, using only the facts above.");
        text
    }
    fn bad_reply(&self, snippet: String, source: serde_json::Error) -> CoverInterviewError {
        CoverInterviewError::BadReply { snippet, source }
    }
    fn assemble(
        &self,
        wire: RawSuggestion,
        _input: CoverSuggestInput,
    ) -> Result<String, CoverInterviewError> {
        Ok(wire.suggestion)
    }
}

const SUGGEST_PROMPT: &str = r#"You help a job candidate prepare a cover letter by drafting ONE possible answer for a single topic - the angle, what to emphasize, the tone, or a motivation prompt - as a starting point they will review, edit, or reject. You are given the job posting, their tailored resume, and which topic this turn is about.

Ground every word in ONLY the posting and the resume you are given. Never invent a fact, a number, an employer, or a feeling that is not present in that material.

How to handle each topic:
- angle: suggest a throughline connecting the resume's strongest evidence to the posting's core asks (e.g. "you might lead with your regulatory build-out experience, since the posting's top requirement is governance").
- emphasis: pick 2-3 already-recorded accomplishments from the resume that best match the posting's top requirements. Select existing facts; invent nothing new.
- tone: suggest a plain, reasonable framing for how the letter should sound, drawing on the resume's own voice if it signals one, otherwise a sensible default.
- motivation: this is the one topic you must handle with the most caution. Why the candidate personally wants this role is not recorded anywhere - it is personal to them, never a fact you can know. NEVER write in the first person and NEVER claim enthusiasm, passion, or personal motivation on the candidate's behalf. You may ONLY surface an evidence-based fit observation - a plain statement of where the posting's ask and the resume's recorded experience line up (e.g. "the posting emphasizes governance work; your recorded experience shows you built out a compliance program, which is a direct match") - framed as something the candidate could build their own answer around, never as a first-person statement of why they want the job. If you cannot produce even that honestly, decline.

If you cannot answer honestly for the topic given, reply with an empty string rather than reaching for anything invented. This is expected and correct far more often for motivation than for the other topics.

If the candidate gave revision notes, follow them, but every rule above still binds even if a note asks you to go further than the evidence supports.

Reply with exactly one JSON object and nothing else - no markdown fences:
{"suggestion": "your one suggestion for this topic, or an empty string if you cannot do it honestly"}"#;

#[derive(Debug, Deserialize)]
pub struct RawSuggestion {
    #[serde(default)]
    suggestion: String,
}

/// What the user chose to do with a suggested answer for a slot. Mirrors
/// `strengthen::SuggestOutcome`.
enum CoverSuggestOutcome {
    /// Take this answer (already guard-clean) and record it.
    Accepted(String),
    /// Decline the suggestion and answer the interview question in their
    /// own words.
    OwnWords,
    /// Leave this slot exactly as it is — no suggestion, no interview.
    Skip,
}

/// Every string a cover-letter suggestion may draw a fact from: the
/// posting's stated requirements and the candidate's own tailored résumé.
/// Mirrors `strengthen::RoleEvidence::texts`, scoped to the whole JD and
/// résumé rather than one role's bullets, since a cover letter's angle or
/// motivation can reasonably draw on any part of either.
fn cover_evidence_texts(jd: &JobRequirements, resume: &TailoredResume) -> Vec<String> {
    let mut texts = vec![jd.title.clone(), jd.company.clone(), resume.summary.clone()];
    texts.extend(jd.required_skills.iter().map(|s| s.name.clone()));
    texts.extend(jd.responsibilities.clone());
    texts.extend(resume.skills_section.skills.clone());
    if let Some(title) = &resume.target_title {
        texts.push(title.clone());
    }
    texts.extend(resume_bullet_lines(resume));
    texts
}

/// Every recorded bullet across the tailored résumé's selected roles,
/// labeled by role — the raw material a suggestion may select from or
/// point to, never invent beyond.
fn resume_bullet_lines(resume: &TailoredResume) -> Vec<String> {
    resume
        .roles
        .iter()
        .flat_map(|role| {
            role.bullets
                .iter()
                .map(move |bullet| format!("{} at {}: {}", role.title, role.company, bullet.text))
        })
        .collect()
}

/// One suggestion attempt, fully guarded. Returns the candidate answer
/// only if the agent produced a non-empty suggestion that differs from
/// `previous` (the suggestion already on screen, if any — `None` on the
/// first attempt) and introduces no fact outside the JD and résumé it was
/// given. Otherwise `None`, and the caller offers nothing new.
async fn run_cover_suggest(
    ctx: &AgentContext<'_>,
    slot: Slot,
    jd: &JobRequirements,
    resume: &TailoredResume,
    notes: &[String],
    previous: Option<&str>,
) -> Option<String> {
    let input = CoverSuggestInput {
        slot: slot.key().to_string(),
        topic: slot.topic().to_string(),
        jd_title: jd.title.clone(),
        jd_company: jd.company.clone(),
        jd_required_skills: jd.required_skills.iter().map(|s| s.name.clone()).collect(),
        jd_responsibilities: jd.responsibilities.clone(),
        resume_target_title: resume.target_title.clone(),
        resume_summary: resume.summary.clone(),
        resume_skills: resume.skills_section.skills.clone(),
        resume_bullets: resume_bullet_lines(resume),
        notes: notes.to_vec(),
    };
    let run = CoverSuggestAgent.run(ctx, input).await.ok()?;
    let suggestion = run.output.trim().to_string();
    if suggestion.is_empty() || Some(suggestion.as_str()) == previous {
        return None; // nothing to offer, or no real change from a tweak
    }
    // May draw only on what the posting states and what the candidate has
    // already recorded on their resume - never a fact from neither.
    let evidence = cover_evidence_texts(jd, resume);
    let allowed: Vec<&str> = evidence.iter().map(String::as_str).collect();
    if within_evidence(&suggestion, &allowed) {
        Some(suggestion)
    } else {
        None
    }
}

/// Offer a guarded, evidence-grounded suggestion for one suggestible slot
/// before the plain interview question for that slot, and let the user
/// accept it, tweak it, switch to their own words, or skip the slot
/// entirely. Returns `None` when no honest suggestion could be produced,
/// so the caller falls through to the interview unchanged. Mirrors
/// `strengthen::suggest_flow`.
///
/// A suggestion that's never shown to the user — declined outright, or a
/// tweak the guard rejects — costs nothing against `MAX_QUESTIONS`,
/// exactly like the interview agent's own "done" signal never counting.
/// Only a suggestion actually PRESENTED to the user (the initial menu,
/// and each re-shown menu after a tweak) counts, the same way a real
/// interview question counts and an empty "I have enough" reply doesn't.
async fn cover_suggest_flow(
    ctx: &AgentContext<'_>,
    slot: Slot,
    jd: &JobRequirements,
    resume: &TailoredResume,
    user: &dyn UserHandle,
    asked: &mut usize,
) -> Result<Option<CoverSuggestOutcome>, AskError> {
    if *asked >= MAX_QUESTIONS {
        return Ok(None);
    }
    let mut notes: Vec<String> = Vec::new();
    let Some(mut suggestion) = run_cover_suggest(ctx, slot, jd, resume, &notes, None).await else {
        return Ok(None); // no honest suggestion; the caller uses the interview
    };
    *asked += 1; // about to show this to the user, same accounting as a real question

    let mut revises_left = SUGGEST_REVISES;
    loop {
        let mut options = vec!["Use this wording".to_string()];
        if revises_left > 0 && *asked < MAX_QUESTIONS {
            options.push("Tweak it".to_string());
        }
        options.push("Answer in my own words".to_string());
        options.push("Skip this one".to_string());

        let choice = match user
            .ask(Question::Select {
                prompt: format!("a possible {}:\n  \"{suggestion}\"", slot.key()),
                options: options.clone(),
            })
            .await?
        {
            Answer::Choice(i) => options.get(i).map(String::as_str),
            _ => Some("Answer in my own words"), // unexpected shape; defer to the user
        };

        match choice {
            Some("Use this wording") => {
                return Ok(Some(CoverSuggestOutcome::Accepted(suggestion)));
            }
            Some("Answer in my own words") => return Ok(Some(CoverSuggestOutcome::OwnWords)),
            Some("Skip this one") => return Ok(Some(CoverSuggestOutcome::Skip)),
            Some("Tweak it") => {
                let note = match user
                    .ask(Question::Text {
                        prompt: "what should change?".to_string(),
                    })
                    .await?
                {
                    Answer::Text(t) if !t.trim().is_empty() => t.trim().to_string(),
                    _ => continue, // no guidance given; re-show the same suggestion
                };
                notes.push(note);
                revises_left -= 1;
                // Keep the prior suggestion if the new one fails, is empty, or
                // is unchanged - a tweak can never regress to something worse.
                if let Some(next) =
                    run_cover_suggest(ctx, slot, jd, resume, &notes, Some(&suggestion)).await
                {
                    suggestion = next;
                }
                *asked += 1; // re-presenting the (possibly updated) suggestion
            }
            _ => return Ok(Some(CoverSuggestOutcome::OwnWords)),
        }
    }
}

// ---------------------------------------------------------------------
// Agent 3: turn one flagged draft paragraph into a single question
// ---------------------------------------------------------------------

/// What the gap-question agent works from: the posting for light grounding,
/// the one flagged draft paragraph, and the honesty check's plain-language
/// note on what the résumé and posting don't support. It is deliberately not
/// given the résumé's facts to answer from — its only job is to phrase a
/// question that puts the flagged claim back to the candidate.
#[derive(Serialize)]
pub struct CoverGapQuestionInput {
    pub jd_title: String,
    pub jd_company: String,
    pub paragraph: String,
    pub unbacked: String,
}

/// Phrases ONE question about a single paragraph the preliminary draft's
/// provenance check flagged as unsupported, inviting the candidate to confirm
/// the claim, add the detail that backs it, or say to drop it. Like
/// [`CoverInterviewAgent`], it only ever asks — its `Output` is a question and
/// it has no field to supply an answer or a rewrite, so nothing it produces
/// can become a recorded fact. It takes the default (mid) tier, the same the
/// other question-asking agents in this module use: phrasing a warm, specific
/// question is judgment work, not the cheap-tier classification the provenance
/// judge ([`CoverClaimAgent`](crate::cover_provenance::CoverClaimAgent)) does.
pub struct CoverGapQuestionAgent;

#[async_trait]
impl Agent for CoverGapQuestionAgent {
    type Input = CoverGapQuestionInput;
    type Wire = RawQuestion;
    type Output = String;
    type Error = CoverInterviewError;

    fn id(&self) -> &'static str {
        "cover_gap_question_v1"
    }
    fn system_prompt(&self) -> &str {
        GAP_QUESTION_PROMPT
    }
    fn reply_budget(&self) -> u32 {
        REPLY_BUDGET
    }
    fn user_message(&self, input: &CoverGapQuestionInput) -> String {
        format!(
            "The posting: {} at {}\n\nA paragraph from the draft cover letter:\n\"{}\"\n\n\
             What the honesty check flagged as unsupported:\n{}\n\n\
             Ask your one question about this paragraph.",
            input.jd_title, input.jd_company, input.paragraph, input.unbacked
        )
    }
    fn bad_reply(&self, snippet: String, source: serde_json::Error) -> CoverInterviewError {
        CoverInterviewError::BadReply { snippet, source }
    }
    fn assemble(
        &self,
        wire: RawQuestion,
        _input: CoverGapQuestionInput,
    ) -> Result<String, CoverInterviewError> {
        Ok(wire.question)
    }
}

const GAP_QUESTION_PROMPT: &str = r#"You help a job candidate review a draft cover letter, one paragraph at a time. A draft was written from their resume and the job posting, and an honesty check flagged one paragraph for making a claim the resume and posting don't clearly support. You are given that paragraph and a short note on what isn't supported.

Ask ONE short, warm, specific question that puts the flagged claim back to the candidate and lets them confirm it, add the detail that backs it up, or tell you to cut it. Point at the actual thing the paragraph claims, in plain words: "The draft says you led the migration to a new payments platform - is that accurate, and is there anything you'd add, or should it come out?"

Rules that always hold:
- You do NOT know whether the claim is true; only the candidate does. Ask; never assert, guess, or imply the answer.
- NEVER rewrite the paragraph or propose replacement wording. You only ask one question.
- Never invent a fact about the candidate or the company. Refer only to what the flagged paragraph already says.
- One question, one or two sentences, concrete and easy to answer.

Reply with exactly one JSON object and nothing else - no markdown fences:
{"question": "your one question"}"#;

/// One paragraph a preliminary draft's provenance check flagged, paired with
/// the reason the candidate should react to. Draft order is preserved from the
/// report, so when there are more gaps than the budget allows, the earliest
/// (top-of-letter) paragraphs are the ones asked about — the plainest ranking,
/// and the one a reader's eye reaches first.
struct GapParagraph {
    text: String,
    reason: String,
}

/// The plain-language reason to put a flagged paragraph back to the candidate:
/// the model's own claim description when it flagged one, otherwise a
/// deterministic note built from the unbacked number(s) the digit guard found.
/// An `Unrecorded` paragraph always fired on at least one of those two axes
/// (see [`check_cover_provenance`](crate::cover_provenance)), so this is always
/// `Some` for the paragraphs [`gather_gaps`] keeps.
fn gap_reason(p: &crate::cover_provenance::ParagraphProvenance) -> Option<String> {
    if let Some(claim) = &p.unbacked_claim {
        return Some(claim.clone());
    }
    if !p.unbacked_digits.is_empty() {
        return Some(format!(
            "states the figure(s) {} that your evidence doesn't record",
            p.unbacked_digits.join(", ")
        ));
    }
    None
}

/// A plain fallback question for one flagged paragraph, used only when the
/// gap-question agent can't be reached — mirrors [`Slot::fallback_question`]
/// so a transient model error surfaces the gap plainly rather than skipping
/// it. Quotes only the draft's own words, never a fact from elsewhere.
fn gap_fallback_question(gap: &GapParagraph) -> String {
    format!(
        "Your draft says: \"{}\" Is that accurate, and is there anything you'd add, \
         or should it come out?",
        gap_snippet(&gap.text)
    )
}

/// The first stretch of a flagged paragraph, for a fallback question — kept
/// short so the prompt stays readable.
fn gap_snippet(text: &str) -> String {
    let snippet: String = text.chars().take(160).collect();
    if snippet.len() < text.len() {
        format!("{}…", snippet.trim_end())
    } else {
        snippet
    }
}

/// Draft-first, step one: draft a naive preliminary letter from the résumé
/// and JD alone (no brief). Called before the candidate is shown anything —
/// see [`run_cover_interview`] — so the tailoring work is already done, and
/// silent, by the time the leading question appears.
///
/// `None` on any model failure or a draft that assembles to nothing; the
/// caller then has no scratch draft to check and [`gather_gaps`] reports no
/// gaps, so the session falls back to the fixed-topic walk-through.
async fn draft_scratch_letter(
    ctx: &AgentContext<'_>,
    resume: &TailoredResume,
    jd: &JobRequirements,
) -> Option<CoverLetter> {
    let (letter, _warnings, _usage) = write_cover_letter(ctx, resume, jd, &[], None).await.ok()?;
    Some(letter)
}

/// Draft-first, step two: run the already-drafted scratch letter's
/// provenance check, and collect the paragraphs it flagged as unsupported,
/// in draft order. Called after the leading question — see
/// [`run_cover_interview`] — so the check gets `brief` with whatever that
/// answer already contributed, and a claim the candidate just grounded
/// doesn't read as a gap.
///
/// Every failure mode collapses to "no gaps": no scratch draft to check
/// (`letter` is `None`) or a failed provenance call both return an empty
/// list, and the caller falls back to the fixed-topic walk-through. The
/// scratch letter itself is never returned or persisted from here — only
/// the list of gaps it revealed leaves this function.
async fn gather_gaps(
    ctx: &AgentContext<'_>,
    letter: Option<&CoverLetter>,
    resume: &TailoredResume,
    jd: &JobRequirements,
    brief: &CoverBrief,
) -> Vec<GapParagraph> {
    let Some(letter) = letter else {
        return Vec::new();
    };
    let Ok(report) = check_cover_provenance(ctx, letter, resume, jd, Some(brief)).await else {
        return Vec::new();
    };
    report
        .paragraphs
        .into_iter()
        .filter(|p| p.status == CoverParagraphStatus::Unrecorded)
        .filter_map(|p| {
            gap_reason(&p).map(|reason| GapParagraph {
                text: p.text,
                reason,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------
// The interview loop
// ---------------------------------------------------------------------

/// Interview the candidate about the cover letter and return what they said
/// as a [`CoverBrief`]. Runs draft-first (see the module doc), split across
/// two points in the flow: a naive preliminary letter is drafted from the
/// résumé and JD alone *before the candidate sees anything at all* — the
/// tailoring work is already done, silently, by the time the first prompt
/// appears. That first prompt is one free-form question — anything the
/// candidate already knows they want said — which costs no model call and
/// lands in `emphasis`. Only after that question is answered does the
/// preliminary draft's honesty check
/// ([`check_cover_provenance`](crate::cover_provenance)) run, now with
/// whatever the leading answer contributed already folded into the brief, so
/// a claim it just grounded doesn't read as a gap. Each paragraph the check
/// still flags becomes one targeted question ([`CoverGapQuestionAgent`])
/// whose answer lands in `emphasis` verbatim. When the preliminary draft has
/// no gaps — or can't be drafted or checked — the session falls back to the
/// original fixed walk-through over the letter's angle, emphasis, tone,
/// motivation, and constraints, each with an optional guarded suggestion
/// first. Every field comes from the user's own typed answers either way,
/// whether typed directly or accepted from a suggestion — the agents only
/// ever ask or propose (see [`CoverBrief`]'s doc comment for how that's
/// enforced structurally).
///
/// The preliminary draft is scratch: it is neither returned nor persisted, and
/// only the list of gaps it revealed shapes the questions. The caller always
/// re-drafts the real letter from the finished brief, so nothing from the
/// scratch draft's own wording can reach the final letter unchanged.
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

    // Draft-first, step one, before anything is shown to the candidate: draft
    // a naive scratch letter from the résumé and JD alone. Its own honesty
    // check runs later (step two, below), once the leading question has had
    // its chance to ground a claim in the brief - checking now, with an
    // always-empty brief, would flag paragraphs that answer was about to
    // cover.
    let scratch_letter = draft_scratch_letter(ctx, resume, jd).await;

    // A single open door before the guided walk-through: some candidates
    // already know exactly what they want said and would rather say it once
    // than answer several separate questions. Fixed text, no model call, so
    // it costs nothing against MAX_QUESTIONS. Purely additive — leaving it
    // blank changes nothing about the slots that follow, and matched (not
    // `?`-propagated) so abandoning it degrades the same way an abandoned
    // slot does, even though nothing has been gathered yet at this point.
    match user
        .ask(Question::Text {
            prompt: "Anything specific you already know you want this letter to say? \
                     Leave blank to skip and answer a few short questions instead."
                .to_string(),
        })
        .await
    {
        Ok(Answer::Text(open)) => {
            let open = open.trim();
            if !open.is_empty() {
                brief.emphasis.push(open.to_string());
            }
        }
        Ok(_) => {}
        Err(_) => return Ok(brief),
    }

    let mut asked = 0usize;

    // Draft-first, step two: the scratch letter is already drafted, so let
    // its own honesty check tell us which paragraphs need the candidate's
    // word, now that the leading answer (if any) is folded into `brief`.
    let gaps = gather_gaps(ctx, scratch_letter.as_ref(), resume, jd, &brief).await;

    // The gap-driven interview: one targeted question per flagged paragraph,
    // up to the shared MAX_QUESTIONS budget (the free leading question doesn't
    // count against it, exactly as before). Draft order ranks them, so the
    // top-of-letter gaps win the budget. Each answer is the candidate's own
    // words, appended verbatim to `emphasis` the same way the leading
    // question's answer is — the model only ever supplied the question's
    // phrasing.
    if !gaps.is_empty() {
        for gap in gaps {
            if asked >= MAX_QUESTIONS {
                break;
            }
            let question = match CoverGapQuestionAgent
                .run(
                    ctx,
                    CoverGapQuestionInput {
                        jd_title: jd.title.clone(),
                        jd_company: jd.company.clone(),
                        paragraph: gap.text.clone(),
                        unbacked: gap.reason.clone(),
                    },
                )
                .await
            {
                Ok(run) => {
                    let text = run.output.trim().to_string();
                    if text.is_empty() {
                        gap_fallback_question(&gap)
                    } else {
                        text
                    }
                }
                // The agent couldn't phrase it; surface the gap plainly rather
                // than skipping the paragraph outright.
                Err(_) => gap_fallback_question(&gap),
            };
            asked += 1;
            match user.ask(Question::Text { prompt: question }).await {
                Ok(Answer::Text(t)) if !t.trim().is_empty() => {
                    brief.emphasis.push(t.trim().to_string());
                }
                Ok(_) => continue,          // blank: leave this gap, move on
                Err(_) => return Ok(brief), // abandoned: keep what's gathered
            }
        }
        return Ok(brief);
    }

    // Fallback: no gaps (every paragraph grounded or exempt) or no usable
    // draft — run the original fixed-topic walk-through exactly as before.
    for slot in Slot::ALL {
        if asked >= MAX_QUESTIONS {
            break;
        }

        // Offer a guarded suggestion first for every slot except
        // constraints (see `Slot::suggestible`). Accepting records the
        // answer through the same `Slot::record` path an interview
        // answer takes, then moves straight to the next slot; declining,
        // skipping, or getting no honest suggestion falls through to (or
        // past) the interview below, which is otherwise unchanged.
        if slot.suggestible() {
            // Matched rather than `?`-propagated: a decline on the suggestion
            // menu itself (the "End session" bail-out, or an equivalent
            // dismissal over the browser bridge) is an `AskError` from
            // `cover_suggest_flow`'s own `user.ask` calls, and this function's
            // contract (see its doc comment) is to degrade to the partial
            // brief on ANY ask failure partway through — not just one in the
            // plain question loop below. Propagating here would drop
            // whatever earlier slots already recorded.
            match cover_suggest_flow(ctx, slot, jd, resume, user, &mut asked).await {
                Ok(Some(CoverSuggestOutcome::Accepted(text))) => {
                    slot.record(&mut brief, text);
                    continue;
                }
                Ok(Some(CoverSuggestOutcome::Skip)) => continue,
                Ok(Some(CoverSuggestOutcome::OwnWords)) | Ok(None) => {}
                Err(_) => return Ok(brief), // abandoned interview: keep what's gathered
            }
            if asked >= MAX_QUESTIONS {
                break;
            }
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

    /// Enqueue the fastest possible resolution for one slot: a declined
    /// suggestion (where the slot offers one) followed by an immediate
    /// "done" from the interviewer, so the slot contributes nothing to
    /// the brief and never asks the user anything. Used to keep a test's
    /// mock script focused on the one slot actually under test.
    fn enqueue_skip(mock: &MockLlmClient, slot: Slot) {
        if slot.suggestible() {
            mock.enqueue(r#"{"suggestion": ""}"#);
        }
        mock.enqueue(r#"{"question": ""}"#);
    }

    /// Enqueue the two model replies the draft-first flow makes before it can
    /// ever reach the fixed-topic fallback: a one-paragraph preliminary letter
    /// (no digits, so it survives the digit guard against the sample résumé),
    /// drafted first, and a `grounded` verdict for it, checked after the
    /// leading question (itself no model call). With no gap found,
    /// `gather_gaps` returns empty and the session falls back to the fixed
    /// walk-through the rest of a script exercises. Prepend this to any test
    /// that drives the fixed topics — the request queue is FIFO regardless of
    /// where the leading question's `ask` falls in between, so these two
    /// replies are still consumed first, in order.
    fn enqueue_grounded_preliminary(mock: &MockLlmClient) {
        mock.enqueue(r#"{"paragraphs": ["A grounded paragraph drawn from the resume."]}"#);
        mock.enqueue(r#"{"paragraphs": [{"status": "grounded", "unbacked": ""}]}"#);
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

    #[test]
    fn the_suggest_prompt_forbids_first_person_motivation_claims() {
        assert!(SUGGEST_PROMPT.contains(
            "NEVER write in the first person and NEVER claim enthusiasm, passion, or personal motivation"
        ));
        assert!(SUGGEST_PROMPT.contains("evidence-based fit observation"));
    }

    #[test]
    fn record_merges_a_second_answer_into_a_scalar_slot_instead_of_overwriting() {
        let mut brief = CoverBrief::default();
        Slot::Angle.record(&mut brief, "lead with reliability".into());
        Slot::Angle.record(&mut brief, "also mention the regulatory background".into());
        assert_eq!(
            brief.angle.as_deref(),
            Some("lead with reliability\nalso mention the regulatory background")
        );
    }

    #[test]
    fn only_constraints_is_not_suggestible() {
        assert!(Slot::Angle.suggestible());
        assert!(Slot::Emphasis.suggestible());
        assert!(Slot::Tone.suggestible());
        assert!(Slot::Motivation.suggestible());
        assert!(!Slot::Constraints.suggestible());
    }

    #[test]
    fn an_empty_json_object_deserializes_to_the_default_brief() {
        let brief: CoverBrief = serde_json::from_str("{}").unwrap();
        assert_eq!(brief, CoverBrief::default());
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

    #[tokio::test]
    async fn a_full_interview_records_only_the_users_answers() {
        let mock = MockLlmClient::default();
        enqueue_grounded_preliminary(&mock); // no gaps -> the fixed loop runs
        // A declined suggestion, then one real question, then an empty
        // "done" reply, for each of the four suggestible topics; the
        // fifth (constraints) skips the suggestion step entirely.
        mock.enqueue(r#"{"suggestion": ""}"#);
        mock.enqueue(r#"{"question": "Lead with scale, or with reliability?"}"#);
        mock.enqueue(r#"{"question": ""}"#);
        mock.enqueue(r#"{"suggestion": ""}"#);
        mock.enqueue(r#"{"question": "What should this letter highlight?"}"#);
        mock.enqueue(r#"{"question": ""}"#);
        mock.enqueue(r#"{"suggestion": ""}"#);
        mock.enqueue(r#"{"question": "Formal or conversational?"}"#);
        mock.enqueue(r#"{"question": ""}"#);
        mock.enqueue(r#"{"suggestion": ""}"#);
        mock.enqueue(r#"{"question": "Why this company specifically?"}"#);
        mock.enqueue(r#"{"question": ""}"#);
        mock.enqueue(r#"{"question": "Anything to avoid mentioning?"}"#);
        mock.enqueue(r#"{"question": ""}"#);

        let user = ScriptedUser::new();
        user.answer(Answer::Text("".into())); // decline the leading open question
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
    async fn a_follow_up_answer_on_a_scalar_slot_is_preserved_alongside_the_first() {
        let mock = MockLlmClient::default();
        enqueue_grounded_preliminary(&mock); // no gaps -> the fixed loop runs
        mock.enqueue(r#"{"suggestion": ""}"#); // angle: no suggestion offered
        mock.enqueue(r#"{"question": "Lead with scale, or with reliability?"}"#); // opening
        mock.enqueue(r#"{"question": "Anything else about the angle?"}"#); // thin answer -> follow-up
        mock.enqueue(r#"{"question": ""}"#); // now satisfied
        enqueue_skip(&mock, Slot::Emphasis);
        enqueue_skip(&mock, Slot::Tone);
        enqueue_skip(&mock, Slot::Motivation);
        enqueue_skip(&mock, Slot::Constraints);

        let user = ScriptedUser::new();
        user.answer(Answer::Text("".into())); // decline the leading open question
        user.answer(Answer::Text("lead with the reliability angle".into()));
        user.answer(Answer::Text(
            "also mention the regulatory build-out work".into(),
        ));

        let brief = run_cover_interview(&sample_resume(), &sample_jd(), &user, &ctx(&mock))
            .await
            .unwrap();

        // Both answers survive - the second doesn't silently discard the first.
        assert_eq!(
            brief.angle.as_deref(),
            Some("lead with the reliability angle\nalso mention the regulatory build-out work")
        );
    }

    #[tokio::test]
    async fn a_blank_answer_leaves_its_slot_empty_without_crashing() {
        let mock = MockLlmClient::default();
        enqueue_grounded_preliminary(&mock); // no gaps -> the fixed loop runs
        mock.enqueue(r#"{"suggestion": ""}"#);
        mock.enqueue(r#"{"question": "Tell me more?"}"#);
        mock.enqueue(r#"{"suggestion": ""}"#);
        mock.enqueue(r#"{"question": "Tell me more?"}"#);
        mock.enqueue(r#"{"suggestion": ""}"#);
        mock.enqueue(r#"{"question": "Tell me more?"}"#);
        mock.enqueue(r#"{"suggestion": ""}"#);
        mock.enqueue(r#"{"question": "Tell me more?"}"#);
        mock.enqueue(r#"{"question": "Tell me more?"}"#); // constraints: no suggestion

        let user = ScriptedUser::new();
        user.answer(Answer::Text("   ".into())); // blank the leading open question too
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
        enqueue_grounded_preliminary(&mock); // no gaps -> the fixed loop runs
        // The interviewer never says "done" and the user never goes
        // blank, so without a cap this would run all five topics to
        // their two-turn-each ceiling. Each of angle/emphasis/tone also
        // declines a suggestion first (which costs nothing against the
        // cap - see `MAX_QUESTIONS`'s doc comment), so the cap still
        // bites after exactly three full topics' worth of real questions.
        mock.enqueue(r#"{"suggestion": ""}"#);
        mock.enqueue(r#"{"question": "question number 0?"}"#);
        mock.enqueue(r#"{"question": "question number 1?"}"#);
        mock.enqueue(r#"{"suggestion": ""}"#);
        mock.enqueue(r#"{"question": "question number 2?"}"#);
        mock.enqueue(r#"{"question": "question number 3?"}"#);
        mock.enqueue(r#"{"suggestion": ""}"#);
        mock.enqueue(r#"{"question": "question number 4?"}"#);
        mock.enqueue(r#"{"question": "question number 5?"}"#);
        let user = ScriptedUser::new();
        user.answer(Answer::Text("".into())); // decline the leading open question
        for i in 0..6 {
            user.answer(Answer::Text(format!("answer number {i}")));
        }

        let brief = run_cover_interview(&sample_resume(), &sample_jd(), &user, &ctx(&mock))
            .await
            .unwrap();

        // Capped at MAX_QUESTIONS real questions asked, not the ten the
        // topics would otherwise allow.
        assert_eq!(
            mock.requests()
                .iter()
                .filter(|r| r.system.as_deref() == Some(QUESTION_PROMPT))
                .count(),
            MAX_QUESTIONS
        );
        // angle, emphasis, and tone each got their full two turns before
        // the cap bit; motivation and constraints never got a chance.
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
        // The leading open question is declined first. Angle then declines
        // a suggestion, and its interview opens normally; its follow-up
        // turn finds the mock exhausted (so it just moves on). Emphasis's
        // suggestion attempt then also finds the mock exhausted (no
        // honest suggestion, agent unreachable), and its interview's
        // fallback opening question finds no answer queued at all - the
        // `ask` fails, and the loop must return what it already gathered
        // rather than erroring the whole flow.
        let mock = MockLlmClient::default();
        enqueue_grounded_preliminary(&mock); // no gaps -> the fixed loop runs
        mock.enqueue(r#"{"suggestion": ""}"#);
        mock.enqueue(r#"{"question": "Lead with scale, or with reliability?"}"#);
        let user = ScriptedUser::new();
        user.answer(Answer::Text("".into())); // decline the leading open question
        user.answer(Answer::Text("lead with the reliability angle".into()));
        // No third answer queued: ScriptedUser::ask fails NotInteractive.

        let brief = run_cover_interview(&sample_resume(), &sample_jd(), &user, &ctx(&mock))
            .await
            .unwrap();

        assert_eq!(
            brief.angle.as_deref(),
            Some("lead with the reliability angle")
        );
        assert!(brief.emphasis.is_empty());
    }

    #[tokio::test]
    async fn abandoning_a_suggestion_menu_keeps_the_earlier_slots_answers() {
        // Angle is answered in full (a declined suggestion, then a plain
        // interview answer). Emphasis then gets a real suggestion, but the
        // user abandons it - the browser bridge maps a dismissed suggestion
        // menu to an `ask` failure (see `bridge::ask_over_js`'s `abort`
        // handling), not a declined-with-a-menu-shown outcome. That failure
        // happens INSIDE `cover_suggest_flow`, one slot after angle already
        // recorded something - proving the fix keeps angle's answer instead
        // of losing it to a propagated error.
        let mock = MockLlmClient::default();
        enqueue_grounded_preliminary(&mock); // no gaps -> the fixed loop runs
        mock.enqueue(r#"{"suggestion": ""}"#);
        mock.enqueue(r#"{"question": "Lead with scale, or with reliability?"}"#);
        mock.enqueue(r#"{"question": ""}"#);
        mock.enqueue(r#"{"suggestion": "you might lead with your incident response work"}"#);
        let user = ScriptedUser::new();
        user.answer(Answer::Text("".into())); // decline the leading open question
        user.answer(Answer::Text("lead with the reliability angle".into()));
        // No Choice queued for emphasis's suggestion menu: `ScriptedUser::ask`
        // fails `NotInteractive`, the same shape a dismissed browser modal
        // produces.

        let brief = run_cover_interview(&sample_resume(), &sample_jd(), &user, &ctx(&mock))
            .await
            .unwrap();

        assert_eq!(
            brief.angle.as_deref(),
            Some("lead with the reliability angle"),
            "angle's answer must survive a later slot's abandoned suggestion menu"
        );
        assert!(brief.emphasis.is_empty());
    }

    #[tokio::test]
    async fn a_guard_clean_suggestion_can_be_accepted() {
        let mock = MockLlmClient::default();
        enqueue_grounded_preliminary(&mock); // no gaps -> the fixed loop runs
        mock.enqueue(
            r#"{"suggestion": "you might lead with your reliability platform work, since the posting's top ask is platform reliability"}"#,
        );
        enqueue_skip(&mock, Slot::Emphasis);
        enqueue_skip(&mock, Slot::Tone);
        enqueue_skip(&mock, Slot::Motivation);
        enqueue_skip(&mock, Slot::Constraints);

        let user = ScriptedUser::new();
        user.answer(Answer::Text("".into())); // decline the leading open question
        user.answer(Answer::Choice(0)); // "Use this wording"

        let brief = run_cover_interview(&sample_resume(), &sample_jd(), &user, &ctx(&mock))
            .await
            .unwrap();

        // Recorded exactly as suggested, via the normal Slot::record path -
        // and no interview question was ever asked for angle.
        assert_eq!(
            brief.angle.as_deref(),
            Some(
                "you might lead with your reliability platform work, since the posting's top ask is platform reliability"
            )
        );
        // The two draft-first calls (preliminary draft + provenance) plus the
        // eight fixed-topic calls the script queues.
        assert_eq!(mock.requests().len(), 10);
    }

    #[tokio::test]
    async fn a_declined_suggestion_falls_through_silently_with_no_menu() {
        let mock = MockLlmClient::default();
        enqueue_grounded_preliminary(&mock); // no gaps -> the fixed loop runs
        enqueue_skip(&mock, Slot::Angle);
        // Emphasis declines a suggestion, then the interview asks and gets
        // an answer directly - proven by there being only ONE queued
        // answer, and it a Text, not a Choice: if a menu had wrongly been
        // shown, it would have consumed this Text as a Select response and
        // the interview's own question would then find nothing queued.
        mock.enqueue(r#"{"suggestion": ""}"#);
        mock.enqueue(r#"{"question": "What should this letter highlight?"}"#);
        mock.enqueue(r#"{"question": ""}"#);
        enqueue_skip(&mock, Slot::Tone);
        enqueue_skip(&mock, Slot::Motivation);
        enqueue_skip(&mock, Slot::Constraints);

        let user = ScriptedUser::new();
        user.answer(Answer::Text("".into())); // decline the leading open question
        user.answer(Answer::Text("the incident response work".into()));

        let brief = run_cover_interview(&sample_resume(), &sample_jd(), &user, &ctx(&mock))
            .await
            .unwrap();

        assert_eq!(
            brief.emphasis,
            vec!["the incident response work".to_string()]
        );
    }

    #[tokio::test]
    async fn a_suggestion_that_invents_a_fact_is_rejected_and_falls_back_to_the_interview() {
        let mock = MockLlmClient::default();
        enqueue_grounded_preliminary(&mock); // no gaps -> the fixed loop runs
        // Neither the JD nor the resume mentions a percentage; the model
        // invents "40%". The guard rejects it before any menu is shown.
        mock.enqueue(r#"{"suggestion": "lead with the 40% reliability improvement"}"#);
        mock.enqueue(r#"{"question": "What angle would you rather take?"}"#);
        mock.enqueue(r#"{"question": ""}"#);
        enqueue_skip(&mock, Slot::Emphasis);
        enqueue_skip(&mock, Slot::Tone);
        enqueue_skip(&mock, Slot::Motivation);
        enqueue_skip(&mock, Slot::Constraints);

        let user = ScriptedUser::new();
        user.answer(Answer::Text("".into())); // decline the leading open question
        // No Choice queued next: a rejected suggestion offers no menu.
        user.answer(Answer::Text("lead with the reliability angle".into()));

        let brief = run_cover_interview(&sample_resume(), &sample_jd(), &user, &ctx(&mock))
            .await
            .unwrap();

        assert_eq!(
            brief.angle.as_deref(),
            Some("lead with the reliability angle")
        );
    }

    #[tokio::test]
    async fn tweaking_a_suggestion_revises_then_accepts() {
        let mock = MockLlmClient::default();
        enqueue_grounded_preliminary(&mock); // no gaps -> the fixed loop runs
        mock.enqueue(r#"{"suggestion": "you might lead with your platform reliability work"}"#);
        mock.enqueue(r#"{"suggestion": "you might lead with your incident response leadership"}"#);
        enqueue_skip(&mock, Slot::Emphasis);
        enqueue_skip(&mock, Slot::Tone);
        enqueue_skip(&mock, Slot::Motivation);
        enqueue_skip(&mock, Slot::Constraints);

        let user = ScriptedUser::new();
        user.answer(Answer::Text("".into())); // decline the leading open question
        user.answer(Answer::Choice(1)); // "Tweak it"
        user.answer(Answer::Text("talk about incident response instead".into()));
        user.answer(Answer::Choice(0)); // "Use this wording" (the revised suggestion)

        let brief = run_cover_interview(&sample_resume(), &sample_jd(), &user, &ctx(&mock))
            .await
            .unwrap();

        assert_eq!(
            brief.angle.as_deref(),
            Some("you might lead with your incident response leadership")
        );
    }

    #[tokio::test]
    async fn a_failed_tweak_keeps_the_prior_suggestion() {
        let mock = MockLlmClient::default();
        enqueue_grounded_preliminary(&mock); // no gaps -> the fixed loop runs
        mock.enqueue(r#"{"suggestion": "you might lead with your platform reliability work"}"#);
        mock.enqueue(r#"{"suggestion": ""}"#); // the tweak declines
        enqueue_skip(&mock, Slot::Emphasis);
        enqueue_skip(&mock, Slot::Tone);
        enqueue_skip(&mock, Slot::Motivation);
        enqueue_skip(&mock, Slot::Constraints);

        let user = ScriptedUser::new();
        user.answer(Answer::Text("".into())); // decline the leading open question
        user.answer(Answer::Choice(1)); // "Tweak it"
        user.answer(Answer::Text("say something else".into()));
        user.answer(Answer::Choice(0)); // "Use this wording" (still the ORIGINAL)

        let brief = run_cover_interview(&sample_resume(), &sample_jd(), &user, &ctx(&mock))
            .await
            .unwrap();

        assert_eq!(
            brief.angle.as_deref(),
            Some("you might lead with your platform reliability work")
        );
    }

    #[tokio::test]
    async fn choosing_own_words_falls_through_to_the_plain_interview() {
        let mock = MockLlmClient::default();
        enqueue_grounded_preliminary(&mock); // no gaps -> the fixed loop runs
        mock.enqueue(r#"{"suggestion": "you might lead with your platform reliability work"}"#);
        mock.enqueue(r#"{"question": "What angle would you rather take?"}"#);
        mock.enqueue(r#"{"question": ""}"#);
        enqueue_skip(&mock, Slot::Emphasis);
        enqueue_skip(&mock, Slot::Tone);
        enqueue_skip(&mock, Slot::Motivation);
        enqueue_skip(&mock, Slot::Constraints);

        let user = ScriptedUser::new();
        user.answer(Answer::Text("".into())); // decline the leading open question
        // Suggestion menu is [Use, Tweak, Answer in my own words, Skip].
        user.answer(Answer::Choice(2)); // "Answer in my own words"
        user.answer(Answer::Text("lead with the reliability angle".into()));

        let brief = run_cover_interview(&sample_resume(), &sample_jd(), &user, &ctx(&mock))
            .await
            .unwrap();

        assert_eq!(
            brief.angle.as_deref(),
            Some("lead with the reliability angle")
        );
    }

    #[tokio::test]
    async fn skipping_a_suggestion_leaves_the_slot_empty_with_no_interview() {
        let mock = MockLlmClient::default();
        enqueue_grounded_preliminary(&mock); // no gaps -> the fixed loop runs
        mock.enqueue(r#"{"suggestion": "you might lead with your platform reliability work"}"#);
        enqueue_skip(&mock, Slot::Emphasis);
        enqueue_skip(&mock, Slot::Tone);
        enqueue_skip(&mock, Slot::Motivation);
        enqueue_skip(&mock, Slot::Constraints);

        let user = ScriptedUser::new();
        user.answer(Answer::Text("".into())); // decline the leading open question
        user.answer(Answer::Choice(3)); // "Skip this one"

        let brief = run_cover_interview(&sample_resume(), &sample_jd(), &user, &ctx(&mock))
            .await
            .unwrap();

        assert_eq!(brief.angle, None);
        // Exactly the scripted calls happened - the two draft-first calls
        // plus the eight fixed-topic ones; no extra interview question was
        // asked for the skipped slot.
        assert_eq!(mock.requests().len(), 10);
    }

    #[tokio::test]
    async fn constraints_never_triggers_a_suggestion_attempt() {
        let mock = MockLlmClient::default();
        enqueue_grounded_preliminary(&mock); // no gaps -> the fixed loop runs
        enqueue_skip(&mock, Slot::Angle);
        enqueue_skip(&mock, Slot::Emphasis);
        enqueue_skip(&mock, Slot::Tone);
        enqueue_skip(&mock, Slot::Motivation);
        mock.enqueue(r#"{"question": "Anything to avoid mentioning?"}"#);
        mock.enqueue(r#"{"question": ""}"#);

        let user = ScriptedUser::new();
        user.answer(Answer::Text("".into())); // decline the leading open question
        user.answer(Answer::Text("don't mention my current employer".into()));

        let brief = run_cover_interview(&sample_resume(), &sample_jd(), &user, &ctx(&mock))
            .await
            .unwrap();

        assert_eq!(
            brief.constraints,
            vec!["don't mention my current employer".to_string()]
        );
        // Exactly one suggestion attempt per suggestible slot (angle,
        // emphasis, tone, motivation) - constraints skips the suggestion
        // step entirely and goes straight to the interview question.
        let suggest_calls = mock
            .requests()
            .iter()
            .filter(|r| r.system.as_deref() == Some(SUGGEST_PROMPT))
            .count();
        assert_eq!(suggest_calls, 4);
    }

    #[tokio::test]
    async fn the_leading_question_records_a_real_answer_as_the_first_emphasis_entry() {
        let mock = MockLlmClient::default();
        enqueue_grounded_preliminary(&mock); // no gaps -> the fixed loop runs
        // Every guided slot declines its suggestion and gets an
        // immediate "done" from the interviewer, so nothing but the
        // leading question ever reaches the user.
        enqueue_skip(&mock, Slot::Angle);
        enqueue_skip(&mock, Slot::Emphasis);
        enqueue_skip(&mock, Slot::Tone);
        enqueue_skip(&mock, Slot::Motivation);
        enqueue_skip(&mock, Slot::Constraints);

        let user = ScriptedUser::new();
        user.answer(Answer::Text(
            "I want to highlight my experience building this exact tool with AI-native practices"
                .into(),
        ));

        let brief = run_cover_interview(&sample_resume(), &sample_jd(), &user, &ctx(&mock))
            .await
            .unwrap();

        assert_eq!(
            brief.emphasis,
            vec![
                "I want to highlight my experience building this exact tool with AI-native practices"
                    .to_string()
            ]
        );
    }

    #[tokio::test]
    async fn a_blank_leading_answer_pushes_nothing_and_the_guided_slots_proceed_normally() {
        let mock = MockLlmClient::default();
        enqueue_grounded_preliminary(&mock); // no gaps -> the fixed loop runs
        mock.enqueue(r#"{"suggestion": ""}"#); // angle: no suggestion offered
        mock.enqueue(r#"{"question": "Lead with scale, or with reliability?"}"#);
        mock.enqueue(r#"{"question": ""}"#);
        enqueue_skip(&mock, Slot::Emphasis);
        enqueue_skip(&mock, Slot::Tone);
        enqueue_skip(&mock, Slot::Motivation);
        enqueue_skip(&mock, Slot::Constraints);

        let user = ScriptedUser::new();
        user.answer(Answer::Text("   ".into())); // blank leading question
        user.answer(Answer::Text("lead with the reliability angle".into()));

        let brief = run_cover_interview(&sample_resume(), &sample_jd(), &user, &ctx(&mock))
            .await
            .unwrap();

        // The blank leading answer pushed nothing into emphasis...
        assert!(brief.emphasis.is_empty());
        // ...and the guided walk-through still ran normally afterward.
        assert_eq!(
            brief.angle.as_deref(),
            Some("lead with the reliability angle")
        );
    }

    #[tokio::test]
    async fn an_exhausted_queue_at_the_leading_question_abandons_before_any_slot_is_reached() {
        let mock = MockLlmClient::default();
        // The scratch draft is generated before the leading question is ever
        // asked, so it still goes out to the model here even though the
        // interview abandons at that very next step.
        mock.enqueue(r#"{"paragraphs": ["A grounded paragraph drawn from the resume."]}"#);
        let user = ScriptedUser::new();
        // No answers queued at all: the leading question's `ask` fails
        // immediately, the same degrade contract as an ask failure
        // partway through the guided slots, except nothing has been
        // gathered yet.

        let brief = run_cover_interview(&sample_resume(), &sample_jd(), &user, &ctx(&mock))
            .await
            .unwrap();

        assert_eq!(brief, CoverBrief::default());
        assert_eq!(
            mock.requests().len(),
            1,
            "the scratch draft costs one model call even though the interview abandons \
             at the leading question and no slot is ever reached"
        );
    }

    #[tokio::test]
    async fn the_scratch_draft_is_generated_before_the_leading_question_is_asked() {
        // Proves the reordering directly: at the moment the leading question
        // (the first and only thing this test's user is ever asked) is put to
        // the user, the scratch draft's `write_cover_letter` call must
        // already have gone out to the model - not after.
        struct AssertsDraftAlreadySent<'a> {
            inner: ScriptedUser,
            mock: &'a MockLlmClient,
            asked: std::sync::atomic::AtomicBool,
        }

        #[async_trait]
        impl UserHandle for AssertsDraftAlreadySent<'_> {
            async fn ask(&self, question: Question) -> Result<Answer, AskError> {
                if !self.asked.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    assert_eq!(
                        self.mock.requests().len(),
                        1,
                        "the scratch draft must be generated before the leading question is asked"
                    );
                }
                self.inner.ask(question).await
            }
            async fn confirm(&self, prompt: &str, default: bool) -> Result<bool, AskError> {
                self.inner.confirm(prompt, default).await
            }
            fn notify(&self, message: &str) {
                self.inner.notify(message);
            }
            fn is_interactive(&self) -> bool {
                true
            }
        }

        let mock = MockLlmClient::default();
        // Only the scratch draft is queued. If the leading question were
        // asked first, no model call would have happened yet and the
        // assertion above would fail before this reply is even consumed.
        mock.enqueue(r#"{"paragraphs": ["A grounded paragraph drawn from the resume."]}"#);

        let user = AssertsDraftAlreadySent {
            inner: ScriptedUser::new(),
            mock: &mock,
            asked: std::sync::atomic::AtomicBool::new(false),
        };
        user.inner.answer(Answer::Text("".into())); // decline the leading open question

        // The provenance check that follows finds the mock exhausted and the
        // session degrades from there (falling toward the fixed-topic loop,
        // which also finds nothing queued) - none of that matters here; the
        // ordering assertion above already ran by the time this returns.
        let _ = run_cover_interview(&sample_resume(), &sample_jd(), &user, &ctx(&mock)).await;
    }

    // --- the draft-first, gap-driven flow ---------------------------------

    #[tokio::test]
    async fn a_gap_in_the_preliminary_draft_drives_a_targeted_question_not_the_fixed_loop() {
        let mock = MockLlmClient::default();
        // The preliminary draft (résumé + JD alone) makes one claim the
        // evidence doesn't back...
        mock.enqueue(r#"{"paragraphs": ["I led the migration to a new payments platform."]}"#);
        // ...and the provenance check flags exactly that paragraph.
        mock.enqueue(
            r#"{"paragraphs": [{"status": "unrecorded", "unbacked": "claims payments-platform experience the resume and posting don't mention"}]}"#,
        );
        // The gap-question agent phrases one question about it.
        mock.enqueue(
            r#"{"question": "The draft says you led the migration to a new payments platform - is that accurate, and is there anything you'd add, or should it come out?"}"#,
        );

        let user = ScriptedUser::new();
        user.answer(Answer::Text("".into())); // decline the leading open question
        user.answer(Answer::Text(
            "yes, I led that migration end to end at my last job".into(),
        ));

        let brief = run_cover_interview(&sample_resume(), &sample_jd(), &user, &ctx(&mock))
            .await
            .unwrap();

        // The candidate's own words about the gap landed in emphasis...
        assert_eq!(
            brief.emphasis,
            vec!["yes, I led that migration end to end at my last job".to_string()]
        );
        // ...and none of the fixed-topic loop ran: no suggestion call, no
        // fixed interview question, and exactly one gap-question call.
        assert_eq!(
            mock.requests()
                .iter()
                .filter(|r| r.system.as_deref() == Some(SUGGEST_PROMPT))
                .count(),
            0,
            "the fixed-topic suggestion flow must not run when there are gaps"
        );
        assert_eq!(
            mock.requests()
                .iter()
                .filter(|r| r.system.as_deref() == Some(QUESTION_PROMPT))
                .count(),
            0,
            "the fixed-topic interview must not run when there are gaps"
        );
        assert_eq!(
            mock.requests()
                .iter()
                .filter(|r| r.system.as_deref() == Some(GAP_QUESTION_PROMPT))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn a_fully_grounded_preliminary_draft_falls_back_to_the_fixed_topic_loop() {
        let mock = MockLlmClient::default();
        // A two-paragraph preliminary draft: one grounded, one exempt -> no
        // gaps at all.
        mock.enqueue(r#"{"paragraphs": ["A grounded paragraph.", "A connective sentence."]}"#);
        mock.enqueue(
            r#"{"paragraphs": [{"status": "grounded", "unbacked": ""}, {"status": "exempt", "unbacked": ""}]}"#,
        );
        // The fixed walk-through then runs exactly as before: angle asks and
        // records, the rest skip.
        mock.enqueue(r#"{"suggestion": ""}"#);
        mock.enqueue(r#"{"question": "Lead with scale, or with reliability?"}"#);
        mock.enqueue(r#"{"question": ""}"#);
        enqueue_skip(&mock, Slot::Emphasis);
        enqueue_skip(&mock, Slot::Tone);
        enqueue_skip(&mock, Slot::Motivation);
        enqueue_skip(&mock, Slot::Constraints);

        let user = ScriptedUser::new();
        user.answer(Answer::Text("".into())); // decline the leading open question
        user.answer(Answer::Text("lead with the reliability angle".into()));

        let brief = run_cover_interview(&sample_resume(), &sample_jd(), &user, &ctx(&mock))
            .await
            .unwrap();

        // The fixed loop ran and recorded the angle...
        assert_eq!(
            brief.angle.as_deref(),
            Some("lead with the reliability angle")
        );
        // ...and the gap-question agent was never called.
        assert_eq!(
            mock.requests()
                .iter()
                .filter(|r| r.system.as_deref() == Some(GAP_QUESTION_PROMPT))
                .count(),
            0,
            "no gaps means no gap questions"
        );
    }

    #[tokio::test]
    async fn gap_answers_are_appended_to_emphasis_after_the_leading_answer() {
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"paragraphs": ["I shipped a fraud-detection service."]}"#);
        mock.enqueue(
            r#"{"paragraphs": [{"status": "unrecorded", "unbacked": "claims fraud-detection work the evidence doesn't mention"}]}"#,
        );
        mock.enqueue(
            r#"{"question": "Did you build a fraud-detection service, and should it stay in the letter?"}"#,
        );

        let user = ScriptedUser::new();
        user.answer(Answer::Text("emphasize my on-call leadership".into())); // leading answer
        user.answer(Answer::Text("yes, I built the fraud service".into())); // gap answer

        let brief = run_cover_interview(&sample_resume(), &sample_jd(), &user, &ctx(&mock))
            .await
            .unwrap();

        // Both the leading answer and the gap answer accumulate in emphasis,
        // in order — the gap answer is appended, never an overwrite, and lands
        // in the same field every free-form answer in this module does.
        assert_eq!(
            brief.emphasis,
            vec![
                "emphasize my on-call leadership".to_string(),
                "yes, I built the fraud service".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn only_max_questions_gap_questions_are_asked_when_there_are_more_gaps() {
        let mock = MockLlmClient::default();
        // Eight digit-free paragraphs (digits would be dropped by the draft's
        // guard against the sample résumé), all flagged -> eight gaps, but only
        // MAX_QUESTIONS of them get asked, in draft order.
        let labels = [
            "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel",
        ];
        let paras: Vec<String> = labels
            .iter()
            .map(|l| format!(r#""An unbacked claim about {l}.""#))
            .collect();
        mock.enqueue(format!(r#"{{"paragraphs": [{}]}}"#, paras.join(", ")));
        let verdicts: Vec<String> = labels
            .iter()
            .map(|l| {
                format!(
                    r#"{{"status": "unrecorded", "unbacked": "claims {l} the evidence lacks"}}"#
                )
            })
            .collect();
        mock.enqueue(format!(r#"{{"paragraphs": [{}]}}"#, verdicts.join(", ")));
        for l in labels.iter().take(MAX_QUESTIONS) {
            mock.enqueue(format!(r#"{{"question": "About {l}?"}}"#));
        }

        let user = ScriptedUser::new();
        user.answer(Answer::Text("".into())); // decline the leading open question
        for l in labels.iter().take(MAX_QUESTIONS) {
            user.answer(Answer::Text(format!("my answer about {l}")));
        }

        let brief = run_cover_interview(&sample_resume(), &sample_jd(), &user, &ctx(&mock))
            .await
            .unwrap();

        // Exactly MAX_QUESTIONS gap questions were asked (the last two gaps
        // never reached the user), and each answer was recorded.
        assert_eq!(
            mock.requests()
                .iter()
                .filter(|r| r.system.as_deref() == Some(GAP_QUESTION_PROMPT))
                .count(),
            MAX_QUESTIONS
        );
        assert_eq!(brief.emphasis.len(), MAX_QUESTIONS);
        // The budget spent the earliest (draft-order) gaps first.
        assert_eq!(brief.emphasis[0], "my answer about alpha");
    }

    #[tokio::test]
    async fn the_preliminary_scratch_draft_never_leaks_into_the_returned_brief() {
        let mock = MockLlmClient::default();
        // The scratch draft overstates a claim in distinctive words...
        mock.enqueue(
            r#"{"paragraphs": ["I architected the Zephyr trading engine single-handedly."]}"#,
        );
        mock.enqueue(
            r#"{"paragraphs": [{"status": "unrecorded", "unbacked": "claims sole authorship of a trading engine the evidence doesn't mention"}]}"#,
        );
        mock.enqueue(
            r#"{"question": "Did you build the Zephyr trading engine, and should the letter keep that?"}"#,
        );

        let user = ScriptedUser::new();
        user.answer(Answer::Text("".into())); // decline the leading open question
        // The candidate corrects the scratch draft's overstatement.
        user.answer(Answer::Text(
            "no, I was one of four engineers on it - please soften that".into(),
        ));

        let brief = run_cover_interview(&sample_resume(), &sample_jd(), &user, &ctx(&mock))
            .await
            .unwrap();

        // Only the candidate's correction is recorded. The scratch draft is
        // never returned or persisted; the caller re-drafts the real letter
        // from this brief, so the overstated wording ("single-handedly") can't
        // reach the final letter unchanged.
        assert_eq!(
            brief.emphasis,
            vec!["no, I was one of four engineers on it - please soften that".to_string()]
        );
        assert!(
            !brief.emphasis.iter().any(|e| e.contains("single-handedly")),
            "the scratch draft's wording must not leak into the brief"
        );
    }
}
