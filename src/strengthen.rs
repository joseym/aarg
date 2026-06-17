//! Bullet strengthening (FR-3.x, the reviewer's "this line is weak" made
//! actionable). The adversarial reviewer flags bullets for *wording*
//! problems — a vague verb, an unsupported claim, generic boilerplate, a
//! missed JD emphasis — but the tailoring loop can't honestly fix those on
//! its own: rephrasing a line stronger than the work history supports is
//! exactly the inflation the never-fabricate rule forbids. A line that
//! reads as "supported the cybersecurity lead" must not become "owned
//! compliance" unless the person actually owned it.
//!
//! So the honest fix has two halves. First, *ask the person* — a leading
//! question per flagged bullet, shaped by what the reviewer objected to —
//! so the new facts come from them, not the model. Then, because a person
//! under interview types facts, not polished resume lines, a second agent
//! *formats their answer* into one crisp bullet, which they refine and
//! approve. That formatting is fenced three ways so it can only rephrase,
//! never inflate:
//!
//! 1. The rewrite agent sees only the user's answer and the original line,
//!    and its prompt forbids adding any fact, number, scope, or ownership
//!    they did not state.
//! 2. The same `digit_runs` guard tailoring and voice use: a rewrite that
//!    introduces a number not present in the answer or the original is
//!    rejected, falling back to the user's own words.
//! 3. The user drives a small accept/revise/keep-mine loop on the rewrite.
//!    They can refine it with feedback, take it, or discard it for their
//!    own wording. The person is always the final gate.
//!
//! Every word that lands therefore traces to the user — the same standard
//! as a verified skill's evidence. A blank answer is a first-class
//! outcome: the line is already as true as it gets, and it is left exactly
//! as it was, the reviewer's objection standing as an honest gap.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::agent::{Agent, AgentContext};
use crate::dataset::types::{BulletId, ResumeDataset};
use crate::llm::LlmError;
use crate::review::ObjectionKind;
use crate::style;
use crate::tailor::within_evidence;
use crate::user::{Answer, AskError, Question, UserHandle};

/// A leading question is one sentence; a single reworded bullet is short.
const REPLY_BUDGET: u32 = 256;

/// Loop caps for one strengthen session, sourced from config so they're
/// tunable. `questions` bounds the interview (one opening question plus
/// follow-ups on thin answers); `revises` bounds the rewrite-revision loop.
/// Both exist only to guarantee termination, so any sane positive value
/// works; `Default` carries the PRD's 3/3.
#[derive(Debug, Clone, Copy)]
pub struct InterviewLimits {
    pub questions: usize,
    pub revises: usize,
}

