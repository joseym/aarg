//! `aarg history`, `aarg diff <a> <b>`, and `aarg history rm <id>...` —
//! the CLI face of `crate::history`.
//!
//! Thin presentation over the build directories: list past runs, compare
//! two of them, or delete one. All output is on stderr like the rest of
//! aarg's human output; stdout stays clean for a future `--json`.

use std::collections::BTreeMap;

use crate::commands::CliError;
use crate::config::Config;
use crate::history::{self, BuildDiff, BuildSummary};
use crate::llm::TokenUsage;
use crate::pricing::{self, Price};
use crate::style;
use crate::terminal::auto_user;
use crate::user::{Answer, Question};

/// `aarg history` — every build, newest first.
pub fn list() -> Result<(), CliError> {
    let builds = history::list()?;
    if builds.is_empty() {
        eprintln!(
            "{}",
            style::suggest("no builds yet - run `aarg tailor <jd>`")
        );
        return Ok(());
    }
    // Prices come from config (built-in family defaults otherwise).
    let prices = Config::load()?.prices;
    let mut total = 0.0;
    let mut any_billed = false;

    let width = style::term_width();

    eprintln!("{}", style::section(format!("{} build(s)", builds.len())));
    for b in &builds {
        let (cell, billed) = cost_cell(b, &prices);
        if let Some(c) = billed {
            total += c;
            any_billed = true;
        }
        for line in build_row(b, &cell, width) {
            eprintln!("{line}");
        }
    }
    if any_billed {
        eprintln!("{}", style::dim(format!("total ~${total:.2}")));
    } else {
        eprintln!("{}", style::dim("total: covered by your Claude plan"));
    }
    Ok(())
}

/// The cost column for one build: a `plan` marker on a subscription run (no
/// marginal cost, so a dollar estimate would mislead), otherwise the priced
/// estimate or a dash for an unpriced model. Returns the cell text and the
/// dollar amount to fold into the billed total (`None` when the run wasn't
/// billed), so the caller sums only what was actually charged.
fn cost_cell(b: &BuildSummary, prices: &BTreeMap<String, Price>) -> (String, Option<f64>) {
    if b.subscription {
        return ("plan".to_string(), None);
    }
    let usage = TokenUsage {
        input_tokens: b.tokens_in,
        output_tokens: b.tokens_out,
    };
    let cost = pricing::cost_usd(&b.model, &usage, prices);
    let cell = cost.map_or_else(|| "    -".to_string(), |c| format!("~${c:.2}"));
    (cell, cost)
}

/// Lay out one build's row. Returns the line(s) to print: a single line when
/// it fits `width`, or a folded two-line form (metadata, then the full title
/// indented beneath) when it wouldn't — so a long title never wraps mid-word
/// into the columns beside it. Pure, so the width decision and the score
/// banding are unit-testable without a real terminal.
fn build_row(b: &BuildSummary, cost_cell: &str, width: usize) -> Vec<String> {
    let id = style::bold(format!("{:>3}", b.id));
    let score = score_cell(b.score);
    let cost = style::dim(format!("{cost_cell:>7}"));

    let trailer = style::dim(format!("{} · {} obj", b.created_at, b.objections));
    let one_line = format!("  {id}  {score}  {cost}  {}  {trailer}", b.target());
    if style::display_width(&one_line) <= width {
        return vec![one_line];
    }
    // Folded: objections + date (both fixed-width) on the first line, the
    // full title on its own indented line below.
    let meta = style::dim(format!("· {:>2} obj  {}", b.objections, b.created_at));
    vec![
        format!("  {id}  {score}  {cost}  {meta}"),
        format!("      {}", b.target()),
    ]
}

/// Color a score by quality so the column reads at a glance. Delegates to the
/// shared `style::grade` banding (green/yellow/red), which `diff` and the
/// tailor loop share — the figure is always shown, color only grades it.
fn score_cell(score: f32) -> String {
    style::grade(score, format!("{score:.2}"))
}

/// `aarg diff <a> <b>` — what changed from build `a` to build `b`.
pub fn diff(from: String, to: String) -> Result<(), CliError> {
    let d = history::diff(&from, &to)?;
    print_diff(&d);
    Ok(())
}

