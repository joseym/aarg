//! aarg-core — the agent runtime extracted from aarg's working
//! features, not designed ahead of them.
//!
//! Three layers, bottom up: `llm` (the `LlmClient` trait over
//! hand-rolled Anthropic and mock clients), `agent` (the `Agent` and
//! `Tool` traits and the spine that runs them: prompt assembly,
//! validation-retry, tool dispatch), and `trace` (one JSON record per
//! run, failures included).
//!
//! This crate is aarg's runtime, not a general framework: it keeps
//! aarg's conventions (storage locations, one-retry defaults) on
//! purpose. Generalization is earned the same way the runtime was —
//! by a second concrete consumer.

pub mod agent;
pub mod llm;
pub mod trace;
