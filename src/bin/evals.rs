//! `cargo run --bin evals` — the keyless eval harness (FR-4.1).
//!
//! Replays recorded-style model replies against `MockLlmClient` and checks
//! each agent's assembled output. No API key, no network. Exits non-zero if
//! any case fails, so it doubles as a CI-runnable gate.

#[tokio::main]
async fn main() {
    let ok = aarg::evals::run_all().await;
    std::process::exit(i32::from(!ok));
}
