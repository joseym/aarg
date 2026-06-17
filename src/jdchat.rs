//! JD chat (`aarg chat`): an honest advisor you can ask about a job posting
//! and how your recorded background fits it.
//!
//! After parsing a JD you can tailor against it, but you can't *ask about it*:
//! what the posting really prioritizes, the seniority bar, must-haves vs
//! nice-to-haves, and how your own experience stacks up. This is the
//! conversational twin of the verification guide (`guide.rs`): a single-shot
//! agent driven in a back-and-forth loop, given the posting, your recorded
//! experience, and the conversation so far.
//!
//! It is **read-only and advisory**, like the guide and the reviewer: it
//! answers questions, it records nothing, and it produces no resume content.
//! The never-fabricate guards govern resume *output*; the chat emits none, so
//! they do not apply. Fit answers stay honest a different way: the agent is
//! given ONLY the recorded dataset and a prompt that forbids claiming
//! unrecorded experience. Anything the user acts on still reaches a resume
//! only through the guarded tailoring flow, never this chat.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::agent::{Agent, AgentContext};
use crate::dataset::types::ResumeDataset;
use crate::jd::JobRequirements;
use crate::llm::LlmError;
use crate::style::Spinner;
use crate::user::{Answer, AskError, Question, UserHandle};

/// An answer is a short paragraph.
const REPLY_BUDGET: u32 = 1024;

#[derive(Debug, thiserror::Error)]
pub enum JdChatError {
    #[error(transparent)]
    Llm(#[from] LlmError),

    #[error("the chat reply was not the expected JSON (reply began {snippet:?})")]
    BadReply {
        snippet: String,
        #[source]
        source: serde_json::Error,
    },
}

/// One turn of the conversation, replayed for context on the next.
#[derive(Debug, Clone, Serialize)]
pub struct ChatTurn {
    /// True if the user said it, false if the assistant did.
    pub from_user: bool,
    pub text: String,
}

/// A compact view of the recorded dataset, for grounding fit answers: each
/// role with its bullets (metrics folded in), the skill names, and the
/// summary. The candidate's whole background is fair game for fit, so this is
/// not scoped the way a single bullet's evidence is.
#[derive(Debug, Clone, Serialize)]
pub struct CareerDigest {
    pub summary: Option<String>,
    pub roles: Vec<RoleBrief>,
    pub skills: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RoleBrief {
    pub title: String,
    pub company: String,
    pub bullets: Vec<String>,
}

/// Build the digest from the dataset.
fn digest(dataset: &ResumeDataset) -> CareerDigest {
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
    CareerDigest {
        summary: dataset.summary.clone(),
        roles,
        skills,
    }
}

/// What the chat agent needs to answer one message: the posting, the
/// candidate's recorded background, the conversation so far, and their latest
/// words.
#[derive(Serialize)]
pub struct JdChatInput {
    pub jd: JobRequirements,
    pub career: CareerDigest,
    pub history: Vec<ChatTurn>,
    pub message: String,
}

/// The JD-chat agent: interprets the posting and grounds fit answers in the
/// recorded background.
pub struct JdChatAgent;

#[async_trait]
impl Agent for JdChatAgent {
    type Input = JdChatInput;
    type Wire = RawChat;
    type Output = String;
    type Error = JdChatError;

