# The agent runtime

`aarg-core` is the agent runtime behind aarg: typed agents, tools,
validation-retry, and run tracing over hand-rolled LLM clients. This
document explains how it's shaped and why, and just as deliberately,
how it got here, because the *order* in which this runtime was built is
its central design decision.

## Extracted, not designed

The runtime was not written first. aarg's four LLM features (resume
ingestion, job-description parsing, gap analysis, and tailoring) were
each written as a plain `async fn` calling the `LlmClient` trait
directly, with hand-assembled prompts, hand-parsed replies, and a
deliberately duplicated fence-stripping helper in all four files. The
duplication was the point: it was the experiment that would reveal
which parts of "an agent" are actually shared and which only look
shared.

What the four working functions proved they share is a five-step spine:

1. build a request (system prompt + user message),
2. call the model,
3. strip code fences from the reply,
4. parse a *lenient* wire shape,
5. deterministically assemble a *strict* typed output.

What they proved is genuinely per-agent: the prompt, the reply budget,
how typed input renders into the user message, the wire shape, the
error type, and the assembly rules. The `Agent` trait
(`crates/aarg-core/src/agent.rs`) is a transcription of that evidence: the spine became the default `run`, the variations became the required
methods, and nothing else got in.

The extraction landed as one reviewable diff with a built-in
correctness proof: every pre-existing test passed unchanged, because
the public functions became thin wrappers with identical signatures.
The four feature modules shrank to their prompts, wire types, assembly
logic, and a ~15-line `impl Agent`.

The same rule (nothing enters the runtime without a concrete consumer
already waiting) governed everything after. Validation-retry arrived
because four agents parse model JSON. Tracing arrived and brought the
trait's `id()` method with it; `id()` did not exist before something
read it. Tools arrived with `fetch_jd` as the first implementor. The
runtime is a record of demands, not a forecast.

## The trait

```rust
#[async_trait]
pub trait Agent: Send + Sync {
    type Input: Serialize + Send + Sync;
    type Wire: DeserializeOwned;
    type Output: Send + Sync;
    type Error: std::error::Error + From<LlmError> + Send + Sync + 'static;

    fn id(&self) -> &'static str;
    fn system_prompt(&self) -> &str;
    fn reply_budget(&self) -> u32;
    fn user_message(&self, input: &Self::Input) -> String;
    fn tools(&self) -> &[Box<dyn Tool>] { &[] }
    fn bad_reply(&self, snippet: String, source: serde_json::Error) -> Self::Error;
    fn assemble(&self, wire: Self::Wire, input: Self::Input) -> Result<Self::Output, Self::Error>;

    async fn run(&self, ctx: &AgentContext<'_>, input: Self::Input)
        -> Result<AgentRun<Self::Output>, Self::Error> { ... }
}
```

Three choices carry most of the weight:

**`Wire` is a first-class associated type.** Every agent tolerates a
sloppy reply at the wire (`#[serde(default)]` everywhere, models omit
fields) and enforces strictness in `assemble`. Splitting "what the
model says" from "what we produce" makes the boundary between trust
levels a type boundary. Assembly is where aarg's never-fabricate
guarantee lives: IDs must resolve, invented numbers revert, unbacked
claims drop. The runtime guarantees assembly always runs while
knowing nothing about what it checks.

**`Error: From<LlmError>`.** Each agent keeps its own error enum (with
a transparent `LlmError` variant), and this bound is what lets the
shared spine use `?` on transport failures and have them land in
whichever enum the concrete agent uses. Errors stay domain-shaped all
the way up; there is no runtime-wide error type for features to
flatten into.

**A default `run`, with its body also exposed as the free function
`run_agent`.** Trait overrides can't call the default they replace
(there is no `super` for default methods), so the spine lives in a
named function that overriders can delegate to. Exactly one agent
overrides `run`: the gap analyzer resolves what it can against the
dataset's alias map first and skips the model entirely when nothing is
left to ask (a zero-token run is its happy path), then delegates to
`run_agent` for the leftovers.