fn print_diff(d: &BuildDiff) {
    eprintln!("\n{}", style::bold(format!("build {} → {}", d.from, d.to)));

    eprintln!(
        "  score       {:.2} → {}  {}",
        d.score_from,
        style::grade(d.score_to, format!("{:.2}", d.score_to)),
        delta(d.score_to - d.score_from)
    );
    eprintln!(
        "  coverage    {:.0}% → {:.0}%",
        d.coverage_from * 100.0,
        d.coverage_to * 100.0
    );
    eprintln!(
        "  objections  {} → {}  {}",
        d.objections_from,
        d.objections_to,
        delta_count(d.objections_from, d.objections_to)
    );

    if !d.skills_added.is_empty() || !d.skills_removed.is_empty() {
        eprintln!("\n  {}", style::bold("skills"));
        for s in &d.skills_added {
            eprintln!("    {} {s}", style::green("+"));
        }
        for s in &d.skills_removed {
            eprintln!("    {} {}", style::red("-"), style::dim(s.clone()));
        }
    }

    let bullets_touched = !d.bullets_added.is_empty()
        || !d.bullets_removed.is_empty()
        || !d.bullets_changed.is_empty();
    if bullets_touched {
        eprintln!("\n  {}", style::bold("bullets"));
        for id in &d.bullets_added {
            eprintln!("    {} {id}", style::green("+"));
        }
        for id in &d.bullets_removed {
            eprintln!("    {} {}", style::red("-"), style::dim(id.clone()));
        }
        for change in &d.bullets_changed {
            eprintln!("    {} {}", style::yellow("~"), change.id);
            // Red for the wording that left, green for what replaced it — the
            // unified-diff convention, so the actual change reads at a glance.
            eprintln!(
                "        {}",
                style::red(format!("- {}", truncate(&change.from)))
            );
            eprintln!(
                "        {}",
                style::green(format!("+ {}", truncate(&change.to)))
            );
        }
    }
}

/// A signed score delta, green when it improved (rose), red when it dropped.
fn delta(change: f32) -> String {
    if change.abs() < 1e-6 {
        style::dim("(no change)")
    } else if change > 0.0 {
        style::green(format!("(+{change:.2})"))
    } else {
        style::red(format!("({change:.2})"))
    }
}

/// A signed objection-count delta. Fewer objections is the improvement, so
/// a drop is green and a rise is red — opposite of the score's coloring.
fn delta_count(from: usize, to: usize) -> String {
    let change = to as i64 - from as i64;
    if change == 0 {
        style::dim("(no change)")
    } else if change < 0 {
        style::green(format!("({change})"))
    } else {
        style::red(format!("(+{change})"))
    }
}

/// Trim a bullet to one readable line for the diff view.
fn truncate(text: &str) -> String {
    const MAX: usize = 80;
    if text.chars().count() <= MAX {
        return text.to_string();
    }
    let kept: String = text.chars().take(MAX).collect();
    format!("{kept}…")
}

