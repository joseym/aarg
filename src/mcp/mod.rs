//! A hand-rolled MCP (Model Context Protocol) server over stdio.
//!
//! This is the `LlmClient` move applied once more: rather than adopt the
//! `rmcp` SDK, AARG implements the slice of MCP it needs directly — the same
//! decision, and for the same reason, as the hand-written `reqwest` clients.
//! MCP over stdio is JSON-RPC 2.0 with newline-delimited messages, and a
//! tools-only server is a small, legible surface:
//!
//! - [`protocol`] — the wire types (`serde` structs), public so the rest of
//!   the codebase and tests can speak them.
//! - [`server`] — the read/dispatch/write loop and the lifecycle handshake.
//! - [`tools`] — the tool registry: thin adapters over the same library
//!   service functions the CLI commands call.
//!
//! The server exposes AARG's capabilities to any MCP client (Claude Desktop,
//! Claude Code, ...): read the dataset and past builds, parse a job
//! description, analyze the fit gap, tailor a resume through the adversarial
//! loop, and (re)ingest a resume. It runs non-interactively — the interactive
//! copilots are gated on a real terminal, which a server doesn't have — so
//! the never-fabricate guards inside the reused pipeline carry over unchanged.

mod client;
pub mod protocol;
mod server;
mod tools;

pub use server::serve;

/// What the MCP server can fail with. The transport is the only failure mode
/// the loop itself owns — an IO error reading stdin or writing stdout; tool
/// failures are reported in-band as part of a normal response, never here.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("the MCP stdio transport failed")]
    Transport(#[from] std::io::Error),
}
