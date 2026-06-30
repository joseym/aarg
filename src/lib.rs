//! AARG — The Adversarial Agentic Resume Generator.
//!
//! This library crate holds everything the `aarg` binary does; `main.rs`
//! is a thin shell that parses arguments and dispatches into here. Keeping
//! the logic in a library makes every module testable with `cargo test`.

// The agent runtime lives in its own crate (the Phase 2 split);
// re-exporting its modules keeps every `crate::agent::...` path in
// this crate working unchanged.
pub use aarg_core::{agent, llm, trace, user};

// The resume-tailoring domain lives in the portable `aarg-domain` crate (the
// wasm split); re-exporting its modules keeps every `crate::tailor::...` /
// `crate::gap::...` path in this crate working unchanged. `dataset` is a local
// shim (it adds on-disk persistence to the re-exported model).
pub use aarg_domain::{gap, jd, keywords, mirror, review, tailor, variant};

pub mod ats;
pub mod builds;
pub mod cli;
pub mod commands;
pub mod config;
pub mod cover;
pub mod dataset;
pub mod enrich;
pub mod evals;
pub mod fetch;
pub mod guide;
pub mod history;
pub mod ingest;
pub mod jdchat;
pub mod jdstore;
pub mod mcp;
pub mod metric;
pub mod pricing;
pub mod readability;
pub mod render;
pub mod repl;
pub mod secrets;
pub mod strengthen;
pub mod style;
pub mod summary;
pub mod templates;
pub mod terminal;
pub mod tune;
pub mod verify;
pub mod vision;
pub mod voice;
pub mod workspace;
