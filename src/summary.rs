//! Summary refine — the objection triage's summary path. The adversarial
//! reviewer can flag the tailored summary as weak (generic phrasing, a buried
//! hook, JD-echo). Unlike a bullet, the summary isn't a stored fact: the
//! tailoring agent generates it fresh each pass. So "refining" it means
//! drafting a stronger summary from the user's WHOLE recorded history and, on
//! the user's confirmation, recording it as authoritative (`summary` +
//! `summary_confirmed`) so tailoring and the human variant use it verbatim
//! instead of regenerating or rewording it.
//!
//! Never-fabricate holds the same three ways as the bullet suggestion:
//! 1. the prompt forbids adding any fact not in the recorded history;
//! 2. the shared digit guard (`tailor::within_evidence`) rejects a draft that
//!    introduces a number the history doesn't state;
//! 3. the user drives use / tweak / write-my-own / skip — every confirmed
//!    summary is the user's own words, the same standard as a verified skill.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::agent::{Agent, AgentContext};
use crate::dataset::types::ResumeDataset;
use crate::llm::LlmError;
use crate::tailor::within_evidence;
use crate::user::{Answer, AskError, Question, UserHandle};

/// A suggested summary is short.
const REPLY_BUDGET: u32 = 256;

#[derive(Debug, thiserror::Error)]
pub enum SummaryError {
    #[error(transparent)]
    Llm(#[from] LlmError),

    #[error("the summary reply was not the expected JSON (reply began {snippet:?})")]
    BadReply {
        snippet: String,
        #[source]
        source: serde_json::Error,
    },
}

/// The user's recorded experience, gathered for grounding a summary: every role
/// with its bullets (metrics folded in) and the skill names. The summary spans
/// the whole career, so — unlike a bullet's evidence — this isn't scoped to one
/// role.
#[derive(Debug, Clone, Serialize)]
pub struct SummaryEvidence {
    pub roles: Vec<RoleBrief>,
    pub skills: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RoleBrief {
    pub title: String,
    pub company: String,
    pub bullets: Vec<String>,
}

impl SummaryEvidence {
    /// Every recorded string a suggestion is allowed to draw a number from.
    fn texts(&self) -> Vec<&str> {
        let mut texts = Vec::new();
        for role in &self.roles {
            texts.push(role.title.as_str());
            texts.push(role.company.as_str());
            texts.extend(role.bullets.iter().map(String::as_str));
        }
        texts.extend(self.skills.iter().map(String::as_str));
        texts
    }
}

/// Gather the whole-dataset grounding for a summary suggestion.
fn gather_evidence(dataset: &ResumeDataset) -> SummaryEvidence {
    let roles = dataset
        .roles
        .iter()
        .map(|role| RoleBrief {
            title: role.title.clone(),
            company: role.company.clone(),
            bullets: role
                .bullets
                .iter()
                .map(|bullet| match &bullet.metric {
                    Some(metric) => format!("{} ({})", bullet.text, metric.0),
                    None => bullet.text.clone(),
                })
                .collect(),
        })
        .collect();
    let skills = dataset
        .skills
        .skills
        .iter()
        .map(|skill| skill.canonical_name.clone())
        .collect();
    SummaryEvidence { roles, skills }
}

/// What the suggestion agent needs: the current summary, the reviewer's
/// concern, the recorded history to draw on, and any revision notes.
#[derive(Serialize)]
pub struct SummarySuggestInput {
    pub current: String,
    pub concern: String,
    pub evidence: SummaryEvidence,
    pub notes: Vec<String>,
}

/// Drafts a stronger summary as a starting point, grounded only in the user's
/// recorded history. It proposes; the user disposes (use / tweak / write own /
/// skip), and the digit guard plus the no-new-facts prompt keep it honest.
pub struct SummarySuggestAgent;

#[async_trait]
impl Agent for SummarySuggestAgent {
    type Input = SummarySuggestInput;
    type Wire = RawSummary;
    type Output = String;
    type Error = SummaryError;