## Two kinds of dispatch, on purpose

The runtime uses both of Rust's polymorphism mechanisms, each where its
trade-off wins:

- **`LlmClient` is a trait object** (`&dyn LlmClient` in
  `AgentContext`). The provider genuinely varies at runtime (Anthropic
  in production, a scripted mock in every test), and its methods have
  uniform signatures. One vtable pointer beats threading a type
  parameter through every function that ever touches a model, and
  nanoseconds of dynamic dispatch vanish next to seconds of network.
- **`Agent` is used generically** (`run_agent<A: Agent>`). Its
  associated types appear in `run`'s signature, which is exactly what
  trait objects can't express without pinning every type, and nothing
  selects an agent at runtime anyway; each call site knows its agent
  statically. The payoff is typed ends: the JD parser returns
  `JobRequirements`, not a `Value` to downcast.

`AgentContext` itself is borrowed (`&dyn LlmClient`, `&str`, `&Tracer`
behind one lifetime), because a single command owns everything for
exactly one run at a time. Shared ownership (`Arc`) is a known future
step for when something holds agents across runs; buying it early would
be flexibility nothing uses.

## Policies in the spine

**Validation-retry is conversational, narrow, and priced.** A reply
that fails to parse goes back to the model once: its own reply as an
assistant turn, then a user turn quoting the exact parse error. Only
wire-level failures retry; an assembly failure is a semantic verdict
that re-asking can't fix, and it surfaces immediately. Token usage sums
across attempts, so a run's recorded cost includes its failures.

**Every run leaves a trace, especially the failed ones.** The spine
records at every exit (success, transport error, final parse failure,
assembly rejection) into one JSON file per run: serialized input, the
full conversation (retry turns and tool exchanges included), the raw
final reply, summed usage, duration, outcome. Trace *writing* is
best-effort and swallows every error: a run that cost real tokens must
never fail over observability plumbing. Trace *reading* (`aarg trace
last|show`) is strict and typed: when a user asks for a record,
"missing" and "corrupt" are real answers. Filenames are zero-padded
timestamps plus the agent id, so lexicographic order is chronological
order and "latest" needs no metadata.

**Tools talk to the model, not the user.** A `Tool` is deterministic
code the model may ask to run (name, description, JSON schema, async
`call`). The dispatch loop echoes the model's tool-use turn back with
results matched by ID and continues until the model answers. A failed
tool, including a call to a tool that doesn't exist, is reported to
the *model* as an error result it can adapt to: different arguments, a
different tool, or answering without one. The only fatal outcome is a
model that keeps reaching for tools past a fixed round cap, which
fails the run with a typed error instead of spending indefinitely.
The first real tool is `fetch_jd`, offered by the JD parser for input
that merely links to a job posting; notably, the tool's *implementation*
lives in the binary crate, because knowing about job boards is domain
knowledge: the runtime defines the capability, the domain provides it.

## The crate boundary

The runtime moved into `crates/aarg-core` only after it was finished
and proven inside the binary; every file in the split was a 100%
rename, and the binary re-exports `agent`, `llm`, and `trace` from its
library root so no feature module changed. The boundary rule that made
the split mechanical: **core is what every feature consumes, and none
of it knows resumes exist.**

`aarg-core` is aarg's core, not a general framework. It keeps this
project's conventions (trace storage under aarg's data directory, the
one-retry default) rather than sprouting configuration for
hypothetical other users. Generalization is held to the same standard
as every abstraction here: it arrives when a second concrete consumer
does.

## Deliberately absent

Things the runtime visibly lacks, each waiting on its consumer rather
than forgotten: per-agent model/sampling configuration (arrives with
multi-provider support and tiered model selection), cost-in-dollars on
`AgentRun` (needs price tables; token counts are recorded now so
history won't have holes), cancellation, agents over streaming
responses (streaming exists in the clients for live display; tool calls
don't traverse it yet), and shared ownership in `AgentContext`. The
shape of each addition is constrained in advance by the same rule that
built everything above it: working code first, abstraction second.