impl Default for InterviewLimits {
    fn default() -> Self {
        Self {
            questions: 3,
            revises: 3,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StrengthenError {
    #[error(transparent)]
    Llm(#[from] LlmError),

    #[error("the strengthen reply was not the expected JSON (reply began {snippet:?})")]
    BadReply {
        snippet: String,
        #[source]
        source: serde_json::Error,
    },
}

/// One bullet the reviewer wants stronger: the bullet to improve, the kind
/// of weakness, and the reviewer's note (message and/or suggestion) so the
/// question can speak to the actual objection.
#[derive(Debug, Clone)]
pub struct StrengthenTarget {
    pub bullet_id: BulletId,
    pub kind: ObjectionKind,
    pub concern: String,
}

/// Whether an objection kind is one a person can strengthen by restating
/// the truth. `NoMetric` is handled by `metric.rs`; layout and catch-all
/// kinds aren't a wording fix the user can talk their way through.
pub fn is_strengthenable(kind: ObjectionKind) -> bool {
    matches!(
        kind,
        ObjectionKind::VagueVerb
            | ObjectionKind::UnsupportedClaim
            | ObjectionKind::GenericPhrasing
            | ObjectionKind::JdMismatch
    )
}

// ---------------------------------------------------------------------
// Agent 1: ask one leading question
// ---------------------------------------------------------------------

/// One exchange in the interview so far — what was asked and answered.
#[derive(Debug, Clone, Serialize)]
pub struct QnA {
    pub question: String,
    pub answer: String,
}

/// What the question agent needs: the bullet's current text, what kind of
/// weakness the reviewer saw, the reviewer's note, and the conversation so
/// far so it can decide whether to dig deeper or stop.
#[derive(Serialize)]
pub struct StrengthenQuestionInput {
    pub bullet: String,
    pub weakness: String,
    pub concern: String,
    pub transcript: Vec<QnA>,
}

/// Runs a short, adaptive interview: opens with a leading question, then
/// follows up only while the answers are too thin to write a strong,
/// specific bullet — and signals when it has enough.
pub struct StrengthenInterviewAgent;

#[async_trait]
impl Agent for StrengthenInterviewAgent {
    type Input = StrengthenQuestionInput;
    type Wire = RawQuestion;
    type Output = String;
    type Error = StrengthenError;

    fn id(&self) -> &'static str {
        "strengthen_interview_v1"
    }
    fn system_prompt(&self) -> &str {
        QUESTION_PROMPT
    }
    fn reply_budget(&self) -> u32 {
        REPLY_BUDGET
    }
    fn user_message(&self, input: &StrengthenQuestionInput) -> String {
        let mut text = format!(
            "The resume bullet:\n{}\n\nThe reviewer's concern ({}): {}\n\n",
            input.bullet, input.weakness, input.concern
        );
        if input.transcript.is_empty() {
            text.push_str("No questions asked yet. Ask your opening question.");
        } else {
            text.push_str("The conversation so far:\n");
            for qa in &input.transcript {
                text.push_str(&format!("Q: {}\nA: {}\n", qa.question, qa.answer));
            }
            text.push_str(
                "\nIf you now have enough specific detail to write one strong, truthful bullet, \
                 reply with an empty question. Otherwise ask the next one.",
            );
        }
        text
    }
    fn bad_reply(&self, snippet: String, source: serde_json::Error) -> StrengthenError {
        StrengthenError::BadReply { snippet, source }
    }
    fn assemble(
        &self,
        wire: RawQuestion,
        _input: StrengthenQuestionInput,
    ) -> Result<String, StrengthenError> {
        Ok(wire.question)
    }
}

const QUESTION_PROMPT: &str = r#"You interview a job candidate to strengthen one weak resume bullet that a skeptical reviewer flagged. You ask one short question at a time and read their answers, drawing out the real facts behind the SAME accomplishment — the precise thing they did, the real scope, the actual ownership.

How to run the interview:
- Open with your single best question about this bullet's specifics, speaking to the reviewer's concern.
- After each answer, judge whether you now have enough concrete detail to write one strong, specific, truthful bullet. If yes, STOP by replying with an empty question (""). If the answer was vague, generic, or skipped the key specific, ask ONE focused follow-up for exactly what's missing — don't re-ask what they already told you.

Rules that always hold:
- NEVER propose, supply, or imply the answer — not a verb, not a claim, not a level of ownership. "Did you own this end to end, or support someone who did?" is good; "You owned this, right?" is forbidden.
- Never lead them to overstate. "I only supported it" is a perfectly good answer; your questions must leave that door fully open, never push them past the truth to sound more impressive.
- One question, one sentence, warm and concrete.

Reply with exactly one JSON object and nothing else — no markdown fences. Use an empty string for the question when you have enough:
{"question": "your next question, or empty string if done"}"#;

#[derive(Debug, Deserialize)]
pub struct RawQuestion {
    #[serde(default)]
    question: String,
}

// ---------------------------------------------------------------------
// Agent 2: format the user's facts into one crisp bullet
// ---------------------------------------------------------------------

/// What the rewrite agent needs: the original line, the user's answer, and
/// any revision notes from earlier rounds. It may use facts from the answer
/// or the original, and nothing else.
#[derive(Serialize)]
pub struct StrengthenRewriteInput {
    pub original: String,
    pub answer: String,
    pub notes: Vec<String>,
}

/// Formats the user's typed facts into a single, resume-grade bullet —
/// rephrasing only, never adding to the claim.
pub struct StrengthenRewriteAgent;

#[async_trait]
impl Agent for StrengthenRewriteAgent {
    type Input = StrengthenRewriteInput;
    type Wire = RawRewrite;
    type Output = String;
    type Error = StrengthenError;

    fn id(&self) -> &'static str {
        "strengthen_rewrite_v1"
    }
    fn system_prompt(&self) -> &str {
        REWRITE_PROMPT
    }
    fn reply_budget(&self) -> u32 {
        REPLY_BUDGET
    }
    fn user_message(&self, input: &StrengthenRewriteInput) -> String {
        let mut text = format!(
            "Original bullet:\n{}\n\nWhat the candidate said actually happened:\n{}\n\n",
            input.original, input.answer
        );
        if !input.notes.is_empty() {
            text.push_str("The candidate asked you to revise your previous attempt:\n");
            for note in &input.notes {
                text.push_str(&format!("- {note}\n"));
            }
            text.push('\n');
        }
        text.push_str(
            "Write the single strongest resume bullet that says only what they told you.",
        );
        text
    }
    fn bad_reply(&self, snippet: String, source: serde_json::Error) -> StrengthenError {
        StrengthenError::BadReply { snippet, source }
    }
    fn assemble(
        &self,
        wire: RawRewrite,
        _input: StrengthenRewriteInput,
    ) -> Result<String, StrengthenError> {
        Ok(wire.bullet)
    }
}

const REWRITE_PROMPT: &str = r#"You turn a job candidate's own words about one accomplishment into a single, polished resume bullet. They are good at the work and bad at phrasing it; your job is to phrase it well — NOT to make it bigger.

Write ONE bullet that:
- Leads with a strong, precise verb and states the concrete accomplishment.
- Uses ONLY facts present in what the candidate said or in the original bullet. Add nothing — no number, no percentage, no scope, no team size, no level of ownership they did not state. If they said "helped with" or "supported", keep it a contribution; do not promote it to "led" or "owned".
- Stays in plain, credible language they could defend in an interview. No buzzwords, no inflation, no invented metrics.
- Uses no em-dashes ("—") — they read as machine-written. Join clauses with a comma, with "and", or as a second sentence; a colon is fine where it fits.

If they gave revision notes, follow them — but the no-inflation rules above still bind, even if the note asks you to make the claim bigger.

If their answer is already a strong line, return it nearly unchanged.

Reply with exactly one JSON object and nothing else — no markdown fences:
{"bullet": "your single bullet here"}"#;

#[derive(Debug, Deserialize)]
pub struct RawRewrite {
    #[serde(default)]
    bullet: String,
}

// ---------------------------------------------------------------------
// Agent 3: suggest a stronger line from the role's recorded experience
// ---------------------------------------------------------------------

/// The user's recorded experience for ONE role — the only material a suggested
/// rewrite may draw on. Scoped to a single role on purpose: a suggestion can
/// recombine the candidate's own recorded facts, but it must never attribute
/// one job's work to another, so other roles are never in view.
#[derive(Debug, Clone, Serialize)]
pub struct RoleEvidence {
    pub title: String,
    pub company: String,
    pub context: Option<String>,
    /// The role's other bullets (the flagged one excluded), each with its
    /// recorded metric folded in.
    pub other_bullets: Vec<String>,
    /// Canonical names of skills attached to the role or the flagged bullet.
    pub skills: Vec<String>,
}

impl RoleEvidence {
    /// Every recorded string a suggestion is allowed to draw a number from, so
    /// the digit guard knows which figures are the candidate's own.
    fn texts(&self) -> Vec<&str> {
        let mut texts = vec![self.title.as_str(), self.company.as_str()];
        if let Some(context) = &self.context {
            texts.push(context);
        }
        texts.extend(self.other_bullets.iter().map(String::as_str));
        texts.extend(self.skills.iter().map(String::as_str));
        texts
    }
}

/// What the suggestion agent needs: the weak bullet, the reviewer's concern,
/// the role's recorded evidence to draw on, and any revision notes from a
/// previous "tweak it" round.
#[derive(Serialize)]
pub struct StrengthenSuggestInput {
    pub bullet: String,
    pub concern: String,
    pub evidence: RoleEvidence,
    pub notes: Vec<String>,
}

/// Drafts a stronger version of a weak bullet as a *starting point*, grounded
/// only in what the candidate has already recorded for that role. It proposes;
/// the user disposes (accept / tweak / own words / skip), and the digit guard
/// plus the prompt's no-new-facts rule keep it from inventing experience.
pub struct StrengthenSuggestAgent;

#[async_trait]
impl Agent for StrengthenSuggestAgent {
    type Input = StrengthenSuggestInput;
    type Wire = RawSuggestion;
    type Output = String;
    type Error = StrengthenError;

    fn id(&self) -> &'static str {
        "strengthen_suggest_v1"
    }
    fn system_prompt(&self) -> &str {
        SUGGEST_PROMPT
    }
    fn reply_budget(&self) -> u32 {
        REPLY_BUDGET
    }
    fn user_message(&self, input: &StrengthenSuggestInput) -> String {
        let mut text = format!(
            "The weak resume bullet:\n{}\n\nThe reviewer's concern: {}\n\nThe role: {} at {}\n",
            input.bullet, input.concern, input.evidence.title, input.evidence.company
        );
        if let Some(context) = &input.evidence.context {
            text.push_str(&format!("Role context: {context}\n"));
        }
        if !input.evidence.other_bullets.is_empty() {
            text.push_str(
                "\nOther accomplishments recorded in THIS role (facts you may draw on):\n",
            );
            for bullet in &input.evidence.other_bullets {
                text.push_str(&format!("- {bullet}\n"));
            }
        }
        if !input.evidence.skills.is_empty() {
            text.push_str(&format!(
                "\nSkills recorded for this role: {}\n",
                input.evidence.skills.join(", ")
            ));
        }
        if !input.notes.is_empty() {
            text.push_str("\nThe candidate asked you to revise your previous suggestion:\n");
            for note in &input.notes {
                text.push_str(&format!("- {note}\n"));
            }
        }
        text.push_str(
            "\nPropose one stronger version of the weak bullet, using only the facts above.",
        );
        text
    }
    fn bad_reply(&self, snippet: String, source: serde_json::Error) -> StrengthenError {
        StrengthenError::BadReply { snippet, source }
    }
    fn assemble(
        &self,
        wire: RawSuggestion,
        _input: StrengthenSuggestInput,
    ) -> Result<String, StrengthenError> {
        Ok(wire.suggestion)
    }
}

const SUGGEST_PROMPT: &str = r#"You help a job candidate strengthen one weak resume bullet a skeptical reviewer flagged. You are given the weak bullet, the reviewer's concern, and the candidate's OTHER recorded accomplishments and skills from the SAME role. You draft one stronger rewrite as a starting point the candidate will review, edit, or reject.

Write ONE bullet that:
- Strengthens the wording of the SAME accomplishment the weak bullet describes. Lead with a precise verb and state the concrete result.
- Uses ONLY facts present in the weak bullet or the role evidence provided. Add nothing the candidate has not recorded — no number, percentage, scope, team size, technology, employer, or level of ownership that isn't there. You may pull in a detail from another bullet in this role only when it genuinely sharpens THIS accomplishment; never staple in unrelated work.
- Never inflates. If the candidate "supported" something, keep it support; do not promote it to "led" or "owned". If the underlying work is genuinely thin, leave the line modest. A weak-but-true line beats an impressive-but-false one, which is the one thing worse than a weak resume.
- Stays in plain, credible language the candidate could defend in an interview. No buzzwords.
- Uses no em-dashes ("—"). Join clauses with a comma, with "and", or as a second sentence; a colon is fine where it fits.

If you cannot strengthen the line honestly from the facts given, reply with an empty string rather than reaching for anything invented.
If the candidate gave revision notes, follow them, but every rule above still binds even if a note asks you to make the claim bigger.

Reply with exactly one JSON object and nothing else — no markdown fences:
{"suggestion": "your single bullet, or an empty string if you cannot do it honestly"}"#;

#[derive(Debug, Deserialize)]
pub struct RawSuggestion {
    #[serde(default)]
    suggestion: String,
}

// ---------------------------------------------------------------------
// The interview loop
// ---------------------------------------------------------------------

/// Interview the user about every flagged bullet, format each answer into a
/// stronger line, and fold the confirmed result into the dataset. Returns
/// how many bullets the user rewrote. A phantom id is skipped; a bullet
/// flagged more than once is asked about once; an agent that can't be
/// reached degrades rather than aborting. Mutates only the in-memory
/// dataset — the caller saves on success.
pub async fn strengthen_bullets(
    dataset: &mut ResumeDataset,
    targets: &[StrengthenTarget],
    user: &dyn UserHandle,
    ctx: &AgentContext<'_>,
    limits: InterviewLimits,
) -> Result<usize, AskError> {
    let mut changed = 0;
    let mut seen: Vec<BulletId> = Vec::new();
    for target in targets {
        if seen.contains(&target.bullet_id) {
            continue; // the reviewer flagged the same line more than once
        }
        seen.push(target.bullet_id.clone());

        let Some((role_label, text)) = bullet_context(dataset, &target.bullet_id) else {
            continue; // a phantom id the reviewer invented
        };

        // Anchor the user once: which role, the exact line, and what the
        // reviewer didn't like, so there's no guessing which job or why. A
        // bold role header with two labeled lines below reads as one block
        // instead of a wall of same-weight text.
        // Three distinct body colors so the lines don't bleed: gray for the
        // reference bullet (recedes), yellow for the reviewer's concern (its
        // warn semantics), and inquire's default white for the question below
        // (the action). Each line is also labelled, so the distinction never
        // rests on color alone — a red/green-colorblind reader reads the
        // labels, and the named colors follow the terminal's own theme.
        user.notify(&format!(
            "\n{}\n  {} {}\n  {} {}",
            style::bold(&role_label),
            style::bold("current:"),
            style::dim(format!("\"{text}\"")),
            style::bold("reviewer:"),
            style::yellow(&target.concern),
        ));

        // Offer an evidence-grounded suggestion as a starting point first, when
        // this role has recorded material to ground one in. The user can take
        // it, tweak it, answer in their own words (the interview below), or
        // skip. A failed guard or an unreachable agent yields no suggestion,
        // and the interview runs exactly as it always has.
        if let Some(evidence) = role_evidence(dataset, &target.bullet_id) {
            match suggest_flow(ctx, &text, &target.concern, &evidence, user, limits.revises).await?
            {
                Some(SuggestOutcome::Accepted(line)) => {
                    replace_bullet_text(dataset, &target.bullet_id, &line);
                    changed += 1;
                    continue;
                }
                Some(SuggestOutcome::Skip) => continue, // an honest gap, left as is
                // Own words or no honest suggestion: fall through to the interview.
                Some(SuggestOutcome::OwnWords) | None => {}
            }
        }

        // A short adaptive interview: the agent asks a follow-up whenever an
        // answer is too thin to write a strong bullet from, and stops (empty
        // question) once it has enough — capped so it never interrogates
        // forever. A blank answer ends the interview early.
        let mut transcript: Vec<QnA> = Vec::new();
        for turn in 0..limits.questions {
            let question = match StrengthenInterviewAgent
                .run(
                    ctx,
                    StrengthenQuestionInput {
                        bullet: text.clone(),
                        weakness: weakness_label(target.kind).to_string(),
                        concern: target.concern.clone(),
                        transcript: transcript.clone(),
                    },
                )
                .await
            {
                Ok(run) => run.output.trim().to_string(),
                // On failure, ask one generic opening; if even that can't be
                // had, give up on this bullet rather than loop on errors.
                Err(_) if turn == 0 => "What actually happened here? What did you do, \
                                        and how far did your ownership go?"
                    .to_string(),
                Err(_) => break,
            };
            if question.is_empty() {
                break; // the interviewer has enough
            }
            match user
                .ask(Question::Text {
                    prompt: question.clone(),
                })
                .await?
            {
                Answer::Text(t) if !t.trim().is_empty() => transcript.push(QnA {
                    question,
                    answer: t.trim().to_string(),
                }),
                _ => break, // blank = the user is done elaborating
            }
        }

        // No answers at all = an honest gap; leave the line exactly as is.
        let facts = transcript
            .iter()
            .map(|qa| qa.answer.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        if facts.trim().is_empty() {
            continue;
        }

        // Format their facts into a crisp line they refine and approve.
        let final_text = polish(ctx, &text, &facts, user, limits.revises).await?;
        replace_bullet_text(dataset, &target.bullet_id, &final_text);
        changed += 1;
    }
    Ok(changed)
}

/// Drive the accept/revise/keep-mine loop over the rewrite. Returns the
/// text to record: the agent's rewrite if the user takes it, or their own
/// words if they keep them or the rewrite can't be produced safely. The
/// user's words are the safe floor — never lost.
async fn polish(
    ctx: &AgentContext<'_>,
    original: &str,
    answer: &str,
    user: &dyn UserHandle,
    max_revises: usize,
) -> Result<String, AskError> {
    let mut notes: Vec<String> = Vec::new();
    // First rewrite. If it can't be produced or trips the digit guard, the
    // user's own words stand — no offer, nothing invented.
    let Some(mut rewrite) = run_rewrite(ctx, original, answer, &notes).await else {
        return Ok(answer.to_string());
    };

    let mut revises_left = max_revises;
    loop {
        // If the rewrite is just their words back, there's nothing to weigh.
        if rewrite == answer {
            return Ok(answer.to_string());
        }

        // Build the choice. "Revise it" drops out once the budget is spent,
        // which guarantees the loop terminates.
        let mut options = vec!["Use this wording".to_string()];
        if revises_left > 0 {
            options.push("Revise it".to_string());
        }
        options.push("Keep my own wording".to_string());

        let choice = match user
            .ask(Question::Select {
                prompt: format!("stronger wording:\n  \"{rewrite}\""),
                options: options.clone(),
            })
            .await?
        {
            Answer::Choice(i) => options.get(i).map(String::as_str),
            _ => Some("Use this wording"), // unexpected shape; rewrite is guard-clean
        };

        match choice {
            Some("Use this wording") => return Ok(rewrite),
            Some("Keep my own wording") => return Ok(answer.to_string()),
            Some("Revise it") => {
                let note = match user
                    .ask(Question::Text {
                        prompt: "what should change?".to_string(),
                    })
                    .await?
                {
                    Answer::Text(t) if !t.trim().is_empty() => t.trim().to_string(),
                    _ => continue, // no guidance given — re-show the same rewrite
                };
                notes.push(note);
                revises_left -= 1;
                // Keep the prior rewrite if the new one fails or trips the
                // guard — never regress to something unsafe.
                if let Some(next) = run_rewrite(ctx, original, answer, &notes).await {
                    rewrite = next;
                }
            }
            _ => return Ok(answer.to_string()),
        }
    }
}

/// One rewrite attempt, fully guarded. Returns the polished bullet only if
/// the agent produced a non-empty line that introduces no number absent
/// from the user's answer or the original. Otherwise `None`, and the caller
/// falls back to the user's words.
async fn run_rewrite(
    ctx: &AgentContext<'_>,
    original: &str,
    answer: &str,
    notes: &[String],
) -> Option<String> {
    let run = StrengthenRewriteAgent
        .run(
            ctx,
            StrengthenRewriteInput {
                original: original.to_string(),
                answer: answer.to_string(),
                notes: notes.to_vec(),
            },
        )
        .await
        .ok()?;
    let rewrite = run.output.trim().to_string();
    if rewrite.is_empty() {
        return None;
    }
    // The rewrite may rephrase, never introduce a number the facts don't
    // support. Allowed evidence = the user's answer and the original line.
    if within_evidence(&rewrite, &[answer, original]) {
        Some(rewrite)
    } else {
        None
    }
}

/// What the user chose to do with a suggested rewrite.
enum SuggestOutcome {
    /// Take this line (already guard-clean) and record it.
    Accepted(String),
    /// Decline the suggestion and answer the interview in their own words.
    OwnWords,
    /// Leave the bullet exactly as it is.
    Skip,
}

/// Offer the evidence-grounded suggestion (when one survives the guard) and let
/// the user accept it, tweak it, switch to their own words, or skip the line.
/// Returns `None` when no honest suggestion could be produced, so the caller
/// falls through to the interview. Mirrors `polish`'s accept/revise loop, but
/// seeded from the dataset rather than the user's typed answer — and the user
/// is still the final gate on every word that lands.
async fn suggest_flow(
    ctx: &AgentContext<'_>,
    bullet: &str,
    concern: &str,
    evidence: &RoleEvidence,
    user: &dyn UserHandle,
    max_revises: usize,
) -> Result<Option<SuggestOutcome>, AskError> {
    let mut notes: Vec<String> = Vec::new();
    let Some(mut suggestion) = run_suggest(ctx, bullet, concern, evidence, &notes).await else {
        return Ok(None); // no honest suggestion; the caller uses the interview
    };

    let mut revises_left = max_revises;
    loop {
        let mut options = vec!["Use this wording".to_string()];
        if revises_left > 0 {
            options.push("Tweak it".to_string());
        }
        options.push("Answer in my own words".to_string());
        options.push("Skip this one".to_string());

        let choice = match user
            .ask(Question::Select {
                prompt: format!("suggested rewrite:\n  \"{suggestion}\""),
                options: options.clone(),
            })
            .await?
        {
            Answer::Choice(i) => options.get(i).map(String::as_str),
            _ => Some("Answer in my own words"), // unexpected shape; defer to the user
        };

        match choice {
            Some("Use this wording") => return Ok(Some(SuggestOutcome::Accepted(suggestion))),
            Some("Answer in my own words") => return Ok(Some(SuggestOutcome::OwnWords)),
            Some("Skip this one") => return Ok(Some(SuggestOutcome::Skip)),
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
                // Keep the prior suggestion if the new one fails or trips the
                // guard, so a tweak never regresses to something unsafe.
                if let Some(next) = run_suggest(ctx, bullet, concern, evidence, &notes).await {
                    suggestion = next;
                }
            }
            _ => return Ok(Some(SuggestOutcome::OwnWords)),
        }
    }
}

/// One suggestion attempt, fully guarded. Returns the candidate line only if
/// the agent produced a non-empty rewrite that differs from the original and
/// introduces no number absent from the bullet or the role evidence it may
/// draw on. Otherwise `None`, and the caller offers no suggestion.
async fn run_suggest(
    ctx: &AgentContext<'_>,
    bullet: &str,
    concern: &str,
    evidence: &RoleEvidence,
    notes: &[String],
) -> Option<String> {
    let run = StrengthenSuggestAgent
        .run(
            ctx,
            StrengthenSuggestInput {
                bullet: bullet.to_string(),
                concern: concern.to_string(),
                evidence: evidence.clone(),
                notes: notes.to_vec(),
            },
        )
        .await
        .ok()?;
    let suggestion = run.output.trim().to_string();
    if suggestion.is_empty() || suggestion == bullet {
        return None; // nothing to offer, or just the same line handed back
    }
    // A suggestion may recombine the role's recorded facts, never mint a number
    // absent from the bullet or that evidence.
    let mut allowed = evidence.texts();
    allowed.push(bullet);
    if within_evidence(&suggestion, &allowed) {
        Some(suggestion)
    } else {
        None
    }
}

/// A short human label for an objection kind, used in the question prompt.
fn weakness_label(kind: ObjectionKind) -> &'static str {
    match kind {
        ObjectionKind::VagueVerb => "vague verb",
        ObjectionKind::UnsupportedClaim => "unsupported claim",
        ObjectionKind::GenericPhrasing => "generic phrasing",
        ObjectionKind::JdMismatch => "misses what the job emphasizes",
        // Not strengthenable — never reached via `is_strengthenable`, but
        // the match must be total.
        ObjectionKind::NoMetric => "missing a number",
        ObjectionKind::LayoutDense => "too dense",
        ObjectionKind::Other => "flagged",
    }
}

