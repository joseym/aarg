//! Metric capture (FR-3.x, the reviewer's "this needs a number" made
//! actionable). The adversarial reviewer flags bullets that state an
//! outcome without quantifying it (`ObjectionKind::NoMetric`), but the
//! tailoring loop can't act on that — inventing a number is exactly what
//! the never-fabricate guard reverts. So the only honest fix is to *ask
//! the person*: a short, leading question per flagged bullet, and their
//! answer — their own words, needing no further proof — becomes part of
//! the recorded bullet.
//!
//! The model's role is strictly to phrase a good question; it never
//! supplies the number. That keeps this on the right side of the
//! fabrication line: the figure on the page traces to the user, the same
//! way a verified skill's evidence does.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::agent::{Agent, AgentContext};
use crate::dataset::types::{BulletId, Metric, ResumeDataset};
use crate::llm::LlmError;
use crate::user::{Answer, AskError, Question, UserHandle};

/// A leading question is one sentence.
const REPLY_BUDGET: u32 = 256;

#[derive(Debug, thiserror::Error)]
pub enum MetricError {
    #[error(transparent)]
    Llm(#[from] LlmError),

    #[error("the metric question reply was not the expected JSON (reply began {snippet:?})")]
    BadReply {
        snippet: String,
        #[source]
        source: serde_json::Error,
    },
}

/// One bullet the reviewer wants quantified: the bullet to enrich and,
/// optionally, what the reviewer felt was missing (its suggestion).
#[derive(Debug, Clone)]
pub struct MetricTarget {
    pub bullet_id: BulletId,
    pub hint: Option<String>,
}

/// What the question agent needs: the bullet's current text and the
/// reviewer's note, if any.
#[derive(Serialize)]
pub struct MetricQuestionInput {
    pub bullet: String,
    pub hint: Option<String>,
}

/// Asks one pointed, leading question for the number a bullet is missing.
pub struct MetricInterviewAgent;

#[async_trait]
impl Agent for MetricInterviewAgent {
    type Input = MetricQuestionInput;
    type Wire = RawQuestion;
    type Output = String;
    type Error = MetricError;

    fn id(&self) -> &'static str {
        "metric_interview_v1"
    }
    fn system_prompt(&self) -> &str {
        SYSTEM_PROMPT
    }
    fn reply_budget(&self) -> u32 {
        REPLY_BUDGET
    }
    fn user_message(&self, input: &MetricQuestionInput) -> String {
        let mut text = format!("The resume bullet:\n{}\n\n", input.bullet);
        if let Some(hint) = &input.hint {
            text.push_str(&format!("What the reviewer felt was missing: {hint}\n\n"));
        }
        text.push_str("Ask one leading question for the number that would make this bullet land.");
        text
    }
    fn bad_reply(&self, snippet: String, source: serde_json::Error) -> MetricError {
        MetricError::BadReply { snippet, source }
    }
    fn assemble(
        &self,
        wire: RawQuestion,
        _input: MetricQuestionInput,
    ) -> Result<String, MetricError> {
        Ok(wire.question)
    }
}

const SYSTEM_PROMPT: &str = r#"You help a job candidate put a concrete number on a resume bullet that currently has none. Given the bullet, ask ONE short, specific question that leads them toward the single most impactful metric behind it — scale (team size, users, requests), a percentage or multiple (cost down, speed up), time saved, money, counts, or a duration/date.

Rules:
- Ask for the KIND of number that fits THIS bullet; refer to what it actually describes.
- NEVER propose, guess, or imply a value. "By roughly what percentage did delivery costs drop?" is good; "Did you cut costs by 30%?" is forbidden — the number must be theirs, not yours.
- One question, one sentence, warm and concrete.

Reply with exactly one JSON object and nothing else — no markdown fences:
{"question": "your question here"}"#;

#[derive(Debug, Deserialize)]
pub struct RawQuestion {
    #[serde(default)]
    question: String,
}

