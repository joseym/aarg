//! AARG — The Adversarial Agentic Resume Generator.
//!
//! This library crate holds everything the `aarg` binary does; `main.rs`
//! is a thin shell that parses arguments and dispatches into here. Keeping
//! the logic in a library makes every module testable with `cargo test`.

// The agent runtime lives in its own crate (the Phase 2 split);
// re-exporting its modules keeps every `crate::agent::...` path in
// this crate working unchanged.
pub use aarg_core::{agent, llm, trace, user};

pub mod ats;
pub mod builds;
pub mod cli;
pub mod commands;
pub mod config;
pub mod dataset;
pub mod enrich;
pub mod fetch;
pub mod gap;
pub mod guide;
pub mod history;
pub mod ingest;
pub mod jd;
pub mod keywords;
pub mod metric;
pub mod mirror;
pub mod pricing;
pub mod render;
pub mod review;
pub mod secrets;
pub mod strengthen;
pub mod style;
pub mod tailor;
pub mod terminal;
pub mod verify;
pub mod voice;
