//! The agent runtime, extracted from four working LLM features (FR-2.1).
//!
//! This module was not designed up front. Through Phase 1, `ingest`,
//! `jd`, `gap`, and `tailor` each carried the same five-step spine —
//! build a request, `complete` it, strip code fences, parse a lenient
//! wire shape, assemble a typed output — written out longhand four
//! times, with four private copies of `strip_fences`. The `Agent` trait
//! below is that spine, extracted in one diff *after* the duplication
//! proved which parts are genuinely shared:
//!
//! - **Shared (the default `run`)**: request assembly, the LLM call,
//!   fence stripping, parse-or-typed-error with a reply snippet, token
//!   accounting.
//! - **Per-agent (the required methods)**: the system prompt, the reply
//!   budget, how typed input becomes the user message, the wire shape,
//!   the error type, and the deterministic assembly of wire into output.
//!
//! Deliberately *smaller* than the runtime's end state: tools,
//! validation-retry, and tracing arrive later in this phase, each
//! pulled by a concrete consumer — the same rule that delayed this
//! trait until four functions demanded it.

use async_trait::async_trait;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::llm::{CompletionRequest, LlmClient, LlmError, Message, TokenUsage};
use crate::trace::{Trace, TraceOutcome, Tracer, trace_id};

/// Everything an agent needs from its surroundings. Borrowed, because
/// today a single command owns the client, the config, and the tracer
/// for exactly one run at a time; shared ownership can be introduced
/// when something actually holds an agent across runs.
// EXERCISE(EX-014)
pub struct AgentContext<'a> {
    pub llm: &'a dyn LlmClient,
    /// The model requests go to, resolved by the caller from config.
    pub model: &'a str,
    /// Where runs get recorded. Tests pass `&Tracer::DISABLED`.
    pub tracer: &'a Tracer,
}

/// What a run produced: the agent's typed output plus the accounting
/// every caller eventually wants. Grows alongside the runtime (traces,
/// cost, duration) as those exist.
#[derive(Debug)]
pub struct AgentRun<T> {
    pub output: T,
    pub usage: TokenUsage,
}

/// One LLM-backed unit of work with typed input and output.
///
/// Implementations provide the five things Phase 1 proved are
/// per-agent; the default `run` provides the spine they shared.
#[async_trait]
pub trait Agent: Send + Sync {
    /// What the agent works from. Owned: a run's input is a value the
    /// caller hands over, not a window into the caller's state — and
    /// `Serialize` because the trace records it.
    type Input: Serialize + Send + Sync;
    /// The lenient shape the model replies in — every agent tolerates
    /// missing fields at the wire and enforces strictness in `assemble`.
    type Wire: DeserializeOwned;
    /// What the agent produces.
    type Output: Send + Sync;
    /// The agent's own error enum; `From<LlmError>` lets the shared
    /// spine propagate transport failures into it with `?`.
    type Error: std::error::Error + From<LlmError> + Send + Sync + 'static;

    /// Stable identifier for traces (e.g. "jd_parser_v1"). Versioned,
    /// so a prompt overhaul can be told apart from old runs in history.
    fn id(&self) -> &'static str;

    /// The fixed instructions: a pure function of the agent, never of
    /// the input.
    fn system_prompt(&self) -> &str;

    /// max_tokens for the reply — sized to the wire shape.
    fn reply_budget(&self) -> u32;

    /// Render the typed input into the user message.
    fn user_message(&self, input: &Self::Input) -> String;

    /// Wrap an unparseable reply in the agent's own error, keeping a
    /// snippet of what the model actually said.
    fn bad_reply(&self, snippet: String, source: serde_json::Error) -> Self::Error;

    /// Deterministic assembly: every wire claim checked, IDs resolved,
    /// anything unusable dropped or reverted. Consumes the input so
    /// assembly can move it into the output where useful.
    fn assemble(&self, wire: Self::Wire, input: Self::Input) -> Result<Self::Output, Self::Error>;

    /// The shared spine. Override only for nonstandard control flow —
    /// the gap analyzer does, to skip the LLM when deterministic
    /// matching already covered everything.
    async fn run(
        &self,
        ctx: &AgentContext<'_>,
        input: Self::Input,
    ) -> Result<AgentRun<Self::Output>, Self::Error> {
        run_agent(self, ctx, input).await
    }
}

/// Attempts at getting parseable output before the typed error
/// surfaces: the first ask plus one corrective retry. A retry resends
/// the whole conversation, so each one roughly doubles the cost — one
/// is the honest default until per-agent config exists.
const PARSE_ATTEMPTS: u32 = 2;

