//! The `aarg` binary: parse arguments, dispatch into the library, and
//! render any error as a diagnostic. All real behavior lives in the
//! library crate so it stays testable.

use aarg::cli::{Cli, Command, DatasetCommand, JdCommand, LlmCommand};
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
        Command::Jd {
            command: JdCommand::Parse { path, json },
        } => aarg::commands::jd::parse(path, json).await?,
        Command::Llm {
            command: LlmCommand::Ping,
        } => aarg::commands::ping::run().await?,
    }
    Ok(())
}
