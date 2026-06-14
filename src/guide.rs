//! The verification guide: an honest advisor that helps a confused user
//! decide whether their real experience matches a skill (FR-3.1, the
//! clarification half).
//!
//! During skill verification, "Have you used Data Engineering?" can be
//! genuinely hard to answer — the user may not know what counts, or
//! wants to describe what they did and be told whether it qualifies.
//! This agent answers that, in a short back-and-forth.
//!
//! Its posture is the whole point: it is an *honest* advisor, not a
//! resume padder. It explains what a skill means, asks what the user
//! actually did, and says plainly when their experience does NOT match
//! — because a resume that claims skills the person can't defend gets
//! them rejected in the interview, which is worse than a gap. It never
//! pressures the user to claim something, and it records nothing: the
//! user still answers the yes/no question themselves. Like the
//! reviewer, its output is advice, not resume content, so it cannot put
//! a claim on the page.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::agent::Agent;
use crate::llm::LlmError;

/// Guidance is a paragraph or two.
const REPLY_BUDGET: u32 = 1024;

#[derive(Debug, thiserror::Error)]
pub enum GuideError {
    #[error(transparent)]
    Llm(#[from] LlmError),

    #[error("the guide's reply was not the expected JSON (reply began {snippet:?})")]
    BadReply {
        snippet: String,
        #[source]
        source: serde_json::Error,
    },
}

/// One turn of the clarification conversation, for context on the next.
#[derive(Debug, Clone, Serialize)]
pub struct GuideTurn {
    /// True if the user said it, false if the guide did.
    pub from_user: bool,
    pub text: String,
}

/// What the guide needs to answer one message: the skill in question,
/// the user's roles (grounding, so it can connect to real experience),
/// the conversation so far, and the user's latest words.
// EXERCISE(EX-018)
#[derive(Serialize)]
pub struct GuideInput {
    pub skill: String,
    pub roles: Vec<String>,
    pub history: Vec<GuideTurn>,
    pub message: String,
}

/// The verification guide agent (PRD §6.3 family — the human-help side
/// of `SkillVerifyAgent`).
pub struct VerificationGuideAgent;

#[async_trait]
impl Agent for VerificationGuideAgent {
    type Input = GuideInput;
    type Wire = RawGuidance;
    type Output = String;
    type Error = GuideError;

    fn id(&self) -> &'static str {
        "verification_guide_v1"
    }
    fn system_prompt(&self) -> &str {
        SYSTEM_PROMPT
    }
    fn reply_budget(&self) -> u32 {
        REPLY_BUDGET
    }
    fn user_message(&self, input: &GuideInput) -> String {
        let mut text = format!("The skill in question: {}\n\n", input.skill);
        if !input.roles.is_empty() {
            text.push_str("The candidate's roles:\n");
            for role in &input.roles {
                text.push_str(&format!("- {role}\n"));
            }
            text.push('\n');
        }
        if !input.history.is_empty() {
            text.push_str("Conversation so far:\n");
            for turn in &input.history {
                let who = if turn.from_user { "Candidate" } else { "You" };
                text.push_str(&format!("{who}: {}\n", turn.text));
            }
            text.push('\n');
        }
        text.push_str(&format!("Candidate: {}", input.message));
        text
    }
    fn bad_reply(&self, snippet: String, source: serde_json::Error) -> GuideError {
        GuideError::BadReply { snippet, source }
    }
    fn assemble(&self, wire: RawGuidance, _input: GuideInput) -> Result<String, GuideError> {
        Ok(wire.reply)
    }
}

const SYSTEM_PROMPT: &str = r#"You help a job-seeker decide, honestly, whether their real experience matches a skill a job is asking for. You are talking with them during a resume-building interview.

How to help:
- Explain plainly what the skill usually means in practice — concrete examples of what doing it looks like.
- Ask about what they actually did. Connect to their real roles when you can.
- If their experience genuinely matches, say so clearly and encourage them to record it against the specific role it happened in.
- If it does NOT match — or only matches tangentially — say so plainly and kindly. A resume that claims skills the person cannot defend in an interview is worse than an honest gap.

Hard rules:
- You are an honest advisor, never a resume padder. NEVER tell the candidate to claim something they did not do, and never inflate a passing exposure into genuine experience.
- You record nothing. After your help, the candidate answers the yes/no question themselves.
- Keep it to a short, warm paragraph. End by nudging them toward the honest answer, whatever it is.

Reply with exactly one JSON object and nothing else — no markdown fences:
{"reply": "your guidance here"}"#;

#[derive(Debug, Deserialize)]
pub struct RawGuidance {
    #[serde(default)]
    reply: String,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::agent::AgentContext;
    use crate::llm::MockLlmClient;
    use crate::trace::Tracer;

    #[tokio::test]
    async fn the_guide_returns_the_models_reply_text() {
        let mock = MockLlmClient::default();
        mock.enqueue(
            r#"{"reply": "Data engineering usually means building pipelines that move and transform data at scale. Your Prometheum role's trade-settlement pipeline sounds like a genuine match."}"#,
        );
        let ctx = AgentContext {
            llm: &mock,
            model: &"m",
            tracer: &Tracer::DISABLED,
        };

        let reply = VerificationGuideAgent
            .run(
                &ctx,
                GuideInput {
                    skill: "Data Engineering".into(),
                    roles: vec!["Director — Prometheum".into()],
                    history: Vec::new(),
                    message: "I built a pipeline for trade data, does that count?".into(),
                },
            )
            .await
            .unwrap()
            .output;

        assert!(reply.contains("pipelines"));

        // The user message carries the skill, the roles, and the question.
        let sent = &mock.requests()[0].messages[0].content;
        assert!(sent.contains("Data Engineering"));
        assert!(sent.contains("Director — Prometheum"));
        assert!(sent.contains("trade data"));
        // The prompt forbids padding.
        assert!(
            mock.requests()[0]
                .system
                .as_deref()
                .unwrap()
                .contains("never a resume padder")
        );
    }

    #[tokio::test]
    #[ignore = "exercise: the guide sees only role titles and companies; feed it each role's strongest bullet text so its advice grounds in what the user actually did, then finish this test"]
    async fn ex_018_guidance_grounds_in_role_bullets() {
        // Once roles carry bullet text: build GuideInput whose role
        // includes a distinctive bullet, run the guide, and assert that
        // bullet text reaches the prompt (mock.requests()[0]).
        let grounding_implemented = false;
        assert!(grounding_implemented);
    }

    #[tokio::test]
    async fn prior_turns_are_replayed_for_context() {
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"reply": "Right — so it does count."}"#);
        let ctx = AgentContext {
            llm: &mock,
            model: &"m",
            tracer: &Tracer::DISABLED,
        };

        VerificationGuideAgent
            .run(
                &ctx,
                GuideInput {
                    skill: "Kafka".into(),
                    roles: Vec::new(),
                    history: vec![
                        GuideTurn {
                            from_user: true,
                            text: "what is it?".into(),
                        },
                        GuideTurn {
                            from_user: false,
                            text: "a streaming platform".into(),
                        },
                    ],
                    message: "ok I used that".into(),
                },
            )
            .await
            .unwrap();

        let sent = &mock.requests()[0].messages[0].content;
        assert!(sent.contains("Conversation so far"));
        assert!(sent.contains("Candidate: what is it?"));
        assert!(sent.contains("You: a streaming platform"));
    }
}
