//! Role enrichment — the history copilot. A thin work-history entry (a
//! role with only a line or two recorded) shouldn't be stripped from a
//! tailored resume or padded with its own weak bullets: work history has
//! value in its own right — tenure, range, progression — independent of
//! how well it matches any one job. So when a role is thin, the honest
//! move is to help the person *say more about what they actually did*.
//!
//! This module asks. A small agent reads the role and what's already
//! recorded, then poses a few short, leading questions; each answer the
//! user types becomes a new bullet on that role. Like metric capture and
//! skill verification, the agent only ever *asks* — the content is the
//! user's own words about a real role, so nothing here can fabricate
//! experience. It is JD-agnostic on purpose: this captures history as it
//! was, not history bent to fit a posting.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::agent::{Agent, AgentContext};
use crate::dataset::types::{Bullet, ResumeDataset, RoleId, Strength};
use crate::llm::LlmError;
use crate::user::{Answer, AskError, Question, UserHandle};

/// A role with fewer than this many recorded bullets counts as thin —
/// enough to invite enrichment without nagging about well-covered roles.
const THIN_ROLE_BULLETS: usize = 3;

/// Up to a handful of questions; a paragraph of JSON.
const REPLY_BUDGET: u32 = 512;

#[derive(Debug, thiserror::Error)]
pub enum EnrichError {
    #[error(transparent)]
    Llm(#[from] LlmError),

    #[error("the enrichment reply was not the expected JSON (reply began {snippet:?})")]
    BadReply {
        snippet: String,
        #[source]
        source: serde_json::Error,
    },
}

/// What an enrichment session accomplished.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct EnrichOutcome {
    pub bullets_added: usize,
    pub roles_touched: usize,
}

impl EnrichOutcome {
    pub fn changed(&self) -> bool {
        self.bullets_added > 0
    }
}

/// Ids of roles thin on recorded detail — the default enrichment targets.
pub fn thin_roles(dataset: &ResumeDataset) -> Vec<RoleId> {
    dataset
        .roles
        .iter()
        .filter(|role| role.bullets.len() < THIN_ROLE_BULLETS)
        .map(|role| role.id.clone())
        .collect()
}

/// What the question agent sees: the role and what's already recorded,
/// so it asks about gaps rather than what's covered.
#[derive(Serialize)]
pub struct RoleSketch {
    pub title: String,
    pub company: String,
    pub period: String,
    pub context: Option<String>,
    pub bullets: Vec<String>,
}

/// Asks a few leading questions to draw real detail out of a thin role.
pub struct RoleEnrichmentAgent;

#[async_trait]
impl Agent for RoleEnrichmentAgent {
    type Input = RoleSketch;
    type Wire = RawQuestions;
    type Output = Vec<String>;
    type Error = EnrichError;

    fn id(&self) -> &'static str {
        "role_enrichment_v1"
    }
    fn system_prompt(&self) -> &str {
        SYSTEM_PROMPT
    }
    fn reply_budget(&self) -> u32 {
        REPLY_BUDGET
    }
    fn user_message(&self, input: &RoleSketch) -> String {
        let mut text = format!(
            "Role: {} at {} ({})\n",
            input.title, input.company, input.period
        );
        if let Some(context) = &input.context {
            text.push_str(&format!("Context: {context}\n"));
        }
        if input.bullets.is_empty() {
            text.push_str("Recorded so far: nothing.\n");
        } else {
            text.push_str("Recorded so far:\n");
            for bullet in &input.bullets {
                text.push_str(&format!("- {bullet}\n"));
            }
        }
        text.push_str("\nAsk a few leading questions to draw out more of what they did here.");
        text
    }
    fn bad_reply(&self, snippet: String, source: serde_json::Error) -> EnrichError {
        EnrichError::BadReply { snippet, source }
    }
    fn assemble(&self, wire: RawQuestions, _input: RoleSketch) -> Result<Vec<String>, EnrichError> {
        Ok(wire.questions)
    }
}

