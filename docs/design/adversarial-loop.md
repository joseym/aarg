# The adversarial loop

aarg tailors a résumé to a job description and then *attacks its own
output*: a skeptical reviewer criticizes each draft, the tailoring agent
revises against the criticism, and the process repeats under tight bounds
until the draft stops improving. This document explains how that loop is
shaped, the three properties that keep it from running away or lying, and
why some objections are handed back to the human instead of fed to another
revision.

The loop is the orchestrator (`src/commands/tailor.rs`). It is not a new
subsystem so much as a Phase-1 sequence, *tailor → render → score*, wrapped
in a bounded review-and-revise cycle once that sequence worked end to end.

## One iteration is a draft plus a verdict

The unit the loop operates on is an *evaluation*: take a draft, get a
number, keep the artifacts.

```rust
async fn evaluate(...) -> Result<Evaluation, CliError> {
    let run = AdversarialReviewerAgent.run(ctx, ReviewInput { draft, jd, dataset }).await?;
    let pdf = render::render_ats(&iter_dir, &resume)?;
    let ats_report = ats::keyword_coverage(jd, gap, dataset, &page_text);
    let score = combined_score(&run.output, ats_report.coverage);
    // ... write draft.json + both reports into iterations/<n>/
}
```

Scoring a draft means *both* asking the reviewer and rendering the PDF for
keyword coverage, because the two catch different failures. The reviewer is
a language model with judgment but no ground truth about an applicant
tracking system; coverage is deterministic and checkable but blind to
whether a bullet is any good. They are fused:

```rust
fn combined_score(report: &AdversarialReport, coverage: f32) -> f32 {
    0.6 * report.overall_score + 0.4 * coverage
}
```

Weighting content above coverage (0.6 / 0.4) says quality leads and
keyword-matching is a constraint, not the goal, but folding coverage in at
all anchors the score to a fact the model can't talk its way around. A
draft can't score well on persuasive prose while missing the phrases a
keyword screen filters on.

Every iteration writes its draft, PDF, and both reports into its own
`iterations/<n>/` directory, so the entire loop is reconstructable after the
fact and the agent calls show up in `aarg trace last`.

## The reviewer is an adversary, not an editor

The reviewer (`src/review.rs`) is prompted as a skeptical hiring manager
and reads the job description's raw text as ground truth. Two design
choices make it useful rather than dangerous:

**It flags, it never edits.** The reviewer returns structured objections: each with a target (a specific bullet, the summary, the skills section), a
severity (blocking / major / minor), and a kind (no metric, vague verb,
unsupported claim, generic phrasing, JD mismatch, dense layout), plus a
short verdict. It never rewrites text. Keeping criticism and rewriting in
separate agents means the thing judging the draft has no power to quietly
launder a fabrication into it.

**Its output is treated as untrusted.** Objections that target a bullet
that doesn't exist are dropped, the overall score is clamped to a valid
range, and unknown enum values decode to the safe default rather than
failing the run. The reviewer is a model; the orchestrator assumes it can
be wrong about what's on the page and refuses to act on phantom targets.

The structured shape is what lets each objection become one precise
revision instruction: "fix this bullet, for this reason" beats "make it
better," which is the whole reason the objection carries an id:

```rust
fn format_objection(objection: &Objection) -> String {
    // "bullet-3 (vague verb, major): \"Helped\" hides what you did, try: lead with the action"
}
```

## Three properties keep the loop honest

A self-criticizing loop has two obvious failure modes: it oscillates
forever, or it talks itself into a worse draft. Three rules close both.

```rust
let mut best = evaluate(..., 0, first.resume, ...).await?;     // iteration 0
for iteration in 1..=max_revisions {
    if best.score >= acceptable_score || !best.report.has_blocking_or_major() {
        break;                                                  // good enough, stop spending
    }
    let objections = best.report.actionable().map(format_objection).collect();
    let revised   = tailor_resume(..., Some(RevisionContext { objections })).await?;
    let candidate = evaluate(..., iteration, revised.resume, ...).await?;
    if candidate.score > best.score { best = candidate; } else { break; }
}
```

- **A hard cap** (`config.limits.revisions`, default 2) bounds total cost
  no matter what the reviewer keeps finding. The loop also exits early once
  the draft clears an acceptable score or has nothing blocking/major left.
  There's no point spending tokens polishing a draft that's already strong.
- **A score-must-improve gate.** A revision that scores no better than what
  we already have is discarded *and the loop stops*. The model isn't
  monotonic; "revise again" can make things worse, and the gate refuses to
  chase it.
- **Best-draft-wins.** `best` is replaced only on strict improvement, so the
  finalized build is the best draft the loop ever saw, never merely the
  last one. The PDF you get is the high-water mark.

## Never-fabricate survives revision

aarg's central guarantee is that nothing reaches the page without tracing to
recorded evidence. A revision loop is where that guarantee is most at risk:
the reviewer says "this bullet needs a number," and the easy fix is to
invent one.

The loop closes that hole structurally rather than by trusting a prompt. A
revision is *the same `tailor_resume` call* as the first draft, just with the
objections attached, so it runs through *the same* assembly: the model
speaks in evidence IDs, a number it introduces that the source bullet
doesn't contain is reverted, and a skill without backing evidence is
dropped. A revision cannot introduce a fabrication the first draft couldn't,
because both pass through identical guards.