/// The role label ("title at company") and text of the bullet with this
/// id, if it exists. The label is what tells the user *which job* the
/// flagged line is from.
fn bullet_context(dataset: &ResumeDataset, id: &BulletId) -> Option<(String, String)> {
    for role in &dataset.roles {
        if let Some(bullet) = role.bullets.iter().find(|b| b.id == *id) {
            return Some((
                format!("{} at {}", role.title, role.company),
                bullet.text.clone(),
            ));
        }
    }
    None
}

/// Gather the recorded experience for the role that owns this bullet — the
/// only material a suggestion may draw on. Returns `None` when the bullet is
/// unknown *or* when the role has nothing recorded beyond the flagged line
/// itself (no other bullets, no skills): there's nothing to ground a stronger
/// version in, so the interview is the right path. Scoped to the one role on
/// purpose, so a suggestion can never borrow another job's work.
fn role_evidence(dataset: &ResumeDataset, id: &BulletId) -> Option<RoleEvidence> {
    let role = dataset
        .roles
        .iter()
        .find(|r| r.bullets.iter().any(|b| b.id == *id))?;

    // The role's other bullets, each with its recorded metric folded in.
    let other_bullets: Vec<String> = role
        .bullets
        .iter()
        .filter(|b| b.id != *id)
        .map(|b| match &b.metric {
            Some(metric) => format!("{} ({})", b.text, metric.0),
            None => b.text.clone(),
        })
        .collect();

    // Skills attached to the role or the flagged bullet, de-duplicated by name.
    let bullet_skill_ids = role
        .bullets
        .iter()
        .find(|b| b.id == *id)
        .map(|b| b.skill_ids.as_slice())
        .unwrap_or(&[]);
    let mut skills: Vec<String> = Vec::new();
    for sid in role.skill_ids.iter().chain(bullet_skill_ids.iter()) {
        if let Some(skill) = dataset.skills.skills.iter().find(|s| s.id == *sid)
            && !skills.contains(&skill.canonical_name)
        {
            skills.push(skill.canonical_name.clone());
        }
    }

    // No recorded material beyond the flagged line: nothing to ground on.
    if other_bullets.is_empty() && skills.is_empty() {
        return None;
    }

    Some(RoleEvidence {
        title: role.title.clone(),
        company: role.company.clone(),
        context: role.context.clone(),
        other_bullets,
        skills,
    })
}

