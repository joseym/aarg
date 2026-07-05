//! `aarg jd parse <path|->` — parse a job description into structured
//! requirements. `aarg jd rate <jd>` — score how well your profile fits a
//! posting. `aarg jd rm` — forget remembered parsed JDs.
//!
//! Thin glue, like the other LLM commands: read the text (file or
//! stdin), call `crate::jd::parse_jd`, present the result. `--json`
//! prints the full `JobRequirements` for scripts; progress goes to
//! stderr so stdout stays clean either way. `rate` reuses gap analysis
//! and projects a tight, importance-weighted coverage score from it — no
//! extra model call, no claim it can't trace. `rm` prunes the reuse cache
//! (`crate::jdstore`), a convenience store the builds don't depend on.

use std::collections::HashSet;
use std::path::PathBuf;

use serde::Serialize;

use crate::agent::AgentContext;
use crate::commands::{CliError, configured_client, load_requirements};
use crate::dataset::types::SkillCategory;
use crate::gap::{GapReport, Weakness, analyze_gap};
use crate::jd::{Importance, JdSkill, JobRequirements, RemotePolicy, Seniority};
use crate::jdstore::StoredJd;
use crate::style::{self, Spinner};
use crate::terminal::auto_user;
use crate::user::{Answer, Question};

pub async fn parse(path: PathBuf, json: bool) -> Result<(), CliError> {
    let (client, config) = configured_client().await?;
    let tracer = super::default_tracer()?;
    let ctx = AgentContext {
        llm: &*client,
        model: config.active_resolver(),
        tracer: &tracer,
        sink: None,
    };

    let requirements = load_requirements(&path, &ctx).await?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&requirements).map_err(CliError::OutputJson)?
        );
        return Ok(());
    }

    // Human summary on stderr (the stream the color helpers detect on); the
    // `--json` form above is the machine output and stays on stdout.
    let title = or_unknown(&requirements.title);
    let company = or_unknown(&requirements.company);
    eprintln!("\n{}", style::bold(format!("{title} @ {company}")));

    let width = 8;
    eprintln!(
        "{}",
        style::kv(
            "level",
            format!(
                "{} · {} · {}",
                seniority_label(requirements.seniority),
                remote_label(requirements.remote),
                requirements
                    .location
                    .as_deref()
                    .unwrap_or("location unstated")
            ),
            width
        )
    );
    if !requirements.domain_keywords.is_empty() {
        eprintln!(
            "{}",
            style::kv("domain", requirements.domain_keywords.join(", "), width)
        );
    }

    print_skills("Required skills", &requirements.required_skills);
    print_skills("Preferred skills", &requirements.preferred_skills);

    if !requirements.responsibilities.is_empty() {
        eprintln!(
            "{}",
            style::section(format!(
                "Responsibilities ({})",
                requirements.responsibilities.len()
            ))
        );
        for r in &requirements.responsibilities {
            eprintln!("  {}", style::bullet(r));
        }
    }
    if !requirements.ats_phrases.is_empty() {
        eprintln!(
            "{}",
            style::section(format!("ATS phrases ({})", requirements.ats_phrases.len()))
        );
        for p in &requirements.ats_phrases {
            eprintln!("  {}", style::bullet(format!("\"{p}\"")));
        }
    }
    Ok(())
}