    fn id(&self) -> &'static str {
        "jd_chat_v1"
    }
    fn system_prompt(&self) -> &str {
        SYSTEM_PROMPT
    }
    fn reply_budget(&self) -> u32 {
        REPLY_BUDGET
    }
    fn user_message(&self, input: &JdChatInput) -> String {
        let mut text = String::from("THE POSTING\n");
        text.push_str(&render_jd(&input.jd));

        text.push_str("\nTHE CANDIDATE'S RECORDED BACKGROUND\n");
        if let Some(summary) = &input.career.summary {
            text.push_str(&format!("Summary: {summary}\n"));
        }
        for role in &input.career.roles {
            text.push_str(&format!("\n{} at {}\n", role.title, role.company));
            for bullet in &role.bullets {
                text.push_str(&format!("  - {bullet}\n"));
            }
        }
        if !input.career.skills.is_empty() {
            text.push_str(&format!("\nSkills: {}\n", input.career.skills.join(", ")));
        }

        if !input.history.is_empty() {
            text.push_str("\nCONVERSATION SO FAR\n");
            for turn in &input.history {
                let who = if turn.from_user { "Candidate" } else { "You" };
                text.push_str(&format!("{who}: {}\n", turn.text));
            }
        }

        text.push_str(&format!("\nCandidate: {}", input.message));
        text
    }
    fn bad_reply(&self, snippet: String, source: serde_json::Error) -> JdChatError {
        JdChatError::BadReply { snippet, source }
    }
    fn assemble(&self, wire: RawChat, _input: JdChatInput) -> Result<String, JdChatError> {
        Ok(wire.reply)
    }
}

/// Render the structured posting compactly for the prompt.
fn render_jd(jd: &JobRequirements) -> String {
    let mut text = format!("Role: {} at {}\n", jd.title, jd.company);
    text.push_str(&format!(
        "Seniority: {:?}  Remote: {:?}  Location: {}\n",
        jd.seniority,
        jd.remote,
        jd.location.as_deref().unwrap_or("unstated")
    ));
    if !jd.domain_keywords.is_empty() {
        text.push_str(&format!("Domain: {}\n", jd.domain_keywords.join(", ")));
    }
    let names = |skills: &[crate::jd::JdSkill]| {
        skills
            .iter()
            .map(|s| s.name.clone())
            .collect::<Vec<_>>()
            .join(", ")
    };
    if !jd.required_skills.is_empty() {
        text.push_str(&format!("Required: {}\n", names(&jd.required_skills)));
    }
    if !jd.preferred_skills.is_empty() {
        text.push_str(&format!("Preferred: {}\n", names(&jd.preferred_skills)));
    }
    if !jd.responsibilities.is_empty() {
        text.push_str("Responsibilities:\n");
        for duty in &jd.responsibilities {
            text.push_str(&format!("  - {duty}\n"));
        }
    }
    text
}

const SYSTEM_PROMPT: &str = r#"You are an honest advisor helping a job-seeker understand a specific job posting and how their real background fits it. You are given the posting, the candidate's recorded experience, the conversation so far, and their latest message.

How to help:
- Interpret the posting from what it actually says: what the role really prioritizes, the seniority and scope, must-haves versus nice-to-haves, and any red flags. Do not invent requirements the posting does not state.
- When you discuss the candidate, use ONLY the recorded experience you were given. Never claim they have a skill, a metric, a scope, or a role that is not in that record. If their record is thin on something the posting wants, say so plainly. An honest gap is more useful to them than a flattering guess.
- Be concrete and specific, and keep answers short. When you say something fits, point to the actual role or line that supports it.

Hard rules:
- You are an advisor, not a resume writer. You give guidance; you produce no resume text and you record nothing. The candidate acts on your advice through the normal tailoring flow.
- Never tell the candidate to claim something they did not do, and never inflate a passing exposure into genuine experience.
- Use no em-dashes. Join clauses with a comma, with "and", or as a second sentence.

Reply with exactly one JSON object and nothing else, no markdown fences:
{"reply": "your answer here"}"#;

#[derive(Debug, Deserialize)]
pub struct RawChat {
    #[serde(default)]
    reply: String,
}

/// Run the chat loop: ask, answer, repeat until the user enters a blank line.
/// Read-only - it never touches the dataset. An agent error degrades to a
/// notice and keeps the session alive.
pub async fn chat(
    jd: &JobRequirements,
    dataset: &ResumeDataset,
    user: &dyn UserHandle,
    ctx: &AgentContext<'_>,
) -> Result<(), AskError> {
    let career = digest(dataset);
    let role = if jd.title.is_empty() {
        "this role".to_string()
    } else {
        jd.title.clone()
    };
    user.notify(&format!(
        "Ask anything about {role}: what they want, how you fit, what to lead with. Blank line to exit."
    ));

    let mut history: Vec<ChatTurn> = Vec::new();
    loop {
        let message = match user
            .ask(Question::Text {
                prompt: "ask about the role (blank to exit)".to_string(),
            })
            .await?
        {
            Answer::Text(text) if !text.trim().is_empty() => text.trim().to_string(),
            _ => break,
        };

        let sp = Spinner::start("thinking");
        let input = JdChatInput {
            jd: jd.clone(),
            career: career.clone(),
            history: history.clone(),
            message: message.clone(),
        };
        match JdChatAgent.run(ctx, input).await {
            Ok(run) => {
                sp.clear();
                user.notify(&run.output);
                history.push(ChatTurn {
                    from_user: true,
                    text: message,
                });
                history.push(ChatTurn {
                    from_user: false,
                    text: run.output,
                });
            }
            // An advisor that can't be reached shouldn't end the session.
            Err(_) => {
                sp.clear();
                user.notify("(couldn't reach the assistant just now; try asking again)");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::dataset::types::SkillCategory;
    use crate::dataset::types::{
        Bullet, BulletId, Contact, EmploymentType, Role, RoleId, Strength, YearMonth,
    };
    use crate::jd::{Importance, JdSkill, RemotePolicy, Seniority};
    use crate::llm::MockLlmClient;
    use crate::trace::Tracer;
    use crate::user::ScriptedUser;

    fn sample_jd() -> JobRequirements {
        JobRequirements {
            company: "Hightouch".into(),
            title: "Staff Engineer".into(),
            seniority: Seniority::Senior,
            location: None,
            remote: RemotePolicy::Remote,
            domain_keywords: vec!["data".into()],
            required_skills: vec![JdSkill {
                name: "Reliability engineering".into(),
                category: SkillCategory::Hard,
                importance: Importance::Critical,
                context_phrases: Vec::new(),
            }],
            preferred_skills: Vec::new(),
            responsibilities: vec!["Own reliability for a high-growth platform".into()],
            ats_phrases: Vec::new(),
            raw_text: "Staff Engineer, reliability at scale.".into(),
            source_url: None,
        }
    }

    fn sample_dataset() -> ResumeDataset {
        let mut d = ResumeDataset::new(Contact {
            full_name: "Ada".into(),
            email: "ada@example.com".into(),
            phone: None,
            location: None,
            links: Vec::new(),
        });
        d.summary = Some("Engineering leader.".into());
        d.roles.push(Role {
            id: RoleId("role-1".into()),
            company: "Prometheum".into(),
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
                text: "Built the on-call rotation and ran incident response".into(),
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
    async fn the_agent_grounds_in_the_posting_and_recorded_background() {
        let mock = MockLlmClient::default();
        mock.enqueue(
            r#"{"reply": "Reliability leads the posting; your on-call work maps to it."}"#,
        );

        let reply = JdChatAgent
            .run(
                &ctx(&mock),
                JdChatInput {
                    jd: sample_jd(),
                    career: digest(&sample_dataset()),
                    history: Vec::new(),
                    message: "how do I fit?".into(),
                },
            )
            .await
            .unwrap()
            .output;

        assert!(reply.contains("on-call"));
        // The prompt carries the posting, a recorded role, and a skill.
        let sent = &mock.requests()[0].messages[0].content;
        assert!(sent.contains("Staff Engineer"));
        assert!(sent.contains("Reliability engineering"));
        assert!(sent.contains("Director of Engineering"));
        assert!(sent.contains("on-call rotation"));
        // The prompt forbids inventing the candidate's experience.
        assert!(
            mock.requests()[0]
                .system
                .as_deref()
                .unwrap()
                .contains("only the recorded experience")
                || mock.requests()[0]
                    .system
                    .as_deref()
                    .unwrap()
                    .contains("ONLY the recorded experience")
        );
    }

    #[tokio::test]
    async fn prior_turns_are_replayed_for_context() {
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"reply": "Yes, lead with that."}"#);

        JdChatAgent
            .run(
                &ctx(&mock),
                JdChatInput {
                    jd: sample_jd(),
                    career: digest(&sample_dataset()),
                    history: vec![
                        ChatTurn {
                            from_user: true,
                            text: "what matters most?".into(),
                        },
                        ChatTurn {
                            from_user: false,
                            text: "reliability at scale".into(),
                        },
                    ],
                    message: "should I lead with on-call?".into(),
                },
            )
            .await
            .unwrap();

        let sent = &mock.requests()[0].messages[0].content;
        assert!(sent.contains("CONVERSATION SO FAR"));
        assert!(sent.contains("Candidate: what matters most?"));
        assert!(sent.contains("You: reliability at scale"));
    }

    #[tokio::test]
    async fn the_loop_answers_each_question_and_exits_on_a_blank_line() {
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"reply": "First answer."}"#);
        mock.enqueue(r#"{"reply": "Second answer."}"#);
        let user = ScriptedUser::new();
        user.answer(Answer::Text("what do they want?".into()));
        user.answer(Answer::Text("how do I fit?".into()));
        user.answer(Answer::Text("   ".into())); // blank exits

        // Read-only by construction: `chat` takes `&ResumeDataset`, so it
        // cannot mutate it. Snapshot anyway to document the intent.
        let dataset = sample_dataset();
        let before = dataset.clone();
        chat(&sample_jd(), &dataset, &user, &ctx(&mock))
            .await
            .unwrap();

        // One agent call per question, both replies shown to the user.
        assert_eq!(mock.requests().len(), 2);
        let notices = user.notices();
        assert!(notices.iter().any(|n| n.contains("First answer.")));
        assert!(notices.iter().any(|n| n.contains("Second answer.")));
        assert_eq!(dataset, before);
    }

    #[tokio::test]
    async fn an_agent_error_keeps_the_session_alive() {
        let mock = MockLlmClient::default(); // no replies enqueued -> the call errors
        let user = ScriptedUser::new();
        user.answer(Answer::Text("anything?".into()));
        user.answer(Answer::Text("".into())); // blank exits after the soft notice

        let dataset = sample_dataset();
        let result = chat(&sample_jd(), &dataset, &user, &ctx(&mock)).await;

        assert!(result.is_ok());
        assert!(
            user.notices()
                .iter()
                .any(|n| n.contains("couldn't reach the assistant"))
        );
    }
}