const SYSTEM_PROMPT: &str = r#"You help a candidate flesh out a thin entry in their work history. Given a role and what's already recorded for it, ask a few short, specific, leading questions that draw out concrete detail they could add — what they built or owned, the scope and scale, the technologies, the decisions they made, and any outcomes.

How to ask:
- Make the questions specific to THIS role and what's already there; don't ask about something already recorded.
- Aim at real substance, not buzzwords. This is about capturing what they genuinely did, even if it isn't relevant to any particular job — a fuller, honest history is the goal.
- Ask 2 to 4 questions. Never propose or imply an answer, a metric, or a fact; only ask. The candidate supplies every word of the content.

Reply with exactly one JSON object and nothing else — no markdown fences:
{"questions": ["...", "..."]}"#;

#[derive(Debug, Deserialize)]
pub struct RawQuestions {
    #[serde(default)]
    questions: Vec<String>,
}

/// Interview the user about each target role and fold their answers in as
/// new bullets. Per role: a confirm to skip it wholesale, then the
/// leading questions — each non-empty answer becomes a bullet in the
/// user's own words. A role that isn't in the dataset is skipped; an
/// agent that can't be reached degrades to generic questions. Mutates
/// only the in-memory dataset; the caller saves on success.
pub async fn enrich_roles(
    dataset: &mut ResumeDataset,
    targets: &[RoleId],
    user: &dyn UserHandle,
    ctx: &AgentContext<'_>,
) -> Result<EnrichOutcome, AskError> {
    let mut outcome = EnrichOutcome::default();
    for role_id in targets {
        let Some(sketch) = role_sketch(dataset, role_id) else {
            continue; // a role id that isn't in the dataset
        };
        let heading = format!("{} at {}", sketch.title, sketch.company);

        if !user.confirm(&format!("flesh out {heading}?"), true).await? {
            continue;
        }

        let questions = match RoleEnrichmentAgent.run(ctx, sketch).await {
            Ok(run) if !run.output.is_empty() => run.output,
            _ => default_questions(),
        };

        let mut added_here = 0;
        for question in questions {
            let answer = match user.ask(Question::Text { prompt: question }).await? {
                Answer::Text(text) if !text.trim().is_empty() => text.trim().to_string(),
                _ => continue, // blank skips this question
            };
            add_bullet(dataset, role_id, answer);
            outcome.bullets_added += 1;
            added_here += 1;
        }
        if added_here > 0 {
            outcome.roles_touched += 1;
            user.notify(&format!("added {added_here} bullet(s) to {heading}"));
        }
    }
    Ok(outcome)
}

/// Fallback questions when the agent can't be reached — still useful.
fn default_questions() -> Vec<String> {
    vec![
        "What was the most significant thing you built or shipped in this role? (blank to skip)"
            .to_string(),
        "What did you own or lead, and at what scale? (blank to skip)".to_string(),
        "Any concrete outcome, technology, or decision worth noting? (blank to skip)".to_string(),
    ]
}

fn role_sketch(dataset: &ResumeDataset, role_id: &RoleId) -> Option<RoleSketch> {
    let role = dataset.roles.iter().find(|r| r.id == *role_id)?;
    let period = format!(
        "{} to {}",
        role.start,
        role.end
            .map_or_else(|| "present".to_string(), |ym| ym.to_string())
    );
    Some(RoleSketch {
        title: role.title.clone(),
        company: role.company.clone(),
        period,
        context: role.context.clone(),
        bullets: role.bullets.iter().map(|b| b.text.clone()).collect(),
    })
}

