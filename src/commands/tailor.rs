//! `aarg tailor <jd>` — the adversarial loop end to end: parse the JD,
//! analyze the gap, tailor a draft, then let a skeptical reviewer
//! criticize it and revise until the draft stops improving or a hard
//! cap is hit. Every iteration's artifacts land under
//! `builds/<id>/iterations/<n>/`; the build root holds the best draft.
//!
//! This is the PRD's Orchestrator: the Phase 1 sequence (tailor →
//! render → coverage) is now one iteration of a bounded review loop
//! (PRD §6.4). Three properties keep the loop honest: a hard revision
//! cap bounds cost, a score-must-improve gate means a worse revision is
//! discarded, and the loop keeps the best draft it saw rather than the
//! last one.

use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::agent::{Agent, AgentContext, ModelTier};
use crate::ats::{self, AtsReport, EvidenceStatus, KeywordKind};
use crate::builds::{self, BuildError, BuildMeta};
use crate::commands::{CliError, configured_client, load_requirements};
use crate::config::Config;
use crate::dataset::store;
use crate::dataset::types::{ResumeDataset, SkillCategory};
use crate::enrich;
use crate::gap::{GapReport, analyze_gap};
use crate::jd::JobRequirements;
use crate::llm::TokenUsage;
use crate::metric::{self, MetricTarget};
use crate::readability::{self, ReadabilityReport};
use crate::render;
use crate::review::{
    AdversarialReport, AdversarialReviewerAgent, Objection, ObjectionKind, ObjectionTarget,
    ReviewInput, Severity,
};
use crate::strengthen::{self, InterviewLimits, StrengthenTarget};
use crate::style::{self, Spinner, StreamReporter};
use crate::summary;
use crate::tailor::{JdId, RevisionContext, TailorOutcome, TailoredResume, tailor_resume};
use crate::templates;
use crate::user::{Answer, Question, UserHandle};
use crate::variant::{self, TemplateId, Variant, VariantAdapterAgent, VariantInput};
use crate::verify::{add_one_skill, unbacked_keywords, verify_keywords};
use crate::voice;