/// `aarg jd rate <jd>` — a tight fit score for your profile against one
/// posting. It runs the same gap analysis as `aarg gap`, then projects a
/// single importance-weighted coverage number from the result plus the few
/// gaps that pull it down hardest. Like `gap`/`tailor`, the JD argument is
/// optional — omit it to pick a remembered one. `--json` emits the full
/// breakdown for scripts; the human view stays short on purpose.
pub async fn rate(jd: Option<PathBuf>, json: bool) -> Result<(), CliError> {
    let dataset = crate::dataset::store::load()?;
    let (client, config) = configured_client().await?;
    let tracer = super::default_tracer()?;
    let ctx = AgentContext {
        llm: &*client,
        model: config.active_resolver(),
        tracer: &tracer,
        sink: None,
    };

    // Same JD resolution as `gap`: a passed argument is parsed; with none, the
    // picker offers remembered JDs (and has already explained itself if it
    // returns nothing — no past JDs, or a piped/CI run).
    let requirements = match &jd {
        Some(path) => load_requirements(path, &ctx).await?,
        None => match super::prompt_for_jd(&ctx).await? {
            Some(requirements) => requirements,
            None => return Ok(()),
        },
    };

    let sp = Spinner::start(format!(
        "rating against {} recorded skills",
        dataset.skills.skills.len()
    ));
    let report = analyze_gap(&ctx, &requirements, &dataset).await?;
    // The score header opens the output, so clear the spinner without a line.
    sp.clear();

    let fit = score(&report);
    let gaps = ranked_gaps(&report);

    if json {
        let out = RateReport {
            score: fit.ratio,
            matched: fit.matched,
            weak: fit.weak,
            unknown: fit.unknown,
            gaps: gaps.iter().map(RateGap::from_gap).collect(),
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&out).map_err(CliError::OutputJson)?
        );
        return Ok(());
    }

    print_rating(&requirements, &fit, &gaps);
    Ok(())
}

/// How many gaps to show before collapsing the rest into a "+N more" line —
/// the targeted few worth acting on, with `aarg gap` for the full list.
const GAPS_SHOWN: usize = 5;

/// Partial credit for a recorded-but-thin (weak) skill: it counts for
/// something — there's real exposure — but not as much as a backed match.
/// A presentation heuristic, like the grade thresholds; nothing downstream
/// reads it.
const WEAK_CREDIT: f32 = 0.5;

/// The fit score plus the bucket counts behind it.
struct FitScore {
    /// Weighted 0.0–1.0 coverage; `None` when the posting lists no skills to
    /// rate against (an empty denominator, not a zero score).
    ratio: Option<f32>,
    matched: usize,
    weak: usize,
    unknown: usize,
}

/// Importance weighting: a make-or-break requirement counts for more than a
/// nice-to-have, so matching (or missing) one moves the score more.
fn importance_weight(i: Importance) -> f32 {
    match i {
        Importance::Critical => 3.0,
        Importance::Required => 2.0,
        Importance::Preferred => 1.0,
    }
}

/// Project a fit score from the gap report: each JD skill contributes its
/// importance weight to the denominator, and earns full credit when matched,
/// [`WEAK_CREDIT`] when weak, and nothing when unknown. Pure, so the rule is
/// unit-testable without a terminal or a model.
fn score(report: &GapReport) -> FitScore {
    let mut earned = 0.0f32;
    let mut total = 0.0f32;
    for m in &report.matched {
        let w = importance_weight(m.jd_skill.importance);
        earned += w;
        total += w;
    }
    for weak in &report.weak {
        let w = importance_weight(weak.matched.jd_skill.importance);
        earned += w * WEAK_CREDIT;
        total += w;
    }
    for u in &report.unknown {
        total += importance_weight(u.importance);
    }
    FitScore {
        ratio: (total > 0.0).then(|| earned / total),
        matched: report.matched.len(),
        weak: report.weak.len(),
        unknown: report.unknown.len(),
    }
}

/// One thing dragging the score down: a JD skill that's missing outright or
/// recorded but thin.
struct Gap {
    name: String,
    importance: Importance,
    kind: GapKind,
}

enum GapKind {
    /// Not in the dataset at all — a full miss.
    Missing,
    /// Recorded, but too thin to lean on, and why.
    Weak(Weakness),
}

/// The gaps worth showing first: most important first, and within one
/// importance a full miss ahead of a merely-weak skill. The sort is stable,
/// so ties keep JD order. Pure, for the same reason as [`score`].
fn ranked_gaps(report: &GapReport) -> Vec<Gap> {
    let mut gaps: Vec<Gap> = Vec::new();
    for u in &report.unknown {
        gaps.push(Gap {
            name: u.name.clone(),
            importance: u.importance,
            kind: GapKind::Missing,
        });
    }
    for w in &report.weak {
        gaps.push(Gap {
            name: w.matched.dataset_name.clone(),
            importance: w.matched.jd_skill.importance,
            kind: GapKind::Weak(w.weakness),
        });
    }
    // Sort by the rank key descending: higher importance first, and a miss
    // (1) ahead of a weak (0) at the same importance. A stable sort, so ties
    // keep JD order.
    gaps.sort_by_key(|g| std::cmp::Reverse(gap_rank(g)));
    gaps
}

