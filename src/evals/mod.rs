//! The keyless eval harness (FR-4.1).
//!
//! A reviewer can run `cargo run --bin evals` with no API key and no
//! network and see the tailoring pipeline and its never-fabricate guards
//! verified on representative model replies. Each case scripts a
//! `MockLlmClient` with a recorded-style reply, runs the real agent through
//! the real spine, and asserts something true about the assembled output.
//! This is a curated showcase, not the whole of the keyless coverage: every
//! agent also carries `MockLlmClient` tests in its own module, which
//! `cargo test` runs.
//!
//! **Why named assertions, not snapshots** (a deliberate choice): a snapshot
//! diff says *something* changed; a named check says *what* the guarantee is
//! and *how* it broke ("invented number reverted", "score clamped"). For a
//! project whose whole claim is a set of honesty guards, the assertion *is*
//! the documentation. It also keeps this a clean standalone binary with no
//! test-harness coupling, while a single `#[tokio::test]` re-runs the same
//! suite so CI fails on a regression too.

mod fixtures;
mod gap;
mod jd;
mod review;
mod tailor;

use crate::style;

/// The running tally as cases execute, plus a record of every failure for
/// the final summary and the process exit code.
pub struct Report {
    passed: usize,
    failed: Vec<String>,
}

impl Report {
    fn new() -> Self {
        Self {
            passed: 0,
            failed: Vec::new(),
        }
    }

    /// Record one case, printing a `✓`/`✗` line as it runs. `outcome` is
    /// `Ok(())` for a pass or `Err(reason)` describing what was wrong.
    pub fn check(&mut self, agent: &str, case: &str, outcome: Result<(), String>) {
        match outcome {
            Ok(()) => {
                self.passed += 1;
                eprintln!("{} {} {}", style::green("✓"), agent, style::dim(case));
            }
            Err(why) => {
                eprintln!("{} {} {}", style::red("✗"), agent, style::dim(case));
                eprintln!("    {}", style::dim(&why));
                self.failed.push(format!("{agent} · {case}: {why}"));
            }
        }
    }

    /// Whether every case so far passed.
    pub fn ok(&self) -> bool {
        self.failed.is_empty()
    }

    fn summary(&self) {
        let total = self.passed + self.failed.len();
        if self.ok() {
            eprintln!(
                "\n{}",
                style::done(style::bold(format!(
                    "{}/{total} eval cases passed",
                    self.passed
                )))
            );
        } else {
            eprintln!(
                "\n{} {}",
                style::red("✗"),
                style::bold(format!(
                    "{}/{total} passed, {} failed",
                    self.passed,
                    self.failed.len()
                ))
            );
        }
    }
}

/// Run every agent's eval cases keyless, print the report, and return
/// whether all passed (the bin turns this into the exit code).
pub async fn run_all() -> bool {
    let mut report = Report::new();
    jd::eval(&mut report).await;
    gap::eval(&mut report).await;
    tailor::eval(&mut report).await;
    review::eval(&mut report).await;
    report.summary();
    report.ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CI coverage: the same suite the bin runs must stay green under
    /// `cargo test`, so a regression fails the build, not just a manual run.
    #[tokio::test]
    async fn all_eval_cases_pass() {
        assert!(
            run_all().await,
            "an eval case failed — run `cargo run --bin evals`"
        );
    }
}
