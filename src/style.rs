//! Terminal presentation: color and a progress spinner, both degrading
//! cleanly where they'd be noise.
//!
//! Two rules from the PRD's accessibility note (FR-6, §16): respect
//! `NO_COLOR`, and show no animations in CI or off a TTY. Both are handled
//! at the source here so callers don't have to think about it:
//!
//! - **Color** goes through `owo-colors`' `if_supports_color`, which checks
//!   `NO_COLOR`, the `TERM`, and whether the stream is a real terminal —
//!   so the color helpers emit ANSI codes interactively and plain text when
//!   piped or `NO_COLOR=1`. All of aarg's human output is on stderr, so the
//!   helpers target that stream; stdout carries no machine output to keep
//!   clean.
//! - **Spinners** animate only when stderr is a TTY and `CI` is unset.
//!   Otherwise `Spinner` prints a one-line "doing X…" and is silent until
//!   the caller finishes it — a clean log line, no escape codes.

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::sync::Mutex;
use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};
use owo_colors::{OwoColorize, Stream};

use crate::agent::StreamSink;
use crate::llm::TokenUsage;
use crate::pricing::{self, Price};

/// Dim text for secondary detail (paths, token counts, sub-items).
pub fn dim(s: impl std::fmt::Display) -> String {
    s.if_supports_color(Stream::Stderr, |t| t.dimmed())
        .to_string()
}

/// Bold text for headers.
pub fn bold(s: impl std::fmt::Display) -> String {
    s.if_supports_color(Stream::Stderr, |t| t.bold())
        .to_string()
}

/// Green for success and good outcomes.
pub fn green(s: impl std::fmt::Display) -> String {
    s.if_supports_color(Stream::Stderr, |t| t.green())
        .to_string()
}

/// Yellow for warnings and "you have it but it's off the page".
pub fn yellow(s: impl std::fmt::Display) -> String {
    s.if_supports_color(Stream::Stderr, |t| t.yellow())
        .to_string()
}

/// Red for a genuine miss or failure.
pub fn red(s: impl std::fmt::Display) -> String {
    s.if_supports_color(Stream::Stderr, |t| t.red()).to_string()
}

/// Cyan for scores and figures worth the eye.
pub fn cyan(s: impl std::fmt::Display) -> String {
    s.if_supports_color(Stream::Stderr, |t| t.cyan())
        .to_string()
}

/// A finished-step line: a green check and some text.
pub fn done(text: impl std::fmt::Display) -> String {
    format!("{} {text}", green("✓"))
}

/// Whether to animate: a real terminal, and not CI. `NO_COLOR` is about
/// color, not motion, so it doesn't suppress the spinner itself — but the
/// spinner frames carry no color, so a `NO_COLOR` run stays plain anyway.
fn spinners_enabled() -> bool {
    std::io::stderr().is_terminal() && std::env::var_os("CI").is_none()
}