/// Replace the bullet's text with the strengthened line. The `metric` field
/// and every other part of the bullet are left untouched.
fn replace_bullet_text(dataset: &mut ResumeDataset, id: &BulletId, text: &str) {
    for role in &mut dataset.roles {
        for bullet in &mut role.bullets {
            if bullet.id == *id {
                bullet.text = text.to_string();
                return;
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::dataset::types::{
        Bullet, Contact, EmploymentType, Metric, Role, RoleId, Strength, YearMonth,
    };
    use crate::llm::MockLlmClient;
    use crate::trace::Tracer;
    use crate::user::ScriptedUser;

    fn dataset_with_bullet(id: &str, text: &str) -> ResumeDataset {
        let mut d = ResumeDataset::new(Contact {
            full_name: "Ada".into(),
            email: "ada@example.com".into(),
            phone: None,
            location: None,
            links: Vec::new(),
        });
        d.roles.push(Role {
            id: RoleId("role-1".into()),
            company: "Acme".into(),
            title: "Director".into(),
            start: YearMonth {
                year: 2020,
                month: 1,
            },
            end: None,
            location: None,
            employment_type: EmploymentType::FullTime,
            bullets: vec![Bullet {
                id: BulletId(id.into()),
                text: text.into(),
                skill_ids: Vec::new(),
                metric: None,
                theme: Vec::new(),
                strength: Strength::Medium,
                variants: Vec::new(),
            }],
            skill_ids: Vec::new(),
            context: None,
        });
        d
    }

    fn ctx<'a>(mock: &'a MockLlmClient) -> AgentContext<'a> {
        AgentContext {
            llm: mock,
            model: &"m",
            tracer: &Tracer::DISABLED,
            sink: None,
        }
    }

    fn target(id: &str, kind: ObjectionKind) -> StrengthenTarget {
        StrengthenTarget {
            bullet_id: BulletId(id.into()),
            kind,
            concern: "reads as supporting cast, not owner".into(),
        }
    }

    fn base_dataset() -> ResumeDataset {
        ResumeDataset::new(Contact {
            full_name: "Ada".into(),
            email: "ada@example.com".into(),
            phone: None,
            location: None,
            links: Vec::new(),
        })
    }

    /// A role carrying several `(bullet_id, text)` bullets — enough recorded
    /// material for `role_evidence` to ground a suggestion in.
    fn role_with_bullets(
        role_id: &str,
        title: &str,
        company: &str,
        bullets: &[(&str, &str)],
    ) -> Role {
        Role {
            id: RoleId(role_id.into()),
            company: company.into(),
            title: title.into(),
            start: YearMonth {
                year: 2020,
                month: 1,
            },
            end: None,
            location: None,
            employment_type: EmploymentType::FullTime,
            bullets: bullets
                .iter()
                .map(|(id, text)| Bullet {
                    id: BulletId((*id).into()),
                    text: (*text).into(),
                    skill_ids: Vec::new(),
                    metric: None,
                    theme: Vec::new(),
                    strength: Strength::Medium,
                    variants: Vec::new(),
                })
                .collect(),
            skill_ids: Vec::new(),
            context: None,
        }
    }

    /// One role with the flagged bullet plus one other bullet — the minimal
    /// dataset that yields `RoleEvidence` (a single lone bullet would not).
    fn dataset_with_two_bullets(flagged: (&str, &str), other: (&str, &str)) -> ResumeDataset {
        let mut d = base_dataset();
        d.roles.push(role_with_bullets(
            "role-1",
            "Director",
            "Acme",
            &[flagged, other],
        ));
        d
    }

    #[tokio::test]
    async fn an_accepted_rewrite_uses_the_polished_line() {
        let mut dataset = dataset_with_bullet("bullet-1", "Worked with the security lead");
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"question": "Did you own this end to end?"}"#);
        mock.enqueue(r#"{"question": ""}"#); // interviewer is satisfied
        mock.enqueue(r#"{"bullet": "Owned engineering audit readiness end to end"}"#);
        let user = ScriptedUser::new();
        user.answer(Answer::Text(
            "i basically ran the whole audit readiness thing".into(),
        ));
        user.answer(Answer::Choice(0)); // "Use this wording"

        let targets = [target("bullet-1", ObjectionKind::VagueVerb)];
        let changed = strengthen_bullets(
            &mut dataset,
            &targets,
            &user,
            &ctx(&mock),
            InterviewLimits::default(),
        )
        .await
        .unwrap();

        assert_eq!(changed, 1);
        assert_eq!(
            dataset.roles[0].bullets[0].text,
            "Owned engineering audit readiness end to end"
        );
    }

    #[tokio::test]
    async fn revising_refines_the_line_then_accepts() {
        let mut dataset = dataset_with_bullet("bullet-1", "Worked with the security lead");
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"question": "What did you do?"}"#);
        mock.enqueue(r#"{"question": ""}"#); // satisfied after one answer
        mock.enqueue(r#"{"bullet": "Drove change management with the security lead"}"#);
        mock.enqueue(r#"{"bullet": "Partnered with the security lead on change management"}"#);
        let user = ScriptedUser::new();
        user.answer(Answer::Text(
            "worked alongside the security lead on change mgmt".into(),
        ));
        user.answer(Answer::Choice(1)); // "Revise it"
        user.answer(Answer::Text(
            "don't say 'drove' — it was a partnership".into(),
        ));
        user.answer(Answer::Choice(0)); // "Use this wording" (the revised line)

        let targets = [target("bullet-1", ObjectionKind::VagueVerb)];
        strengthen_bullets(
            &mut dataset,
            &targets,
            &user,
            &ctx(&mock),
            InterviewLimits::default(),
        )
        .await
        .unwrap();

        assert_eq!(
            dataset.roles[0].bullets[0].text,
            "Partnered with the security lead on change management"
        );
    }

    #[tokio::test]
    async fn keeping_my_wording_uses_the_users_own_words() {
        let mut dataset = dataset_with_bullet("bullet-1", "Worked with the security lead");
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"question": "What did you do?"}"#);
        mock.enqueue(r#"{"question": ""}"#); // satisfied after one answer
        mock.enqueue(r#"{"bullet": "Owned the entire security program"}"#);
        let user = ScriptedUser::new();
        user.answer(Answer::Text(
            "supported the security lead on change management".into(),
        ));
        user.answer(Answer::Choice(2)); // "Keep my own wording" (Use / Revise / Keep)

        let targets = [target("bullet-1", ObjectionKind::VagueVerb)];
        strengthen_bullets(
            &mut dataset,
            &targets,
            &user,
            &ctx(&mock),
            InterviewLimits::default(),
        )
        .await
        .unwrap();

        assert_eq!(
            dataset.roles[0].bullets[0].text,
            "supported the security lead on change management"
        );
    }

    #[tokio::test]
    async fn a_rewrite_that_invents_a_number_is_rejected_without_asking() {
        let mut dataset = dataset_with_bullet("bullet-1", "Reduced onboarding time");
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"question": "By how much, and how?"}"#);
        mock.enqueue(r#"{"question": ""}"#); // satisfied after one answer
        // The user gave no number; the model invents "by 80%".
        mock.enqueue(r#"{"bullet": "Cut onboarding time by 80% with a new pipeline"}"#);
        let user = ScriptedUser::new();
        user.answer(Answer::Text(
            "built a setup script so new devs start faster".into(),
        ));
        // No Select answer queued: the guard reverts before any choice is
        // offered. If the code asked, ScriptedUser would error.

        let targets = [target("bullet-1", ObjectionKind::GenericPhrasing)];
        strengthen_bullets(
            &mut dataset,
            &targets,
            &user,
            &ctx(&mock),
            InterviewLimits::default(),
        )
        .await
        .unwrap();

        assert_eq!(
            dataset.roles[0].bullets[0].text,
            "built a setup script so new devs start faster"
        );
    }

    #[tokio::test]
    async fn a_blank_answer_leaves_an_honest_gap_untouched() {
        let mut dataset = dataset_with_bullet("bullet-1", "Worked with the security lead");
        let before = dataset.clone();
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"question": "Did you own it or support it?"}"#);
        let user = ScriptedUser::new();
        user.answer(Answer::Text("   ".into())); // only supported it — leave honest

        let targets = [target("bullet-1", ObjectionKind::VagueVerb)];
        let changed = strengthen_bullets(
            &mut dataset,
            &targets,
            &user,
            &ctx(&mock),
            InterviewLimits::default(),
        )
        .await
        .unwrap();

        assert_eq!(changed, 0);
        assert_eq!(dataset, before);
    }

    #[tokio::test]
    async fn replacing_text_preserves_the_metric_field() {
        let mut dataset = dataset_with_bullet("bullet-1", "Did the thing");
        dataset.roles[0].bullets[0].metric = Some(Metric("20% faster".into()));
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"question": "What precisely did you build?"}"#);
        mock.enqueue(r#"{"question": ""}"#); // satisfied after one answer
        mock.enqueue(r#"{"bullet": "Built the deploy pipeline"}"#);
        let user = ScriptedUser::new();
        user.answer(Answer::Text("built the deploy pipeline".into()));
        user.answer(Answer::Choice(0)); // Use this wording

        let targets = [target("bullet-1", ObjectionKind::GenericPhrasing)];
        strengthen_bullets(
            &mut dataset,
            &targets,
            &user,
            &ctx(&mock),
            InterviewLimits::default(),
        )
        .await
        .unwrap();

        assert_eq!(
            dataset.roles[0].bullets[0].text,
            "Built the deploy pipeline"
        );
        assert_eq!(
            dataset.roles[0].bullets[0].metric,
            Some(Metric("20% faster".into()))
        );
    }

    #[tokio::test]
    async fn a_phantom_bullet_id_is_skipped_not_an_error() {
        let mut dataset = dataset_with_bullet("bullet-1", "Did things");
        let mock = MockLlmClient::default();
        let user = ScriptedUser::new();

        let targets = [target("bullet-99", ObjectionKind::VagueVerb)];
        let changed = strengthen_bullets(
            &mut dataset,
            &targets,
            &user,
            &ctx(&mock),
            InterviewLimits::default(),
        )
        .await
        .unwrap();

        assert_eq!(changed, 0);
        assert!(mock.requests().is_empty());
    }

    #[tokio::test]
    async fn a_thin_answer_draws_a_follow_up_before_rewriting() {
        let mut dataset = dataset_with_bullet("bullet-1", "Helped on the deploy work");
        let mock = MockLlmClient::default();
        // Opening question, then a follow-up because the first answer is
        // thin, then the interviewer is satisfied, then the rewrite.
        mock.enqueue(r#"{"question": "What did you do on the deploy work?"}"#);
        mock.enqueue(r#"{"question": "What changed as a result?"}"#);
        mock.enqueue(r#"{"question": ""}"#);
        mock.enqueue(r#"{"bullet": "Built the deploy pipeline that made releases routine"}"#);
        let user = ScriptedUser::new();
        user.answer(Answer::Text("i helped out".into())); // thin -> follow-up
        user.answer(Answer::Text(
            "built the pipeline, releases got routine".into(),
        ));
        user.answer(Answer::Choice(0)); // Use this wording

        let targets = [target("bullet-1", ObjectionKind::GenericPhrasing)];
        let changed = strengthen_bullets(
            &mut dataset,
            &targets,
            &user,
            &ctx(&mock),
            InterviewLimits::default(),
        )
        .await
        .unwrap();

        assert_eq!(changed, 1);
        assert_eq!(
            dataset.roles[0].bullets[0].text,
            "Built the deploy pipeline that made releases routine"
        );
        // Three interview turns (open, follow-up, "done") + one rewrite.
        assert_eq!(mock.requests().len(), 4);
    }

    #[test]
    fn only_wording_kinds_are_strengthenable() {
        assert!(is_strengthenable(ObjectionKind::VagueVerb));
        assert!(is_strengthenable(ObjectionKind::UnsupportedClaim));
        assert!(is_strengthenable(ObjectionKind::GenericPhrasing));
        assert!(is_strengthenable(ObjectionKind::JdMismatch));
        assert!(!is_strengthenable(ObjectionKind::NoMetric));
        assert!(!is_strengthenable(ObjectionKind::LayoutDense));
        assert!(!is_strengthenable(ObjectionKind::Other));
    }

    #[tokio::test]
    async fn a_grounded_suggestion_can_be_accepted() {
        let mut dataset = dataset_with_two_bullets(
            ("bullet-1", "Helped on incident response"),
            ("bullet-2", "Built the on-call rotation for the team"),
        );
        let mock = MockLlmClient::default();
        // The suggest agent runs first, before any interview question.
        mock.enqueue(r#"{"suggestion": "Drove incident response across the on-call rotation"}"#);
        let user = ScriptedUser::new();
        user.answer(Answer::Choice(0)); // "Use this wording"

        let targets = [target("bullet-1", ObjectionKind::VagueVerb)];
        let changed = strengthen_bullets(
            &mut dataset,
            &targets,
            &user,
            &ctx(&mock),
            InterviewLimits::default(),
        )
        .await
        .unwrap();

        assert_eq!(changed, 1);
        assert_eq!(
            dataset.roles[0].bullets[0].text,
            "Drove incident response across the on-call rotation"
        );
    }

    #[tokio::test]
    async fn answering_in_own_words_falls_through_to_the_interview() {
        let mut dataset = dataset_with_two_bullets(
            ("bullet-1", "Helped on the deploy work"),
            ("bullet-2", "Ran the release process"),
        );
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"suggestion": "Owned the deploy and release process"}"#); // offered
        mock.enqueue(r#"{"question": "What did you actually do on deploys?"}"#); // interview opens
        mock.enqueue(r#"{"question": ""}"#); // satisfied after one answer
        mock.enqueue(r#"{"bullet": "Automated the deploy pipeline"}"#); // rewrite
        let user = ScriptedUser::new();
        // Suggestion menu is [Use, Tweak, Answer in my own words, Skip].
        user.answer(Answer::Choice(2)); // "Answer in my own words"
        user.answer(Answer::Text("automated the deploy pipeline".into())); // interview answer
        user.answer(Answer::Choice(0)); // "Use this wording" (the rewrite)

        let targets = [target("bullet-1", ObjectionKind::VagueVerb)];
        let changed = strengthen_bullets(
            &mut dataset,
            &targets,
            &user,
            &ctx(&mock),
            InterviewLimits::default(),
        )
        .await
        .unwrap();

        // The interview ran after the suggestion was declined, and its line landed.
        assert_eq!(changed, 1);
        assert_eq!(
            dataset.roles[0].bullets[0].text,
            "Automated the deploy pipeline"
        );
    }

    #[tokio::test]
    async fn a_suggestion_that_invents_a_number_is_rejected_and_falls_back() {
        let mut dataset = dataset_with_two_bullets(
            ("bullet-1", "Improved onboarding"),
            ("bullet-2", "Wrote the setup docs"),
        );
        let mock = MockLlmClient::default();
        // No number in the bullet or the role evidence; the model invents "60%".
        // The guard rejects it, so no Select is offered and the interview runs.
        mock.enqueue(r#"{"suggestion": "Cut onboarding time by 60%"}"#);
        mock.enqueue(r#"{"question": "What changed about onboarding?"}"#);
        mock.enqueue(r#"{"question": ""}"#);
        mock.enqueue(r#"{"bullet": "Streamlined onboarding with setup docs"}"#);
        let user = ScriptedUser::new();
        // No Choice queued first: a rejected suggestion offers no menu, so the
        // first answer is the interview's. If the code wrongly offered the
        // invented-number suggestion, this Text would be mismatched.
        user.answer(Answer::Text(
            "wrote setup docs so onboarding got smoother".into(),
        ));
        user.answer(Answer::Choice(0)); // use the interview rewrite

        let targets = [target("bullet-1", ObjectionKind::GenericPhrasing)];
        let changed = strengthen_bullets(
            &mut dataset,
            &targets,
            &user,
            &ctx(&mock),
            InterviewLimits::default(),
        )
        .await
        .unwrap();

        assert_eq!(changed, 1);
        assert_eq!(
            dataset.roles[0].bullets[0].text,
            "Streamlined onboarding with setup docs"
        );
    }

    #[tokio::test]
    async fn a_suggestion_draws_only_on_the_bullets_own_role() {
        let mut dataset = base_dataset();
        dataset.roles.push(role_with_bullets(
            "role-1",
            "Director",
            "Acme",
            &[
                ("bullet-1", "Helped on incident response"),
                ("bullet-2", "Built the on-call rotation"),
            ],
        ));
        dataset.roles.push(role_with_bullets(
            "role-2",
            "Engineer",
            "Globex",
            &[("bullet-9", "Migrated the billing service to Kubernetes")],
        ));
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"suggestion": "Drove incident response across the on-call rotation"}"#);
        let user = ScriptedUser::new();
        user.answer(Answer::Choice(0)); // "Use this wording"

        let targets = [target("bullet-1", ObjectionKind::VagueVerb)];
        strengthen_bullets(
            &mut dataset,
            &targets,
            &user,
            &ctx(&mock),
            InterviewLimits::default(),
        )
        .await
        .unwrap();

        // The suggest agent's prompt carried this role's other bullet, never
        // the other role's work — same-role grounding contains attribution.
        let sent = &mock.requests()[0].messages[0].content;
        assert!(sent.contains("on-call rotation"));
        assert!(!sent.contains("Kubernetes"));
        assert!(!sent.contains("billing service"));
    }

    #[tokio::test]
    async fn tweaking_a_suggestion_revises_then_accepts() {
        let mut dataset = dataset_with_two_bullets(
            ("bullet-1", "Worked with the security lead"),
            ("bullet-2", "Handled SOC 2 evidence collection"),
        );
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"suggestion": "Drove SOC 2 audit readiness with the security lead"}"#);
        mock.enqueue(r#"{"suggestion": "Supported SOC 2 audit readiness with the security lead"}"#);
        let user = ScriptedUser::new();
        user.answer(Answer::Choice(1)); // "Tweak it"
        user.answer(Answer::Text("say supported, not drove".into())); // revision note
        user.answer(Answer::Choice(0)); // "Use this wording" (the revised line)

        let targets = [target("bullet-1", ObjectionKind::UnsupportedClaim)];
        strengthen_bullets(
            &mut dataset,
            &targets,
            &user,
            &ctx(&mock),
            InterviewLimits::default(),
        )
        .await
        .unwrap();

        assert_eq!(
            dataset.roles[0].bullets[0].text,
            "Supported SOC 2 audit readiness with the security lead"
        );
    }
}
