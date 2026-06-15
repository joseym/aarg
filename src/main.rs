//! The `aarg` binary: parse arguments, dispatch into the library, and
//! render any error as a diagnostic. With no subcommand it drops into the
//! interactive REPL. All real behavior lives in the library crate so it
//! stays testable.

use aarg::cli::Cli;
use clap::Parser;

#[tokio::main]
async fn main() -> miette::Result<()> {
    match Cli::parse().command {
        Some(command) => aarg::commands::dispatch(command).await?,
        None => aarg::repl::run().await?,
    }
    Ok(())
}
