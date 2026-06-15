//! The `aarg` binary: parse arguments, dispatch into the library, and
//! render any error as a diagnostic. All real behavior lives in the
//! library crate so it stays testable.

use aarg::cli::{
    Cli, Command, DatasetCommand, HistoryCommand, JdCommand, LlmCommand, RolesCommand,
    SkillsCommand, TraceCommand, VoiceCommand,
};
use clap::Parser;

#[tokio::main]
async fn main() -> miette::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Init => aarg::commands::init::run().await?,
        Command::Config => aarg::commands::config::run().await?,
        Command::Ingest { path } => aarg::commands::ingest::run(path).await?,
        Command::Dataset {
            command: DatasetCommand::Show,
        } => aarg::commands::dataset::show().await?,
        Command::Dataset {
            command: DatasetCommand::Validate,
        } => aarg::commands::dataset::validate().await?,
        Command::Dataset {
            command: DatasetCommand::Edit,
        } => aarg::commands::dataset::edit().await?,
        Command::Jd {
            command: JdCommand::Parse { path, json },
        } => aarg::commands::jd::parse(path, json).await?,
        Command::Gap { jd, json } => aarg::commands::gap::run(jd, json).await?,
        Command::Tailor {
            jd,
            variant,
            template,
        } => aarg::commands::tailor::run(jd, variant.variants(), template).await?,
        Command::Attack { build } => aarg::commands::attack::run(build).await?,
        Command::History { command: None } => aarg::commands::history::list()?,
        Command::History {
            command: Some(HistoryCommand::Rm { ids }),
        } => aarg::commands::history::remove(ids).await?,
        Command::Diff { from, to } => aarg::commands::history::diff(from, to)?,
        Command::Skills {
            command: SkillsCommand::Verify,
        } => aarg::commands::skills::verify().await?,
        Command::Skills {
            command: SkillsCommand::Dedup,
        } => aarg::commands::skills::dedup().await?,
        Command::Voice {
            command: VoiceCommand::Add { context },
        } => aarg::commands::voice::add(context).await?,
        Command::Voice {
            command: VoiceCommand::List,
        } => aarg::commands::voice::list().await?,
        Command::Voice {
            command: VoiceCommand::Remove { id },
        } => aarg::commands::voice::remove(id).await?,
        Command::Roles {
            command: RolesCommand::Enrich { id },
        } => aarg::commands::roles::enrich(id).await?,
        Command::Trace {
            command: TraceCommand::Last,
        } => aarg::commands::trace::last().await?,
        Command::Trace {
            command: TraceCommand::Show { id },
        } => aarg::commands::trace::show(id).await?,
        Command::Llm {
            command: LlmCommand::Ping,
        } => aarg::commands::ping::run().await?,
    }
    Ok(())
}
