# AARG: The Adversarial Agentic Resume Generator

AARG tailors your résumé to a specific job posting and then argues with itself
about the result. A skeptical reviewer agent reads each draft the way a hiring
manager looking for reasons to pass would, files specific objections, and a
tailoring agent revises against them under tight bounds. The winner renders
with [Typst](https://typst.app) into two PDFs, one formatted to survive
applicant tracking systems and one designed for a person to read.

It runs on your machine, against your own career data, and it will not invent
experience you don't have. [It won't make things up](#it-wont-make-things-up)
explains how that's enforced.

You can drive it from a command-line tool or from a local browser workspace,
which runs the same Rust in the page via WebAssembly alongside a small
companion server (`aarg serve`).

> **Status:** working end to end, from a command line or a local browser
> workspace. Ingest, tailoring, the adversarial loop, both résumé variants,
> history/diff, an interactive shell, and the in-browser build screen all work
> today. Anthropic is the default model provider; LM Studio and Ollama run the
> whole product on a local model with no API key (see [Local models](#local-models)).

## Demo

<p align="center">
  <img src="docs/demo.gif" alt="aarg ingesting a resume, tailoring it to a posting through the adversarial loop, and exporting the PDFs" width="820">
</p>

A full run on a fictional candidate and posting, from `ingest` through the
review loop to the exported PDFs.

## How the loop works

1. `ingest` turns an existing résumé into a structured **dataset**: roles,
   bullets, and skills, each tied to evidence.
2. `tailor <job>` parses the posting, runs a **gap analysis** against your
   dataset, and writes a first draft that mirrors the posting's language without
   overstating what you've actually done.
3. The **adversarial reviewer** scores the draft's content and files
   objections: no metric, vague verb, unsupported claim, and so on. The loop's
   evaluator blends that verdict with deterministic keyword coverage computed
   by pure code.
4. The tailoring agent revises against those objections and re-scores. A
   revision that doesn't improve the score is discarded and the loop stops; the
   build keeps the best draft it ever saw.
5. The winner renders to an **ATS** PDF and a **human** PDF (same facts,
   different presentation), and every iteration is written to disk so you can
   inspect or diff it later.

When an objection can't be satisfied without lying ("this bullet states an
outcome with no number"), the loop stops guessing and asks you. A short
interview folds your real figures back into the dataset, then re-tailors.
The facts have to come from you. The model is only allowed to rephrase them.

The full write-up is in [docs/design/adversarial-loop.md](docs/design/adversarial-loop.md).

## The browser workspace

The same loop, and everything around it, is also a browser app. Run `aarg serve`
and open the page, and you get a single build screen that pulls the pieces
together:

<p align="center">
  <img src="docs/screenshots/desktop.png" alt="The browser workspace: recent builds on the left, the tailored résumé in the centre with per-line provenance, the reviewer's verdict and objection cards on the right" width="860">
</p>

- **All the scores in one place.** Weighted coverage, the reviewer's verdict,
  ATS keyword coverage, and the list-ranking score sit in one band, each with a
  plain-words explanation of what it measures.
- **A coverage map** of the posting's requirements against your dataset (exact
  match, semantic match, or gap), with a per-requirement **Refine**,
  **Strengthen**, or **Fill the gap** that drops you into the right copilot.
- **An editable, provenance-checked preview.** Every line of the draft is
  free-editable and labelled by where it traces: **verbatim** from a bullet,
  **grounded** in your evidence, or **unrecorded**. An unrecorded line carries a
  claim badge and a confirm-as-evidence button, so you can see exactly which
  lines still need backing before the résumé goes anywhere.
- **Interactive copilots.** The strengthen, metric, summary, and skills
  interviews from the CLI, run through a Q&A modal, plus a layout copilot for
  presentation-only objections. It's the browser mirror of the CLI's
  `UserHandle`: the agent asks the same questions either way.
- **New Build and Retailor**, both running the full adversarial loop *in the
  page* with live iteration, score, and cost, and a **Stop** button that keeps
  the best draft so far. Start from a pasted posting, a URL, or a previous
  build's parsed JD.
- **Pixel-perfect PDF preview** from real Typst renders, with a template picker.
- **Edit persistence:** save your edits into the build behind the same
  claim-divergence guard, with an append-only edit log, history, and revert. A
  sticky pending-edits bar (with `Cmd`/`Ctrl+S`) keeps the save action in reach.

<p align="center">
  <img src="docs/screenshots/copilot.png" alt="A copilot question posed in a modal over the workspace, the browser mirror of the CLI's evidence interviews" width="820">
</p>

<p align="center">
  <img src="docs/screenshots/pixel.png" alt="Pixel-perfect preview: the real Typst-rendered PDF shown under the fidelity toggle and template picker" width="380">
</p>

<p align="center">
  <img src="docs/screenshots/edit-bar.png" alt="A line edited in the provenance-checked preview, with the sticky pending-edits bar offering save, undo, and record-as-evidence" width="820">
</p>

Under the hood, the whole domain pipeline is compiled to WebAssembly and runs
in the page. `aarg serve` is a small native companion for the things a page
can't do itself: proxy one model completion through your keychain-held key,
shell out to Typst, read and write your workspace on disk, and fetch a
cross-origin posting. It binds to loopback by default; opt into `--bind` and
`--allow-host` to reach it from a phone on a network you trust.

The screenshots on this page use a bundled fictional demo dataset (a candidate
named Sam Rivera), so there's no real résumé data in them. The workspace holds up
on a phone, too:

<p align="center">
  <img src="docs/screenshots/mobile.png" alt="The same build screen on a narrow phone viewport" width="300">
  &nbsp;&nbsp;
  <img src="docs/screenshots/coverage.png" alt="The coverage map on a phone: each posting requirement is a state-railed card marked exact match, semantic match, or gap, with a per-requirement action" width="300">
</p>

## It won't make things up

Every skill, date, employer, and number in the output traces to evidence in your
dataset. The model isn't trusted to follow that rule on its own. Three separate
mechanisms hold it:

- A skill with no backing evidence fails type-level validation and never
  reaches a draft.
- During assembly, the model speaks in evidence IDs: a number it introduces
  that the source bullet doesn't contain is reverted, and an unbacked skill is
  dropped. The same checks run on the first draft and on every revision,
  because both go through the same code.
- At review, the reviewer flags unsupported claims, and a separate lint refuses
  to ship the build if the two PDFs ever diverge on what they claim.

Keyword-coverage gaps, where the posting wants something you didn't surface, are
reported to you and stop there. They never feed back into a prompt, which closes
the obvious backdoor where an ATS miss turns into an invented bullet.

<p align="center">
  <img src="docs/demo-evidence.gif" alt="aarg refusing to add a skill without evidence: it interviews the user for a real role and a sentence that becomes the backing evidence" width="820">
</p>

Adding a skill the posting wants but your résumé never mentioned means
answering two questions: which real role demonstrates it, and what you actually
did there. Your answer gets tightened into résumé wording and becomes the
backing evidence. If you can't point at a real role, the skill stays off the
page.

The evidence checks and the claim-divergence lint are compiled into the
WebAssembly bundle, so they also run client-side while you edit in the browser.
The server re-checks everything anyway: a draft or edit submitted by a page
(`POST /api/builds`, `POST /api/builds/:id/edits`) goes through the same
deterministic divergence guard before anything is written, a saved dataset is
re-validated the way `aarg dataset validate` would, and a variant that claims
more than the canonical draft is rejected with a `422`. A page could be buggy
or tampered with, so the process that owns the disk and the key checks for
itself.

## Features

Beyond the core loop:

- **Two résumé variants from one draft.** A plain, parser-safe ATS PDF and a
  designed human PDF, lint-checked to make the same claims. Five templates ship
  built-in, or point `tailor --template` at your own Typst layout.
- **Gap interviews.** When the reviewer wants a number, a stronger verb, or a
  skill you didn't surface, AARG asks you for the real thing and re-tailors with
  your answer. Thin roles and unbacked keywords work the same way.
- **Voice.** Capture a few writing samples and AARG rewrites the AI-sounding lines
  toward how you actually write, without changing any facts.
- **Cover letters.** Drafted from the tailored résumé and the posting, under the
  same never-fabricate guards (`aarg cover`, or `tailor --cover`).
- **History and diff.** Every build is a self-contained folder on disk. List them,
  compare two field by field, re-review an old one (`aarg attack`), or re-render
  it without paying for a new tailor.
- **Flexible input.** Ingest a résumé from text, Markdown, or a PDF, including
  scanned ones read with the model's vision. Give a posting as a file, a
  Greenhouse, Lever, or LinkedIn URL, stdin, or a paste.
- **A browser workspace.** The whole build screen, served locally by
  `aarg serve`. See [The browser workspace](#the-browser-workspace).
- **Use it from Claude.** Run AARG as an MCP server and drive it by chatting with
  Claude Desktop or Claude Code, on this machine or over SSH, with the copilots as
  in-chat prompts and the PDFs exposed as resources. See [docs/mcp.md](docs/mcp.md).
- **An interactive shell.** Run `aarg` with no arguments for a REPL that takes
  every command without the prefix.

## Getting started

### Prerequisites

- **[Typst](https://github.com/typst/typst)** on your `PATH`, however you
  install aarg; rendering shells out to it. If it's missing, aarg says so and
  tells you how to install it.
- **Rust 1.89 or newer** (2024 edition) to build from source, whether with
  `cargo install aarg` or from a clone. The installer script needs no toolchain.
- An **Anthropic API key** or a Claude Pro/Max subscription (see
  [Authentication](#authentication)), **or** a local model through LM Studio or
  Ollama with no key at all (see [Local models](#local-models)).
- **[wasm-pack](https://rustwasm.github.io/wasm-pack/) and Node.js with npm**,
  only for developing the web app. Every install already carries the built
  workspace, so skip these unless you're changing the front end.

### Install

The installer script is the standard path. It downloads a prebuilt binary with
the browser workspace baked in:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/joseym/aarg/releases/latest/download/aarg-installer.sh | sh
```

Install [Typst](https://github.com/typst/typst) separately (`brew install typst`
on a Mac); rendering shells out to it.

`cargo install aarg` from crates.io compiles the same binary locally, workspace
and all. You still need Typst:

```sh
cargo install aarg
```

From a clone, for contributors:

```sh
git clone git@github.com:joseym/aarg.git
cd aarg
cargo install --path .
```

A from-source install embeds the browser workspace only if you build the web
app first (the wasm-pack and npm steps under
[Running the browser workspace](#running-the-browser-workspace)). Skip that and
you get the full CLI with `aarg serve` in API-only mode.

### A first run

```sh
aarg init                  # set up a workspace here, store your key in the OS keychain
aarg ingest resume.pdf     # build your dataset from an existing résumé (text, Markdown, or a text-layer PDF)
aarg tailor job.txt        # parse, gap-analyze, tailor, review, revise, render
```

`tailor` writes `resume.ats.pdf` and `resume.human.pdf` into the build directory
and prints where they landed, the reviewer's verdict, keyword coverage, and what
the run cost. Run `aarg` with no arguments to drop into an interactive shell that
takes the same commands without the prefix.

You don't have to keep job postings in files. `tailor` and `gap` also accept a
Greenhouse, Lever, or LinkedIn URL or `-` for stdin, and with no argument at all they let you
paste a posting in or reuse one you've already entered.

If you'd rather work in a browser: once you have a dataset, run `aarg serve`.
The server starts on `http://127.0.0.1:8787` with the workspace already served,
and everything in
[The browser workspace](#the-browser-workspace) runs from the page, the loop
included. Release installs carry the app; a from-source install picks it up
after the web build described later. It stays on loopback unless you ask
otherwise. Reaching it from a phone, and building the app from source, are
covered in [Running the browser workspace](#running-the-browser-workspace).

### Running the browser workspace

Run `aarg serve` and open the URL it prints:

```sh
aarg serve   # http://127.0.0.1:8787
```

The browser app is compiled into the binary, so there is nothing to point the
server at. It opens on loopback and serves the workspace at `/`.

Flags:

- `--port <PORT>` (default `8787`): the port to bind.
- `--bind <ADDR>` (default `127.0.0.1`): loopback only by default; `0.0.0.0` reaches the server from another device on your network.
- `--allow-host <HOST>` (repeatable): extra `Host` header values to accept once bound past loopback; this machine's own hostname is allowed automatically.
- `--dir <PATH>`: serve a different web app build at `/` instead of the built-in one, for developing the app (see below).

To open the workspace from a phone on a network you trust, bind past loopback and
name your host:

```sh
aarg serve --bind 0.0.0.0 --allow-host <your-hostname>
```

Then browse to `http://<your-hostname>.local:8787` from the phone. Binding past
loopback also exposes your dataset and the key-spending model proxy to that
network, so use only one you trust. On loopback the `Host` allowlist and a JSON
content-type gate defend the server against DNS-rebinding and drive-by
cross-origin `POST`s, and it sends no CORS headers, so only the page it serves
can talk to it.

`aarg serve` is a long-running process, so a newly installed binary does not take
effect until you stop and restart it.

#### Developing the web app

The WebAssembly bundle and the compiled app are not checked in. At compile
time a build script bakes whatever is in `web/dist/aarg/browser` into the
binary. That means a `cargo install --path .` from a fresh clone ships an
API-only binary until you build the web dist, and after rebuilding the app you
reinstall to pick up the change.

Build the WebAssembly bundle and the Angular app:

```sh
# Compile the domain pipeline to the WebAssembly bundle the page runs on.
wasm-pack build crates/aarg-wasm --target web --out-dir ../../web/src/wasm/pkg --out-name aarg_wasm

# Install the web app's dependencies (first build only) and compile it.
cd web && npm install && npm run build && cd ..
```

The Angular build lands in `web/dist/aarg/browser`. To iterate on the app
without reinstalling the binary each time, serve that directory directly:

```sh
aarg serve --dir web/dist/aarg/browser   # http://127.0.0.1:8787
```

`--dir` overrides the embedded app, so a rebuild of the web dist shows up on the
next page load with no reinstall.

## Authentication

Keys live in your OS keychain, never in a config file. `aarg init` walks you
through it; `aarg key add|use|remove|list` manages more than one.

```sh
aarg key add work          # an Anthropic API key, filed under a label
aarg key use work
```

You can also authenticate against a Claude subscription rather than
pay-as-you-go billing, either by pasting a token from `claude setup-token` or by
delegating to the official `ant` CLI so it refreshes for you. Subscription auth
is **experimental**: Anthropic scopes plan credit to its own tools, so the
API-key path is the supported one. For headless or CI use, set `ANTHROPIC_API_KEY`
(or `ANTHROPIC_AUTH_TOKEN`) and skip the keychain entirely. If those standard
names conflict with another tool, point AARG at private ones with `api_key_env`
/ `auth_token_env` under `[anthropic]` and leave the standard vars free.

## Local models

AARG can run the whole loop against a model on your own machine, with no API key
and no per-token cost. Two local providers are supported: [LM Studio](https://lmstudio.ai)
(through its OpenAI-compatible server) and [Ollama](https://ollama.com). Start the
server, run `aarg init`, and pick the provider; on a local provider `init` skips
the key step and lets you choose a model the server already has.

Set the provider and model in `config.toml`, either through `aarg init` or by
hand:

```toml
# LM Studio (defaults to http://127.0.0.1:1234)
provider = "lmstudio"

[lmstudio]
model = "qwen2.5-coder-7b-instruct"
```

```toml
# Ollama (defaults to http://127.0.0.1:11434)
provider = "ollama"

[ollama]
model = "qwen3:8b"
num_ctx = 8192      # context floor; grows per request when a prompt needs more
keep_alive = "5m"   # how long the model stays resident between calls
```

A few things to know:

- **Context window.** AARG's prompts run 4k-8k tokens, so load the model with a
  context window of at least 8192. `aarg llm ping` reports the loaded window and
  warns when it's too small; on LM Studio, reload the model with a larger context
  length.
- **Prefer non-thinking models,** or raise `max_tokens`. A reasoning model spends
  its output budget on hidden thinking before it answers, so a small budget can
  leave nothing for the resume itself.
- **No default model.** AARG ships no local model name, so a local provider is
  unusable until you set one; if you forget, it says exactly which config key to
  set.
- **Typst is still required** to render the PDFs, the same as on Anthropic.
- `aarg key` still manages the Anthropic keychain even while a local provider is
  active, so you can keep a key on hand and switch back with `aarg config`.

## Commands

| | |
|---|---|
| `aarg init` | create a workspace and store a key |
| `aarg ingest <file>` | build your dataset from a résumé (text, Markdown, or a text-layer PDF) |
| `aarg tailor [job]` | the adversarial loop, end to end |
| `aarg chat [job]` | ask about a posting and how your background fits it |
| `aarg gap [job]` | compare a posting against your dataset |
| `aarg jd parse \| rate \| rm` | parse a posting, rate how you fit it, forget remembered ones |
| `aarg dataset show \| validate \| edit` | inspect and correct your data |
| `aarg skills add \| verify \| dedup` | add a skill via an evidence interview, back unverified ones, collapse duplicates |
| `aarg roles enrich [id]` | flesh out thin roles with a short interview |
| `aarg experience add \| list \| remove` | record a project or non-job experience and link the skills it backs |
| `aarg voice add \| list \| remove` | writing samples that steer phrasing |
| `aarg cover [build]` | draft a cover letter for a past build |
| `aarg render [build]` | re-render a build's PDFs without re-tailoring |
| `aarg export [build] [--to <dir>]` | copy a build's PDFs out under friendly company names |
| `aarg attack [build]` | re-review a saved build without re-tailoring |
| `aarg history` / `aarg diff <a> <b>` | list past builds, compare two |
| `aarg templates list \| use <name>` | choose a résumé template |
| `aarg trace last \| show <id>` | inspect recorded agent runs |
| `aarg mcp` | run as an MCP server for Claude Desktop, Claude Code, and other clients ([docs](docs/mcp.md)) |
| `aarg serve` | run the companion server for the browser workspace |

Two ATS templates (`classic`, `minimal`) and three human ones (`modern`,
`technical`, `editorial`) ship built-in; point `tailor --template <file.typ>` at
your own to render the human variant however you like.

Flags for `aarg serve`, the phone-on-your-network recipe, and the web app
build steps are in
[Running the browser workspace](#running-the-browser-workspace).

## How it's built

**AARG** is a Rust workspace in four crates: `aarg-core` (the agent runtime),
`aarg-domain` (the résumé pipeline, pure code that transforms data and calls out
through the runtime), `aarg-wasm` (a thin wasm-bindgen wrapper that exports the
pipeline to the page), and the `aarg` binary (the CLI, the REPL, the MCP server,
and `serve`). The browser front end is an Angular app, but it doesn't
reimplement any of the pipeline: `aarg-domain` and its runtime compile to
WebAssembly and run in the page, so both front ends sit on the same domain
code.

```mermaid
flowchart TB
    subgraph cli["CLI · the aarg binary"]
        direction TB
        CLIF["clap commands · reedline REPL · inquire prompts"]
        ORCH["Orchestrator<br/>routes commands · human-in-the-loop · drives the loop"]
        CLIF --> ORCH
    end
    subgraph web["Browser workspace"]
        direction TB
        NG["Angular UI · runs the loop in the page"]
        WASM["aarg-domain + runtime, compiled to WebAssembly"]
        SERVE["aarg serve · native companion<br/>API key · Typst · workspace disk · cross-origin JD fetch"]
        NG --> WASM
        NG --> SERVE
    end
    subgraph domain["aarg-domain · the résumé pipeline · pure"]
        direction TB
        AGENTS["Agents<br/>jd parser · gap · tailor · adversarial reviewer<br/>variant adapter · voice · skill / role / metric interviews"]
        SERVICES["Services · deterministic, no LLM<br/>coverage · readability · provenance · claim-divergence lint"]
    end
    subgraph core["aarg-core · the agent runtime"]
        direction TB
        RUNTIME["trait Agent · AgentContext · Tool · Tracer<br/>validation-retry · token accounting"]
        CLIENTS["LLM clients<br/>Anthropic · LM Studio · Ollama · scripted Mock<br/>hand-rolled on reqwest, behind a two-method LlmClient trait"]
        RUNTIME --> CLIENTS
    end
    ORCH --> AGENTS
    ORCH --> SERVICES
    WASM --> AGENTS
    WASM --> SERVICES
    AGENTS --> RUNTIME
```

There's no agent framework underneath, and no web framework either: the Anthropic
client is written directly against the HTTP API behind a small trait with a
scripted mock, and `aarg serve` is written directly against `hyper`, the same
by-hand move as the MCP server, one tokio task per connection, so the whole thing
tests without a network or a key. In the browser, `aarg-core`'s `LlmClient` is
answered by a JS callback across a small `Send`-preserving channel bridge, so the
real domain agents run unchanged over the browser's own model calls.

One difference to know about: the in-browser loop's improve-or-stop gate scores
on the reviewer's verdict alone, while the CLI blends in deterministic ATS
keyword coverage. The agents and drafts are identical. The browser just weighs
a revision slightly differently.

### Where the agent runtime came from

The first three model-backed features (JD parsing, gap analysis, tailoring)
shipped as plain `async` functions, each carrying its own copy of prompt
assembly, schema-validated parsing, retry-on-bad-output, and cost accounting.
By the third, the duplication made the shared shape obvious, so the next phase
lifted a generic `Agent` trait out of the three working cases in one reviewable
diff. The adversarial loop and the keyless eval harness got cheap after that,
since every agent speaks the same contract. I try not to add an abstraction
until the second consumer shows up, and the commit history is there if you want
to check that this one actually happened that way.

The reasoning behind the trait, and the alternatives weighed against it, is in
[docs/design/agent-runtime.md](docs/design/agent-runtime.md). The convergence
problem the loop solves, and the score-must-improve gate that keeps it from
oscillating, is in [docs/design/adversarial-loop.md](docs/design/adversarial-loop.md).

ATS keyword coverage and readability are computed by plain code with no model
in the loop, so the facts the score leans on can't be talked around.

## Roadmap

**Done and working**: the tailor/review/revise loop, both résumé variants with the
claim-divergence lint, gap analysis, the skills/roles/metric interviews, voice
rewriting, cover-letter generation (`aarg cover` or `tailor --cover`), history and
diff, templates, résumé ingestion from text-layer PDFs, an interactive Q&A about a
posting (`aarg chat`), exporting finished PDFs under friendly company names
(`aarg export`), the REPL, experimental subscription auth, an MCP server
(`aarg mcp`) that lets Claude Desktop and other MCP clients drive AARG by chat
([docs/mcp.md](docs/mcp.md)), and the browser workspace (`aarg serve`).

**Not there yet**: an experimental vision pass
that reads the rendered layout the way a recruiter skims it; streamed (SSE) model
responses in the browser, which today waits for a whole completion; and reaching
the workspace safely past loopback (single-user, local-first is the design for
now; `--bind` past localhost is opt-in and unauthenticated).

## Contributing

Build instructions, the workspace layout, and pull request expectations are
in [CONTRIBUTING.md](CONTRIBUTING.md).

## Documentation

- **[Use AARG from Claude (MCP)](docs/mcp.md):** run AARG as an MCP server for
  Claude Desktop or Claude Code, locally or over SSH, and drive it by chat.
- **[The agent runtime](docs/design/agent-runtime.md):** how the `Agent` trait
  grew out of three working features, and the alternatives weighed against it.
- **[The adversarial loop](docs/design/adversarial-loop.md):** the convergence
  problem the review-and-revise loop solves, and the score-must-improve gate that
  keeps it from oscillating.

## License

Dual-licensed under either [Apache 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT),
at your option.
