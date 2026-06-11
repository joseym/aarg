//! The `aarg` binary: parse arguments, dispatch into the library, and
//! render any error as a diagnostic. All real behavior lives in the
//! library crate so it stays testable.

use aarg::cli::{Cli, Command, LlmCommand};
use clap::Parser;

#[tokio::main]
async fn main() -> miette::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Init => aarg::commands::init::run().await?,
        Command::Config => aarg::commands::config::run().await?,
        Command::Llm {
            command: LlmCommand::Ping,
        } => aarg::commands::ping::run().await?,
    }
    Ok(())
}