/// The ordering key for [`ranked_gaps`]: `(importance, miss-before-weak)`,
/// both higher-is-first.
fn gap_rank(g: &Gap) -> (u8, u8) {
    let importance = match g.importance {
        Importance::Critical => 2,
        Importance::Required => 1,
        Importance::Preferred => 0,
    };
    let kind = match g.kind {
        GapKind::Missing => 1,
        GapKind::Weak(_) => 0,
    };
    (importance, kind)
}

/// The tight human view: the role, the score graded by band, the bucket
/// counts, then the few biggest gaps. Everything is on stderr (the stream the
/// color helpers detect); `--json` above is the machine output on stdout.
fn print_rating(requirements: &JobRequirements, fit: &FitScore, gaps: &[Gap]) {
    let title = or_unknown(&requirements.title);
    let company = or_unknown(&requirements.company);
    eprintln!("\n{}", style::bold(format!("Fit · {title} @ {company}")));

    let Some(ratio) = fit.ratio else {
        eprintln!(
            "{}",
            style::dim("this posting lists no skills to rate against")
        );
        return;
    };
    let pct = (ratio * 100.0).round() as u32;
    eprintln!(
        "{}  {}",
        style::grade(ratio, format!("{pct}%")),
        style::dim(format!(
            "· {} matched · {} weak · {} missing",
            fit.matched, fit.weak, fit.unknown
        ))
    );

    if gaps.is_empty() {
        eprintln!(
            "{}",
            style::success("every listed skill is backed by evidence")
        );
        return;
    }

    eprintln!("{}", style::section("Biggest gaps"));
    for g in gaps.iter().take(GAPS_SHOWN) {
        eprintln!("  {}", gap_line(g));
    }
    if gaps.len() > GAPS_SHOWN {
        eprintln!(
            "  {}",
            style::dim(format!(
                "… +{} more · `aarg gap` for the full breakdown",
                gaps.len() - GAPS_SHOWN
            ))
        );
    }
}

/// One styled gap line: a red ✗ for a miss, a yellow ⚠ (with the reason) for
/// a weak skill, each tagged with its importance.
fn gap_line(g: &Gap) -> String {
    let imp = style::dim(format!("({})", importance_label(g.importance)));
    match &g.kind {
        GapKind::Missing => style::fail(format!("{}  {imp}", g.name)),
        GapKind::Weak(weakness) => {
            let why = style::dim(format!("· {}", weakness_label(*weakness)));
            style::warn(format!("{} {why}  {imp}", g.name))
        }
    }
}

/// The machine view of a rating: the score, the bucket counts, and every gap
/// (the human view truncates, scripts get the lot).
#[derive(Serialize)]
struct RateReport {
    /// Weighted 0.0–1.0 coverage; `null` when the posting lists no skills.
    score: Option<f32>,
    matched: usize,
    weak: usize,
    unknown: usize,
    gaps: Vec<RateGap>,
}

/// One gap in the JSON breakdown. `status` is `"missing"` or `"weak"`;
/// `reason` is present only for a weak skill.
#[derive(Serialize)]
struct RateGap {
    name: String,
    importance: &'static str,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
}

impl RateGap {
    fn from_gap(g: &Gap) -> Self {
        match &g.kind {
            GapKind::Missing => RateGap {
                name: g.name.clone(),
                importance: importance_label(g.importance),
                status: "missing",
                reason: None,
            },
            GapKind::Weak(weakness) => RateGap {
                name: g.name.clone(),
                importance: importance_label(g.importance),
                status: "weak",
                reason: Some(weakness_label(*weakness)),
            },
        }
    }
}

/// Why a recorded skill is too thin to count as a full match.
fn weakness_label(w: Weakness) -> &'static str {
    match w {
        Weakness::NoEvidence => "no evidence recorded",
        Weakness::LowProficiency => "proficiency is only familiar",
    }
}

