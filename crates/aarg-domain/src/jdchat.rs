//! The chat engine (`aarg chat`, and the browser chat panel): an honest
//! advisor you can ask about a job posting, an in-progress build, and how your
//! recorded background fits.
//!
//! After parsing a JD you can tailor against it, but you can't *ask about it*:
//! what the posting really prioritizes, the seniority bar, must-haves vs
//! nice-to-haves, and how your own experience stacks up. Once a build exists
//! you'll want more: why a bullet scored low, what an interviewer might probe,
//! which recorded experience the resume left on the table. This module answers
//! all of it from the same grounded material.
//!
//! It is **read-only and advisory**, like the guide and the reviewer: it
//! answers questions, records nothing, and produces no resume content. The
//! never-fabricate guards govern resume *output*; the chat emits none, so they
//! do not apply. Fit answers stay honest a different way: the agent is given
//! ONLY the recorded dataset (and, when present, the draft that shipped) plus a
//! prompt that forbids claiming unrecorded experience. Anything the user acts
//! on still reaches a resume only through the guarded tailoring flow, never
//! this chat.
//!
//! Two ways to drive it, one set of honesty rules:
//! - [`JdChatAgent`] runs on the `Agent` trait and replies in a `{"reply":...}`
//!   JSON envelope, buffered — the shape the CLI parses. History is flattened
//!   into one user message.
//! - [`stream_reply`] streams plain assistant prose (no envelope) over native
//!   multi-turn messages — the shape a browser SSE delta path wants. It reuses
//!   the exact same system prompt and grounding assembly, so the honesty logic
//!   lives in one place.

use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::agent::{Agent, AgentContext, ModelTier};
use crate::dataset::types::ResumeDataset;
use crate::jd::JobRequirements;
use crate::llm::{CompletionRequest, LlmError, Message, StreamEvent, TokenUsage};
use crate::review::{AdversarialReport, format_objection};
use crate::tailor::TailoredResume;

/// An answer is a short paragraph.
const REPLY_BUDGET: u32 = 1024;

/// The versioned id used in traces and as the model-resolver key, shared by
/// both the buffered agent and the streaming turn.
const CHAT_AGENT_ID: &str = "jd_chat_v1";

/// The model tier chat runs on, the single source of truth for both paths.
/// Chat is moderate-judgment advice (interpreting a posting, weighing fit,
/// naming an honest gap), the same class of work the `Agent` trait defaults
/// to, so it stays on `Mid`. Both the buffered agent and the streaming turn
/// resolve this exact tier under the shared `CHAT_AGENT_ID`, so the same
/// conversation costs the same whichever path drives it. The streaming path
/// does not depend on the tier to stream, it drives `LlmClient::stream`
/// directly.
const CHAT_TIER: ModelTier = ModelTier::Mid;

/// How many prior turns (a turn is one message, user *or* assistant) a request
/// carries. The conversation grows without bound in a long session, but the
/// context window does not, so only the most recent `HISTORY_TURNS` are
/// replayed. 20 messages is roughly ten exchanges, enough for continuity
/// without crowding out the posting, the background, and the build.
const HISTORY_TURNS: usize = 20;

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

/// One turn of the conversation, replayed for context on the next. `Deserialize`
/// too, so a browser can hand back the prior transcript as JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatTurn {
    /// True if the user said it, false if the assistant did.
    pub from_user: bool,
    pub text: String,
}

/// A compact view of the recorded dataset, for grounding fit answers: each
/// role with its bullets (metrics folded in), the skill names, and the
/// summary. The candidate's whole background is fair game for fit, so this is
/// not scoped the way a single bullet's evidence is.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CareerDigest {
    pub summary: Option<String>,
    pub roles: Vec<RoleBrief>,
    pub skills: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleBrief {
    pub title: String,
    pub company: String,
    pub bullets: Vec<String>,
}

