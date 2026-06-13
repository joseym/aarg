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

use crate::agent::{Agent, AgentContext};
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
use crate::tailor::{JdId, RevisionContext, TailoredResume, tailor_resume};
use crate::terminal::auto_user;
use crate::trace::Tracer;
use crate::verify::{unbacked_keywords, verify_keywords};
use crate::voice;

/// How many revision passes the loop may take past the first draft.
const MAX_REVISIONS: usize = 2;

/// A draft scoring at least this well is done — no revision attempted.
const ACCEPTABLE_SCORE: f32 = 0.85;

pub async fn run(path: PathBuf) -> Result<(), CliError> {
    let mut dataset = store::load()?;
    let (client, config) = configured_client().await?;
    let tracer = Tracer::to_default_dir()?;
    let ctx = AgentContext {
        llm: &client,
        model: &config.anthropic.model,
        tracer: &tracer,
    };
    let model = ctx.model;

    let requirements = load_requirements(&path, &ctx).await?;

    eprintln!("analyzing the gap...");
    let mut gap = analyze_gap(&ctx, &requirements, &dataset).await?;

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
                        eprintln!(
                            "recorded {} new skill(s); re-analyzing the gap...",
                            outcome.verified
                        );
                        gap = analyze_gap(&ctx, &requirements, &dataset).await?;
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
                        "added {} bullet(s) across {} role(s); tailoring with the fuller history...",
                        outcome.bullets_added, outcome.roles_touched
                    );
                }
            }
        }
    }

    let build = builds::create_next()?;
    let jd_id = JdId(slug(&requirements.company, &requirements.title));
    builds::write_json(&build.dir, "jd.json", &requirements)?;
    builds::write_json(&build.dir, "gap_report.json", &gap)?;

    eprintln!(
        "build {}: tailoring for {} @ {} with {model}...",
        build.id.0, requirements.title, requirements.company
    );
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
    for warning in &first.warnings {
        eprintln!("warning: {warning}");
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
    eprintln!(
        "iteration 0: score {:.2} ({} objections, {:.0}% coverage)",
        best.score,
        best.report.objections.len(),
        best.ats_report.coverage * 100.0
    );

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
                    eprintln!("added {added} metric(s); re-tailoring with them...");
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
                    eprintln!("iteration 0 (with metrics): score {:.2}", best.score);
                }
            }
        }
    }

    for iteration in 1..=MAX_REVISIONS {
        // Stop early when the draft is good enough or has nothing major
        // left to fix — no point spending tokens to polish a strong draft.
        if best.score >= ACCEPTABLE_SCORE || !best.report.has_blocking_or_major() {
            break;
        }
        let objections: Vec<String> = best.report.actionable().map(format_objection).collect();
        eprintln!(
            "revising (pass {iteration}): addressing {} objections...",
            objections.len()
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
        eprintln!("iteration {iteration}: score {:.2}", candidate.score);

        // Score-must-improve: a revision that scored no better is
        // discarded, and the loop stops — the best draft is already in
        // hand.
        if candidate.score > best.score {
            best = candidate;
        } else {
            eprintln!("that revision didn't improve the draft; keeping the best one");
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
        match voice::rewrite_to_voice(&ctx, &best.resume, &samples).await {
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
                        MAX_REVISIONS + 1,
                        voiced,
                        &requirements,
                        &dataset,
                        &gap,
                    )
                    .await?;
                    add_usage(&mut total, voiced_eval.review_usage);
                    if voiced_eval.score >= best.score {
                        eprintln!(
                            "voice: rewrote {} line(s) toward your samples{reverted}",
                            stats.rewritten
                        );
                        best = voiced_eval;
                    } else {
                        eprintln!(
                            "voice: the rewrite scored lower ({:.2} < {:.2}); kept the un-voiced draft",
                            voiced_eval.score, best.score
                        );
                    }
                }
            }
            // Voice is a finish, not a gate: if it fails, ship the draft.
            Err(e) => eprintln!("voice: skipped ({e})"),
        }
    }

    // Finalize from the best draft seen.
    builds::write_json(&build.dir, "canonical.json", &best.resume)?;
    builds::write_json(&build.dir, "adversarial_report.json", &best.report)?;
    builds::write_json(&build.dir, "ats_report.json", &best.ats_report)?;
    eprintln!("rendering the best draft...");
    let pdf = render::render_ats(&build.dir, &best.resume)?;
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

    if !best.report.persona_notes.is_empty() {
        println!("\nreviewer: {}", best.report.persona_notes);
    }
    if best.score > starting_score {
        println!(
            "score improved {starting_score:.2} -> {:.2} after revision",
            best.score
        );
    } else {
        println!("score {:.2}", best.score);
    }
    print_coverage(&best.ats_report);
    println!("\nsaved build {}:", build.id.0);
    println!("  {}", pdf.display());
    println!(
        "tokens: {} in, {} out",
        total.input_tokens, total.output_tokens
    );
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

    let score = combined_score(&report, ats_report.coverage);

    builds::write_json(&iter_dir, "draft.json", &resume)?;
    builds::write_json(&iter_dir, "adversarial_report.json", &report)?;
    builds::write_json(&iter_dir, "ats_report.json", &ats_report)?;

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

/// One objection as a single revision-prompt line.
fn format_objection(objection: &Objection) -> String {
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

    println!(
        "\nkeyword coverage: {required_hits}/{required_total} required ({:.0}%)",
        report.coverage * 100.0
    );
    for miss in &report.keyword_misses {
        match &miss.evidence {
            EvidenceStatus::Backed { dataset_skill } => println!(
                "  miss: {:?} ({}) - recorded as {:?}, but it didn't reach the page",
                miss.phrase,
                kind_label(miss.kind),
                dataset_skill
            ),
            EvidenceStatus::Unbacked => println!(
                "  miss: {:?} ({}) - no supporting evidence in the dataset; not inserted",
                miss.phrase,
                kind_label(miss.kind)
            ),
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