/// `aarg jd rm` — forget remembered parsed JDs. With `--all`, clears the
/// whole reuse cache after a confirm; otherwise offers a checklist (the
/// entries have no stable id, so picking is by selection, like
/// `history rm`). A non-interactive run with no `--all` explains how to
/// proceed rather than guessing. The store is a convenience cache, so this
/// never touches the authoritative per-build `jd.json` files.
pub async fn rm(all: bool) -> Result<(), CliError> {
    let stored = crate::jdstore::recent()?;
    if stored.is_empty() {
        eprintln!(
            "{}",
            style::suggest("no remembered job descriptions to remove")
        );
        return Ok(());
    }
    let user = auto_user();

    if all {
        let confirmed = user
            .confirm(
                &format!("forget all {} remembered job description(s)?", stored.len()),
                false,
            )
            .await
            .unwrap_or(false);
        if !confirmed {
            eprintln!("{}", style::dim("cancelled · nothing removed"));
            return Ok(());
        }
        crate::jdstore::clear()?;
        eprintln!(
            "{}",
            style::success(format!("forgot {} job description(s)", stored.len()))
        );
        return Ok(());
    }

    if !user.is_interactive() {
        eprintln!(
            "{}",
            style::suggest("run interactively to pick, or `aarg jd rm --all` to forget them all")
        );
        return Ok(());
    }

    let options: Vec<String> = stored.iter().map(jd_label).collect();
    let picks = match user
        .ask(Question::MultiSelect {
            prompt: "select job descriptions to forget (space toggles, enter confirms)".into(),
            options,
        })
        .await?
    {
        Answer::Choices(indexes) => indexes,
        _ => Vec::new(),
    };
    if picks.is_empty() {
        eprintln!("{}", style::dim("nothing selected · nothing removed"));
        return Ok(());
    }

    let total = stored.len();
    let kept = keep_unpicked(stored, &picks);
    let removed = total - kept.len();
    crate::jdstore::save(&kept)?;
    eprintln!(
        "{}",
        style::success(format!("forgot {removed} job description(s)"))
    );
    Ok(())
}

/// The stored JDs whose positions the user did *not* pick — what survives a
/// prune. Pure, so the keep/drop logic is unit-testable without a terminal.
fn keep_unpicked(stored: Vec<StoredJd>, picks: &[usize]) -> Vec<StoredJd> {
    let drop: HashSet<usize> = picks.iter().copied().collect();
    stored
        .into_iter()
        .enumerate()
        .filter(|(index, _)| !drop.contains(index))
        .map(|(_, jd)| jd)
        .collect()
}

/// "Company · Title · entered <date>" for one stored JD — the checklist label.
fn jd_label(stored: &StoredJd) -> String {
    format!(
        "{} · {} · entered {}",
        or_unknown(&stored.requirements.company),
        or_unknown(&stored.requirements.title),
        stored.saved_at.format("%Y-%m-%d %H:%M"),
    )
}

fn print_skills(heading: &str, skills: &[JdSkill]) {
    if skills.is_empty() {
        return;
    }
    eprintln!(
        "{}",
        style::section(format!("{heading} ({})", skills.len()))
    );
    for skill in skills {
        let meta = style::dim(format!(
            "({}, {})",
            category_label(skill.category),
            importance_label(skill.importance)
        ));
        let mut line = format!("{} {meta}", skill.name);
        if let Some(quote) = skill.context_phrases.first() {
            line.push_str(&format!(" {}", style::dim(format!("· \"{quote}\""))));
        }
        eprintln!("  {}", style::bullet(line));
    }
}

fn or_unknown(value: &str) -> &str {
    if value.is_empty() {
        "(not stated)"
    } else {
        value
    }
}

fn seniority_label(s: Seniority) -> &'static str {
    match s {
        Seniority::Junior => "junior",
        Seniority::Mid => "mid",
        Seniority::Senior => "senior",
        Seniority::Staff => "staff",
        Seniority::Principal => "principal",
        Seniority::Manager => "manager",
        Seniority::Director => "director",
        Seniority::Executive => "executive",
        Seniority::Unspecified => "seniority unstated",
    }
}

fn remote_label(r: RemotePolicy) -> &'static str {
    match r {
        RemotePolicy::Remote => "remote",
        RemotePolicy::Hybrid => "hybrid",
        RemotePolicy::OnSite => "on-site",
        RemotePolicy::Unspecified => "remote policy unstated",
    }
}

fn importance_label(i: Importance) -> &'static str {
    match i {
        Importance::Critical => "critical",
        Importance::Required => "required",
        Importance::Preferred => "preferred",
    }
}