Two narrower decisions reinforce it:

**Only content objections drive revisions.** `actionable()` filters to
objections scoped to the canonical draft; layout complaints are held back
(presentation is a separate concern, addressed by the variant layer in a
later phase). A "too dense" objection can't trigger a rewrite of the facts.

**Coverage informs the score, never a prompt.** The ATS report's
*missing* keywords feed the combined score and the user-facing coverage
summary, and stop there. They never enter a revision or reviewer prompt.
The obvious-but-wrong feature ("ATS says this phrase is missing, so insert
it") is exactly the backdoor never-fabricate forbids, so the loop revises on
reviewer objections alone and the missing-keyword path is reporting-only by
construction.

## What the loop can't fix, a human supplies

Some objections are unfixable *by a revision* without lying. "This bullet
states an outcome with no number" can't be satisfied by a model that's
forbidden to invent the number. "This wording is weak / generic / doesn't
match the JD" can't be satisfied by making the claim bigger than the truth.
That's inflation, the same thing never-fabricate exists to stop.

So the orchestrator pulls those objection kinds *out* of the revision stream
and turns them into a short interview instead:

- **Metric capture** takes the `no metric` objections, asks the applicant a
  leading question per flagged bullet, and folds their real figures into the
  dataset.
- **Strengthen** takes the wording objections (vague verb, unsupported
  claim, generic phrasing, JD mismatch), interviews the applicant for the
  underlying facts, and drafts a stronger bullet *they approve* before it's
  recorded.

Both then **re-tailor from the corrected history**, so the applicant's own
words flow back through the same guards as everything else. The number
traces to the person, never the model. This is the loop's organizing
principle made explicit: the machine revises what it can honestly revise,
and the human is asked for exactly the facts the machine is forbidden to
guess. These detours run only when someone is at the keyboard; a piped or CI
run tailors straight through.

A third detour handles the opposite case, an objection the applicant
judges *intentional* (a one-line 2013 role that's meant to stay one line).
Because the reviewer is stateless and re-reads the whole résumé every run,
it would re-raise that forever. The loop lets the applicant **accept** an
objection; accepted objections are remembered and filtered from future runs.
The score is always computed from the *full* report and the full
report is what's written to disk. Accepting a weakness stops the nagging;
it does not inflate the number or hide the weakness from the record.

## Voice runs last, but is still judged

After the loop names its winner, an optional voice pass rewrites the
draft's AI-sounding lines toward the applicant's own writing samples. Voice
runs *outside* the loop, which would normally mean its output is the one
piece of page text the reviewer never saw. So the voiced draft is run back
through `evaluate` and adopted only if it scores at least as well:

```rust
let voiced_eval = evaluate(..., voiced, ...).await?;
if voiced_eval.score >= best.score { best = voiced_eval; }   // else keep the un-voiced draft
```

That re-score restores both safety legs at once: the reviewer vets the
rewritten text (a non-numeric inflation that slipped the fact guard draws an
objection), and the loop's own score-must-improve rule means a voice rewrite
can never ship a draft worse than the one it replaced.

## Live, interruptible, and priced

The loop's expensive calls (the tailoring, the review, the voice rewrite)
run on the strongest model tier, and a full pass can be several of them.
Rather than make the applicant wait blind, those calls *stream*: the model's
output and a running cost estimate appear live as it generates.

```
⠹ tailoring · ~1.2k tok · ~$0.18 so far
```

This is wired through the runtime, not bolted onto the command. An optional
streaming sink on the agent context lets the orchestrator install a live
status line; the spine streams a run when a sink is listening, the run has no
tools, and it's on the strong tier (the long, costly calls, exactly the
ones worth showing). The runtime stays ignorant of terminals and dollars;
the binary supplies the rendering and the price tables. Token usage is
exact only at the end of a stream, so the live figure is an estimate that
trues up to the real total as each call settles. The figure is marked `~` throughout, a
budget signal rather than an invoice. The point is interruptibility: a run
heading somewhere expensive is visible while it happens, not after.

The same machinery powers `aarg attack`, which re-runs just the reviewer
against a saved build's draft and the *current* dataset, a cheap second
opinion after backing a skill or accepting an objection, without paying for
a fresh tailor-and-render loop.

## Everything is inspectable

The loop optimizes a single combined score, but it never throws away the
evidence for that score. Each iteration's draft, rendered PDF, adversarial
report, and coverage report are written under `builds/<id>/iterations/<n>/`;
the build root holds the winning artifacts and a `meta.json` whose token
total sums *every* agent call in the loop (failed attempts and all), so a
build's recorded cost is honest. The reviewer's full report is always the
on-disk artifact, even when the applicant has accepted some of its
objections; the filtered view is for the live display and the next
iteration, not the record.

## Deliberately later

Held for a later phase, each waiting on its own consumer rather than
forgotten: the human-facing résumé variant and its claim-divergence lint
(which is why layout objections are filtered out of revisions now; they'll
route to the variant adapter), deterministic readability checks on the
rendered PDF, and an experimental vision pass over the layout. The loop's
shape is built to accommodate them: the canonical draft stays the single
source of claims, and presentation concerns already travel a separate
channel. But none of them is written until the variant layer that consumes
them exists.
