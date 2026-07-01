//! aarg-domain — AARG's resume-tailoring domain logic, portable over the
//! `aarg-core` runtime.
//!
//! This is the pipeline that turns a job description and a dataset of real
//! experience into a tailored, non-fabricated resume: `dataset` (the data
//! model and its validation), `jd` (parse a posting into requirements), `gap`
//! (what the dataset covers and misses), `tailor` (the canonical draft, with
//! the never-fabricate guards), `review` (the adversarial reviewer),
//! `variant` (project the canonical draft into per-template payloads), and the
//! `mirror`/`keywords` services.
//!
//! Every module here is pure: it transforms data and calls out through the
//! `aarg-core` traits (`LlmClient`, `UserHandle`, `StreamSink`, `Tracer`). It
//! opens no files, spawns no processes, and makes no network calls directly —
//! which is what lets it compile to `wasm32` and run in a browser over a
//! host-provided client. The native shell (storage, rendering, the CLI) stays
//! in the `aarg` binary crate, which consumes this one.

// Re-export the runtime modules so every `crate::agent::...` / `crate::llm::...`
// / `crate::trace::...` path inside this crate's modules resolves unchanged
// after the extraction from the binary crate (the binary re-exports the same
// way).
pub use aarg_core::{agent, llm, trace, user};

pub mod dataset;
pub mod enrich;
pub mod gap;
pub mod jd;
pub mod keywords;
pub mod metric;
pub mod mirror;
pub mod provenance;
pub mod review;
pub mod strengthen;
pub mod summary;
pub mod tailor;
pub mod variant;
