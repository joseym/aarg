//! JD chat (`aarg chat`): an honest advisor you can ask about a job posting
//! and how your recorded background fits it.
//!
//! The chat *engine* — the agent, the never-fabricate honesty prompt, the
//! grounding digest, and the streaming turn — lives in
//! [`aarg_domain::jdchat`], portable so the browser chat panel drives the same
//! code. This file is the CLI shell: the interactive ask/answer loop over a
//! terminal [`UserHandle`], with a spinner and soft error recovery. It runs the
//! buffered, JSON-envelope path ([`JdChatAgent`]); the browser runs
//! [`aarg_domain::jdchat::stream_reply`] instead.
//!
//! It is **read-only and advisory**: it answers questions, records nothing, and
//! produces no resume content, so the never-fabricate guards (which govern
//! resume *output*) do not apply. Fit answers stay honest because the agent is
//! given ONLY the recorded dataset and a prompt that forbids claiming
//! unrecorded experience.

// Re-export the engine so `crate::jdchat::{JdChatAgent, JdChatInput, ...}` keeps
// resolving after the extraction into `aarg-domain`.
pub use aarg_domain::jdchat::{
    BuildContext, CareerDigest, ChatTurn, JdChatAgent, JdChatError, JdChatInput, RawChat,
    RoleBrief, digest, stream_reply,
};

use crate::agent::{Agent, AgentContext};
use crate::dataset::types::ResumeDataset;
use crate::jd::JobRequirements;
use crate::style::Spinner;
use crate::user::{Answer, AskError, Question, UserHandle};

/// Run the chat loop: ask, answer, repeat until the user enters a blank line.
/// Read-only - it never touches the dataset. An agent error degrades to a
/// notice and keeps the session alive. This CLI path has no open build, so it
/// grounds in the posting and recorded background only.
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
            build: None,
            history: history.clone(),
            message: message.clone(),
        };
        match JdChatAgent::new(input.build.is_some())
            .run(ctx, input)
            .await
        {
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
    use crate::agent::AgentContext;
    use crate::dataset::types::{
        Bullet, BulletId, Contact, EmploymentType, Role, RoleId, Strength, YearMonth,
    };
    use crate::jd::{Importance, JdSkill, RemotePolicy, Seniority};
    use crate::llm::MockLlmClient;
    use crate::trace::Tracer;
    use aarg_domain::dataset::types::SkillCategory;

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
    async fn the_loop_answers_each_question_and_exits_on_a_blank_line() {
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"reply": "First answer."}"#);
        mock.enqueue(r#"{"reply": "Second answer."}"#);
        let user = crate::user::ScriptedUser::new();
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
        let user = crate::user::ScriptedUser::new();
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
