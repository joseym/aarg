//! `aarg history`, `aarg diff <a> <b>`, and `aarg history rm <id>...` —
//! the CLI face of `crate::history`.
//!
//! Thin presentation over the build directories: list past runs, compare
//! two of them, or delete one. All output is on stderr like the rest of
//! aarg's human output; stdout stays clean for a future `--json`.

use crate::commands::CliError;
use crate::config::Config;
use crate::history::{self, BuildDiff};
use crate::llm::TokenUsage;
use crate::pricing;
use crate::style;
use crate::terminal::auto_user;
use crate::user::{Answer, Question};

/// `aarg history` — every build, newest first.
pub fn list() -> Result<(), CliError> {
    let builds = history::list()?;
    if builds.is_empty() {
        eprintln!("no builds yet — run `aarg tailor <jd>`");
        return Ok(());
    }
    // Prices come from config (built-in family defaults otherwise).
    let prices = Config::load()?.prices;
    let mut total = 0.0;

    eprintln!("{}", style::bold(format!("{} build(s)", builds.len())));
    for b in &builds {
        let usage = TokenUsage {
            input_tokens: b.tokens_in,
            output_tokens: b.tokens_out,
        };
        let cost = pricing::cost_usd(&b.model, &usage, &prices);
        if let Some(c) = cost {
            total += c;
        }
        let cost_cell = cost.map_or_else(|| "    —".to_string(), |c| format!("~${c:.2}"));
        eprintln!(
            "  {}  {}  {}  {}  {}",
            style::bold(format!("{:>3}", b.id)),
            style::cyan(format!("{:.2}", b.score)),
            style::dim(format!("{cost_cell:>7}")),
            b.target,
            style::dim(format!("{} · {} obj", b.created_at, b.objections))
        );
    }
    eprintln!("{}", style::dim(format!("total ~${total:.2}")));
    Ok(())
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
        style::cyan(format!("{:.2}", d.score_to)),
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
            eprintln!(
                "        {}",
                style::dim(format!("- {}", truncate(&change.from)))
            );
            eprintln!(
                "        {}",
                style::dim(format!("+ {}", truncate(&change.to)))
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
            eprintln!("specify build ids to remove, e.g. `aarg history rm 019 020`");
            return Ok(());
        }
        let options: Vec<String> = builds
            .iter()
            .map(|b| format!("{}  {:.2}  {}  {}", b.id, b.score, b.target, b.created_at))
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
        eprintln!("nothing selected — nothing deleted");
        return Ok(());
    }

    eprintln!(
        "{} {}",
        style::yellow("about to permanently delete build(s):"),
        ids.join(", ")
    );
    let confirmed = user
        .confirm("delete them and all their artifacts?", false)
        .await
        .unwrap_or(false);
    if !confirmed {
        eprintln!("cancelled — nothing deleted");
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
                eprintln!("{}", style::yellow(format!("no build {id} — skipped")));
            }
            Err(other) => return Err(other.into()),
        }
    }
    eprintln!("{}", style::dim(format!("removed {removed} build(s)")));
    Ok(())
}