// Each copilot guards on two distinct conditions: is there work to do (an
// objection, a thin role, an unbacked keyword) and is a person here to answer.
// They're kept as nested `if`s for readability; they only *read* as collapsible
// because the refactor to an injected `user` removed the binding that used to
// sit between the two checks.
#[allow(clippy::collapsible_if)]
pub async fn run(
    jd: Option<PathBuf>,
    variants: Vec<Variant>,
    human_template: Option<PathBuf>,
    cover: bool,
    user: &dyn UserHandle,
) -> Result<(), CliError> {
    // A custom template applies to the human variant; reject it up front if
    // that variant won't be rendered, rather than silently ignoring it.
    if human_template.is_some() && !variants.contains(&Variant::Human) {
        return Err(CliError::TemplateWithoutHuman);
    }
    // Fail fast if `typst` isn't installed: the loop renders to score and to
    // finalize, so without this the missing binary only surfaces after a whole
    // round of (paid) LLM calls. Check before spending anything.
    crate::render::ensure_available()?;
    let mut dataset = store::load()?;
    let (client, config) = configured_client().await?;
    let tracer = super::default_tracer()?;
    // On a Claude plan the request cost is covered by the flat fee, so dollar
    // estimates are suppressed everywhere this run reports cost.
    let subscription = client.is_subscription();
    // Live progress + running cost for the long smart-tier calls, so the
    // loop is visibly working and the user can interrupt (FR-3.8). The spine
    // drives it for streamed runs; cheap/interactive calls leave it idle.
    let reporter = StreamReporter::new(config.prices.clone(), subscription);
    let ctx = AgentContext {
        llm: &client,
        model: &config.anthropic,
        tracer: &tracer,
        sink: Some(&reporter),
    };
    // Tailoring runs on the smart tier; show/record that model.
    let model = ctx.model.resolve("tailoring_v1", ModelTier::Smart);

    // Loop limits come from config (with PRD defaults), so a longer or
    // cheaper run is a config edit, not a recompile.
    let max_revisions = config.limits.revisions;
    let acceptable_score = config.limits.acceptable_score;
    let interview_limits = InterviewLimits {
        questions: config.limits.strengthen_questions,
        revises: config.limits.strengthen_revises,
    };

    // A JD argument is parsed (file/URL/stdin); with none, offer the JDs from
    // past builds to reuse — loaded off disk, so no model call. A picker that
    // returns nothing (no past builds, or a piped/CI run) has already said how
    // to proceed, so there's nothing left to tailor.
    let requirements = match &jd {
        Some(path) => load_requirements(path, &ctx).await?,
        None => match super::prompt_for_jd(&ctx).await? {
            Some(requirements) => requirements,
            None => return Ok(()),
        },
    };

    eprintln!(
        "\n{}",
        style::bold(format!("{} @ {}", requirements.title, requirements.company))
    );
    eprintln!("{}", style::dim(format!("tailoring on {model}")));

    let sp = Spinner::start("analyzing the gap");
    let mut gap = analyze_gap(&ctx, &requirements, &dataset).await?;
    sp.finish(style::done(format!(
        "gap analyzed: {} matched, {} weak, {} unknown",
        gap.matched.len(),
        gap.weak.len(),
        gap.unknown.len()
    )));

    // When a person is driving, offer to back the JD keywords the
    // dataset can't yet support — unmatched skills *and* ATS phrases the
    // user might genuinely have. This turns real-but-unrecorded
    // experience into evidence the tailoring can use. Already-declined
    // keywords are excluded by `unbacked_keywords`, so the list shrinks
    // each run; a piped or CI run skips the whole detour (no one to ask).
    let candidates = unbacked_keywords(&dataset, &requirements, &gap);
    if !candidates.is_empty() {
        if user.is_interactive() {
            let wants = user
                .confirm(
                    &format!(
                        "review {} job keyword(s) you might be able to back?",
                        candidates.len()
                    ),
                    true,
                )
                .await
                .unwrap_or(false);
            if wants {
                let outcome = verify_keywords(&mut dataset, &candidates, user, Some(&ctx)).await?;
                if outcome.changed() {
                    dataset.metadata.updated_at = Utc::now();
                    store::save(&dataset)?;
                    // Only a newly added skill changes the gap; bare
                    // declines just get remembered, no re-analysis needed.
                    if outcome.verified > 0 {
                        let sp = Spinner::start(format!(
                            "recorded {} new skill(s); re-analyzing the gap",
                            outcome.verified
                        ));
                        gap = analyze_gap(&ctx, &requirements, &dataset).await?;
                        sp.finish(style::done("gap re-analyzed"));
                    }
                }
            }
        }
    }

    // History copilot: a role thin on detail shouldn't be stripped or
    // padded with its own weak lines — offer to flesh it out first, so
    // tailoring works from a fuller history. The added bullets are the
    // user's own words (`enrich`), JD-agnostic on purpose. A piped/CI run
    // skips this silently.
    let thin = enrich::thin_roles(&dataset);
    if !thin.is_empty() {
        if user.is_interactive() {
            let names: Vec<String> = thin
                .iter()
                .filter_map(|id| dataset.roles.iter().find(|r| &r.id == id))
                .map(|r| r.company.clone())
                .collect();
            let wants = user
                .confirm(
                    &format!(
                        "{} role(s) are thin on detail ({}). Flesh them out with a few questions?",
                        thin.len(),
                        names.join(", ")
                    ),
                    true,
                )
                .await
                .unwrap_or(false);
            if wants {
                let outcome = enrich::enrich_roles(&mut dataset, &thin, user, &ctx).await?;
                if outcome.changed() {
                    dataset.metadata.updated_at = Utc::now();
                    store::save(&dataset)?;
                    eprintln!(
                        "{}",
                        style::done(format!(
                            "added {} bullet(s) across {} role(s)",
                            outcome.bullets_added, outcome.roles_touched
                        ))
                    );
                }
            }
        }
    }

    let build = builds::create_next()?;
    let jd_id = JdId(slug(&requirements.company, &requirements.title));
    builds::write_json(&build.dir, "jd.json", &requirements)?;
    builds::write_json(&build.dir, "gap_report.json", &gap)?;
    eprintln!("{}", style::dim(format!("build {}", build.id.0)));

    let mut total = TokenUsage::default();

    // The streamed smart-tier calls below (tailoring, review, voice) show a
    // live token/cost line via the reporter instead of a spinner.
    let mut first = tailor_resume(
        &ctx,
        build.id.clone(),
        jd_id.clone(),
        &requirements,
        &dataset,
        &gap,
        None,
    )
    .await?;
    add_usage(&mut total, first.usage);
    eprintln!("{}", style::done("first draft tailored"));
    print_tailor_warnings(&first);

    // Inline "add what's missing" pivot. The model wanted skills the dataset
    // can't back (the "not a recorded skill" drops). Rather than just warn,
    // offer to record the ones the user genuinely has — the same evidence
    // interview as `skills add`, with the typed line polished into resume
    // wording — then re-tailor so they land in this build instead of the
    // next. Interactive only; a piped/CI run keeps the warnings and moves on.
    {
        if user.is_interactive() && !first.dropped_unrecorded.is_empty() {
            let added =
                offer_inline_skill_add(&mut dataset, &first.dropped_unrecorded, user, &ctx).await?;
            if added > 0 {
                dataset.metadata.updated_at = Utc::now();
                store::save(&dataset)?;
                let retailor = user
                    .confirm(
                        &format!("re-tailor to include the {added} new skill(s)?"),
                        true,
                    )
                    .await
                    .unwrap_or(false);
                if retailor {
                    let sp = Spinner::start("re-analyzing the gap");
                    gap = analyze_gap(&ctx, &requirements, &dataset).await?;
                    sp.finish(style::done("gap re-analyzed"));
                    builds::write_json(&build.dir, "gap_report.json", &gap)?;
                    first = tailor_resume(
                        &ctx,
                        build.id.clone(),
                        jd_id.clone(),
                        &requirements,
                        &dataset,
                        &gap,
                        None,
                    )
                    .await?;
                    add_usage(&mut total, first.usage);
                    eprintln!("{}", style::done("re-tailored with the new skill(s)"));
                    print_tailor_warnings(&first);
                } else {
                    eprintln!(
                        "{}",
                        style::dim(
                            "recorded - this build keeps the current draft; they'll apply next run"
                        )
                    );
                }
            }
        }
    }

    let mut best = evaluate(
        &ctx,
        &build.dir,
        0,
        first.resume,
        &requirements,
        &dataset,
        &gap,
    )
    .await?;
    add_usage(&mut total, best.review_usage);
    let starting_score = best.score;
    eprintln!("{}", iteration_line("iteration 0", &best));

    // Metric capture: the reviewer flags bullets that state an outcome
    // without a number, but the loop can't invent one (the digit-runs
    // guard reverts it). So ask the person — a leading question per
    // flagged bullet — and re-tailor with their real figures folded in.
    let metric_targets: Vec<MetricTarget> = best
        .report
        .objections
        .iter()
        .filter(|o| o.kind == ObjectionKind::NoMetric)
        .filter_map(|o| match &o.target {
            ObjectionTarget::Bullet(id) => Some(MetricTarget {
                bullet_id: id.clone(),
                hint: o.suggestion.clone().or_else(|| Some(o.message.clone())),
            }),
            _ => None,
        })
        .collect();
    if !metric_targets.is_empty() {
        if user.is_interactive() {
            let wants = user
                .confirm(
                    &format!(
                        "the reviewer flagged {} bullet(s) that would land harder with a real number. Answer a few quick questions?",
                        metric_targets.len()
                    ),
                    true,
                )
                .await
                .unwrap_or(false);
            if wants {
                let added =
                    metric::capture_metrics(&mut dataset, &metric_targets, user, &ctx).await?;
                if added > 0 {
                    dataset.metadata.updated_at = Utc::now();
                    store::save(&dataset)?;
                    eprintln!(
                        "{}",
                        style::dim(format!("added {added} metric(s); re-tailoring"))
                    );
                    let retailored = tailor_resume(
                        &ctx,
                        build.id.clone(),
                        jd_id.clone(),
                        &requirements,
                        &dataset,
                        &gap,
                        None,
                    )
                    .await?;
                    eprintln!("{}", style::done(format!("folded in {added} metric(s)")));
                    add_usage(&mut total, retailored.usage);
                    best = evaluate(
                        &ctx,
                        &build.dir,
                        0,
                        retailored.resume,
                        &requirements,
                        &dataset,
                        &gap,
                    )
                    .await?;
                    add_usage(&mut total, best.review_usage);
                    eprintln!("{}", iteration_line("iteration 0 (with metrics)", &best));
                }
            }
        }
    }

    // Strengthen copilot: the reviewer also flags bullets for weak wording
    // — vague verbs, unsupported or generic claims, missed JD emphasis. The
    // loop can't rephrase a line stronger than the truth allows (that's the
    // inflation never-fabricate forbids), so copilot the person: a leading
    // question per flagged bullet, and their restatement — their own words —
    // replaces it. Runs after metric capture so a freshly quantified bullet
    // is judged on its latest text, and re-tailors from the corrected
    // history. A piped/CI run skips this silently.
    let strengthen_targets: Vec<StrengthenTarget> = best
        .report
        .objections
        .iter()
        .filter(|o| strengthen::is_strengthenable(o.kind))
        .filter_map(|o| match &o.target {
            ObjectionTarget::Bullet(id) => Some(StrengthenTarget {
                bullet_id: id.clone(),
                kind: o.kind,
                concern: o.message.clone(),
            }),
            _ => None,
        })
        .collect();
    if !strengthen_targets.is_empty() {
        if user.is_interactive() {
            let wants = user
                .confirm(
                    &format!(
                        "the reviewer flagged {} bullet(s) as weakly worded. Restate them in your own words?",
                        strengthen_targets.len()
                    ),
                    true,
                )
                .await
                .unwrap_or(false);
            if wants {
                let changed = strengthen::strengthen_bullets(
                    &mut dataset,
                    &strengthen_targets,
                    user,
                    &ctx,
                    interview_limits,
                )
                .await?;
                if changed > 0 {
                    dataset.metadata.updated_at = Utc::now();
                    store::save(&dataset)?;
                    eprintln!(
                        "{}",
                        style::dim(format!("strengthened {changed} bullet(s); re-tailoring"))
                    );
                    let retailored = tailor_resume(
                        &ctx,
                        build.id.clone(),
                        jd_id.clone(),
                        &requirements,
                        &dataset,
                        &gap,
                        None,
                    )
                    .await?;
                    eprintln!(
                        "{}",
                        style::done(format!("strengthened {changed} bullet(s)"))
                    );
                    add_usage(&mut total, retailored.usage);
                    best = evaluate(
                        &ctx,
                        &build.dir,
                        0,
                        retailored.resume,
                        &requirements,
                        &dataset,
                        &gap,
                    )
                    .await?;
                    add_usage(&mut total, best.review_usage);
                    eprintln!("{}", iteration_line("iteration 0 (strengthened)", &best));
                }
            }
        }
    }

    // Triage the remaining objections one at a time: refine the wording
    // (routing eligible bullet objections through the strengthen suggestion
    // flow), accept one as intentional ("this 2013 line stays one sentence" —
    // remembered like a declined skill and filtered at evaluate time, score
    // untouched), or leave it for the revision loop. Interactive only; a
    // piped/CI run skips it.
    if !best.report.objections.is_empty() {
        if user.is_interactive() {
            let wants = user
                .confirm(
                    &format!(
                        "review the {} remaining objection(s)? (refine, accept, or leave each)",
                        best.report.objections.len()
                    ),
                    true,
                )
                .await
                .unwrap_or(false);
            if wants {
                let mut accepted = 0;
                let mut refined = 0;
                // Clone the list: refining mutates the dataset (not the report),
                // and the report is rebuilt by the re-tailor at the end.
                for objection in best.report.objections.clone() {
                    eprintln!("{}", objection_card(&objection));
                    let mut options = Vec::new();
                    if refine_eligible(&objection) {
                        options.push("Refine it".to_string());
                    }
                    options.push("Accept as intentional".to_string());
                    options.push("Leave it".to_string());

                    let choice = match user
                        .ask(Question::Select {
                            prompt: "what would you like to do?".into(),
                            options: options.clone(),
                        })
                        .await?
                    {
                        Answer::Choice(i) => options.get(i).map(String::as_str),
                        _ => Some("Leave it"),
                    };

                    match choice {
                        // A bullet refines through the strengthen flow; the
                        // summary through its own grounded-suggestion flow.
                        // Both write to the dataset and count toward `refined`,
                        // so the re-tailor below picks up the change.
                        Some("Refine it") => {
                            let changed = match &objection.target {
                                ObjectionTarget::Bullet(id) => {
                                    strengthen::strengthen_bullets(
                                        &mut dataset,
                                        &[StrengthenTarget {
                                            bullet_id: id.clone(),
                                            kind: objection.kind,
                                            concern: objection.message.clone(),
                                        }],
                                        user,
                                        &ctx,
                                        interview_limits,
                                    )
                                    .await?
                                }
                                ObjectionTarget::Summary => usize::from(
                                    summary::refine_summary(
                                        &mut dataset,
                                        &objection.message,
                                        user,
                                        &ctx,
                                        interview_limits.revises,
                                    )
                                    .await?,
                                ),
                                // refine_eligible guarantees Bullet|Summary.
                                _ => 0,
                            };
                            if changed > 0 {
                                dataset.metadata.updated_at = Utc::now();
                                store::save(&dataset)?;
                                refined += changed;
                            }
                        }
                        Some("Accept as intentional") => {
                            let dismissal = objection.dismissal();
                            if !dataset.metadata.dismissed_objections.contains(&dismissal) {
                                dataset.metadata.dismissed_objections.push(dismissal);
                                accepted += 1;
                            }
                        }
                        _ => {} // Leave it (or an unexpected answer): nothing
                    }
                }

                if accepted > 0 {
                    dataset.metadata.updated_at = Utc::now();
                    store::save(&dataset)?;
                    // Drop the accepted ones from this run's draft too, so the
                    // revision loop below doesn't act on them.
                    best.report
                        .objections
                        .retain(|o| !o.is_dismissed(&dataset.metadata.dismissed_objections));
                    eprintln!(
                        "{}",
                        style::done(format!(
                            "accepted {accepted} objection(s); they won't be flagged again"
                        ))
                    );
                }

                // A refine changed the history, so re-tailor and re-review once
                // (as the strengthen step does) before the revision loop runs.
                if refined > 0 {
                    eprintln!(
                        "{}",
                        style::dim(format!("refined {refined} bullet(s); re-tailoring"))
                    );
                    let retailored = tailor_resume(
                        &ctx,
                        build.id.clone(),
                        jd_id.clone(),
                        &requirements,
                        &dataset,
                        &gap,
                        None,
                    )
                    .await?;
                    add_usage(&mut total, retailored.usage);
                    best = evaluate(
                        &ctx,
                        &build.dir,
                        0,
                        retailored.resume,
                        &requirements,
                        &dataset,
                        &gap,
                    )
                    .await?;
                    add_usage(&mut total, best.review_usage);
                    eprintln!("{}", iteration_line("iteration 0 (refined)", &best));
                }
            }
        }
    }

    for iteration in 1..=max_revisions {
        // Stop early when the draft is good enough or has nothing major
        // left to fix — no point spending tokens to polish a strong draft.
        if best.score >= acceptable_score || !best.report.has_blocking_or_major() {
            break;
        }
        let objections: Vec<String> = best.report.actionable().map(format_objection).collect();
        let count = objections.len();
        eprintln!(
            "{}",
            style::dim(format!(
                "revising (pass {iteration}): addressing {count} objection(s)"
            ))
        );
        let revised = tailor_resume(
            &ctx,
            build.id.clone(),
            jd_id.clone(),
            &requirements,
            &dataset,
            &gap,
            Some(RevisionContext { objections }),
        )
        .await?;
        eprintln!("{}", style::dim(format!("revision {iteration} drafted")));
        add_usage(&mut total, revised.usage);

        let candidate = evaluate(
            &ctx,
            &build.dir,
            iteration,
            revised.resume,
            &requirements,
            &dataset,
            &gap,
        )
        .await?;
        add_usage(&mut total, candidate.review_usage);
        eprintln!(
            "{}",
            iteration_line(&format!("iteration {iteration}"), &candidate)
        );

        // Score-must-improve: a revision that scored no better is
        // discarded, and the loop stops — the best draft is already in
        // hand.
        if candidate.score > best.score {
            best = candidate;
        } else {
            eprintln!(
                "{}",
                style::dim("that revision didn't improve the draft; keeping the best one")
            );
            break;
        }
    }

    // Voice pass: rewrite the AI-sounding lines of the best draft toward
    // the user's own writing samples, then re-score. Voice only changes
    // phrasing — facts are held by the same digit-runs guard tailoring
    // uses — but a non-numeric inflation would slip that guard, so the
    // voiced draft is run back through `evaluate`: the reviewer vets it
    // and it's adopted only if it doesn't score worse. That keeps voice
    // honest (no rewrite ships unreviewed) and from ever unseating a
    // stronger draft. Skipped without samples to anchor to.
    if !dataset.voice_samples.is_empty() {
        let samples: Vec<String> = dataset
            .voice_samples
            .iter()
            .map(|s| s.text.clone())
            .collect();
        // Streams via the reporter when there are lines to rewrite; a draft
        // with nothing flagged returns immediately and shows nothing.
        let voiced_result = voice::rewrite_to_voice(&ctx, &best.resume, &samples).await;
        match voiced_result {
            Ok((voiced, stats)) => {
                add_usage(&mut total, stats.usage);
                if stats.rewritten > 0 {
                    let reverted = if stats.reverted > 0 {
                        format!(" ({} reverted for drifting from the facts)", stats.reverted)
                    } else {
                        String::new()
                    };
                    let voiced_eval = evaluate(
                        &ctx,
                        &build.dir,
                        max_revisions + 1,
                        voiced,
                        &requirements,
                        &dataset,
                        &gap,
                    )
                    .await?;
                    add_usage(&mut total, voiced_eval.review_usage);
                    if voiced_eval.score >= best.score {
                        eprintln!(
                            "{}",
                            style::done(format!(
                                "voice: rewrote {} line(s) toward your samples{reverted}",
                                stats.rewritten
                            ))
                        );
                        best = voiced_eval;
                    } else {
                        eprintln!(
                            "{}",
                            style::dim(format!(
                                "voice: the rewrite scored lower ({:.2} < {:.2}); kept the un-voiced draft",
                                voiced_eval.score, best.score
                            ))
                        );
                    }
                }
            }
            // Voice is a finish, not a gate: if it fails, ship the draft.
            Err(e) => eprintln!("{}", style::dim(format!("voice: skipped ({e})"))),
        }
    }

    // Finalize from the best draft seen. Strip AI-tell em/en dashes from the
    // canonical prose first, so the stored JSON, the ATS projection, the human
    // adapter's input, and the cover letter's input all start clean. Punctuation
    // only — no claim changes, so it runs after scoring without re-review.
    crate::tailor::scrub_resume_text(&mut best.resume);
    builds::write_json(&build.dir, "canonical.json", &best.resume)?;
    builds::write_json(&build.dir, "adversarial_report.json", &best.report)?;
    builds::write_json(&build.dir, "ats_report.json", &best.ats_report)?;
    // Render the requested variant(s). The ATS payload is a deterministic
    // projection of the canonical draft; the human variant is reworded by the
    // adapter and then checked against the canonical, so the two PDFs can
    // differ in presentation but never in claims. `render` writes each
    // variant's payload JSON (ats_payload.json / human_payload.json) too.
    let mut pdfs: Vec<(Variant, PathBuf)> = Vec::new();
    let mut readability_reports: Vec<(Variant, ReadabilityReport)> = Vec::new();
    // The template id that lands in `meta.json` — the ATS one when it renders
    // (the "upload this" PDF), else the human one.
    let mut meta_template = String::new();
    if variants.contains(&Variant::Ats) {
        let chosen = resolve_ats_template(&config)?;
        let sp = Spinner::start("rendering the ATS resume");
        let mut ats = variant::ats_payload(&best.resume);
        ats.template = TemplateId(chosen.id.clone());
        let pdf = render::render(&build.dir, &ats, &chosen.template)?;
        sp.finish(style::done("ATS resume rendered"));
        check_readability(&pdf, &ats, Variant::Ats, &mut readability_reports);
        pdfs.push((Variant::Ats, pdf));
        meta_template = chosen.id;
    }
    if variants.contains(&Variant::Human) {
        // Resolve the template before the LLM call so a bad name/path fails
        // fast rather than after the (expensive) reshaping.
        let chosen = resolve_human_template(&human_template, &config)?;
        let sp = Spinner::start("reshaping for a human reader");
        let run = VariantAdapterAgent
            .run(
                &ctx,
                VariantInput {
                    draft: best.resume.clone(),
                    variant: Variant::Human,
                    // A user-confirmed summary stays verbatim in the human PDF too.
                    summary_locked: dataset.summary_confirmed,
                },
            )
            .await?;
        add_usage(&mut total, run.usage);
        let human = run.output;
        sp.finish(style::done("reshaped for a human reader"));
        // Re-review the reword and revert any overclaim to the canonical text
        // — the non-numeric backstop, matching the voice pass (the digit and
        // skill/role guards already caught fabricated numbers and entities).
        let (human, review_usage) =
            variant::vet_human(&ctx, &best.resume, human, &requirements, &dataset).await?;
        add_usage(&mut total, review_usage);
        // The guarantee behind the LLM rewording: a variant may differ in
        // presentation, never in claims. A divergence fails the build.
        variant::check_claims(&best.resume, &human)?;
        let mut human = human;
        // The adapter reworded the canonical prose, so it can have re-introduced
        // em-dashes the canonical scrub already removed; clean the human payload
        // before it renders. Claim-divergence was already vetted above.
        variant::scrub_variant_text(&mut human);
        human.template = TemplateId(chosen.id.clone());
        let sp = Spinner::start("rendering the human resume");
        let pdf = render::render(&build.dir, &human, &chosen.template)?;
        sp.finish(style::done("human resume rendered"));
        check_readability(&pdf, &human, Variant::Human, &mut readability_reports);
        pdfs.push((Variant::Human, pdf));
        if meta_template.is_empty() {
            meta_template = chosen.id;
        }
    }
    if !readability_reports.is_empty() {
        let by_variant: std::collections::BTreeMap<String, &ReadabilityReport> =
            readability_reports
                .iter()
                .map(|(v, r)| (format!("{v:?}").to_lowercase(), r))
                .collect();
        builds::write_json(&build.dir, "readability_report.json", &by_variant)?;
    }
    // Optional cover letter (`--cover`): draft it from the canonical draft
    // and render it next to the resume. It runs the same never-fabricate
    // guards the resume does; warnings are surfaced, not fatal, and its
    // tokens are folded into the build total before meta.json is written.
    let mut cover_pdf: Option<PathBuf> = None;
    if cover {
        let samples: Vec<String> = dataset
            .voice_samples
            .iter()
            .map(|sample| sample.text.clone())
            .collect();
        let sp = Spinner::start("drafting a cover letter");
        let (letter, cover_warnings, cover_usage) =
            crate::cover::write_cover_letter(&ctx, &best.resume, &requirements, &samples).await?;
        add_usage(&mut total, cover_usage);
        let pdf = render::render_cover(&build.dir, &letter, &render::Template::cover())?;
        sp.finish(style::done("cover letter drafted"));
        for warning in &cover_warnings {
            eprintln!("{} {warning}", style::yellow("cover:"));
        }
        cover_pdf = Some(pdf);
    }

    builds::write_json(
        &build.dir,
        "meta.json",
        &BuildMeta {
            created_at: Utc::now(),
            model: model.to_string(),
            template: meta_template,
            tailor_usage: total,
            subscription,
        },
    )?;

    // The result summary: the reviewer's verdict, coverage, then where it
    // landed. On stderr like the rest of the human output.
    if !best.report.persona_notes.is_empty() {
        eprintln!(
            "\n{}  {}",
            style::bold("reviewer verdict"),
            style::grade(
                best.report.overall_score,
                format!("{:.2}", best.report.overall_score)
            )
        );
        eprintln!("  {}", best.report.persona_notes);
    }
    print_coverage(&best.ats_report);

    let score = if best.score > starting_score {
        style::grade(
            best.score,
            format!("score {:.2} (up from {starting_score:.2})", best.score),
        )
    } else {
        style::grade(best.score, format!("score {:.2}", best.score))
    };
    eprintln!(
        "\n{}",
        style::done(style::bold(format!("build {} saved", build.id.0)))
    );
    for (v, pdf) in &pdfs {
        eprintln!(
            "  {}  {}",
            style::dim(pdf.display()),
            style::dim(format!("· {}", v.purpose()))
        );
    }
    if let Some(pdf) = &cover_pdf {
        eprintln!(
            "  {}  {}",
            style::dim(pdf.display()),
            style::dim("· cover letter")
        );
    }
    // Readability problems worth the eye, if any. The "pdfium unavailable"
    // note is an environment limitation, not a resume problem — it stays in
    // the JSON report but is filtered from the human output.
    for (v, report) in &readability_reports {
        let problems: Vec<&str> = report
            .issues
            .iter()
            .filter(|i| !i.contains("pdfium"))
            .map(String::as_str)
            .collect();
        if !problems.is_empty() {
            eprintln!(
                "  {} {}",
                style::yellow(format!(
                    "readability ({}):",
                    format!("{v:?}").to_lowercase()
                )),
                style::dim(problems.join("; "))
            );
        }
    }
    eprintln!("  {score}");

    // Cost summary. On a Claude plan the run is covered by the flat fee, so
    // show tokens and say so rather than a misleading dollar figure, and skip
    // the budget nudge (a dollar budget is meaningless on a plan).
    if subscription {
        eprintln!(
            "  {}",
            style::dim(format!(
                "{} in / {} out tokens  ·  covered by your Claude plan",
                total.input_tokens, total.output_tokens
            ))
        );
    } else {
        let cost = crate::pricing::cost_usd(model, &total, &config.prices);
        let cost_note = match cost {
            Some(c) => format!(
                "~${c:.2}  ·  {} in / {} out tokens",
                total.input_tokens, total.output_tokens
            ),
            None => format!(
                "{} in / {} out tokens",
                total.input_tokens, total.output_tokens
            ),
        };
        eprintln!("  {}", style::dim(cost_note));
        if let (Some(c), Some(budget)) = (cost, config.limits.budget_usd)
            && c > budget
        {
            eprintln!(
                "  {}",
                style::yellow(format!(
                    "over your ${budget:.2} budget by ${:.2}",
                    c - budget
                ))
            );
        }
    }
    Ok(())
}