/// The default `run` body as a free function, so an agent that
/// overrides `run` for control flow can still delegate to the spine
/// (trait default methods have no `super` to call).
pub async fn run_agent<A>(
    agent: &A,
    ctx: &AgentContext<'_>,
    input: A::Input,
) -> Result<AgentRun<A::Output>, A::Error>
where
    A: Agent + ?Sized,
{
    let started_at = chrono::Utc::now();
    let timer = std::time::Instant::now();
    // Serialized up front: `input` is moved into `assemble` later, and
    // the failure paths still want to record what the run was asked.
    let input_json = serde_json::to_value(&input).unwrap_or(serde_json::Value::Null);

    let mut messages = vec![Message::user(agent.user_message(&input))];
    let mut usage = TokenUsage::default();

    // One trace per run, whatever the ending — failed runs are exactly
    // the ones worth replaying.
    let finish = |messages: &[Message], reply: Option<&str>, usage, outcome| {
        ctx.tracer.record(&Trace {
            trace_id: trace_id(agent.id(), started_at),
            agent: agent.id().to_string(),
            started_at,
            duration_ms: u64::try_from(timer.elapsed().as_millis()).unwrap_or(u64::MAX),
            model: ctx.model.to_string(),
            input: input_json.clone(),
            system: agent.system_prompt().to_string(),
            messages: messages.to_vec(),
            reply: reply.map(str::to_string),
            usage,
            outcome,
        });
    };

    let mut attempt = 1;
    loop {
        let request = CompletionRequest {
            model: ctx.model.to_string(),
            max_tokens: agent.reply_budget(),
            system: Some(agent.system_prompt().to_string()),
            messages: messages.clone(),
            temperature: None,
        };
        let response = match ctx.llm.complete(request).await {
            Ok(response) => response,
            Err(error) => {
                finish(
                    &messages,
                    None,
                    usage,
                    TraceOutcome::Failed {
                        error: error.to_string(),
                    },
                );
                return Err(error.into());
            }
        };
        usage.input_tokens += response.usage.input_tokens;
        usage.output_tokens += response.usage.output_tokens;

        let json = strip_fences(&response.text);
        match serde_json::from_str::<A::Wire>(json) {
            Ok(wire) => {
                // Assembly failures are semantic verdicts (bad IDs, an
                // empty selection), not malformed output — retrying the
                // same question would burn tokens on a different
                // problem, so they surface immediately.
                match agent.assemble(wire, input) {
                    Ok(output) => {
                        finish(
                            &messages,
                            Some(&response.text),
                            usage,
                            TraceOutcome::Succeeded,
                        );
                        return Ok(AgentRun { output, usage });
                    }
                    Err(error) => {
                        finish(
                            &messages,
                            Some(&response.text),
                            usage,
                            TraceOutcome::Failed {
                                error: error.to_string(),
                            },
                        );
                        return Err(error);
                    }
                }
            }
            // Validation-retry: show the model its own reply and the
            // parse error, and ask again — once.
            Err(source) if attempt < PARSE_ATTEMPTS => {
                attempt += 1;
                messages.push(Message::assistant(response.text.clone()));
                messages.push(Message::user(format!(
                    "That reply did not parse as the required JSON ({source}). \
                     Reply again with exactly one JSON object in the shape \
                     originally requested - no commentary, no code fences."
                )));
            }
            Err(source) => {
                let error = agent.bad_reply(snippet(json), source);
                finish(
                    &messages,
                    Some(&response.text),
                    usage,
                    TraceOutcome::Failed {
                        error: error.to_string(),
                    },
                );
                return Err(error);
            }
        }
    }
}

/// Models often wrap JSON in ```fences``` despite instructions; strip
/// one outer fence pair (and its info string, e.g. ```json) if present.
/// Phase 1 carried four private copies of this; the extraction is where
/// they finally merged.
pub fn strip_fences(text: &str) -> &str {
    let trimmed = text.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    let body = match rest.split_once('\n') {
        Some((_info_string, body)) => body,
        None => rest,
    };
    body.trim_end().strip_suffix("```").unwrap_or(body).trim()
}

