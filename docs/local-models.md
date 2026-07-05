# Running aarg on local models

aarg can do all of its LLM work against a model running on your own machine, through either [LM Studio](https://lmstudio.ai) or [Ollama](https://ollama.com). No API key, no per-token cost, and your resume data never leaves the machine. This page covers picking a model, configuring each server, and what to expect.

Typst is still required for PDF rendering, exactly as on the hosted provider.

## Picking a model

This choice matters more than anything else on this page. The same pipeline that scores 71 with a hosted model scored 12 with a 7B instruct model and 62 with a well-chosen local one, on the same job description and dataset.

What to look for:

- **A mixture-of-experts (MoE) model with thinking disabled.** MoE models keep all their weights in memory but activate only a few billion per token, so they generate several times faster than a dense model of similar quality. On one comparison, a 35B MoE (qwen3.6-35b-a3b) finished a full build in about two and a half minutes where a dense 70B took fifteen, and scored higher.
- **At least an 8192-token context window.** aarg's tailoring prompts run 4k to 8k tokens. A window smaller than that either fails loudly or, worse, silently drops part of your evidence, depending on the server. `aarg llm ping` reports the loaded window and warns when it is too small.
- **Thinking off.** Reasoning models spend their output budget on hidden deliberation before producing any text, and on a strict budget they can spend all of it and return nothing. On Ollama, aarg disables thinking automatically for models that declare the capability. On LM Studio it is a per-model setting you flip once (below).
- **Small instruct models work but draft thin.** A 7B completes a build in about 90 seconds and every guard behaves, but the drafts are weak and the built-in reviewer scores erratically. Fine for trying the pipeline, wrong for a resume you plan to send.

## LM Studio

1. Install LM Studio and download a model from its catalog.
2. Open the model's settings (the gear next to its name) and check three things on the **Inference** tab:
   - **Enable Thinking: off**, if the model has the toggle. LM Studio's API has no per-request switch, so this setting is what the server obeys.
   - **Context Overflow: stop at limit.** The default "Truncate Middle" silently cuts the middle out of an oversized prompt, which for a resume pipeline means dropping evidence. aarg detects a clipped reply and refuses it, but a clear server error is better.
   - **Context Length: 8192 or more.**
3. Start the server (the Developer tab, or `lms server start`). The default address is `http://127.0.0.1:1234`.
4. Point aarg at it:

```toml
provider = "lmstudio"

[lmstudio]
model = "qwen/qwen3.6-35b-a3b"

[lmstudio.tiers]
cheap = "qwen2.5-vl-7b-instruct"
```

The `model` line is required; aarg ships no default because it cannot know what you have downloaded. The optional `tiers` table lets you use a small fast model for the cheap tier (job-description parsing) and save the big one for tailoring and review. `base_url` is only needed if the server is not on the default port.

## Ollama

1. Install Ollama and pull a model:

```sh
ollama pull qwen3:30b-a3b
```

2. Point aarg at it:

```toml
provider = "ollama"

[ollama]
model = "qwen3:30b-a3b"

[ollama.tiers]
cheap = "qwen3:8b"
```

That is the whole setup. aarg speaks Ollama's native API, so it sizes the context window per request (Ollama's default would otherwise silently clip long prompts at about 4k tokens), verifies the model's maximum through `/api/show` before sending, and turns thinking off for models that declare it. Two optional knobs live in the same section: `num_ctx` raises the floor for the per-request window (default 8192) and `keep_alive` controls how long the model stays loaded after a request (default "5m").

## Verify the setup

```sh
aarg llm ping
```

A healthy ping prints the model, a reply, latency, token counts, and the context window. Three warnings are worth acting on:

- **A context warning** means the loaded window is under 8192 and long prompts will be refused or clipped. On LM Studio, reload the model with a larger context length.
- **A hidden-reasoning note** (LM Studio) means the model spent tokens thinking before it answered. Flip Enable Thinking off in the model's settings, or pick an instruct model.
- **A connection error** names the address it tried and what to start. If the server is running on a different port, fix `base_url` in the provider's config section.

## What to expect

- **Speed** depends almost entirely on the model. Rough figures from an M-series Mac with 128GB: a 7B instruct model finishes a full build in about 90 seconds, a 35B MoE in about two and a half minutes, a dense 70B in about fifteen minutes.
- **Quality** trails hosted models mainly in keyword coverage: local models write reasonable prose but are worse at working the job description's exact phrases into the draft. The build report's "missing, but you have the evidence" list shows what the model failed to surface, and a second iteration sometimes picks it up.
- **Every guard still applies.** The evidence check drops skills the model invents, the reviewer still objects, and variants still render from one canonical draft. Cost lines read "local model, no cost".
- **A workable split** is drafting and iterating locally for free, then switching the provider line back to Anthropic for the final build. The config keeps all provider sections side by side, so switching is one word.

## When something fails

Local model failures are typed and specific. The ones you may meet:

- *"the local model server at ... is not responding"*: start LM Studio or run `ollama serve`, or fix `base_url`.
- *"provider is set to ... but no model is configured"*: add the `model` line shown in the message.
- *"the model spent N tokens on hidden reasoning"*: the model is thinking with its whole output budget. Disable thinking or choose an instruct model.
- *"this prompt needs roughly N tokens; the model supports M"*: the prompt cannot fit. Use a larger-context model, or trim the dataset.
- A reply that arrives as malformed JSON is repaired when the flaw is a trailing comma and retried once with the parse error otherwise; if a model persistently fails here, it is usually too small for the structured work the smart tier demands.