/// `aarg history rm [id...]` — delete builds. With no ids, offer a
/// checklist of the builds to pick from. Destructive (removes the PDF and
/// every artifact), so it confirms first and a non-interactive run declines
/// by default rather than deleting unprompted.
pub async fn remove(ids: Vec<String>) -> Result<(), CliError> {
    let user = auto_user();

    // No ids given: let the user tick the ones to remove off a list.
    let ids = if ids.is_empty() {
        let builds = history::list()?;
        if builds.is_empty() {
            eprintln!("no builds to remove");
            return Ok(());
        }
        if !user.is_interactive() {
            eprintln!(
                "{}",
                style::suggest("specify build ids to remove, e.g. `aarg history rm 019 020`")
            );
            return Ok(());
        }
        let options: Vec<String> = builds
            .iter()
            .map(|b| format!("{}  {:.2}  {}  {}", b.id, b.score, b.target(), b.created_at))
            .collect();
        match user
            .ask(Question::MultiSelect {
                prompt: "select builds to remove (space toggles, enter confirms)".into(),
                options,
            })
            .await?
        {
            Answer::Choices(picks) => picks
                .iter()
                .filter_map(|&i| builds.get(i))
                .map(|b| b.id.clone())
                .collect(),
            _ => Vec::new(),
        }
    } else {
        ids
    };

    if ids.is_empty() {
        eprintln!("{}", style::dim("nothing selected - nothing deleted"));
        return Ok(());
    }

    eprintln!(
        "{}",
        style::warn(format!(
            "about to permanently delete build(s): {}",
            ids.join(", ")
        ))
    );
    let confirmed = user
        .confirm("delete them and all their artifacts?", false)
        .await
        .unwrap_or(false);
    if !confirmed {
        eprintln!("{}", style::dim("cancelled - nothing deleted"));
        return Ok(());
    }

    let mut removed = 0;
    for id in &ids {
        match history::remove(id) {
            Ok(()) => {
                removed += 1;
                eprintln!("{}", style::done(format!("removed build {id}")));
            }
            Err(history::HistoryError::NotFound { .. }) => {
                eprintln!("{}", style::warn(format!("no build {id} - skipped")));
            }
            Err(other) => return Err(other.into()),
        }
    }
    eprintln!("{}", style::dim(format!("removed {removed} build(s)")));
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn sample(target: &str, score: f32) -> BuildSummary {
        // `target` here is the pre-split `"title @ company"` form these tests
        // were already written with; split it back apart so `BuildSummary`
        // round-trips through `.target()` to the exact same string.
        let (title, company) = target
            .split_once(" @ ")
            .map(|(t, c)| (t.to_string(), c.to_string()))
            .unwrap_or_else(|| (target.to_string(), String::new()));
        BuildSummary {
            id: "029".into(),
            created_at: "2026-06-16 17:19".into(),
            title,
            company,
            template: "ats/classic".into(),
            model: "claude-sonnet-4-6".into(),
            score,
            review_score: score,
            coverage: 0.9,
            objections: 11,
            tokens_in: 1000,
            tokens_out: 2000,
            subscription: false,
        }
    }

    #[test]
    fn a_row_that_fits_stays_on_one_line() {
        let b = sample("VP Engineering @ Mainstay", 0.81);
        let rows = build_row(&b, "~$1.55", usize::MAX);
        assert_eq!(rows.len(), 1);
        // Title and trailing date both ride the single line.
        assert!(rows[0].contains("VP Engineering @ Mainstay"));
        assert!(rows[0].contains("2026-06-16 17:19"));
    }

    #[test]
    fn a_row_too_wide_folds_the_title_onto_its_own_line() {
        let b = sample(
            "Engineering Director, Business Applications @ Ladders (Client)",
            0.77,
        );
        // A deliberately narrow terminal forces the fold.
        let rows = build_row(&b, "~$2.42", 30);
        assert_eq!(rows.len(), 2);
        // Metadata stays on the first line; the title moves below it intact.
        assert!(!rows[0].contains("Ladders"));
        assert!(rows[0].contains("2026-06-16 17:19"));
        assert!(rows[1].trim_start().starts_with("Engineering Director"));
    }

    #[test]
    fn the_score_number_is_always_shown_whatever_the_band() {
        // Color is suppressed off a TTY, so the banding isn't visible here —
        // which is exactly the guarantee: the figure must read without it.
        assert!(score_cell(0.92).contains("0.92")); // green band
        assert!(score_cell(0.72).contains("0.72")); // yellow band
        assert!(score_cell(0.41).contains("0.41")); // red band
    }

    #[test]
    fn cost_cell_marks_a_subscription_build_as_plan_and_leaves_it_unbilled() {
        let mut sub = sample("VP Engineering @ Mainstay", 0.81);
        sub.subscription = true;
        let (cell, billed) = cost_cell(&sub, &BTreeMap::new());
        assert_eq!(cell.trim(), "plan");
        assert!(billed.is_none());

        // A billed build on a priceable model still shows a dollar figure.
        let billed_build = sample("VP Engineering @ Mainstay", 0.81);
        let (cell, amount) = cost_cell(&billed_build, &BTreeMap::new());
        assert!(cell.contains('$'));
        assert!(amount.is_some());
    }
}
