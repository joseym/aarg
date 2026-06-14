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

use std::io::IsTerminal;
use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};
use owo_colors::{OwoColorize, Stream};

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
            let bar = ProgressBar::new_spinner();
            // An uncolored frame set, so the spinner respects NO_COLOR with
            // no special-casing. The template failing to parse is not worth
            // erroring a build over — fall back to the default style.
            if let Ok(style) = ProgressStyle::with_template("{spinner} {msg}") {
                bar.set_style(style.tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "));
            }
            bar.enable_steady_tick(Duration::from_millis(90));
            bar.set_message(message);
            Self { bar: Some(bar) }
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