/// One draft's score and the artifacts that produced it. Each lands in
/// its own `iterations/<n>/` directory so the whole loop is inspectable.
struct Evaluation {
    resume: TailoredResume,
    report: AdversarialReport,
    ats_report: AtsReport,
    score: f32,
    review_usage: TokenUsage,
}

/// Review, render, and score one draft, writing its artifacts under
/// `iterations/<iteration>/`.
/// A resolved template plus the id to stamp into the payload and `meta.json`.
/// `pub(crate)` so `aarg render` can re-render a saved build with the same
/// template resolution this command uses.
pub(crate) struct ChosenTemplate {
    pub(crate) id: String,
    pub(crate) template: render::Template,
}

/// The ATS template to render with: the configured built-in (default
/// `classic`). ATS is built-in only, so this never reads a user file.
pub(crate) fn resolve_ats_template(config: &Config) -> Result<ChosenTemplate, CliError> {
    let name = config.templates.ats_name();
    Ok(ChosenTemplate {
        id: format!("ats/{name}"),
        template: templates::resolve(name, Variant::Ats)?,
    })
}

/// The human template to render with. An explicit `--template` wins — a file
/// path is used directly, anything else is treated as a template name — else
/// the configured default (a name, default `modern`).
pub(crate) fn resolve_human_template(
    arg: &Option<PathBuf>,
    config: &Config,
) -> Result<ChosenTemplate, CliError> {
    if let Some(value) = arg {
        if value.is_file() {
            let stem = value
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("custom");
            return Ok(ChosenTemplate {
                id: format!("human/{stem}"),
                template: render::Template::User(value.clone()),
            });
        }
        let name = value.to_string_lossy().into_owned();
        let template = templates::resolve(&name, Variant::Human)?;
        return Ok(ChosenTemplate {
            id: format!("human/{name}"),
            template,
        });
    }
    let name = config.templates.human_name();
    Ok(ChosenTemplate {
        id: format!("human/{name}"),
        template: templates::resolve(name, Variant::Human)?,
    })
}