/// Interview the user about every flagged bullet and fold their answers
/// into the dataset. Returns how many bullets gained a metric. A bullet
/// the reviewer named but that isn't in the dataset (a phantom id) is
/// skipped; an agent that can't be reached degrades to a generic
/// question rather than aborting the interview. Mutates only the
/// in-memory dataset — the caller saves on success.
// EXERCISE(EX-019)
pub async fn capture_metrics(
    dataset: &mut ResumeDataset,
    targets: &[MetricTarget],
    user: &dyn UserHandle,
    ctx: &AgentContext<'_>,
) -> Result<usize, AskError> {
    let mut added = 0;
    let mut seen: Vec<BulletId> = Vec::new();
    for target in targets {
        if seen.contains(&target.bullet_id) {
            continue; // the reviewer flagged the same line twice
        }
        seen.push(target.bullet_id.clone());

        let Some((role_label, text)) = bullet_context(dataset, &target.bullet_id) else {
            continue; // a phantom id the reviewer invented
        };
        if bullet_has_metric(dataset, &target.bullet_id) {
            continue; // already quantified — don't re-ask the same bullet
        }

        let question = match MetricInterviewAgent
            .run(
                ctx,
                MetricQuestionInput {
                    bullet: text.clone(),
                    hint: target.hint.clone(),
                },
            )
            .await
        {
            Ok(run) => run.output,
            Err(_) => "What's the most concrete number behind this — scale, percentage, \
                       time saved, or count? (blank to skip)"
                .to_string(),
        };

        // Anchor the user: which role, then the exact line — no guessing
        // which of several jobs this bullet belongs to.
        user.notify(&format!("\n— {role_label} —\n  \"{text}\""));
        let answer = match user.ask(Question::Text { prompt: question }).await? {
            Answer::Text(t) if !t.trim().is_empty() => t.trim().to_string(),
            _ => continue, // blank = skip this bullet
        };

        apply_metric(dataset, &target.bullet_id, &answer);
        added += 1;
    }
    Ok(added)
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

/// Record the user's number on the bullet's `metric` field — cleanly,
/// leaving the text untouched. Tailoring surfaces that field to the model
/// (so it can weave the number in) and its digit-runs guard counts the
/// metric as part of the allowed source, so the number survives a rewrite
/// without ever polluting the recorded bullet with "(...)" appends. The
/// user's words go in verbatim — they need no further proof.
fn apply_metric(dataset: &mut ResumeDataset, id: &BulletId, answer: &str) {
    for role in &mut dataset.roles {
        for bullet in &mut role.bullets {
            if bullet.id == *id {
                bullet.metric = Some(Metric(answer.to_string()));
                return;
            }
        }
    }
}

/// Whether the bullet already carries a metric — in which case it's been
/// quantified and shouldn't be re-asked (or the appends would pile up).
fn bullet_has_metric(dataset: &ResumeDataset, id: &BulletId) -> bool {
    dataset
        .roles
        .iter()
        .flat_map(|role| &role.bullets)
        .find(|bullet| bullet.id == *id)
        .is_some_and(|bullet| bullet.metric.is_some())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::dataset::types::{
        Bullet, Contact, EmploymentType, Role, RoleId, Strength, YearMonth,
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

    #[tokio::test]
    async fn an_answer_becomes_the_bullets_metric_and_enters_the_text() {
        let mut dataset = dataset_with_bullet("bullet-1", "Reduced delivery costs");
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"question": "By roughly what percentage did delivery costs drop?"}"#);
        let user = ScriptedUser::new();
        user.answer(Answer::Text("cut delivery costs ~30%".into()));

        let targets = [MetricTarget {
            bullet_id: BulletId("bullet-1".into()),
            hint: Some("quantify the cost reduction".into()),
        }];
        let added = capture_metrics(&mut dataset, &targets, &user, &ctx(&mock))
            .await
            .unwrap();

        assert_eq!(added, 1);
        let bullet = &dataset.roles[0].bullets[0];
        // The number lands on the metric field, recorded cleanly...
        assert_eq!(
            bullet.metric,
            Some(Metric("cut delivery costs ~30%".into()))
        );
        // ...and the bullet text is left untouched — no "(...)" appended.
        assert_eq!(bullet.text, "Reduced delivery costs");
        // The user saw both the role it belongs to and the exact line —
        // no guessing which job.
        assert!(
            user.notices()
                .iter()
                .any(|n| n.contains("Director at Acme") && n.contains("Reduced delivery costs"))
        );
    }

    #[tokio::test]
    async fn a_blank_answer_leaves_the_bullet_untouched() {
        let mut dataset = dataset_with_bullet("bullet-1", "Reduced delivery costs");
        let before = dataset.clone();
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"question": "By how much?"}"#);
        let user = ScriptedUser::new();
        user.answer(Answer::Text("   ".into())); // blank

        let targets = [MetricTarget {
            bullet_id: BulletId("bullet-1".into()),
            hint: None,
        }];
        let added = capture_metrics(&mut dataset, &targets, &user, &ctx(&mock))
            .await
            .unwrap();

        assert_eq!(added, 0);
        assert_eq!(dataset, before);
    }

    #[tokio::test]
    async fn a_phantom_bullet_id_is_skipped_not_an_error() {
        let mut dataset = dataset_with_bullet("bullet-1", "Did things");
        let mock = MockLlmClient::default();
        let user = ScriptedUser::new();

        let targets = [MetricTarget {
            bullet_id: BulletId("bullet-99".into()), // not in the dataset
            hint: None,
        }];
        let added = capture_metrics(&mut dataset, &targets, &user, &ctx(&mock))
            .await
            .unwrap();

        assert_eq!(added, 0);
        // The agent was never even consulted for a bullet that isn't there.
        assert!(mock.requests().is_empty());
    }

    #[tokio::test]
    async fn a_bullet_that_already_has_a_metric_is_not_re_asked() {
        let mut dataset = dataset_with_bullet("bullet-1", "Reduced delivery costs");
        dataset.roles[0].bullets[0].metric = Some(Metric("30%".into()));
        let before = dataset.clone();
        let mock = MockLlmClient::default();
        let user = ScriptedUser::new();

        let targets = [MetricTarget {
            bullet_id: BulletId("bullet-1".into()),
            hint: None,
        }];
        let added = capture_metrics(&mut dataset, &targets, &user, &ctx(&mock))
            .await
            .unwrap();

        assert_eq!(added, 0);
        assert_eq!(dataset, before);
        // No question asked — it's already quantified, so nothing re-appends.
        assert!(mock.requests().is_empty());
    }

    #[tokio::test]
    #[ignore = "exercise: when the user's answer contains no digit (e.g. \"halved it\"), ask one follow-up nudging for an explicit number before accepting it, then finish this test"]
    async fn ex_019_a_numberless_answer_gets_one_follow_up() {
        let nudges_for_a_number = false;
        assert!(nudges_for_a_number);
    }
}
