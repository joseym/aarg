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
use crate::dataset::store;
use crate::dataset::types::ResumeDataset;
use crate::enrich;
use crate::gap::{GapReport, analyze_gap};
use crate::jd::JobRequirements;
use crate::llm::TokenUsage;
use crate::metric::{self, MetricTarget};
use crate::render;
use crate::review::{
    AdversarialReport, AdversarialReviewerAgent, Objection, ObjectionKind, ObjectionTarget,
    ReviewInput, Severity,
};
use crate::strengthen::{self, InterviewLimits, StrengthenTarget};
use crate::style::{self, Spinner};
use crate::tailor::{JdId, RevisionContext, TailoredResume, tailor_resume};
use crate::terminal::auto_user;
use crate::trace::Tracer;
use crate::user::{Answer, Question};
use crate::verify::{unbacked_keywords, verify_keywords};
use crate::voice;

pub async fn run(path: PathBuf) -> Result<(), CliError> {
    let mut dataset = store::load()?;
    let (client, config) = configured_client().await?;
    let tracer = Tracer::to_default_dir()?;
    let ctx = AgentContext {
        llm: &client,
        model: &config.anthropic,
        tracer: &tracer,
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

    let requirements = load_requirements(&path, &ctx).await?;

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
        let user = auto_user();
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
                let outcome =
                    verify_keywords(&mut dataset, &candidates, user.as_ref(), Some(&ctx)).await?;
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
        let user = auto_user();
        if user.is_interactive() {
            let names: Vec<String> = thin
                .iter()
                .filter_map(|id| dataset.roles.iter().find(|r| &r.id == id))
                .map(|r| r.company.clone())
                .collect();
            let wants = user
                .confirm(
                    &format!(
                        "{} role(s) are thin on detail ({}) — flesh them out with a few questions?",
                        thin.len(),
                        names.join(", ")
                    ),
                    true,
                )
                .await
                .unwrap_or(false);
            if wants {
                let outcome =
                    enrich::enrich_roles(&mut dataset, &thin, user.as_ref(), &ctx).await?;
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

    let sp = Spinner::start("tailoring the first draft");
    let first = tailor_resume(
        &ctx,
        build.id.clone(),
        jd_id.clone(),
        &requirements,
        &dataset,
        &gap,
        None,
    )
    .await?;
    sp.finish(style::done("first draft tailored"));
    for warning in &first.warnings {
        eprintln!("{} {warning}", style::yellow("warning:"));
    }

    let mut total = TokenUsage::default();
    add_usage(&mut total, first.usage);

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
        let user = auto_user();
        if user.is_interactive() {
            let wants = user
                .confirm(
                    &format!(
                        "the reviewer flagged {} bullet(s) that would land harder with a real number — answer a few quick questions?",
                        metric_targets.len()
                    ),
                    true,
                )
                .await
                .unwrap_or(false);
            if wants {
                let added =
                    metric::capture_metrics(&mut dataset, &metric_targets, user.as_ref(), &ctx)
                        .await?;
                if added > 0 {
                    dataset.metadata.updated_at = Utc::now();
                    store::save(&dataset)?;
                    let sp =
                        Spinner::start(format!("added {added} metric(s); re-tailoring with them"));
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
                    sp.finish(style::done(format!("folded in {added} metric(s)")));
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
        let user = auto_user();
        if user.is_interactive() {
            let wants = user
                .confirm(
                    &format!(
                        "the reviewer flagged {} bullet(s) as weakly worded — restate them in your own words?",
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
                    user.as_ref(),
                    &ctx,
                    interview_limits,
                )
                .await?;
                if changed > 0 {
                    dataset.metadata.updated_at = Utc::now();
                    store::save(&dataset)?;
                    let sp = Spinner::start(format!(
                        "strengthened {changed} bullet(s); re-tailoring with them"
                    ));
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
                    sp.finish(style::done(format!("strengthened {changed} bullet(s)")));
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

    // Objection dismissal: a remaining objection the user judges
    // intentional ("this 2013 line stays one sentence") can be accepted, so
    // it's neither auto-revised this run nor flagged on future ones —
    // remembered like a declined skill and filtered at evaluate time. The
    // score is untouched: accepting a weakness stops the churn, it doesn't
    // pretend it's gone. Interactive only; a piped/CI run skips it.
    if !best.report.objections.is_empty() {
        let user = auto_user();
        if user.is_interactive() {
            let wants = user
                .confirm(
                    &format!(
                        "accept any of the {} remaining objection(s) as intentional, so they stop being flagged?",
                        best.report.objections.len()
                    ),
                    false,
                )
                .await
                .unwrap_or(false);
            if wants {
                let options: Vec<String> = best
                    .report
                    .objections
                    .iter()
                    .map(format_objection)
                    .collect();
                if let Answer::Choices(picks) = user
                    .ask(Question::MultiSelect {
                        prompt: "select objections to accept (they won't be raised again)".into(),
                        options,
                    })
                    .await?
                {
                    let mut added = 0;
                    for pick in &picks {
                        if let Some(objection) = best.report.objections.get(*pick) {
                            let dismissal = objection.dismissal();
                            if !dataset.metadata.dismissed_objections.contains(&dismissal) {
                                dataset.metadata.dismissed_objections.push(dismissal);
                                added += 1;
                            }
                        }
                    }
                    if added > 0 {
                        dataset.metadata.updated_at = Utc::now();
                        store::save(&dataset)?;
                        // Drop them from this run's draft too, so the
                        // revision loop below doesn't act on them.
                        best.report
                            .objections
                            .retain(|o| !o.is_dismissed(&dataset.metadata.dismissed_objections));
                        eprintln!(
                            "{}",
                            style::done(format!(
                                "accepted {added} objection(s); they won't be flagged again"
                            ))
                        );
                    }
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
        let sp = Spinner::start(format!(
            "revising (pass {iteration}): addressing {} objection(s)",
            objections.len()
        ));
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
        sp.finish(style::dim(format!("revision {iteration} drafted")));
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
        let sp = Spinner::start("voicing toward your writing samples");
        let voiced_result = voice::rewrite_to_voice(&ctx, &best.resume, &samples).await;
        sp.clear();
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

    // Finalize from the best draft seen.
    builds::write_json(&build.dir, "canonical.json", &best.resume)?;
    builds::write_json(&build.dir, "adversarial_report.json", &best.report)?;
    builds::write_json(&build.dir, "ats_report.json", &best.ats_report)?;
    let sp = Spinner::start("rendering the best draft");
    let pdf = render::render_ats(&build.dir, &best.resume)?;
    sp.finish(style::done("rendered"));
    builds::write_json(
        &build.dir,
        "meta.json",
        &BuildMeta {
            created_at: Utc::now(),
            model: model.to_string(),
            template: "ats/classic".into(),
            tailor_usage: total,
        },
    )?;

    // The result summary: the reviewer's verdict, coverage, then where it
    // landed. On stderr like the rest of the human output.
    if !best.report.persona_notes.is_empty() {
        eprintln!(
            "\n{}",
            style::bold(format!(
                "reviewer verdict ({:.2})",
                best.report.overall_score
            ))
        );
        eprintln!("  {}", best.report.persona_notes);
    }
    print_coverage(&best.ats_report);

    let score = if best.score > starting_score {
        style::cyan(format!(
            "score {:.2} (up from {starting_score:.2})",
            best.score
        ))
    } else {
        style::cyan(format!("score {:.2}", best.score))
    };
    eprintln!(
        "\n{}",
        style::done(style::bold(format!("build {} saved", build.id.0)))
    );
    eprintln!("  {}", style::dim(pdf.display()));
    eprintln!("  {score}");

    // Cost estimate (and a budget nudge), priced at the tailoring model.
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

    let sp = Spinner::start(format!("reviewing & scoring iteration {iteration}"));
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

    let pdf = render::render_ats(&iter_dir, &resume)?;
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

fn add_usage(total: &mut TokenUsage, other: TokenUsage) {
    total.input_tokens += other.input_tokens;
    total.output_tokens += other.output_tokens;
}

/// A one-line iteration summary: the label, the score in cyan, and the
/// objection count and coverage as dim context.
fn iteration_line(label: &str, eval: &Evaluation) -> String {
    format!(
        "{label}  {}  {}",
        style::cyan(format!("score {:.2}", eval.score)),
        style::dim(format!(
            "{} objection(s), {:.0}% coverage",
            eval.report.objections.len(),
            eval.ats_report.coverage * 100.0
        ))
    )
}

/// One objection as a single revision-prompt line. Shared with `attack`.
pub(crate) fn format_objection(objection: &Objection) -> String {
    let target = match &objection.target {
        ObjectionTarget::Bullet(id) => id.0.clone(),
        ObjectionTarget::Summary => "summary".to_string(),
        ObjectionTarget::SkillsSection => "skills".to_string(),
        ObjectionTarget::Layout => "layout".to_string(),
        ObjectionTarget::Overall => "overall".to_string(),
    };
    let mut line = format!(
        "{target} ({}, {}): {}",
        kind_str(objection.kind),
        severity_str(objection.severity),
        objection.message
    );
    if let Some(suggestion) = &objection.suggestion {
        line.push_str(&format!(" — try: {suggestion}"));
    }
    line
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
                        "({}) — recorded as {:?}, didn't reach the page",
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
            "bullet-3 (vague verb, major): \"Helped\" hides what you did — try: lead with the action"
        );
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