/// Append the user's verbatim answer as a new bullet on the role.
fn add_bullet(dataset: &mut ResumeDataset, role_id: &RoleId, text: String) {
    let id = dataset.next_bullet_id();
    if let Some(role) = dataset.roles.iter_mut().find(|r| r.id == *role_id) {
        role.bullets.push(Bullet {
            id,
            text,
            skill_ids: Vec::new(),
            metric: None,
            theme: Vec::new(),
            strength: Strength::Medium,
            variants: Vec::new(),
        });
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::dataset::types::{BulletId, Contact, EmploymentType, Role, RoleId, YearMonth};
    use crate::llm::MockLlmClient;
    use crate::trace::Tracer;
    use crate::user::ScriptedUser;

    fn dataset_with_roles(bullet_counts: &[(&str, usize)]) -> ResumeDataset {
        let mut d = ResumeDataset::new(Contact {
            full_name: "Ada".into(),
            email: "ada@example.com".into(),
            phone: None,
            location: None,
            links: Vec::new(),
        });
        let mut next = 1u32;
        for (role_id, count) in bullet_counts {
            let bullets = (0..*count)
                .map(|_| {
                    let b = Bullet {
                        id: BulletId(format!("bullet-{next}")),
                        text: "did a thing".into(),
                        skill_ids: Vec::new(),
                        metric: None,
                        theme: Vec::new(),
                        strength: Strength::Medium,
                        variants: Vec::new(),
                    };
                    next += 1;
                    b
                })
                .collect();
            d.roles.push(Role {
                id: RoleId((*role_id).into()),
                company: format!("Co-{role_id}"),
                title: "Engineer".into(),
                start: YearMonth {
                    year: 2018,
                    month: 1,
                },
                end: None,
                location: None,
                employment_type: EmploymentType::FullTime,
                bullets,
                skill_ids: Vec::new(),
                context: None,
            });
        }
        d
    }

    fn ctx<'a>(mock: &'a MockLlmClient) -> AgentContext<'a> {
        AgentContext {
            llm: mock,
            model: &"m",
            tracer: &Tracer::DISABLED,
        }
    }

    #[test]
    fn thin_roles_are_the_ones_under_the_threshold() {
        let dataset = dataset_with_roles(&[("role-1", 1), ("role-2", 3), ("role-3", 2)]);
        let thin = thin_roles(&dataset);
        // role-2 (3 bullets) is not thin; role-1 (1) and role-3 (2) are.
        assert_eq!(thin, vec![RoleId("role-1".into()), RoleId("role-3".into())]);
    }

    #[tokio::test]
    async fn answers_become_new_bullets_on_the_role() {
        let mut dataset = dataset_with_roles(&[("role-1", 1)]);
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"questions": ["What did you build?", "Any outcome?"]}"#);
        let user = ScriptedUser::new();
        user.confirm_with(true); // yes, flesh out role-1
        user.answer(Answer::Text("Rebuilt the billing service in PHP".into()));
        user.answer(Answer::Text("   ".into())); // blank skips the 2nd

        let outcome = enrich_roles(&mut dataset, &[RoleId("role-1".into())], &user, &ctx(&mock))
            .await
            .unwrap();

        assert_eq!(outcome.bullets_added, 1);
        assert_eq!(outcome.roles_touched, 1);
        let role = &dataset.roles[0];
        assert_eq!(role.bullets.len(), 2); // the original + the new one
        assert_eq!(role.bullets[1].text, "Rebuilt the billing service in PHP");
        // The new bullet's id continues the sequence and carries no skill.
        assert_eq!(role.bullets[1].id, BulletId("bullet-2".into()));
        assert!(role.bullets[1].skill_ids.is_empty());
    }

    #[tokio::test]
    async fn declining_a_role_leaves_it_untouched_and_asks_nothing() {
        let mut dataset = dataset_with_roles(&[("role-1", 1)]);
        let before = dataset.clone();
        let mock = MockLlmClient::default();
        let user = ScriptedUser::new();
        user.confirm_with(false); // no, skip role-1

        let outcome = enrich_roles(&mut dataset, &[RoleId("role-1".into())], &user, &ctx(&mock))
            .await
            .unwrap();

        assert_eq!(outcome, EnrichOutcome::default());
        assert_eq!(dataset, before);
        // The agent was never consulted for a role the user skipped.
        assert!(mock.requests().is_empty());
    }
}
