//! `aarg render [build]` — re-render a saved build's resume PDFs without
//! running the tailor loop.
//!
//! This is `tailor`'s finalize step on its own: the ATS variant is a
//! deterministic projection of the saved canonical draft (no model), and the
//! human variant is re-projected through the variant adapter (one Mid-tier
//! call), re-vetted, lint-checked, and rendered with the current templates.
//! No JD parse, gap analysis, review loop, or interviews. It picks up template
//! and payload-logic improvements (a layout fix, re-curated and grouped
//! skills) for roughly the cost of a cover letter.
//!
//! `--no-llm` skips the model entirely: it re-renders the build's saved
//! variant payloads with the current templates, which picks up template and
//! layout changes alone (the saved skills, not re-curated ones).

use std::path::{Path, PathBuf};

use crate::agent::{Agent, AgentContext, ModelTier};
use crate::builds;
use crate::commands::tailor::{resolve_ats_template, resolve_human_template};
use crate::commands::{CliError, configured_client};
use crate::config::Config;
use crate::dataset::store;
use crate::jd::JobRequirements;
use crate::llm::TokenUsage;
use crate::render;
use crate::style::{self, Spinner};
use crate::tailor::TailoredResume;
use crate::variant::{
    self, TemplateId, Variant, VariantAdapterAgent, VariantInput, VariantPayload,
};

pub async fn run(
    build: Option<String>,
    no_llm: bool,
    template: Option<PathBuf>,
) -> Result<(), CliError> {
    // Resolve which build: an explicit id is used as-is; with none we offer a
    // picker, and a piped/CI run gets a typed pointer instead of a hang.
    let build = match build {
        Some(id) => id,
        None => match super::pick_build("pick a build to re-render", "aarg render 029").await? {
            Some(id) => id,
            None => return Ok(()),
        },
    };
    let build_dir = builds::builds_root()?.join(&build);
    eprintln!("{}", style::section(format!("re-rendering build {build}")));

    if no_llm {
        // Model-free path: re-render the saved payloads with the current
        // templates. Picks up template/layout changes only — the skills are
        // whatever the build saved, not re-curated.
        let config = Config::load()?;
        let ats: VariantPayload = crate::history::read_artifact(&build, "ats_payload.json")?;
        let human: VariantPayload = crate::history::read_artifact(&build, "human_payload.json")?;
        let sp = Spinner::start("rendering from saved payloads (no model)");
        let ats_pdf = render::render(&build_dir, &ats, &resolve_ats_template(&config)?.template)?;
        let human_pdf = render::render(
            &build_dir,
            &human,
            &resolve_human_template(&template, &config)?.template,
        )?;
        sp.finish(style::done("re-rendered"));
        report(&ats_pdf, &human_pdf);
        eprintln!(
            "  {}",
            style::info(
                "layout only; re-curated and grouped skills need a model re-render (drop --no-llm)"
            )
        );
        return Ok(());
    }

    // Full re-render: re-project from the saved canonical draft, exactly as the
    // tailor loop's finalize does, but without the loop.
    let canonical: TailoredResume = crate::history::read_artifact(&build, "canonical.json")?;
    let jd: JobRequirements = crate::history::read_artifact(&build, "jd.json")?;
    let dataset = store::load()?;
    let (client, config) = configured_client().await?;
    let tracer = super::default_tracer()?;
    let ctx = AgentContext {
        llm: &client,
        model: &config.anthropic,
        tracer: &tracer,
        sink: None,
    };

    // ATS: a deterministic projection of the saved draft, no model call.
    let ats_tpl = resolve_ats_template(&config)?;
    let mut ats = variant::ats_payload(&canonical);
    ats.template = TemplateId(ats_tpl.id.clone());
    let sp = Spinner::start("rendering the ATS resume");
    let ats_pdf = render::render(&build_dir, &ats, &ats_tpl.template)?;
    sp.finish(style::done("ATS resume rendered"));

    // Human: reword through the adapter, revert any overclaim, hard-fail on a
    // claim divergence, then render. Same guards as the tailor finalize.
    let mut total = TokenUsage::default();
    let human_tpl = resolve_human_template(&template, &config)?;
    let sp = Spinner::start("reshaping for a human reader");
    let adapted = VariantAdapterAgent
        .run(
            &ctx,
            VariantInput {
                draft: canonical.clone(),
                variant: Variant::Human,
                // Keep a user-confirmed summary verbatim on re-render too.
                summary_locked: dataset.summary_confirmed,
            },
        )
        .await?;
    add_usage(&mut total, adapted.usage);
    let (mut human, review_usage) =
        variant::vet_human(&ctx, &canonical, adapted.output, &jd, &dataset).await?;
    add_usage(&mut total, review_usage);
    variant::check_claims(&canonical, &human)?;
    human.template = TemplateId(human_tpl.id.clone());
    let human_pdf = render::render(&build_dir, &human, &human_tpl.template)?;
    sp.finish(style::done("human resume rendered"));

    report(&ats_pdf, &human_pdf);
    // Priced at the smart (reviewer) tier, which dominates the two calls.
    let model = ctx
        .model
        .resolve("adversarial_reviewer_v1", ModelTier::Smart);
    if let Some(cost) = crate::pricing::cost_usd(model, &total, &config.prices) {
        eprintln!("  {}", style::dim(format!("~${cost:.2}")));
    }
    Ok(())
}

/// Print where the two PDFs landed, in the same style the tailor summary uses.
fn report(ats_pdf: &Path, human_pdf: &Path) {
    eprintln!("\n{}", style::done(style::bold("re-rendered")));
    eprintln!(
        "  {}  {}",
        style::dim(ats_pdf.display()),
        style::dim(format!("- {}", Variant::Ats.purpose()))
    );
    eprintln!(
        "  {}  {}",
        style::dim(human_pdf.display()),
        style::dim(format!("- {}", Variant::Human.purpose()))
    );
}

fn add_usage(total: &mut TokenUsage, other: TokenUsage) {
    total.input_tokens += other.input_tokens;
    total.output_tokens += other.output_tokens;
}