async fn evaluate(
    ctx: &AgentContext<'_>,
    build_dir: &Path,
    iteration: usize,
    resume: TailoredResume,
    jd: &JobRequirements,
    dataset: &ResumeDataset,
    gap: &GapReport,
) -> Result<Evaluation, CliError> {
    let iter_dir = build_dir.join("iterations").join(iteration.to_string());
    std::fs::create_dir_all(&iter_dir).map_err(|source| BuildError::Io {
        path: iter_dir.clone(),
        source,
    })?;

    // The review streams (a live token/cost line via the reporter); the
    // render and scoring that follow are deterministic, so a plain spinner
    // covers them.
    let run = AdversarialReviewerAgent
        .run(
            ctx,
            ReviewInput {
                draft: resume.clone(),
                jd: jd.clone(),
                dataset: dataset.clone(),
            },
        )
        .await?;
    let report = run.output;

    let sp = Spinner::start(format!("rendering & scoring iteration {iteration}"));
    // Coverage is an ATS concern: render the deterministic ATS projection.
    let pdf = render::render(
        &iter_dir,
        &variant::ats_payload(&resume),
        &render::Template::ats(),
    )?;
    let page_text = ats::extract_pdf_text(&pdf)?;
    let ats_report = ats::keyword_coverage(jd, gap, dataset, &page_text);
    sp.finish(style::dim(format!("iteration {iteration} reviewed")));

    // Score from the *full* report — accepting an objection stops the
    // churn, it must not inflate the honest assessment.
    let score = combined_score(&report, ats_report.coverage);

    builds::write_json(&iter_dir, "draft.json", &resume)?;
    // The on-disk artifact is the reviewer's full, unedited record.
    builds::write_json(&iter_dir, "adversarial_report.json", &report)?;
    builds::write_json(&iter_dir, "ats_report.json", &ats_report)?;

    // What the loop, copilots, and display work from has the user's
    // accepted objections filtered out, so they're not re-litigated.
    let report = report.without_dismissed(&dataset.metadata.dismissed_objections);

    Ok(Evaluation {
        resume,
        report,
        ats_report,
        score,
        review_usage: run.usage,
    })
}

