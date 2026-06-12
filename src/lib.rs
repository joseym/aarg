//! AARG — The Adversarial Agentic Resume Generator.
//!
//! This library crate holds everything the `aarg` binary does; `main.rs`
//! is a thin shell that parses arguments and dispatches into here. Keeping
//! the logic in a library makes every module testable with `cargo test`.

pub mod cli;
pub mod commands;
pub mod config;
pub mod dataset;
pub mod ingest;
pub mod jd;
pub mod llm;
pub mod secrets;