fn category_label(c: SkillCategory) -> &'static str {
    match c {
        SkillCategory::Hard => "hard",
        SkillCategory::Soft => "soft",
        SkillCategory::Domain => "domain",
        SkillCategory::Tool => "tool",
        SkillCategory::Language => "language",
        SkillCategory::Framework => "framework",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::dataset::types::SkillId;
    use crate::gap::{SkillMatch, WeakMatch};
    use crate::jd::JobRequirements;

    fn jd_skill(name: &str, importance: Importance) -> JdSkill {
        JdSkill {
            name: name.to_string(),
            category: SkillCategory::Hard,
            importance,
            context_phrases: Vec::new(),
        }
    }

    fn matched(name: &str, importance: Importance) -> SkillMatch {
        SkillMatch {
            jd_skill: jd_skill(name, importance),
            skill_id: SkillId(name.to_lowercase()),
            dataset_name: name.to_string(),
            semantic: false,
        }
    }

    fn weak(name: &str, importance: Importance, weakness: Weakness) -> WeakMatch {
        WeakMatch {
            matched: matched(name, importance),
            weakness,
        }
    }

    #[test]
    fn score_weights_by_importance_and_half_credits_weak_skills() {
        // Critical matched (3/3), Required weak (1/2), Preferred missing (0/1):
        // earned 4.0 over total 6.0 = 0.6667.
        let report = GapReport {
            matched: vec![matched("Rust", Importance::Critical)],
            weak: vec![weak("Go", Importance::Required, Weakness::NoEvidence)],
            unknown: vec![jd_skill("Elixir", Importance::Preferred)],
        };
        let fit = score(&report);
        let ratio = fit.ratio.unwrap();
        assert!((ratio - 4.0 / 6.0).abs() < 1e-4, "ratio was {ratio}");
        assert_eq!((fit.matched, fit.weak, fit.unknown), (1, 1, 1));
    }

    #[test]
    fn score_has_no_ratio_when_the_posting_lists_no_skills() {
        let report = GapReport {
            matched: Vec::new(),
            weak: Vec::new(),
            unknown: Vec::new(),
        };
        assert!(score(&report).ratio.is_none());
    }

    #[test]
    fn ranked_gaps_lead_with_the_most_important_miss() {
        // A critical weak and a critical miss share the top importance; the
        // miss must sort ahead of the weak. A preferred miss trails both.
        let report = GapReport {
            matched: Vec::new(),
            weak: vec![weak(
                "TypeScript",
                Importance::Critical,
                Weakness::LowProficiency,
            )],
            unknown: vec![
                jd_skill("Bystander", Importance::Preferred),
                jd_skill("Kubernetes", Importance::Critical),
            ],
        };
        let gaps = ranked_gaps(&report);
        let names: Vec<&str> = gaps.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, vec!["Kubernetes", "TypeScript", "Bystander"]);
        assert!(matches!(gaps[0].kind, GapKind::Missing));
        assert!(matches!(gaps[1].kind, GapKind::Weak(_)));
    }

    fn stored(company: &str) -> StoredJd {
        StoredJd {
            saved_at: chrono::DateTime::from_timestamp(1, 0).unwrap(),
            requirements: JobRequirements {
                company: company.to_string(),
                title: "Engineer".to_string(),
                seniority: Seniority::Unspecified,
                location: None,
                remote: RemotePolicy::Unspecified,
                domain_keywords: Vec::new(),
                required_skills: Vec::new(),
                preferred_skills: Vec::new(),
                responsibilities: Vec::new(),
                ats_phrases: Vec::new(),
                raw_text: String::new(),
                source_url: None,
            },
        }
    }

    #[test]
    fn keep_unpicked_drops_only_the_selected_positions() {
        let list = vec![stored("Acme"), stored("Globex"), stored("Initech")];
        // The user ticked entries 0 and 2; only the unticked one survives.
        let kept = keep_unpicked(list, &[0, 2]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].requirements.company, "Globex");
    }

    #[test]
    fn keep_unpicked_with_no_picks_keeps_everything() {
        let list = vec![stored("Acme"), stored("Globex")];
        assert_eq!(keep_unpicked(list, &[]).len(), 2);
    }
}