/// The combined metric the loop optimizes: the reviewer's judgment of
/// the content, weighted with deterministic keyword coverage. Both are
/// in 0.0..1.0, so the result is too.
fn combined_score(report: &AdversarialReport, coverage: f32) -> f32 {
    0.6 * report.overall_score + 0.4 * coverage
}

/// Run the deterministic readability checks on a rendered PDF, non-fatally:
/// a tooling hiccup reading the PDF must not fail a build that already
/// rendered and verified its claims. A report is collected; an error is a
/// dim note and nothing more.
fn check_readability(
    pdf: &Path,
    payload: &variant::VariantPayload,
    variant: Variant,
    out: &mut Vec<(Variant, ReadabilityReport)>,
) {
    match readability::check(pdf, payload) {
        Ok(report) => out.push((variant, report)),
        Err(e) => eprintln!(
            "{} {}",
            style::yellow("readability:"),
            style::dim(format!("skipped for {variant:?} ({e})"))
        ),
    }
}

fn add_usage(total: &mut TokenUsage, other: TokenUsage) {
    total.input_tokens += other.input_tokens;
    total.output_tokens += other.output_tokens;
}

/// Print a draft's guard warnings, one yellow line each.
fn print_tailor_warnings(outcome: &TailorOutcome) {
    for warning in &outcome.warnings {
        eprintln!("{} {warning}", style::yellow("warning:"));
    }
}