/// The first stretch of an unparseable reply, for error messages:
/// enough to tell refusal prose from truncated JSON.
fn snippet(json: &str) -> String {
    json.chars().take(120).collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::llm::MockLlmClient;
    use serde::Deserialize;

    /// A minimal agent exercising the spine end to end: doubles a
    /// number the model "computes".
    struct EchoAgent;

    #[derive(Debug, Deserialize)]
    struct EchoWire {
        value: i64,
    }

    #[derive(Debug, thiserror::Error)]
    enum EchoError {
        #[error(transparent)]
        Llm(#[from] LlmError),
        #[error("bad reply ({snippet})")]
        BadReply {
            snippet: String,
            #[source]
            source: serde_json::Error,
        },
    }

    #[async_trait]
    impl Agent for EchoAgent {
        type Input = i64;
        type Wire = EchoWire;
        type Output = i64;
        type Error = EchoError;

        fn id(&self) -> &'static str {
            "echo_test"
        }
        fn system_prompt(&self) -> &str {
            "Reply with {\"value\": <the number>}."
        }
        fn reply_budget(&self) -> u32 {
            16
        }
        fn user_message(&self, input: &i64) -> String {
            format!("the number is {input}")
        }
        fn bad_reply(&self, snippet: String, source: serde_json::Error) -> EchoError {
            EchoError::BadReply { snippet, source }
        }
        fn assemble(&self, wire: EchoWire, input: i64) -> Result<i64, EchoError> {
            // "Assembly" that uses both wire and input, like the real four.
            Ok(wire.value + input)
        }
    }

    #[tokio::test]
    async fn the_spine_runs_request_parse_assemble() {
        let mock = MockLlmClient::default();
        mock.enqueue("```json\n{\"value\": 40}\n```");
        let ctx = AgentContext {
            llm: &mock,
            model: "test-model",
            tracer: &Tracer::DISABLED,
        };

        let run = EchoAgent.run(&ctx, 2).await.unwrap();

        assert_eq!(run.output, 42);
        assert!(run.usage.output_tokens > 0);
        let requests = mock.requests();
        assert_eq!(requests[0].model, "test-model");
        assert_eq!(requests[0].max_tokens, 16);
        assert_eq!(
            requests[0].system.as_deref(),
            Some(EchoAgent.system_prompt())
        );
        assert_eq!(requests[0].messages[0].content, "the number is 2");
    }

    #[tokio::test]
    async fn a_parse_failure_is_retried_once_with_the_error_shown() {
        let mock = MockLlmClient::default();
        mock.enqueue("Sure! Here's your JSON: {\"value\":");
        mock.enqueue("{\"value\": 40}");
        let ctx = AgentContext {
            llm: &mock,
            model: "m",
            tracer: &Tracer::DISABLED,
        };

        let run = EchoAgent.run(&ctx, 2).await.unwrap();

        assert_eq!(run.output, 42);
        let requests = mock.requests();
        assert_eq!(requests.len(), 2);
        // The retry carries the conversation: original ask, the model's
        // own bad reply, and a correction naming the parse error.
        let retry = &requests[1].messages;
        assert_eq!(retry.len(), 3);
        assert_eq!(retry[0].content, "the number is 2");
        assert!(retry[1].content.starts_with("Sure!"));
        assert!(retry[2].content.contains("did not parse"));
        // Both attempts are paid for: the mock reports at least one
        // output token per reply, so a summed total must show two.
        assert!(run.usage.output_tokens >= 2);
    }

    #[tokio::test]
    async fn a_second_bad_reply_surfaces_the_typed_error() {
        let mock = MockLlmClient::default();
        mock.enqueue("I'm sorry, I can't do math today.");
        mock.enqueue("Still prose, still not JSON.");
        let ctx = AgentContext {
            llm: &mock,
            model: "m",
            tracer: &Tracer::DISABLED,
        };

        match EchoAgent.run(&ctx, 1).await.unwrap_err() {
            // The error carries the FINAL reply's snippet — that's what
            // the model said after seeing its mistake.
            EchoError::BadReply { snippet, .. } => assert!(snippet.starts_with("Still prose")),
            other => panic!("expected BadReply, got {other:?}"),
        }
        assert_eq!(mock.requests().len(), 2, "exactly one retry, then stop");
    }

    #[tokio::test]
    async fn every_run_lands_in_the_trace_directory() {
        let dir = tempfile::tempdir().unwrap();
        let tracer = Tracer::to_dir(dir.path());
        let mock = MockLlmClient::default();
        mock.enqueue("not json at all");
        mock.enqueue("{\"value\": 40}");
        let ctx = AgentContext {
            llm: &mock,
            model: "m",
            tracer: &tracer,
        };

        EchoAgent.run(&ctx, 2).await.unwrap();

        let trace = crate::trace::latest_in(dir.path()).unwrap();
        assert_eq!(trace.agent, "echo_test");
        assert!(matches!(trace.outcome, TraceOutcome::Succeeded));
        // The retry conversation is preserved: ask, bad reply, correction.
        assert_eq!(trace.messages.len(), 3);
        assert_eq!(trace.input, serde_json::json!(2));
        assert_eq!(trace.reply.as_deref(), Some("{\"value\": 40}"));
        // Usage in the trace is the summed cost of both attempts.
        assert!(trace.usage.output_tokens >= 2);
    }

    #[test]
    #[ignore = "exercise: every agent runs at the provider's default temperature; add a sampling_temperature method to the Agent trait (defaulting to None) that the spine threads into the request, then finish this test"]
    fn ex_014_agents_can_request_a_temperature() {
        // Once the method exists: give EchoAgent (or a second test
        // agent) a Some(0.0) override, run it against the mock, and
        // assert the recorded request carries it — and that an agent
        // without an override still sends None.
        let temperature_implemented = false;
        assert!(temperature_implemented);
    }

    #[test]
    fn strip_fences_handles_the_common_shapes() {
        assert_eq!(strip_fences("{\"a\": 1}"), "{\"a\": 1}");
        assert_eq!(strip_fences("```json\n{\"a\": 1}\n```"), "{\"a\": 1}");
        assert_eq!(strip_fences("```\n{\"a\": 1}\n```"), "{\"a\": 1}");
        assert_eq!(strip_fences("  {\"a\": 1}  "), "{\"a\": 1}");
    }
}
