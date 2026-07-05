# Changelog
All notable changes to this project will be documented in this file. See [conventional commits](https://www.conventionalcommits.org/) for commit guidelines.

- - -
## v0.5.0 - 2026-07-05
#### Features
- (**cli**) make init, config, and key provider-aware - (0b55d33) - Josey Morton
- (**config**) add LM Studio and Ollama provider settings - (fdbf8fd) - Josey Morton
- (**llm**) surface hidden reasoning token counts from lm studio - (e37c341) - Josey Morton
- (**llm**) add a hand-rolled Ollama native client - (22a9765) - Josey Morton
- (**llm**) add a hand-rolled OpenAI-compatible client for LM Studio - (7187959) - Josey Morton
- (**llm**) estimate prompt size and flag silent context truncation - (2969f0b) - Josey Morton
- (**ping**) warn when the pinged model reasons before answering - (396b71c) - Josey Morton
- (**providers**) run every seam through the active provider client - (568339c) - Josey Morton
#### Bug Fixes
- (**agent**) repair trailing-comma JSON before the parse fails - (c30f296) - Josey Morton
- (**llm**) turn off thinking for ollama models that declare it - (71a0d92) - Josey Morton
- (**llm**) refuse a reply spent entirely on hidden reasoning - (2499480) - Josey Morton
- (**llm**) trim base_url slashes and refuse a 200 that carries an error - (923d766) - Josey Morton
- (**llm**) verify num_ctx pre-send instead of trusting the count post-hoc - (b211d5e) - Josey Morton
- (**llm**) stop double-counting the sizing margin in the reserve check - (7e140b9) - Josey Morton
- (**llm**) rebuild the truncation guard on Ollama's probed clip arithmetic - (de5a2c2) - Josey Morton
- (**llm**) give prefill the full request budget before the idle timer - (445e186) - Josey Morton
- (**llm**) fail local streams that end before their terminator - (9a86420) - Josey Morton
- (**llm**) type provider refusals as Unsupported instead of a fake HTTP 0 - (c12ec8f) - Josey Morton
- (**serve**) never pass a provider's 2xx through with an error body - (d5be6c8) - Josey Morton
- (**serve**) a no_model kind, and the local hint for hung servers - (f8149fa) - Josey Morton
- (**tailor**) keep live dollar figures off local runs - (4a6626b) - Josey Morton
- (**wasm**) surface the model's own error message in export rejections - (60efa25) - Josey Morton
- (**web**) reject failed LLM proxy calls with the message, not the body - (81862b7) - Josey Morton
#### Documentation
- (**readme**) recommend MoE models and the LM Studio thinking toggle - (e907487) - Josey Morton
- (**readme**) update the local thinking-model guidance - (c77fa04) - Josey Morton
- (**readme**) document the local model providers - (c785a92) - Josey Morton
- reflow the local models guide to one line per paragraph - (147b4f8) - Josey Morton
- a setup guide for local models - (c141bc3) - Josey Morton
- add contributing and build instructions - (d952609) - Josey Morton
#### Refactoring
- (**llm**) share line draining between the local stream parsers - (57dab66) - Josey Morton

- - -

## v0.4.0 - 2026-07-04
#### Features
- (**release**) publish a Homebrew formula to joseym/homebrew-tap - (18070bd) - Josey Morton

- - -

## v0.3.1 - 2026-07-04
#### Bug Fixes
- (**ci**) review findings in the signing workflow - (2eb8e3e) - Josey Morton
#### Documentation
- the pr-run-mode comment describes the setting, not the rehearsal - (a293130) - Josey Morton
#### Continuous Integration
- PR runs return to plan-only - (223e6c1) - Josey Morton
- sign and notarize macOS release binaries - (b66726c) - Josey Morton

- - -

## v0.3.0 - 2026-07-04
#### Features
- (**fetch**) add LinkedIn job postings and a fetch timeout - (e1a3af2) - Josey Morton
- (**serve**) remove a build over DELETE /api/builds/:id - (010dd78) - Josey Morton
- (**serve**) persist objection triage per build - (5e45150) - Josey Morton
- (**serve**) stream /api/llm completions over server-sent events - (919b37c) - Josey Morton
- (**serve**) log upstream LLM and fetch failures to stderr - (884fe1b) - Josey Morton
- (**web**) collapse or expand all groups at once - (a44473e) - Josey Morton
- (**web**) collapsible groups, sort direction, and removal in the build list - (69dface) - Josey Morton
- (**web**) group and sort the build list - (80145de) - Josey Morton
- (**web**) persist objection triage across reloads - (4205a15) - Josey Morton
- (**web**) humanize the run overlay's streaming progress - (b70bcbd) - Josey Morton
- (**web**) consume the /api/llm token stream and tick live progress - (26fcb60) - Josey Morton
- (**web**) hold a screen wake lock during cancellable runs - (59d22ea) - Josey Morton
- (**web**) retry transient LLM proxy failures with backoff - (a0213be) - Josey Morton
#### Bug Fixes
- (**fetch**) keep escaped entities as literal JD text, accept more LinkedIn URLs - (8bf45bd) - Josey Morton
- (**serve**) chain the cause on a credential failure - (79fff03) - Josey Morton
- (**serve**) tag transient in-stream failures for the client retry budget - (89abdde) - Josey Morton
- (**serve**) log and return upstream causes, not just headlines - (d834614) - Josey Morton
- (**wasm**) render full error cause chains at the boundary - (75da35e) - Josey Morton
- (**web**) review findings in the build list controls - (9d1cc0c) - Josey Morton
- (**web**) keep triage.json true when objections resolve another way - (35a76fe) - Josey Morton
- (**web**) drop the word count from the streaming progress line - (e6d61ed) - Josey Morton
- (**web**) mobile topbar taps swallowed by the always-on drawer scrim - (34bb672) - Josey Morton
#### Build system
- reinstall script that re-signs for a stable keychain grant - (d96903d) - Josey Morton
#### Continuous Integration
- (**release**) publish aarg with --allow-dirty - (b97ac50) - Josey Morton

- - -

## v0.2.0 - 2026-07-03
#### Features
- (**agent**) give agents tools, with fetch_jd as the first - (39842c1) - Josey Morton
- (**agent**) retry once when a reply fails to parse - (d80d7df) - Josey Morton
- (**attack**) pick a build interactively when no id is given - (d9d1686) - Josey Morton
- (**attack**) aarg attack re-reviews a saved build without re-tailoring - (51a7465) - Josey Morton
- (**chat**) add an interactive Q/A about a job description - (24e052d) - Josey Morton
- (**cli**) paste a job description and reuse entered ones - (78ed400) - Josey Morton
- (**cli**) pick a past JD when tailor/gap get none - (21ac7e6) - Josey Morton
- (**cli**) manage templates and render with the selected one - (e83053b) - Josey Morton
- (**cli**) wire up init, config, and llm ping - (2677656) - Josey Morton
- (**completions**) print shell completion scripts - (0e525a4) - Josey Morton
- (**config**) make headless auth env var names configurable - (b819370) - Josey Morton
- (**config**) fetch a delegated key's token via a configurable command - (8409a2f) - Josey Morton
- (**config**) track named API keys and an active label - (e0c81f2) - Josey Morton
- (**config**) make the tailor loop limits tunable via [limits] - (b73a932) - Josey Morton
- (**config**) load and save settings in a per-os config file - (3df159a) - Josey Morton
- (**cover**) generate a cover letter from a build - (e85f1f6) - Josey Morton
- (**dataset**) edit the dataset in your editor - (c8f4447) - Josey Morton
- (**dataset**) add show and validate commands - (d5170b7) - Josey Morton
- (**dataset**) persist the dataset as schema-versioned json - (8cb8e84) - Josey Morton
- (**dataset**) model the resume as evidence-linked types - (be7050f) - Josey Morton
- (**domain**) extract the remaining copilots into the domain - (6c673bc) - Josey Morton
- (**domain**) classify each draft line's provenance against the dataset - (578bb1f) - Josey Morton
- (**domain**) extract the copilots and the revision loop into the domain - (6393f0a) - Josey Morton
- (**enrich**) copilot the user through fleshing out thin roles - (6a0b686) - Josey Morton
- (**evals**) add a keyless eval harness runnable as a binary - (6fff01b) - Josey Morton
- (**experience**) record non-job experience and link it to skills - (e44e1aa) - Josey Morton
- (**export**) copy a build's PDFs out under friendly names - (60f550b) - Josey Morton
- (**gap**) sort jd requirements into matched, weak, and unknown - (b5df99f) - Josey Morton
- (**guide**) let users ask an honest advisor during verification - (b03465a) - Josey Morton
- (**history**) pick builds to delete from a checklist when no ids are given - (e034a65) - Josey Morton
- (**history**) aarg history, diff, and history rm over the build dirs - (6c9653b) - Josey Morton
- (**ingest**) read images and scanned PDFs with model vision - (ca90eb6) - Josey Morton
- (**ingest**) read text-layer PDFs for resumes and job descriptions - (582a88f) - Josey Morton
- (**ingest**) offer to capture a voice sample after onboarding - (08110eb) - Josey Morton
- (**ingest**) build the dataset from an existing resume - (59c78ef) - Josey Morton
- (**init**) create a local workspace by default - (e47e5f3) - Josey Morton
- (**jd**) remember parsed postings and rate fit against them - (29f8eab) - Josey Morton
- (**jd**) fetch greenhouse and lever postings by url - (4c88fd0) - Josey Morton
- (**jd**) parse job descriptions into structured requirements - (d42f59b) - Josey Morton
- (**keys**) delegate plan tokens to the official Anthropic CLI - (38af0fd) - Josey Morton
- (**keys**) store and select Claude-subscription credentials - (fb5c62b) - Josey Morton
- (**keys**) store keys per label, detect on init, and manage them - (92b611c) - Josey Morton
- (**llm**) carry image and PDF attachments in the message wire format - (7fc41a3) - Josey Morton
- (**llm**) authenticate with a Claude-plan OAuth token - (daa194d) - Josey Morton
- (**llm**) add a hand-rolled anthropic client - (671af4b) - Josey Morton
- (**llm**) add a mock client for offline tests - (0953954) - Josey Morton
- (**llm**) define the client trait and shared request types - (c80617a) - Josey Morton
- (**mcp**) preview a build's resume as an inline image - (5098761) - Josey Morton
- (**mcp**) run AARG as an MCP server over stdio with elicitation - (68f081e) - Josey Morton
- (**metric**) ask the user for the numbers the reviewer wants - (a92ab75) - Josey Morton
- (**mirror**) surface JD wording for keywords a recorded skill backs - (2a4c7d2) - Josey Morton
- (**model**) tier each agent's model by the judgment its job needs - (bd528e9) - Josey Morton
- (**open**) open a build's PDFs in the system viewer - (6f72a64) - Josey Morton
- (**pricing**) per-build cost estimate and a budget warning - (1fa721a) - Josey Morton
- (**readability**) check rendered resumes for page count and density - (4ed8e18) - Josey Morton
- (**render**) resolve typst from config and standard locations - (6569bb9) - Josey Morton
- (**render**) re-render a saved build without the tailor loop - (e919f09) - Josey Morton
- (**render**) add the editorial built-in template - (41a3b26) - Josey Morton
- (**render**) ship minimal and technical built-in templates - (038a28c) - Josey Morton
- (**repl**) offer build-id completion for aarg cover - (f0b0b1a) - Josey Morton
- (**repl**) complete build ids and key labels - (be2491b) - Josey Morton
- (**repl**) tab-complete commands, subcommands, and flags - (4b9b9b0) - Josey Morton
- (**repl**) drop into an interactive shell when run with no command - (c647230) - Josey Morton
- (**review**) let the user accept an objection so it stops being flagged - (e737fb7) - Josey Morton
- (**review**) add the adversarial reviewer agent - (7882973) - Josey Morton
- (**secrets**) store provider api keys in the os keychain - (9c36e75) - Josey Morton
- (**serve**) embed the built web app in the binary - (c1fb255) - Josey Morton
- (**serve**) GET /api/models exposes the configured model tiers - (99dda2a) - Josey Morton
- (**serve**) POST /api/builds to persist a browser-run build - (f64726d) - Josey Morton
- (**serve**) include the variant payloads in a build's detail - (b0f1f25) - Josey Morton
- (**serve**) reach the app from the network behind an opt-in flag - (e9ea7cd) - Josey Morton
- (**serve**) fall back to index.html for client-side routes - (6b37ed4) - Josey Morton
- (**serve**) give the browser split build fields and a cost route - (87d6fd2) - Josey Morton
- (**serve**) serve the workspace, model, and renderer over local HTTP - (3b1a94a) - Josey Morton
- (**skills**) add a skill through an evidence interview - (5b756c9) - Josey Morton
- (**skills**) gate near-duplicate skills and add `skills dedup` to prune them - (53dcf87) - Josey Morton
- (**stream**) show live token count and cost during the tailoring loop - (86f6a99) - Josey Morton
- (**strengthen**) suggest a grounded rewrite for a flagged bullet - (f883189) - Josey Morton
- (**strengthen**) copilot the user through the reviewer's weak-wording flags - (033bbc8) - Josey Morton
- (**style**) syntax-highlight trace JSON and diff rewordings - (28a9fe6) - Josey Morton
- (**style**) add semantic output vocabulary with glyphs and grading - (2441f89) - Josey Morton
- (**style**) color and a progress spinner for the tailor loop - (e18c4f6) - Josey Morton
- (**summary**) refine the resume summary from recorded history - (9bd1f65) - Josey Morton
- (**tailor**) tune the finished draft in plain words before saving - (c07ceb6) - Josey Morton
- (**tailor**) offer to refine any flagged bullet or the summary - (7f29242) - Josey Morton
- (**tailor**) strip AI-tell em and en dashes from finalized prose - (3e4582a) - Josey Morton
- (**tailor**) offer to record missing skills during a tailor run - (43b600d) - Josey Morton
- (**tailor**) render achievements and surface unbacked skills in output - (31fb243) - Josey Morton
- (**tailor**) triage remaining objections with refine or accept - (116f7ca) - Josey Morton
- (**tailor**) honor a user-confirmed summary in tailoring - (7b59465) - Josey Morton
- (**tailor**) render the human variant with a custom --template - (01f8f1a) - Josey Morton
- (**tailor**) render an ATS and a human resume from one draft - (0d8d87c) - Josey Morton
- (**tailor**) dedup and cap the skills section - (85cbcc5) - Josey Morton
- (**tailor**) cap bullets per role so the resume can't run long - (33d9cb5) - Josey Morton
- (**tailor**) add a target-title headline derived from the JD - (d8a21fd) - Josey Morton
- (**tailor**) revise the draft against reviewer objections in a loop - (72c11f1) - Josey Morton
- (**tailor**) tailor the resume to a jd and render the ats pdf - (cbd3a11) - Josey Morton
- (**templates**) resolve template names to built-ins or user files - (f1042ed) - Josey Morton
- (**trace**) record every agent run and read it back from the cli - (e4bd943) - Josey Morton
- (**tune**) re-tune a saved build from the command line - (0e7787e) - Josey Morton
- (**tune**) classify plain-language edits into grounded changes - (8c4d594) - Josey Morton
- (**variant**) group the human resume's skills - (1473e75) - Josey Morton
- (**variant**) project the canonical draft into claim-checked variants - (54586ec) - Josey Morton
- (**verify**) never offer the job title as a verifiable skill - (95d0b8d) - Josey Morton
- (**verify**) expose clarification while populating and collapse duplicate keywords - (adc39c1) - Josey Morton
- (**verify**) add a help-me-decide pass after the keyword checklist - (5697a06) - Josey Morton
- (**verify**) triage all unbacked job keywords, not just unmatched skills - (f195cf3) - Josey Morton
- (**verify**) remember declined skills and lead clarification with a description - (8ac952d) - Josey Morton
- (**verify**) interview the gap's unknown skills inside tailor - (3bea3ff) - Josey Morton
- (**verify**) interview the user to back skills with evidence - (47c6bca) - Josey Morton
- (**voice**) rewrite caller-named lines toward a requested tone - (4f0eacd) - Josey Morton
- (**voice**) tighten rewrites for impact, bounded to the user's own register - (5189534) - Josey Morton
- (**voice**) remove a sample with `aarg voice remove <id>` - (9dd93c6) - Josey Morton
- (**voice**) rewrite cliche-laden lines toward the user's samples - (41ae270) - Josey Morton
- (**voice**) capture writing samples with `aarg voice add|list` - (030b9c0) - Josey Morton
- (**wasm**) bind the human variant, live loop progress, and coverage - (b4d32f2) - Josey Morton
- (**wasm**) bind the remaining copilots to the browser - (36f017f) - Josey Morton
- (**wasm**) drive the interviews and the revision loop from the browser - (b8681a2) - Josey Morton
- (**wasm**) run the model-driven pipeline over a host callback - (a025d34) - Josey Morton
- (**wasm**) add a browser harness for the deterministic core - (ed3f89b) - Josey Morton
- (**wasm**) bind the deterministic domain core to JavaScript - (69f0af3) - Josey Morton
- (**web**) picking a template jumps to Pixel-perfect - (5692365) - Josey Morton
- (**web**) a sticky pending-edits bar replaces the buried edit actions - (aaa62f6) - Josey Morton
- (**web**) edits flow into the rendered PDFs, with undo history - (66b2bb3) - Josey Morton
- (**web**) make the claim-check badge actionable - (056e958) - Josey Morton
- (**web**) lay the score panel out as a full-width stat band - (e2f1918) - Josey Morton
- (**web**) one score language + centered layout on large screens - (8e1362c) - Josey Morton
- (**web**) retailor from an existing JD - (df1e108) - Josey Morton
- (**web**) wire the coverage-map actions to the copilots - (edd07a0) - Josey Morton
- (**web**) New-Build live loop + honor the server's model tiers - (a6ce1d7) - Josey Morton
- (**web**) record a free edit back into the dataset - (fa685a9) - Josey Morton
- (**web**) run the refine copilots from the drawer - (5393788) - Josey Morton
- (**web**) interactive copilot foundation — Q&A modal + live progress - (281043d) - Josey Morton
- (**web**) restore build provenance in the unified build header - (c4a42d1) - Josey Morton
- (**web**) share one animated coverage score across both screens - (bae15d0) - Josey Morton
- (**web**) build the coverage and tailoring screens - (f4f19f3) - Josey Morton
- (**web**) scaffold the Angular + Tailwind browser app - (3389f59) - Josey Morton
- (**workspace**) redirect the workspace from the global config - (90b8505) - Josey Morton
- (**workspace**) resolve storage roots through a workspace module - (e8a7bb2) - Josey Morton
- persist workspace edits into the build, with an honest history - (ac2bff7) - Josey Morton
- pixel-perfect PDF preview, template picker, collapsible sidebar - (3e154d5) - Josey Morton
- let the adversarial loop be stopped between passes - (ca42fa9) - Josey Morton
- target "Fill the gap" at the clicked requirement - (170cd00) - Josey Morton
- extract the resume domain into a portable aarg-domain crate - (fe0ea9f) - Josey Morton
#### Bug Fixes
- (**ats**) credit recorded skills in the coverage report - (aaae570) - Josey Morton
- (**cli**) route 429 rate limits to their own diagnostic - (f06d1e8) - Josey Morton
- (**completions**) tell users how to install the script - (b47d0a2) - Josey Morton
- (**core**) keep time from panicking on browser wasm - (dd1b025) - Josey Morton
- (**llm**) present plan tokens as the official client so they aren't rate-limited - (ca5df83) - Josey Morton
- (**llm**) strip whitespace from credentials before the auth header - (2b55dff) - Josey Morton
- (**mcp**) hand rendered PDFs to the client as resource links - (ae87329) - Josey Morton
- (**metric**) show which role a flagged bullet belongs to - (5c9402c) - Josey Morton
- (**metric**) record the number on the field, not appended to the text - (6aabd57) - Josey Morton
- (**mirror**) skip a phrase a recorded skill already covers verbatim - (5b972da) - Josey Morton
- (**pricing**) don't show per-request dollar cost on a subscription - (9e235d4) - Josey Morton
- (**render**) check typst is installed before the work that needs it - (c4f7089) - Josey Morton
- (**render**) keep role headers clear of their first bullet - (fed8150) - Josey Morton
- (**serve**) enforce the claim-divergence guard on POST /api/builds - (d7ea506) - Josey Morton
- (**serve**) set cache headers so a rebuilt app never serves stale - (fe1e4e9) - Josey Morton
- (**tailor**) collapse near-duplicate skills instead of stuffing the list - (17615a1) - Josey Morton
- (**tailor**) make the captured-metric directive mandatory - (4032edb) - Josey Morton
- (**tailor**) keep metric-bearing bullets when capping a role - (8e1aed2) - Josey Morton
- (**tailor**) tell the model to fold in a captured metric, not just offer it - (48851b9) - Josey Morton
- (**tailor**) keep a per-role bullet floor so resumes don't go lopsided - (fb4ab07) - Josey Morton
- (**tailor**) keep every role so the work history has no gaps - (a4ae19e) - Josey Morton
- (**templates**) drop em-dashes from rendered resume output - (3a62c83) - Josey Morton
- (**templates**) space out grouped skills in the editorial sidebar - (3241a9e) - Josey Morton
- (**templates**) keep Experience on the first page of the human resume - (5ab24c7) - Josey Morton
- (**variant**) tighten the human resume's skill grouping - (78b9adf) - Josey Morton
- (**variant**) curate the human resume's skills for a person - (3185bb9) - Josey Morton
- (**voice**) steer the bullet writers off em-dashes - (c5fef84) - Josey Morton
- (**voice**) also rewrite raw, un-bullet-like lines, not just cliches - (69d9d49) - Josey Morton
- (**voice**) open an editor for `voice add` instead of silently reading stdin - (fdde241) - Josey Morton
- (**wasm**) surface load failures and stale snapshots in the harness - (dabca97) - Josey Morton
- (**web**) no em-dashes reach the screen - (a17f840) - Josey Morton
- (**web**) group each coverage requirement as a card on mobile - (6d7e2b5) - Josey Morton
- (**web**) address copilot review findings (H1/H2/M2/M4) - (e44515e) - Josey Morton
- (**web**) match the view toggle across both screens - (317e4ae) - Josey Morton
- (**web**) match the provenance JSON shape so the workspace renders - (3ab6448) - Josey Morton
- (**web**) preview the ATS payload when a build has no human variant - (255f7e4) - Josey Morton
- bump actions/checkout to v5 - (ba88847) - Josey Morton
- gate render-dependent readability tests on typst availability - (fac5155) - Josey Morton
- clean PDF viewer chrome + reframe the weak screenshots - (18da667) - Josey Morton
- Download PDF 400s + make unrecorded lines visually loud - (86d2294) - Josey Morton
- harden the copilot loop — aborts keep evidence, saves keep the dataset - (b21270e) - Josey Morton
#### Documentation
- (**demo**) commit a PII-free demo workspace fixture + capture script - (2efe5dd) - Josey Morton
- (**design**) document the adversarial review loop - (096224a) - Josey Morton
- (**design**) describe the agent runtime - (7f055b8) - Josey Morton
- (**readme**) document features, commands, the MCP server, and demos - (6ad4f77) - Josey Morton
- (**readme**) add architecture diagram, demo GIF, and the runtime narrative - (56f51c0) - Josey Morton
- (**readme**) list cover-letter generation as shipped - (89f60b1) - Josey Morton
- (**readme**) note cover-letter generation as a stretch goal - (5637dc6) - Josey Morton
- clean leftover writing tells in older docs - (afbd136) - Josey Morton
- another pass for writing that reads machine-made - (934ab05) - Josey Morton
- the releasing checklist is maintainer-internal - (947fd43) - Josey Morton
- no-clone install channels and a releasing checklist - (9f82f12) - Josey Morton
- sweep the README against the wider tell catalog - (6aec1f2) - Josey Morton
- rewrite the README in a human register - (4a8da3e) - Josey Morton
- a real path from clone to browser workspace - (2e8c76a) - Josey Morton
- capture five more workspace screens and show the strongest three - (b4ff8cf) - Josey Morton
- strip the em-dashes the rewrite introduced - (d590151) - Josey Morton
- bring the README into the browser era - (df2a377) - Josey Morton
- add the demo tapes, fixtures, and recorded GIFs - (0cafe28) - Josey Morton
- introduce the project in the readme - (7c28547) - Josey Morton
#### Build system
- add core dependencies for the cli skeleton - (3af2f12) - Josey Morton
- scaffold the aarg binary crate - (774f1d5) - Josey Morton
#### Continuous Integration
- (**release**) publish the crates from the release workflow - (4e42452) - Josey Morton
- (**release**) PR runs go back to plan-only - (820aa38) - Josey Morton
- (**release**) cargo-dist pipeline with the frontend built on runners - (15aa446) - Josey Morton
- check formatting, lints, and tests on every push - (0585790) - Josey Morton
#### Refactoring
- (**agent**) extract the agent runtime from four working features - (367f0c7) - Josey Morton
- (**commands**) share the provider preamble across llm commands - (53dffbc) - Josey Morton
- (**core**) split the agent runtime into the aarg-core crate - (54b0fb3) - Josey Morton
- (**web**) collapse the two build screens into one - (1546cd4) - Josey Morton
- (**web**) share the coverage map and view toggle across both screens - (305e3e0) - Josey Morton
#### Miscellaneous Chores
- (**config**) default new installs to haiku - (3083d30) - Josey Morton
- (**release**) version bumps via cog bump - (f7f9133) - Josey Morton
- (**release**) version 0.2.0, correct repo URLs, crates.io packaging - (201db64) - Josey Morton
- dual-license under MIT or Apache-2.0 - (8702070) - Josey Morton
- add vs code workspace - (0795680) - Josey Morton
- ignore cargo build artifacts - (6b64689) - Josey Morton
- enforce conventional commit messages with cocogitto - (b62b6ad) - Josey Morton
#### Style
- (**cli**) route ingest output through the vocabulary - (d5d2a8e) - Josey Morton
- (**cli**) apply the output vocabulary across commands - (b4c2858) - Josey Morton
- (**template**) tighten editorial margins and fix name spacing - (8e4b776) - Josey Morton

- - -

Changelog generated by [cocogitto](https://github.com/cocogitto/cocogitto).