    fn id(&self) -> &'static str {
        "summary_suggest_v1"
    }
    fn system_prompt(&self) -> &str {
        SUGGEST_PROMPT
    }
    fn reply_budget(&self) -> u32 {
        REPLY_BUDGET
    }
    fn user_message(&self, input: &SummarySuggestInput) -> String {
        let mut text = format!(
            "The current resume summary:\n{}\n\nThe reviewer's concern: {}\n\nThe candidate's recorded history (draw only on these facts):\n",
            input.current, input.concern
        );
        for role in &input.evidence.roles {
            text.push_str(&format!("\n{} at {}\n", role.title, role.company));
            for bullet in &role.bullets {
                text.push_str(&format!("  - {bullet}\n"));
            }
        }
        if !input.evidence.skills.is_empty() {
            text.push_str(&format!("\nSkills: {}\n", input.evidence.skills.join(", ")));
        }
        if !input.notes.is_empty() {
            text.push_str("\nThe candidate asked you to revise your previous summary:\n");
            for note in &input.notes {
                text.push_str(&format!("- {note}\n"));
            }
        }
        text.push_str("\nPropose one stronger summary, using only the facts above.");
        text
    }
    fn bad_reply(&self, snippet: String, source: serde_json::Error) -> SummaryError {
        SummaryError::BadReply { snippet, source }
    }
    fn assemble(
        &self,
        wire: RawSummary,
        _input: SummarySuggestInput,
    ) -> Result<String, SummaryError> {
        Ok(wire.summary)
    }
}

const SUGGEST_PROMPT: &str = r#"You help a job candidate strengthen the SUMMARY at the top of their resume, which a skeptical reviewer flagged. You are given the current summary, the reviewer's concern, and the candidate's full recorded history (roles, accomplishments, skills). You draft one stronger summary as a starting point they will review, edit, or reject.

Write a summary that:
- Is 2-3 tight sentences. Lead with the candidate's strongest, most distinctive hook drawn from the recorded history; cut filler and JD-keyword stuffing.
- Uses ONLY facts present in the recorded history or the current summary. Add nothing the candidate has not recorded — no number, percentage, scope, team size, title, employer, or seniority that isn't there.
- Never inflates. Reflect the real level and scope of the work. A credible summary the candidate could defend in an interview beats an impressive-sounding one they cannot, which is the one thing worse than a weak summary.
- Uses no em-dashes ("—"). Join clauses with a comma, with "and", or as a second sentence; a colon is fine where it fits.

If you cannot strengthen the summary honestly from the facts given, reply with an empty string.
If the candidate gave revision notes, follow them, but every rule above still binds.

Reply with exactly one JSON object and nothing else — no markdown fences:
{"summary": "your summary, or an empty string if you cannot do it honestly"}"#;

#[derive(Debug, Deserialize)]
pub struct RawSummary {
    #[serde(default)]
    summary: String,
}

/// Refine the summary: draft a grounded suggestion, let the user use / tweak /
/// write their own / skip, and on acceptance record it as the user's confirmed
/// summary (`summary` + `summary_confirmed`) so tailoring and the human variant
/// use it verbatim. Returns whether the summary changed. Mutates only the
/// in-memory dataset — the caller saves and re-tailors.
pub async fn refine_summary(
    dataset: &mut ResumeDataset,
    concern: &str,
    user: &dyn UserHandle,
    ctx: &AgentContext<'_>,
    max_revises: usize,
) -> Result<bool, AskError> {
    let current = dataset.summary.clone().unwrap_or_default();
    let evidence = gather_evidence(dataset);

    let mut notes: Vec<String> = Vec::new();
    let mut suggestion = run_suggest(ctx, &current, concern, &evidence, &notes).await;

    let mut revises_left = max_revises;
    loop {
        // "Use this" / "Tweak it" appear only when there's a clean suggestion to
        // act on; "Write my own" and "Skip" always do, so the user is never
        // stuck if no honest draft could be produced.
        let mut options = Vec::new();
        if suggestion.is_some() {
            options.push("Use this wording".to_string());
            if revises_left > 0 {
                options.push("Tweak it".to_string());
            }
        }
        options.push("Write my own".to_string());
        options.push("Skip this one".to_string());

        let prompt = match &suggestion {
            Some(s) => format!("suggested summary:\n  \"{s}\""),
            None => {
                "no grounded suggestion could be drafted from your recorded history".to_string()
            }
        };

        let choice = match user
            .ask(Question::Select {
                prompt,
                options: options.clone(),
            })
            .await?
        {
            Answer::Choice(i) => options.get(i).map(String::as_str),
            _ => Some("Skip this one"),
        };

        match choice {
            Some("Use this wording") => {
                if let Some(s) = suggestion {
                    confirm_summary(dataset, s);
                    return Ok(true);
                }
                return Ok(false);
            }
            Some("Write my own") => {
                match user
                    .ask(Question::Text {
                        prompt: "write your summary:".to_string(),
                    })
                    .await?
                {
                    Answer::Text(t) if !t.trim().is_empty() => {
                        confirm_summary(dataset, t.trim().to_string());
                        return Ok(true);
                    }
                    _ => return Ok(false), // blank = leave the summary as is
                }
            }
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
                if let Some(next) = run_suggest(ctx, &current, concern, &evidence, &notes).await {
                    suggestion = Some(next);
                }
            }
            _ => return Ok(false), // Skip (or an unexpected answer): leave it
        }
    }
}

/// Record the user's confirmed summary as authoritative: stored on the dataset
/// and flagged so tailoring and the human variant use it verbatim.
fn confirm_summary(dataset: &mut ResumeDataset, text: String) {
    dataset.summary = Some(text);
    dataset.summary_confirmed = true;
}