/// The inline "add what's missing" pivot. The first draft named skills the
/// dataset can't back; offer the user a checklist of exactly those and run
/// the same evidence interview as `skills add` on each one they tick — so a
/// real-but-unrecorded skill becomes usable rather than just a warning.
/// Returns how many were added (the caller saves and offers a re-tailor).
/// The category is unknown for a model-proposed skill, so a new one is
/// recorded as `Hard`, which the user can refine later via `dataset edit`.
async fn offer_inline_skill_add(
    dataset: &mut ResumeDataset,
    dropped: &[String],
    user: &dyn UserHandle,
    ctx: &AgentContext<'_>,
) -> Result<usize, CliError> {
    let wants = user
        .confirm(
            &format!(
                "the model wanted {} skill(s) you haven't recorded - add any you have?",
                dropped.len()
            ),
            true,
        )
        .await
        .unwrap_or(false);
    if !wants {
        return Ok(0);
    }

    let picks = match user
        .ask(Question::MultiSelect {
            prompt: "check the ones you genuinely have (space toggles, enter confirms)".into(),
            options: dropped.to_vec(),
        })
        .await?
    {
        Answer::Choices(indexes) => indexes,
        _ => Vec::new(),
    };

    let mut added = 0;
    for index in picks {
        let Some(name) = dropped.get(index) else {
            continue;
        };
        let outcome = add_one_skill(dataset, name, SkillCategory::Hard, user, Some(ctx)).await?;
        added += outcome.verified;
    }
    Ok(added)
}