/// A steady-ticking spinner bar with `message`. Shared by `Spinner` (a
/// fixed label) and `StreamReporter` (a label that updates as tokens
/// arrive). The uncolored frame set keeps `NO_COLOR` honest with no
/// special-casing; a template that fails to parse falls back to the
/// default style rather than erroring a build.
fn spinner_bar(message: String) -> ProgressBar {
    let bar = ProgressBar::new_spinner();
    if let Ok(style) = ProgressStyle::with_template("{spinner} {msg}") {
        bar.set_style(style.tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "));
    }
    bar.enable_steady_tick(Duration::from_millis(90));
    bar.set_message(message);
    bar
}

/// A spinner for one long, non-interactive wait (an LLM call, a render).
/// Animated on a TTY; a single log line otherwise. Must be finished before
/// any interactive prompt, so the animation never fights `inquire`.
pub struct Spinner {
    bar: Option<ProgressBar>,
}

impl Spinner {
    /// Begin a wait labelled `message`.
    pub fn start(message: impl Into<String>) -> Self {
        let message = message.into();
        if spinners_enabled() {
            Self {
                bar: Some(spinner_bar(message)),
            }
        } else {
            eprintln!("{}", dim(format!("{message}…")));
            Self { bar: None }
        }
    }

    /// Stop the animation and print `line` where it was.
    pub fn finish(self, line: impl std::fmt::Display) {
        if let Some(bar) = self.bar {
            bar.finish_and_clear();
        }
        eprintln!("{line}");
    }

    /// Stop the animation, leaving nothing behind — for when the caller
    /// prints the outcome itself (e.g. a result computed after the wait).
    pub fn clear(self) {
        if let Some(bar) = self.bar {
            bar.finish_and_clear();
        }
    }
}

/// Compact token counts for a status line: `840`, `1.2k`, `15.0k`.
fn fmt_tokens(n: usize) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// A short verb for the streaming line, from the agent's stable id. The
/// smart-tier agents get hand-picked words; anything else falls back to a
/// cleaned-up id, so a future streaming agent still reads sensibly.
fn human_label(agent_id: &str) -> String {
    match agent_id {
        "tailoring_v1" => "tailoring".to_string(),
        "adversarial_reviewer_v1" => "reviewing".to_string(),
        "voice_rewrite_v1" => "voicing".to_string(),
        other => other.trim_end_matches("_v1").replace('_', " "),
    }
}

/// A live status line for the streamed, smart-tier runs of the tailoring
/// loop: the model's output building as a token count, and a running cost
/// across the whole command — so a long call is visibly working and the
/// user can interrupt it (FR-3.8; CLAUDE.md non-negotiable #6).
///
/// It implements the runtime's `StreamSink`: the agent spine drives it
/// (`begin → delta* → end`), and here — in the binary, where the terminal
/// and the price tables live — is where it renders. Degrades like
/// `Spinner`: an animated bar on a TTY outside CI, a single log line
/// otherwise. One reporter serves a whole command, summing real cost at
/// each `end` so the line shows total spend, not just the current call.
pub struct StreamReporter {
    /// Per-model overrides from config; built-in family rates fill the gaps.
    prices: BTreeMap<String, Price>,
    state: Mutex<ReporterState>,
}

#[derive(Default)]
struct ReporterState {
    /// The live bar while a call streams. `None` between calls, and always
    /// `None` off a TTY (where `begin` logs a line instead).
    bar: Option<ProgressBar>,
    /// Human label for the current call (e.g. "tailoring").
    label: String,
    /// The model the current call streams on, for pricing its estimate.
    model: String,
    /// Characters streamed so far in the current call (~4 per token).
    chars: usize,
    /// Real cost of completed calls this command, summed at each `end`,
    /// over the models we can price.
    spent: f64,
}

impl StreamReporter {
    pub fn new(prices: BTreeMap<String, Price>) -> Self {
        Self {
            prices,
            state: Mutex::new(ReporterState::default()),
        }
    }

    /// The live line: `tailoring · ~1.2k tok · ~$0.18 so far`. The token
    /// count and the current call's cost are estimated from the streamed
    /// character count (real usage only lands at `end`); the running total
    /// folds in the real cost of completed calls. Everything is marked `~`
    /// — a budget signal, not an invoice.
    fn line(&self, st: &ReporterState) -> String {
        let est_tokens = st.chars / 4;
        let toks = dim(format!("~{} tok", fmt_tokens(est_tokens)));
        let est_cost = pricing::cost_usd(
            &st.model,
            &TokenUsage {
                input_tokens: 0,
                output_tokens: est_tokens as u64,
            },
            &self.prices,
        );
        match est_cost {
            Some(call) => format!(
                "{} · {} · {}",
                st.label,
                toks,
                cyan(format!("~${:.2} so far", st.spent + call))
            ),
            // Unpriced model (e.g. a local one): tokens only, never a
            // wrong dollar figure.
            None => format!("{} · {}", st.label, toks),
        }
    }
}

impl StreamSink for StreamReporter {
    fn begin(&self, agent_id: &str, model: &str) {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        st.label = human_label(agent_id);
        st.model = model.to_string();
        st.chars = 0;
        if spinners_enabled() {
            let line = self.line(&st);
            st.bar = Some(spinner_bar(line));
        } else {
            eprintln!("{}", dim(format!("{}…", st.label)));
            st.bar = None;
        }
    }

    fn delta(&self, chunk: &str) {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        st.chars += chunk.chars().count();
        if st.bar.is_some() {
            let line = self.line(&st);
            if let Some(bar) = st.bar.as_ref() {
                bar.set_message(line);
            }
        }
    }

    fn end(&self, usage: TokenUsage) {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(cost) = pricing::cost_usd(&st.model, &usage, &self.prices) {
            st.spent += cost;
        }
        if let Some(bar) = st.bar.take() {
            bar.finish_and_clear();
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn token_counts_format_compactly() {
        assert_eq!(fmt_tokens(840), "840");
        assert_eq!(fmt_tokens(1200), "1.2k");
        assert_eq!(fmt_tokens(15000), "15.0k");
    }

    #[test]
    fn agent_ids_map_to_short_verbs() {
        assert_eq!(human_label("tailoring_v1"), "tailoring");
        assert_eq!(human_label("adversarial_reviewer_v1"), "reviewing");
        assert_eq!(human_label("voice_rewrite_v1"), "voicing");
        // Fallback: an unmapped id is cleaned, not shown raw.
        assert_eq!(human_label("metric_interview_v1"), "metric interview");
    }

    #[test]
    fn the_live_line_shows_tokens_and_a_running_cost() {
        let reporter = StreamReporter::new(BTreeMap::new());
        // Mid-stream on a priced model: ~4k chars ≈ 1k tokens.
        let st = ReporterState {
            bar: None,
            label: "tailoring".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            chars: 4000,
            spent: 0.10,
        };
        let line = reporter.line(&st);
        assert!(line.contains("tailoring"));
        assert!(line.contains("~1.0k tok"));
        assert!(line.contains("~$"));
    }

    #[test]
    fn an_unpriced_model_shows_tokens_but_no_dollars() {
        let reporter = StreamReporter::new(BTreeMap::new());
        let st = ReporterState {
            bar: None,
            label: "tailoring".to_string(),
            model: "some-local-llama".to_string(),
            chars: 4000,
            spent: 0.0,
        };
        let line = reporter.line(&st);
        assert!(line.contains("~1.0k tok"));
        assert!(!line.contains("$"));
    }

    #[test]
    fn end_accrues_real_cost_into_the_running_total() {
        let reporter = StreamReporter::new(BTreeMap::new());
        reporter.begin("tailoring_v1", "claude-sonnet-4-6");
        reporter.delta("some streamed text");
        // 1M output tokens on Sonnet = $15.
        reporter.end(TokenUsage {
            input_tokens: 0,
            output_tokens: 1_000_000,
        });
        let st = reporter.state.lock().unwrap();
        assert!((st.spent - 15.0).abs() < 1e-9);
        // The bar (if any) is torn down at end.
        assert!(st.bar.is_none());
    }
}
