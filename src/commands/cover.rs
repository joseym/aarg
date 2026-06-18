//! `aarg cover [build]` — draft a cover letter for a past build, reusing
//! its saved resume and JD without re-tailoring, the way `aarg attack`
//! reuses a build's draft for a second review.
//!
//! The letter is written by the shared `CoverLetterAgent` (held to the same
//! never-fabricate guards as the resume) and rendered with the built-in
//! cover template, landing next to the build's resume PDFs.

use crate::agent::{AgentContext, ModelTier};
use crate::builds;
use crate::commands::{CliError, configured_client};
use crate::cover::write_cover_letter;
use crate::dataset::store;
use crate::jd::JobRequirements;
use crate::render;
use crate::style::{self, Spinner};
use crate::tailor::TailoredResume;

pub async fn run(build: Option<String>) -> Result<(), CliError> {
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
        llm: &client,
        model: &config.anthropic,
        tracer: &tracer,
        sink: None,
    };
    let model = ctx.model.resolve("cover_letter_v1", ModelTier::Mid);

    eprintln!(
        "{}",
        style::section(format!("cover letter · {} @ {}", jd.title, jd.company))
    );
    let sp = Spinner::start(format!("drafting on {model}"));
    let (letter, warnings, usage) = write_cover_letter(&ctx, &resume, &jd, &samples).await?;
    sp.finish(style::done("cover letter drafted"));
    for warning in &warnings {
        eprintln!("{}", style::warn(warning));
    }

    let build_dir = builds::builds_root()?.join(&build);
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
