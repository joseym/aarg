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

use std::collections::HashSet;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::agent::{Agent, AgentContext};
use crate::dataset::types::{BulletId, ResumeDataset};
use crate::llm::LlmError;
use crate::review::ObjectionKind;
use crate::tailor::digit_runs;
use crate::user::{Answer, AskError, Question, UserHandle};

/// A leading question is one sentence; a single reworded bullet is short.
const REPLY_BUDGET: u32 = 256;

/// How many times the user may ask for another rewrite before the loop
/// offers only take-it-or-keep-mine, so it always terminates.
const MAX_REVISES: usize = 3;

/// How many questions the interview may ask in total — one opening
/// question plus follow-ups when an answer is too thin to write a strong,
/// specific bullet from. Caps the conversation so it can't interrogate
/// forever.
const MAX_QUESTIONS: usize = 3;

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
        // reviewer didn't like — no guessing which job or why.
        user.notify(&format!(
            "\n— {role_label} —\n  \"{text}\"\n  reviewer: {}",
            target.concern
        ));

        // A short adaptive interview: the agent asks a follow-up whenever an
        // answer is too thin to write a strong bullet from, and stops (empty
        // question) once it has enough — capped so it never interrogates
        // forever. A blank answer ends the interview early.
        let mut transcript: Vec<QnA> = Vec::new();
        for turn in 0..MAX_QUESTIONS {
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
                Err(_) if turn == 0 => "What actually happened here — what did you do, \
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
        let final_text = polish(ctx, &text, &facts, user).await?;
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
) -> Result<String, AskError> {
    let mut notes: Vec<String> = Vec::new();
    // First rewrite. If it can't be produced or trips the digit guard, the
    // user's own words stand — no offer, nothing invented.
    let Some(mut rewrite) = run_rewrite(ctx, original, answer, &notes).await else {
        return Ok(answer.to_string());
    };

    let mut revises_left = MAX_REVISES;
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
    // support. Allowed digits = those in the user's answer or the original.
    let allowed: HashSet<String> = digit_runs(answer)
        .union(&digit_runs(original))
        .cloned()
        .collect();
    if digit_runs(&rewrite).is_subset(&allowed) {
        Some(rewrite)
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
        }
    }

    fn target(id: &str, kind: ObjectionKind) -> StrengthenTarget {
        StrengthenTarget {
            bullet_id: BulletId(id.into()),
            kind,
            concern: "reads as supporting cast, not owner".into(),
        }
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
        let changed = strengthen_bullets(&mut dataset, &targets, &user, &ctx(&mock))
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
        strengthen_bullets(&mut dataset, &targets, &user, &ctx(&mock))
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
        strengthen_bullets(&mut dataset, &targets, &user, &ctx(&mock))
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
        strengthen_bullets(&mut dataset, &targets, &user, &ctx(&mock))
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
        let changed = strengthen_bullets(&mut dataset, &targets, &user, &ctx(&mock))
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
        strengthen_bullets(&mut dataset, &targets, &user, &ctx(&mock))
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
        let changed = strengthen_bullets(&mut dataset, &targets, &user, &ctx(&mock))
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
        let changed = strengthen_bullets(&mut dataset, &targets, &user, &ctx(&mock))
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
}
