//! `aarg cover [build]` — draft a cover letter for a past build, reusing
//! its saved resume and JD without re-tailoring, the way `aarg attack`
//! reuses a build's draft for a second review.
//!
//! The letter is written by the shared `CoverLetterAgent` (held to the same
//! never-fabricate guards as the resume) and rendered with the built-in
//! cover template, landing next to the build's resume PDFs.
//!
//! `--interactive` walks a short interview first (`cover_interview`'s
//! angle/emphasis/tone/motivation/constraints topics), the same
//! propose-and-dispose pattern `strengthen` and `enrich` use, and hands the
//! resulting `CoverBrief` to the same generation call as grounding. It needs
//! a real person to answer, so a piped/CI run (or a terminal that doesn't
//! report as interactive) drafts without it instead of hanging, matching
//! `aarg tune`'s house rule for the same situation. The brief is saved as
//! `cover_brief.json` next to the build's other artifacts; a later
//! `--interactive` run on the same build offers to reuse it instead of
//! re-interviewing from scratch.

use crate::agent::{AgentContext, ModelTier};
use crate::builds;
use crate::commands::{CliError, configured_client};
use crate::cover::write_cover_letter;
use crate::cover_interview::{CoverBrief, run_cover_interview};
use crate::dataset::store;
use crate::jd::JobRequirements;
use crate::render;
use crate::style::{self, Spinner};
use crate::tailor::TailoredResume;
use crate::terminal::auto_user;

pub async fn run(build: Option<String>, interactive: bool) -> Result<(), CliError> {
    // Fail fast if `typst` isn't installed, before picking a build or paying
    // for the cover-letter draft — the letter still has to render at the end.
    crate::render::ensure_available()?;
    // Resolve which build: an explicit id is used as-is; with none we offer
    // a picker, and a piped/CI run gets a typed pointer instead of a hang.
    let build = match build {
        Some(id) => id,
        None => {
            match super::pick_build("pick a build to write a cover letter for", "aarg cover 029")
                .await?
            {
                Some(id) => id,
                None => return Ok(()),
            }
        }
    };

    // The resume and JD the letter is grounded in come straight off the
    // build; the dataset supplies the writing samples for tone.
    let resume: TailoredResume = crate::history::read_artifact(&build, "canonical.json")?;
    let jd: JobRequirements = crate::history::read_artifact(&build, "jd.json")?;
    let dataset = store::load()?;
    let samples: Vec<String> = dataset
        .voice_samples
        .iter()
        .map(|sample| sample.text.clone())
        .collect();

    let (client, config) = configured_client().await?;
    let tracer = super::default_tracer()?;
    let ctx = AgentContext {
        llm: &*client,
        model: config.active_resolver(),
        tracer: &tracer,
        sink: None,
    };
    let model = ctx.model.resolve("cover_letter_v1", ModelTier::Mid);
    let build_dir = builds::builds_root()?.join(&build);

    let brief = if interactive {
        interview_brief(&build_dir, &resume, &jd, &ctx).await?
    } else {
        None
    };

    eprintln!(
        "{}",
        style::section(format!("cover letter · {} @ {}", jd.title, jd.company))
    );
    let sp = Spinner::start(format!("drafting on {model}"));
    let (letter, warnings, usage) =
        write_cover_letter(&ctx, &resume, &jd, &samples, brief.as_ref()).await?;
    sp.finish(style::done("cover letter drafted"));
    for warning in &warnings {
        eprintln!("{}", style::warn(warning));
    }

    let sp = Spinner::start("rendering the cover letter");
    let pdf = render::render_cover(&build_dir, &letter, &render::Template::cover())?;
    sp.finish(style::done("cover letter rendered"));

    eprintln!("\n{}", style::done(style::bold("cover letter saved")));
    eprintln!("  {}", style::dim(pdf.display()));
    if let Some(cost) = crate::pricing::cost_usd(model, &usage, &config.prices) {
        eprintln!("  {}", style::dim(format!("~${cost:.2}")));
    }
    Ok(())
}

/// Get the `CoverBrief` to draft from when `--interactive` was passed: reuse
/// a brief already saved for this build (offered, never assumed), or run a
/// fresh interview and save its answers. Returns `None` without asking
/// anything when the current terminal can't actually hold an interview
/// (piped/CI) — the caller still drafts the letter, just without the brief,
/// rather than hanging on a prompt nobody can answer.
async fn interview_brief(
    build_dir: &std::path::Path,
    resume: &TailoredResume,
    jd: &JobRequirements,
    ctx: &AgentContext<'_>,
) -> Result<Option<CoverBrief>, CliError> {
    let user = auto_user();
    if !user.is_interactive() {
        eprintln!(
            "{}",
            style::warn("--interactive needs a terminal; drafting without the interview")
        );
        return Ok(None);
    }

    let saved_path = build_dir.join("cover_brief.json");
    let saved: Option<CoverBrief> = std::fs::read_to_string(&saved_path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());

    let brief = match saved {
        Some(prior) => {
            let reuse = user
                .confirm(
                    "reuse the brief from a previous interview on this build?",
                    true,
                )
                .await
                .unwrap_or(true);
            if reuse {
                prior
            } else {
                eprintln!("{}", style::section("cover letter interview"));
                run_cover_interview(resume, jd, user.as_ref(), ctx).await?
            }
        }
        None => {
            eprintln!("{}", style::section("cover letter interview"));
            run_cover_interview(resume, jd, user.as_ref(), ctx).await?
        }
    };

    builds::write_json(build_dir, "cover_brief.json", &brief)?;
    Ok(Some(brief))
}
