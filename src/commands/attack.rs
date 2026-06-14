//! `aarg attack <build>` (FR-3.7) — re-run the adversarial reviewer on a
//! build's saved draft, without re-tailoring.
//!
//! The reviewer is non-deterministic and reads the *current* dataset, so a
//! second pass is a useful second opinion: after backing a skill or
//! dismissing an objection, attack tells you what the reviewer thinks now —
//! for the cost of one review call, not a full Opus tailor + render loop.
//! It's a read-only re-look: the build's stored report is left untouched.

use crate::agent::{Agent, AgentContext, ModelTier};
use crate::ats::AtsReport;
use crate::commands::tailor::format_objection;
use crate::commands::{CliError, configured_client};
use crate::dataset::store;
use crate::jd::JobRequirements;
use crate::review::{AdversarialReviewerAgent, ReviewInput, Severity};
use crate::style::{self, Spinner};
use crate::tailor::TailoredResume;
use crate::terminal::auto_user;
use crate::trace::Tracer;
use crate::user::{Answer, Question};

pub async fn run(build: Option<String>) -> Result<(), CliError> {
    // Resolve which build to re-review. An explicit id is used as-is; with
    // no id we offer a single-select picker — but only to a real person.
    // A piped/CI run gets a typed pointer, never a hang or a silent pick.
    let build = match build {
        Some(id) => id,
        None => match pick_build().await? {
            Some(id) => id,
            // No builds, or a non-interactive run: `pick_build` has already
            // said why, so there's nothing left to do.
            None => return Ok(()),
        },
    };

    // The draft and the JD it was tailored for; coverage is reused from the
    // stored ATS report (the draft hasn't changed, so coverage hasn't).
    let resume: TailoredResume = crate::history::read_artifact(&build, "canonical.json")?;
    let jd: JobRequirements = crate::history::read_artifact(&build, "jd.json")?;
    let ats: AtsReport = crate::history::read_artifact(&build, "ats_report.json")?;

    let dataset = store::load()?;
    let (client, config) = configured_client().await?;
    let tracer = Tracer::to_default_dir()?;
    let ctx = AgentContext {
        llm: &client,
        model: &config.anthropic,
        tracer: &tracer,
        sink: None,
    };
    let model = ctx
        .model
        .resolve("adversarial_reviewer_v1", ModelTier::Smart);

    let sp = Spinner::start(format!("re-reviewing build {build} (no re-tailor)"));
    let run = AdversarialReviewerAgent
        .run(
            &ctx,
            ReviewInput {
                draft: resume,
                jd,
                dataset: dataset.clone(),
            },
        )
        .await?;
    sp.finish(style::done("re-reviewed"));

    let report = run.output;
    // Objections the user has already accepted are filtered from the view,
    // same as a tailor run; the honest score still uses the full report.
    let visible = report.without_dismissed(&dataset.metadata.dismissed_objections);
    let score = 0.6 * report.overall_score + 0.4 * ats.coverage;

    if !report.persona_notes.is_empty() {
        eprintln!(
            "\n{}",
            style::bold(format!("reviewer verdict ({:.2})", report.overall_score))
        );
        eprintln!("  {}", report.persona_notes);
    }
    eprintln!(
        "  {}  {}",
        style::cyan(format!("score {score:.2}")),
        style::dim(format!("(review · {:.0}% coverage)", ats.coverage * 100.0))
    );

    if visible.objections.is_empty() {
        eprintln!("  {}", style::green("no open objections"));
    } else {
        eprintln!(
            "\n{}",
            style::bold(format!("{} objection(s)", visible.objections.len()))
        );
        for objection in &visible.objections {
            let line = format_objection(objection);
            // Blocking/major stand out; minors are dimmed.
            match objection.severity {
                Severity::Blocking | Severity::Major => {
                    eprintln!("  {} {line}", style::yellow("•"))
                }
                Severity::Minor => eprintln!("  {}", style::dim(format!("· {line}"))),
            }
        }
    }

    if let Some(cost) = crate::pricing::cost_usd(model, &run.usage, &config.prices) {
        eprintln!("\n{}", style::dim(format!("~${cost:.2} (review only)")));
    }
    Ok(())
}

/// Ask the user which saved build to re-review when none was named on the
/// command line. Returns the chosen id, or `None` when there's nothing to
/// pick (no builds, or a non-interactive run) — in which case it has
/// already printed why, and the caller should stop cleanly.
///
/// Mirrors `history rm`'s no-ids path, but a single selection: `attack`
/// re-reviews exactly one build, so this is a `Select`, not a checklist.
async fn pick_build() -> Result<Option<String>, CliError> {
    let user = auto_user();
    let builds = crate::history::list()?;
    if builds.is_empty() {
        eprintln!("no builds yet — run `aarg tailor <jd>`");
        return Ok(None);
    }
    // A piped/CI run can't answer a picker; point it at the explicit form
    // rather than hanging on an `ask` or silently choosing for it.
    if !user.is_interactive() {
        eprintln!("specify a build id to re-review, e.g. `aarg attack 021`");
        return Ok(None);
    }

    // One readable line per build, newest first (the order `list` returns),
    // in the same style as `history rm`'s checklist.
    let options: Vec<String> = builds
        .iter()
        .map(|b| {
            format!(
                "{}  {:.2}  {}  {} · {} obj",
                b.id, b.score, b.target, b.created_at, b.objections
            )
        })
        .collect();
    match user
        .ask(Question::Select {
            prompt: "pick a build to re-review".into(),
            options,
        })
        .await?
    {
        // Map the chosen index back to its build id.
        Answer::Choice(i) => Ok(builds.get(i).map(|b| b.id.clone())),
        _ => Ok(None),
    }
}