/// One suggestion attempt, fully guarded. Returns the candidate summary only if
/// the agent produced a non-empty draft that differs from the current summary
/// and introduces no number absent from the recorded history. Otherwise `None`,
/// and the caller offers no suggestion (write-your-own / skip remain).
async fn run_suggest(
    ctx: &AgentContext<'_>,
    current: &str,
    concern: &str,
    evidence: &SummaryEvidence,
    notes: &[String],
) -> Option<String> {
    let run = SummarySuggestAgent
        .run(
            ctx,
            SummarySuggestInput {
                current: current.to_string(),
                concern: concern.to_string(),
                evidence: evidence.clone(),
                notes: notes.to_vec(),
            },
        )
        .await
        .ok()?;
    let suggestion = run.output.trim().to_string();
    if suggestion.is_empty() || suggestion == current {
        return None;
    }
    // A suggestion may recombine the recorded history, never mint a number
    // absent from it or the current summary.
    let mut allowed = evidence.texts();
    allowed.push(current);
    if within_evidence(&suggestion, &allowed) {
        Some(suggestion)
    } else {
        None
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::dataset::types::{
        Bullet, BulletId, Contact, EmploymentType, Role, RoleId, Strength, YearMonth,
    };
    use crate::llm::MockLlmClient;
    use crate::trace::Tracer;
    use crate::user::ScriptedUser;

    fn dataset_with_history(summary: &str) -> ResumeDataset {
        let mut d = ResumeDataset::new(Contact {
            full_name: "Ada".into(),
            email: "ada@example.com".into(),
            phone: None,
            location: None,
            links: Vec::new(),
        });
        d.summary = Some(summary.into());
        d.roles.push(Role {
            id: RoleId("role-1".into()),
            company: "Acme".into(),
            title: "Director of Engineering".into(),
            start: YearMonth {
                year: 2020,
                month: 1,
            },
            end: None,
            location: None,
            employment_type: EmploymentType::FullTime,
            bullets: vec![Bullet {
                id: BulletId("bullet-1".into()),
                text: "Grew the team from a single engineer to a 20 person org".into(),
                skill_ids: Vec::new(),
                metric: None,
                theme: Vec::new(),
                strength: Strength::High,
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

    #[tokio::test]
    async fn a_grounded_suggestion_is_confirmed_as_authoritative() {
        let mut dataset = dataset_with_history("Leader with delivery focus and a track record.");
        let mock = MockLlmClient::default();
        mock.enqueue(
            r#"{"summary": "Engineering leader who grew a team from a single engineer to a 20 person org."}"#,
        );
        let user = ScriptedUser::new();
        user.answer(Answer::Choice(0)); // "Use this wording"

        let changed = refine_summary(
            &mut dataset,
            "front-loads everything",
            &user,
            &ctx(&mock),
            3,
        )
        .await
        .unwrap();

        assert!(changed);
        assert!(dataset.summary_confirmed);
        assert_eq!(
            dataset.summary.as_deref(),
            Some("Engineering leader who grew a team from a single engineer to a 20 person org.")
        );
    }

    #[tokio::test]
    async fn write_my_own_records_the_users_words() {
        let mut dataset = dataset_with_history("Generic opener.");
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"summary": "An adequate but not chosen suggestion."}"#);
        let user = ScriptedUser::new();
        // Menu is [Use, Tweak, Write my own, Skip].
        user.answer(Answer::Choice(2)); // "Write my own"
        user.answer(Answer::Text("My own crisp two sentence summary.".into()));

        let changed = refine_summary(&mut dataset, "too generic", &user, &ctx(&mock), 3)
            .await
            .unwrap();

        assert!(changed);
        assert!(dataset.summary_confirmed);
        assert_eq!(
            dataset.summary.as_deref(),
            Some("My own crisp two sentence summary.")
        );
    }

    #[tokio::test]
    async fn a_suggestion_inventing_a_number_offers_no_use_option() {
        let mut dataset = dataset_with_history("Leader.");
        let mock = MockLlmClient::default();
        // "40%" appears nowhere in the history or the current summary.
        mock.enqueue(r#"{"summary": "Engineering leader who cut delivery time by 40%."}"#);
        let user = ScriptedUser::new();
        // Guard rejects the draft, so the menu is [Write my own, Skip]; Skip is
        // index 1. (A "Use this" at index 0 would mean the draft was offered.)
        user.answer(Answer::Choice(1)); // "Skip this one"

        let changed = refine_summary(&mut dataset, "needs a stronger hook", &user, &ctx(&mock), 3)
            .await
            .unwrap();

        assert!(!changed);
        assert!(!dataset.summary_confirmed);
        assert_eq!(dataset.summary.as_deref(), Some("Leader."));
    }

    #[tokio::test]
    async fn tweaking_re_suggests_then_accepts() {
        let mut dataset = dataset_with_history("Leader.");
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"summary": "Engineering leader who scaled a team."}"#);
        mock.enqueue(r#"{"summary": "Engineering leader who built a team from one engineer."}"#);
        let user = ScriptedUser::new();
        user.answer(Answer::Choice(1)); // "Tweak it"
        user.answer(Answer::Text("say built from one engineer".into()));
        user.answer(Answer::Choice(0)); // "Use this wording" (the revised line)

        refine_summary(&mut dataset, "vague", &user, &ctx(&mock), 3)
            .await
            .unwrap();

        assert!(dataset.summary_confirmed);
        assert_eq!(
            dataset.summary.as_deref(),
            Some("Engineering leader who built a team from one engineer.")
        );
    }
}