/// The open build a turn can be grounded in: the canonical draft that actually
/// shipped (its real bullets and skills) and, optionally, the reviewer's
/// verdict on it. Additive — a conversation with a JD but no build simply
/// leaves this `None`, and the pre-build fit chat works exactly as before.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildContext {
    /// The canonical `TailoredResume` the build produced.
    pub draft: TailoredResume,
    /// The adversarial reviewer's objections and score, when the build has
    /// been reviewed. Absent for a draft that hasn't reached review yet.
    #[serde(default)]
    pub report: Option<AdversarialReport>,
}

/// Build the digest from the dataset.
pub fn digest(dataset: &ResumeDataset) -> CareerDigest {
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

/// What the chat needs to answer one message: the posting, the candidate's
/// recorded background, the open build (if any), the conversation so far, and
/// their latest words.
#[derive(Serialize)]
pub struct JdChatInput {
    pub jd: JobRequirements,
    pub career: CareerDigest,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build: Option<BuildContext>,
    pub history: Vec<ChatTurn>,
    pub message: String,
}

/// The JD-chat agent: interprets the posting, grounds fit answers in the
/// recorded background, and (when a build is present) reasons about the draft
/// that shipped. Holds its composed system prompt so it can vary with whether
/// a build is in play while `system_prompt` stays a borrow.
pub struct JdChatAgent {
    system_prompt: String,
}

impl JdChatAgent {
    /// Build the agent for one turn. `has_build` selects whether the system
    /// prompt gains the build-aware guidance; the JSON envelope instruction is
    /// always appended, because this agent is the buffered, envelope-parsing
    /// path (the browser uses [`stream_reply`], which omits the envelope).
    pub fn new(has_build: bool) -> Self {
        Self {
            system_prompt: compose_system_prompt(has_build, true),
        }
    }
}

#[async_trait]
impl Agent for JdChatAgent {
    type Input = JdChatInput;
    type Wire = RawChat;
    type Output = String;
    type Error = JdChatError;

    fn id(&self) -> &'static str {
        CHAT_AGENT_ID
    }
    fn system_prompt(&self) -> &str {
        &self.system_prompt
    }
    fn reply_budget(&self) -> u32 {
        REPLY_BUDGET
    }
    fn model_tier(&self) -> ModelTier {
        CHAT_TIER
    }
    fn user_message(&self, input: &JdChatInput) -> String {
        // The buffered path flattens everything into one user message: the
        // grounding block, then the conversation so far, then the latest line.
        // It replays the WHOLE history (not the streaming path's cap): the CLI
        // never streamed and its context budget was fine unbounded, so this is
        // byte-identical to the pre-extraction command.
        let mut text = grounding_block(&input.jd, &input.career, input.build.as_ref());

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
    // TOOLS SEAM: `tools()` defaults to empty, so today the chat is pure
    // advice. When a future phase gives the chat a capability (look up a skill,
    // fetch the JD source), populate `tools()` here; the buffered path already
    // dispatches them through the `Agent` spine, and `stream_reply` documents
    // the matching round it would grow.
}

/// Stream one chat turn as plain assistant prose — no JSON envelope — over
/// native multi-turn messages, for a browser SSE delta path. Returns the whole
/// assistant reply once the stream ends; when a [`crate::agent::StreamSink`] is
/// set on the context, it also drives `begin`/`delta`/`end` so a host can
/// render the text as it arrives.
///
/// The honesty is identical to the buffered agent: the same composed system
/// prompt (built by [`compose_system_prompt`], minus the envelope instruction)
/// and the same [`grounding_block`]. Only the *transport* differs — real
/// `User`/`Assistant` messages instead of one flattened user turn, and raw
/// text instead of `{"reply":...}` — because a clean assistant turn is what
/// streams well.
pub async fn stream_reply(
    ctx: &AgentContext<'_>,
    input: &JdChatInput,
) -> Result<String, JdChatError> {
    // The grounding rides in the system prompt for the streaming path, leaving
    // `messages` as pure conversation — the shape native multi-turn wants.
    let mut system = compose_system_prompt(input.build.is_some(), false);
    system.push_str("\n\n");
    system.push_str(&grounding_block(
        &input.jd,
        &input.career,
        input.build.as_ref(),
    ));

    // Prior turns become real messages (the flatten-into-one-user trick the
    // buffered path uses does not stream cleanly), capped to the most recent
    // exchanges, then the latest user line.
    let mut messages: Vec<Message> = cap_history(&input.history)
        .iter()
        .map(|turn| {
            if turn.from_user {
                Message::user(turn.text.clone())
            } else {
                Message::assistant(turn.text.clone())
            }
        })
        .collect();
    messages.push(Message::user(input.message.clone()));

    let model = ctx.model.resolve(CHAT_AGENT_ID, CHAT_TIER).to_string();
    let request = CompletionRequest {
        model,
        max_tokens: REPLY_BUDGET,
        system: Some(system),
        messages,
        temperature: None,
        // TOOLS SEAM: empty today. To give the streaming chat tools, build
        // this from `JdChatAgent`'s `tools()` and add a non-streaming
        // tool-dispatch round before the final streamed answer, the way the
        // `Agent` spine (`run_agent`) does. Left empty so the turn streams.
        tools: Vec::new(),
    };

    stream_and_collect(ctx, request).await
}

/// Drive one streaming model call to completion, feeding the sink if present
/// and accumulating the full text. Mirrors the `Agent` spine's stream handling
/// closely enough to behave the same, kept local so this raw-text path owns no
/// JSON-envelope assumptions.
async fn stream_and_collect(
    ctx: &AgentContext<'_>,
    request: CompletionRequest,
) -> Result<String, JdChatError> {
    let model = request.model.clone();
    let mut events = ctx.llm.stream(request).await?;
    if let Some(sink) = ctx.sink {
        sink.begin(CHAT_AGENT_ID, &model);
    }

    let mut text = String::new();
    let mut usage = TokenUsage::default();
    let mut stream_error = None;
    while let Some(event) = events.next().await {
        match event {
            Ok(StreamEvent::TextDelta(chunk)) => {
                if let Some(sink) = ctx.sink {
                    sink.delta(&chunk);
                }
                text.push_str(&chunk);
            }
            Ok(StreamEvent::Done {
                usage: final_usage, ..
            }) => usage = final_usage,
            Err(error) => {
                stream_error = Some(error);
                break;
            }
        }
    }

    if let Some(sink) = ctx.sink {
        sink.end(usage);
    }
    if let Some(error) = stream_error {
        return Err(error.into());
    }
    Ok(text)
}

/// The most recent `HISTORY_TURNS` messages, oldest-first. A short session
/// returns everything; a long one drops the earliest turns so the request
/// stays within a sane context budget. Streaming-only: the buffered CLI path
/// replays the whole history.
fn cap_history(history: &[ChatTurn]) -> &[ChatTurn] {
    let start = history.len().saturating_sub(HISTORY_TURNS);
    &history[start..]
}

/// The grounding shared by both paths: the posting, the recorded background,
/// and (when present) the open build. Everything the model is allowed to
/// reason from, and nothing it isn't.
fn grounding_block(
    jd: &JobRequirements,
    career: &CareerDigest,
    build: Option<&BuildContext>,
) -> String {
    let mut text = String::from("THE POSTING\n");
    text.push_str(&render_jd(jd));

    text.push_str("\nTHE CANDIDATE'S RECORDED BACKGROUND\n");
    if let Some(summary) = &career.summary {
        text.push_str(&format!("Summary: {summary}\n"));
    }
    for role in &career.roles {
        text.push_str(&format!("\n{} at {}\n", role.title, role.company));
        for bullet in &role.bullets {
            text.push_str(&format!("  - {bullet}\n"));
        }
    }
    if !career.skills.is_empty() {
        text.push_str(&format!("\nSkills: {}\n", career.skills.join(", ")));
    }

    if let Some(build) = build {
        text.push('\n');
        text.push_str(&render_build(build));
    }

    text
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

/// Render the open build: the draft that shipped (headline, summary, each
/// role's tailored bullets, the skills line) and the reviewer's verdict
/// (score, notes, and each objection as its one-line form). This is the same
/// material the CLI shows after a build, laid out for the model to reason over.
fn render_build(build: &BuildContext) -> String {
    let draft = &build.draft;
    let mut text = String::from("THE TAILORED RESUME THAT SHIPPED\n");
    if let Some(title) = &draft.target_title {
        text.push_str(&format!("Target role: {title}\n"));
    }
    if !draft.summary.is_empty() {
        text.push_str(&format!("Summary: {}\n", draft.summary));
    }
    for role in &draft.roles {
        text.push_str(&format!("\n{} at {}\n", role.title, role.company));
        for bullet in &role.bullets {
            text.push_str(&format!("  - {}\n", bullet.text));
        }
    }
    if !draft.skills_section.skills.is_empty() {
        text.push_str(&format!(
            "\nSkills on the resume: {}\n",
            draft.skills_section.skills.join(", ")
        ));
    }

    if let Some(report) = &build.report {
        text.push_str(&format!(
            "\nTHE REVIEWER'S TAKE\nScore: {:.2} out of 1.0\n",
            report.overall_score
        ));
        if !report.persona_notes.is_empty() {
            text.push_str(&format!("Notes: {}\n", report.persona_notes));
        }
        if !report.objections.is_empty() {
            text.push_str("Objections:\n");
            for objection in &report.objections {
                text.push_str(&format!("  - {}\n", format_objection(objection)));
            }
        }
    }

    text
}

/// Assemble the system prompt from the shared honesty rules, the optional
/// build-aware addendum, and the optional JSON-envelope instruction. One
/// honesty text, composed for whichever path asks — so the never-fabricate
/// posture is stated once, never duplicated across the buffered and streaming
/// entry points.
fn compose_system_prompt(has_build: bool, envelope: bool) -> String {
    let mut prompt = String::from(HONESTY_PROMPT);
    if has_build {
        prompt.push_str("\n\n");
        prompt.push_str(BUILD_ADDENDUM);
    }
    if envelope {
        prompt.push_str("\n\n");
        prompt.push_str(ENVELOPE_INSTRUCTION);
    }
    prompt
}

/// The honesty contract, shared by every chat turn. Deliberately says nothing
/// about the reply format (that is the caller's concern): the buffered path
/// appends the JSON-envelope instruction, the streaming path does not.
const HONESTY_PROMPT: &str = r#"You are an honest advisor helping a job-seeker understand a specific job posting and how their real background fits it. You are given the posting, the candidate's recorded experience, the conversation so far, and their latest message.

How to help:
- Interpret the posting from what it actually says: what the role really prioritizes, the seniority and scope, must-haves versus nice-to-haves, and any red flags. Do not invent requirements the posting does not state.
- When you discuss the candidate, use ONLY the recorded experience you were given. Never claim they have a skill, a metric, a scope, or a role that is not in that record. If their record is thin on something the posting wants, say so plainly. An honest gap is more useful to them than a flattering guess.
- Be concrete and specific, and keep answers short. When you say something fits, point to the actual role or line that supports it.

Hard rules:
- You are an advisor, not a resume writer. You give guidance; you produce no resume text and you record nothing. The candidate acts on your advice through the normal tailoring flow.
- Never tell the candidate to claim something they did not do, and never inflate a passing exposure into genuine experience.
- Use no em-dashes. Join clauses with a comma, with "and", or as a second sentence."#;

/// Added when the turn includes an open build, so the advisor can reason about
/// the draft that shipped and the reviewer's verdict without ever loosening the
/// never-fabricate line.
const BUILD_ADDENDUM: &str = r#"This conversation also has an OPEN BUILD: the tailored resume that shipped and, if it was reviewed, the skeptical reviewer's objections and score. You may reason about it:
- Explain why a bullet or the draft as a whole scored the way it did, grounding your explanation in the reviewer's actual objections.
- If asked what an interviewer might probe, base it on what is on the resume and in the recorded background, never on invented detail.
- If asked what relevant experience the resume left out, compare the recorded background against the shipped draft and name the recorded items the draft did not use. Only name experience that is actually in the record.
The same rule holds: the draft and the record are the floor on what you may say, not a thing you may add to."#;

/// The reply-format instruction the buffered (CLI) path appends. The streaming
/// path omits it so the model answers in plain prose.
const ENVELOPE_INSTRUCTION: &str = r#"Reply with exactly one JSON object and nothing else, no markdown fences:
{"reply": "your answer here"}"#;

#[derive(Debug, Deserialize)]
pub struct RawChat {
    #[serde(default)]
    pub reply: String,
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
    use crate::review::{Objection, ObjectionKind, ObjectionScope, ObjectionTarget, Severity};
    use crate::tailor::{
        BuildId, JdId, SkillsSection, TailoredBullet, TailoredResume, TailoredRole,
    };
    use crate::trace::Tracer;

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

    /// A build whose shipped draft used one recorded bullet, plus a reviewer
    /// verdict with a single scored objection.
    fn sample_build() -> BuildContext {
        let draft = TailoredResume {
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
            roles: vec![TailoredRole {
                id: RoleId("role-1".into()),
                company: "Prometheum".into(),
                title: "Director of Engineering".into(),
                start: YearMonth {
                    year: 2020,
                    month: 1,
                },
                end: None,
                location: None,
                bullets: vec![TailoredBullet {
                    source_id: BulletId("bullet-1".into()),
                    text: "Built the on-call rotation and ran incident response".into(),
                }],
            }],
            education: Vec::new(),
            skills_section: SkillsSection {
                skills: vec!["Reliability engineering".into()],
            },
            projects: Vec::new(),
            achievements: Vec::new(),
            certifications: Vec::new(),
        };
        let report = AdversarialReport {
            objections: vec![Objection {
                target: ObjectionTarget::Bullet(BulletId("bullet-1".into())),
                severity: Severity::Major,
                kind: ObjectionKind::NoMetric,
                scope: ObjectionScope::Canonical,
                message: "the on-call bullet has no number".into(),
                suggestion: Some("add the incident volume or MTTR".into()),
            }],
            overall_score: 0.62,
            persona_notes: "Solid but under-quantified.".into(),
        };
        BuildContext {
            draft,
            report: Some(report),
        }
    }

    fn ctx<'a>(mock: &'a MockLlmClient) -> AgentContext<'a> {
        AgentContext {
            llm: mock,
            model: &"m",
            tracer: &Tracer::DISABLED,
            sink: None,
        }
    }

    fn input(build: Option<BuildContext>, history: Vec<ChatTurn>, message: &str) -> JdChatInput {
        JdChatInput {
            jd: sample_jd(),
            career: digest(&sample_dataset()),
            build,
            history,
            message: message.into(),
        }
    }

    #[tokio::test]
    async fn the_agent_grounds_in_the_posting_and_recorded_background() {
        let mock = MockLlmClient::default();
        mock.enqueue(
            r#"{"reply": "Reliability leads the posting; your on-call work maps to it."}"#,
        );

        let reply = JdChatAgent::new(false)
            .run(&ctx(&mock), input(None, Vec::new(), "how do I fit?"))
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
        let requests = mock.requests();
        let system = requests[0].system.as_deref().unwrap();
        assert!(system.contains("ONLY the recorded experience"));
        // No build was present, so the build-aware guidance is absent.
        assert!(!system.contains("OPEN BUILD"));
    }

    #[tokio::test]
    async fn a_build_turn_grounds_in_the_draft_and_the_reviewer() {
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"reply": "It scored 0.62 because the on-call bullet lacks a metric."}"#);

        JdChatAgent::new(true)
            .run(
                &ctx(&mock),
                input(
                    Some(sample_build()),
                    Vec::new(),
                    "why did that bullet score low?",
                ),
            )
            .await
            .unwrap();

        // The user message carries the shipped draft and the reviewer verdict.
        let sent = &mock.requests()[0].messages[0].content;
        assert!(sent.contains("THE TAILORED RESUME THAT SHIPPED"));
        assert!(sent.contains("THE REVIEWER'S TAKE"));
        assert!(sent.contains("0.62"));
        assert!(sent.contains("no metric")); // the objection's kind, formatted
        // The system prompt gains the build-aware guidance, and still forbids
        // invention (the honesty rule is shared, never dropped).
        let requests = mock.requests();
        let system = requests[0].system.as_deref().unwrap();
        assert!(system.contains("OPEN BUILD"));
        assert!(system.contains("ONLY the recorded experience"));
    }

    #[test]
    fn build_context_is_additive_absent_by_default() {
        // A JD-only turn assembles without any build material, so the pre-build
        // conversation is unaffected by the build-aware code paths.
        let with = JdChatAgent::new(false);
        let grounding = with.user_message(&input(None, Vec::new(), "hi"));
        assert!(!grounding.contains("THE TAILORED RESUME THAT SHIPPED"));
        assert!(!with.system_prompt().contains("OPEN BUILD"));
    }

    #[tokio::test]
    async fn prior_turns_are_replayed_for_context() {
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"reply": "Yes, lead with that."}"#);

        let history = vec![
            ChatTurn {
                from_user: true,
                text: "what matters most?".into(),
            },
            ChatTurn {
                from_user: false,
                text: "reliability at scale".into(),
            },
        ];
        JdChatAgent::new(false)
            .run(
                &ctx(&mock),
                input(None, history, "should I lead with on-call?"),
            )
            .await
            .unwrap();

        let sent = &mock.requests()[0].messages[0].content;
        assert!(sent.contains("CONVERSATION SO FAR"));
        assert!(sent.contains("Candidate: what matters most?"));
        assert!(sent.contains("You: reliability at scale"));
    }

    #[tokio::test]
    async fn the_streaming_turn_returns_raw_text_over_native_multi_turn() {
        let mock = MockLlmClient::default();
        // Plain prose, not a JSON envelope: the streaming path returns it as-is.
        mock.enqueue("Reliability leads the posting, and your on-call work maps to it.");

        let history = vec![
            ChatTurn {
                from_user: true,
                text: "what matters most?".into(),
            },
            ChatTurn {
                from_user: false,
                text: "reliability at scale".into(),
            },
        ];
        let reply = stream_reply(
            &ctx(&mock),
            &input(
                Some(sample_build()),
                history,
                "why did that bullet score low?",
            ),
        )
        .await
        .unwrap();

        // The raw assistant text comes back verbatim, no envelope parsing.
        assert_eq!(
            reply,
            "Reliability leads the posting, and your on-call work maps to it."
        );

        let request = &mock.requests()[0];
        // Native multi-turn: real user/assistant messages, not one flattened
        // user turn. Two history turns plus the latest user line.
        assert_eq!(request.messages.len(), 3);
        assert_eq!(request.messages[0].content, "what matters most?");
        assert_eq!(request.messages[1].content, "reliability at scale");
        assert_eq!(
            request.messages[2].content,
            "why did that bullet score low?"
        );
        // No envelope instruction in the streaming system prompt, but the
        // grounding and honesty rules are still there.
        let system = request.system.as_deref().unwrap();
        assert!(!system.contains("\"reply\""));
        assert!(system.contains("ONLY the recorded experience"));
        assert!(system.contains("THE TAILORED RESUME THAT SHIPPED"));
        assert!(system.contains("OPEN BUILD"));
        // No tools this phase, so the turn takes the streaming path.
        assert!(request.tools.is_empty());
    }

    #[tokio::test]
    async fn history_is_capped_to_the_most_recent_turns() {
        let mock = MockLlmClient::default();
        mock.enqueue("ok");

        // More turns than the cap; only the most recent HISTORY_TURNS survive.
        let mut history = Vec::new();
        for i in 0..HISTORY_TURNS + 6 {
            history.push(ChatTurn {
                from_user: i % 2 == 0,
                text: format!("turn number {i}"),
            });
        }
        stream_reply(&ctx(&mock), &input(None, history, "latest"))
            .await
            .unwrap();

        let request = &mock.requests()[0];
        // The capped history plus the one latest user line.
        assert_eq!(request.messages.len(), HISTORY_TURNS + 1);
        // The earliest turns were dropped; the oldest survivor is turn 6.
        assert_eq!(request.messages[0].content, "turn number 6");
        assert_eq!(request.messages.last().unwrap().content, "latest");
    }
}
