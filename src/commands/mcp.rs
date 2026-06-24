//! `aarg mcp` — run AARG as a Model Context Protocol server over stdio.
//!
//! Deliberately thin: the whole server lives in `crate::mcp`, where it's
//! unit-testable without a live client. This is the command-layer shell that
//! the CLI dispatches to, mirroring `ping`/`ingest` and the rest.
//!
//! There is no human output here on success — the process *is* the transport,
//! and stdout belongs to the JSON-RPC stream. A connecting client (Claude
//! Desktop, Claude Code) speaks the protocol; a person who runs `aarg mcp` in
//! a terminal just sees the stderr "ready" log and can paste a JSON-RPC line
//! to poke it.

use crate::commands::CliError;

pub async fn run() -> Result<(), CliError> {
    crate::mcp::serve().await?;
    Ok(())
}