/// A one-line iteration summary: the label, the score color-graded by quality,
/// and the objection count and coverage as dim context.
fn iteration_line(label: &str, eval: &Evaluation) -> String {
    format!(
        "{label}  {}  {}",
        style::grade(eval.score, format!("score {:.2}", eval.score)),
        style::dim(format!(
            "{} objection(s), {:.0}% coverage",
            eval.report.objections.len(),
            eval.ats_report.coverage * 100.0
        ))
    )
}

/// One objection as a single revision-prompt line. Shared with `attack`.
pub(crate) fn format_objection(objection: &Objection) -> String {
    let mut line = format!(
        "{} ({}, {}): {}",
        target_label(&objection.target),
        kind_str(objection.kind),
        severity_str(objection.severity),
        objection.message
    );
    if let Some(suggestion) = &objection.suggestion {
        line.push_str(&format!(" · try: {suggestion}"));
    }
    line
}

/// The short label for an objection's target ("bullet-3", "summary", ...).
fn target_label(target: &ObjectionTarget) -> String {
    match target {
        ObjectionTarget::Bullet(id) => id.0.clone(),
        ObjectionTarget::Summary => "summary".to_string(),
        ObjectionTarget::SkillsSection => "skills".to_string(),
        ObjectionTarget::Layout => "layout".to_string(),
        ObjectionTarget::Overall => "overall".to_string(),
    }
}

/// Color an objection's severity so urgency reads at a glance: a blocking flaw
/// red, a major one yellow, a minor one dim. The word is always shown, so the
/// cue survives `NO_COLOR`.
fn severity_color(severity: Severity, text: impl std::fmt::Display) -> String {
    match severity {
        Severity::Blocking => style::red(text),
        Severity::Major => style::yellow(text),
        Severity::Minor => style::dim(text),
    }
}

/// A readable card for one objection in the triage: the target in bold, the
/// kind and severity-colored severity beside it, the reviewer's message below,
/// and its suggestion (when present) as a dim `try:` line. The multi-line card
/// replaces the dense one-line `format_objection` on the review screen.
fn objection_card(objection: &Objection) -> String {
    let mut card = format!(
        "\n{}  {} {} {}",
        style::bold(target_label(&objection.target)),
        kind_str(objection.kind),
        style::dim("·"),
        severity_color(objection.severity, severity_str(objection.severity)),
    );
    card.push_str(&format!("\n  {}", objection.message));
    if let Some(suggestion) = &objection.suggestion {
        card.push_str(&format!("\n  {}", style::dim(format!("try: {suggestion}"))));
    }
    card
}

/// Whether an objection can be refined through a grounded-suggestion flow: it
/// must target a specific bullet or the summary — the free-prose fields with a
/// refine path. Any flagged bullet or summary qualifies, whatever the flaw
/// kind: the evidence interview can act on a weak verb, a generic line, a
/// missing metric, or a catch-all alike, all by asking the user to restate the
/// truth. (This used to also require a "strengthenable" kind, which left the
/// user only "accept" or "leave" on a bullet the reviewer had even printed a
/// suggestion for — a fix with no way to act on it.) Skills, layout, and
/// "overall" have no single line to refine, so they still offer accept/leave.
fn refine_eligible(objection: &Objection) -> bool {
    matches!(
        objection.target,
        ObjectionTarget::Bullet(_) | ObjectionTarget::Summary
    )
}

fn kind_str(kind: ObjectionKind) -> &'static str {
    match kind {
        ObjectionKind::NoMetric => "no metric",
        ObjectionKind::VagueVerb => "vague verb",
        ObjectionKind::UnsupportedClaim => "unsupported claim",
        ObjectionKind::GenericPhrasing => "generic phrasing",
        ObjectionKind::JdMismatch => "jd mismatch",
        ObjectionKind::LayoutDense => "dense layout",
        ObjectionKind::Other => "issue",
    }
}

fn severity_str(severity: Severity) -> &'static str {
    match severity {
        Severity::Blocking => "blocking",
        Severity::Major => "major",
        Severity::Minor => "minor",
    }
}

fn print_coverage(report: &AtsReport) {
    let required_total = report
        .keyword_hits
        .iter()
        .map(|h| h.kind)
        .chain(report.keyword_misses.iter().map(|m| m.kind))
        .filter(|k| *k == KeywordKind::RequiredSkill)
        .count();
    let required_hits = report
        .keyword_hits
        .iter()
        .filter(|h| h.kind == KeywordKind::RequiredSkill)
        .count();

    let pct = format!("{:.0}%", report.coverage * 100.0);
    let pct = if report.coverage >= 1.0 {
        style::green(pct)
    } else {
        style::yellow(pct)
    };
    eprintln!(
        "\n{} {}",
        style::bold("keyword coverage"),
        style::dim(format!("{required_hits}/{required_total} required ({pct})"))
    );

    // Two honest groups: keywords you *have* but that didn't reach the page
    // (a placement issue), and keywords with no backing evidence (never
    // inserted — the never-fabricate line in plain sight).
    let backed: Vec<_> = report
        .keyword_misses
        .iter()
        .filter(|m| matches!(m.evidence, EvidenceStatus::Backed { .. }))
        .collect();
    let unbacked: Vec<_> = report
        .keyword_misses
        .iter()
        .filter(|m| matches!(m.evidence, EvidenceStatus::Unbacked))
        .collect();

    if !backed.is_empty() {
        eprintln!("  {}", style::yellow("missing, but you have the evidence:"));
        for miss in &backed {
            if let EvidenceStatus::Backed { dataset_skill } = &miss.evidence {
                eprintln!(
                    "    {} {}",
                    miss.phrase,
                    style::dim(format!(
                        "({}) · recorded as {:?}, didn't reach the page",
                        kind_label(miss.kind),
                        dataset_skill
                    ))
                );
            }
        }
    }
    if !unbacked.is_empty() {
        eprintln!("  {}", style::dim("missing, no evidence (never inserted):"));
        for miss in &unbacked {
            eprintln!(
                "    {} {}",
                style::dim(miss.phrase.clone()),
                style::dim(format!("({})", kind_label(miss.kind)))
            );
        }
    }
}

fn kind_label(kind: KeywordKind) -> &'static str {
    match kind {
        KeywordKind::RequiredSkill => "required skill",
        KeywordKind::PreferredSkill => "preferred skill",
        KeywordKind::AtsPhrase => "ats phrase",
    }
}

/// "amplo" + "Senior Engineering Manager" -> "amplo-senior-engineering-manager"
fn slug(company: &str, title: &str) -> String {
    let mut out = String::new();
    for c in format!("{company} {title}").chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::dataset::types::BulletId;
    use crate::review::ObjectionScope;

    #[test]
    fn slugs_are_lowercase_hyphenated_and_trimmed() {
        assert_eq!(
            slug("amplo", "Senior Engineering Manager"),
            "amplo-senior-engineering-manager"
        );
        assert_eq!(
            slug("Acme, Inc.", "Staff Engineer (L6)!"),
            "acme-inc-staff-engineer-l6"
        );
        assert_eq!(slug("", ""), "");
    }

    #[test]
    fn objections_format_as_actionable_revision_lines() {
        let objection = Objection {
            target: ObjectionTarget::Bullet(BulletId("bullet-3".into())),
            severity: Severity::Major,
            kind: ObjectionKind::VagueVerb,
            scope: ObjectionScope::Canonical,
            message: "\"Helped\" hides what you did".into(),
            suggestion: Some("lead with the action".into()),
        };
        assert_eq!(
            format_objection(&objection),
            "bullet-3 (vague verb, major): \"Helped\" hides what you did · try: lead with the action"
        );
    }

    fn objection(
        target: ObjectionTarget,
        kind: ObjectionKind,
        severity: Severity,
        suggestion: Option<&str>,
    ) -> Objection {
        Objection {
            target,
            severity,
            kind,
            scope: ObjectionScope::Canonical,
            message: "the reviewer's note".into(),
            suggestion: suggestion.map(String::from),
        }
    }

    #[test]
    fn an_objection_card_shows_target_kind_severity_message_and_suggestion() {
        let card = objection_card(&objection(
            ObjectionTarget::Bullet(BulletId("bullet-3".into())),
            ObjectionKind::VagueVerb,
            Severity::Major,
            Some("lead with the action"),
        ));
        // Color is suppressed off a TTY, so the words stand alone.
        assert!(card.contains("bullet-3"));
        assert!(card.contains("vague verb"));
        assert!(card.contains("major"));
        assert!(card.contains("the reviewer's note"));
        assert!(card.contains("try: lead with the action"));
    }

    #[test]
    fn an_objection_card_omits_the_try_line_without_a_suggestion() {
        let card = objection_card(&objection(
            ObjectionTarget::Summary,
            ObjectionKind::GenericPhrasing,
            Severity::Minor,
            None,
        ));
        assert!(card.contains("summary"));
        assert!(!card.contains("try:"));
    }

    #[test]
    fn refine_is_eligible_for_any_bullet_or_the_summary() {
        // A wording objection on a specific bullet: eligible (bullet flow).
        assert!(refine_eligible(&objection(
            ObjectionTarget::Bullet(BulletId("b1".into())),
            ObjectionKind::VagueVerb,
            Severity::Major,
            None,
        )));
        // The summary is also free prose with a refine path: eligible.
        assert!(refine_eligible(&objection(
            ObjectionTarget::Summary,
            ObjectionKind::GenericPhrasing,
            Severity::Minor,
            None,
        )));
        // A bullet flagged for a missing metric is now eligible too: the
        // interview can act on it, so a printed suggestion is never a dead end.
        assert!(refine_eligible(&objection(
            ObjectionTarget::Bullet(BulletId("b2".into())),
            ObjectionKind::NoMetric,
            Severity::Major,
            None,
        )));
        // Targets with no single free-prose line to refine: skills, layout,
        // and "overall" still offer only accept/leave.
        assert!(!refine_eligible(&objection(
            ObjectionTarget::SkillsSection,
            ObjectionKind::GenericPhrasing,
            Severity::Minor,
            None,
        )));
        assert!(!refine_eligible(&objection(
            ObjectionTarget::Layout,
            ObjectionKind::LayoutDense,
            Severity::Minor,
            None,
        )));
        assert!(!refine_eligible(&objection(
            ObjectionTarget::Overall,
            ObjectionKind::VagueVerb,
            Severity::Minor,
            None,
        )));
    }

    #[test]
    fn the_combined_score_weights_review_and_coverage() {
        let report = AdversarialReport {
            objections: Vec::new(),
            overall_score: 1.0,
            persona_notes: String::new(),
        };
        // 0.6 * 1.0 + 0.4 * 0.5 = 0.8
        assert!((combined_score(&report, 0.5) - 0.8).abs() < f32::EPSILON);
    }
}
