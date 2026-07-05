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

/// Gray text for secondary detail (paths, token counts, sub-items). Uses
/// bright-black rather than ANSI "faint" (code 2): faint is unevenly
/// supported and washes out to near-invisible on many themes, while
/// bright-black is a real color that stays legibly gray on both dark and
/// light backgrounds — secondary, but never lost.
pub fn dim(s: impl std::fmt::Display) -> String {
    s.if_supports_color(Stream::Stderr, |t| t.bright_black())
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

/// Blue for neutral, informational notes.
pub fn blue(s: impl std::fmt::Display) -> String {
    s.if_supports_color(Stream::Stderr, |t| t.blue())
        .to_string()
}

// ---- JSON content highlighting (stdout) ----------------------------------
//
// Unlike everything above, this colors content written to STDOUT (the agent
// I/O `trace show` prints), not the stderr chrome. That content is a
// documented contract: piping or redirecting must yield byte-clean, valid
// JSON (`aarg trace show | jq`, `> file`). So the color is gated on
// `Stream::Stdout` — emitted only when stdout is a real terminal, absent the
// moment it's piped — and string escaping is delegated to serde, so the
// result is valid JSON whether colored or not.

/// Pretty-print a JSON value with terminal-gated syntax highlighting: cyan
/// keys, green strings, yellow numbers, magenta booleans, dim null and
/// punctuation. Two-space indented. When stdout isn't a terminal the colors
/// drop out and the result is plain, valid, pretty JSON.
pub fn json(value: &serde_json::Value) -> String {
    let mut out = String::new();
    write_json(&mut out, value, 0);
    out
}

fn write_json(out: &mut String, value: &serde_json::Value, depth: usize) {
    use serde_json::Value;
    match value {
        Value::Null => out.push_str(&j_null("null")),
        Value::Bool(b) => out.push_str(&j_bool(&b.to_string())),
        Value::Number(n) => out.push_str(&j_number(&n.to_string())),
        Value::String(s) => out.push_str(&j_string(&quote(s))),
        Value::Array(items) if items.is_empty() => out.push_str(&j_punct("[]")),
        Value::Array(items) => {
            out.push_str(&j_punct("["));
            let last = items.len() - 1;
            for (i, item) in items.iter().enumerate() {
                out.push('\n');
                indent(out, depth + 1);
                write_json(out, item, depth + 1);
                if i != last {
                    out.push_str(&j_punct(","));
                }
            }
            out.push('\n');
            indent(out, depth);
            out.push_str(&j_punct("]"));
        }
        Value::Object(map) if map.is_empty() => out.push_str(&j_punct("{}")),
        Value::Object(map) => {
            out.push_str(&j_punct("{"));
            let last = map.len() - 1;
            for (i, (key, val)) in map.iter().enumerate() {
                out.push('\n');
                indent(out, depth + 1);
                out.push_str(&j_key(&quote(key)));
                out.push_str(&j_punct(": "));
                write_json(out, val, depth + 1);
                if i != last {
                    out.push_str(&j_punct(","));
                }
            }
            out.push('\n');
            indent(out, depth);
            out.push_str(&j_punct("}"));
        }
    }
}

/// JSON-quote and escape a string the way serde would, so colored output is
/// still valid JSON. Falls back to debug quoting on the (practically
/// impossible) serialization failure rather than panicking.
fn quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| format!("{s:?}"))
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn j_key(s: &str) -> String {
    s.if_supports_color(Stream::Stdout, |t| t.cyan())
        .to_string()
}
fn j_string(s: &str) -> String {
    s.if_supports_color(Stream::Stdout, |t| t.green())
        .to_string()
}
fn j_number(s: &str) -> String {
    s.if_supports_color(Stream::Stdout, |t| t.yellow())
        .to_string()
}
fn j_bool(s: &str) -> String {
    s.if_supports_color(Stream::Stdout, |t| t.magenta())
        .to_string()
}
fn j_null(s: &str) -> String {
    s.if_supports_color(Stream::Stdout, |t| t.bright_black())
        .to_string()
}
fn j_punct(s: &str) -> String {
    s.if_supports_color(Stream::Stdout, |t| t.bright_black())
        .to_string()
}

// ---- semantic vocabulary -------------------------------------------------
//
// One documented "meaning → glyph + color" set so every command speaks the
// same visual language. The rule that makes it accessible: the **glyph carries
// the meaning, color only reinforces it**. Output therefore still reads under
// `NO_COLOR`, when piped to a file, and for color-blind users — status is
// never encoded in color alone. The glyphs:
//
//   ✓  success · present · matched      (green)
//   ⚠  warning · "have it, off the page" (yellow)
//   ✗  miss · failure                   (red)
//   →  suggestion · next action          (cyan)
//   ℹ  note · neutral info               (blue)
//   ·  detail · list item                (dim)

/// ✓ A good outcome: a step finished, an item present, a skill matched.
pub fn success(text: impl std::fmt::Display) -> String {
    format!("{} {text}", green("✓"))
}

/// ⚠ A warning: something to notice but not a hard failure — a weak claim, a
/// skill you have that didn't reach the page, a non-fatal hiccup.
pub fn warn(text: impl std::fmt::Display) -> String {
    format!("{} {text}", yellow("⚠"))
}

/// ✗ A genuine miss or failure: a required keyword with no backing, an error.
pub fn fail(text: impl std::fmt::Display) -> String {
    format!("{} {text}", red("✗"))
}

/// → A suggestion or next action — "try this", "run that". The one helper for
/// telling the user what they can *do* next.
pub fn suggest(text: impl std::fmt::Display) -> String {
    format!("{} {text}", cyan("→"))
}

/// ℹ A neutral, informational note — context, not a problem.
pub fn info(text: impl std::fmt::Display) -> String {
    format!("{} {text}", blue("ℹ"))
}

/// · A list item or secondary detail. The dim bullet sets sub-items off from
/// the section header above them without shouting.
pub fn bullet(text: impl std::fmt::Display) -> String {
    format!("{} {text}", dim("·"))
}

/// A finished-step line: a green check and some text. Alias of [`success`],
/// kept for the many callers that already read `done("…")`.
pub fn done(text: impl std::fmt::Display) -> String {
    success(text)
}

/// Color a 0.0–1.0 quality figure by band so a column reads at a glance: a
/// strong value green, a middling one yellow, a weak one red. The caller hands
/// in the raw `ratio` to grade by and the already-formatted `text` to show, so
/// it works for `"0.81"`, `"81%"`, or `"score 0.81"` alike. Same rule as the
/// glyphs: the figure is always printed, color only grades it, so the value
/// survives `NO_COLOR` and a color-blind reader. Thresholds are presentation
/// only — nothing downstream reads them.
pub fn grade(ratio: f32, text: impl std::fmt::Display) -> String {
    if ratio >= 0.80 {
        green(text)
    } else if ratio >= 0.60 {
        yellow(text)
    } else {
        red(text)
    }
}

/// A section header: bold, with a blank line above so blocks of output
/// breathe. Returns the text (with the leading newline) for the caller to
/// print — keeping the vertical rhythm consistent across every command in one
/// place rather than each caller hand-spacing.
pub fn section(title: impl std::fmt::Display) -> String {
    format!("\n{}", bold(title))
}

/// An aligned `key: value` status line. The key is padded to `width` and
/// dimmed; the value is printed as given (the caller colors it if it carries
/// status). Padding is applied to the *plain* key before coloring, so ANSI
/// codes never throw the alignment off.
pub fn kv(key: &str, value: impl std::fmt::Display, width: usize) -> String {
    let label = format!("{key}:");
    format!("{}  {}", dim(format!("{label:<width$}")), value)
}

/// The stderr terminal's column count, for width-adaptive layouts (a list
/// that lays a row on one line when it fits and folds it when it wouldn't).
/// Returns `usize::MAX` when stderr isn't a terminal — piped, redirected, or
/// CI — because there's no margin to wrap against there, so the caller's
/// single-line form is the right one and logs stay stable instead of
/// reflowing to whatever width a capture buffer reports.
pub fn term_width() -> usize {
    if !std::io::stderr().is_terminal() {
        return usize::MAX;
    }
    // `.size()` is `(rows, cols)`; we want the columns.
    console::Term::stderr().size().1 as usize
}

/// The on-screen width of `s`, ignoring ANSI color codes and counting
/// double-width characters as two columns. The counterpart to [`term_width`]:
/// a *styled* line carries escape codes that `str::len` would miscount, so
/// measure it through here before deciding whether it fits the terminal.
pub fn display_width(s: &str) -> usize {
    console::measure_text_width(s)
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
    // `{wide_msg}` truncates the message to a single terminal line. `{msg}`
    // would let a long live-cost line wrap to two rows on a narrow terminal,
    // and indicatif's redraw only clears the one line it tracks — so each tick
    // left the wrapped remainder behind, stacking into garbled fragments.
    // Truncating to one line keeps every redraw cleanly erasable.
    if let Ok(style) = ProgressStyle::with_template("{spinner} {wide_msg}") {
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
    /// How the run is billed. Only a metered API key gets a live dollar
    /// estimate; a Claude plan covers the cost in its flat fee and a local
    /// model is free, so both show tokens only. Without this, a local model
    /// whose id happens to contain a priced family name (community merges
    /// named after opus or haiku exist) would show dollar figures mid-loop.
    billing: crate::config::Billing,
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
    pub fn new(prices: BTreeMap<String, Price>, billing: crate::config::Billing) -> Self {
        Self {
            prices,
            billing,
            state: Mutex::new(ReporterState::default()),
        }
    }

    /// The live line: `tailoring · ~1.2k tok · ~$0.18 so far`. The token
    /// count and the current call's cost are estimated from the streamed
    /// character count (real usage only lands at `end`); the running total
    /// folds in the real cost of completed calls. Everything is marked `~`
    /// (a budget signal rather than an invoice). Anything but a metered API key
    /// drops the dollar figure (a plan covers it, a local model is free),
    /// leaving tokens only.
    fn line(&self, st: &ReporterState) -> String {
        let est_tokens = st.chars / 4;
        let toks = dim(format!("~{} tok", fmt_tokens(est_tokens)));
        if self.billing != crate::config::Billing::Metered {
            return format!("{} · {}", st.label, toks);
        }
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
    fn json_highlight_stays_valid_json_matching_the_input() {
        // Tests run with stdout not a terminal, so `json` emits plain JSON.
        // The contract that matters: whatever it emits round-trips to the same
        // value (so a piped `trace show` stays machine-readable).
        let value = serde_json::json!({
            "name": "Ada \"Lovelace\"",
            "skills": ["Rust", "Go"],
            "years": 8,
            "remote": true,
            "manager": null,
            "meta": {},
            "tags": []
        });
        let rendered = json(&value);
        let back: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(back, value);
        // Pretty-printed (multi-line) and free of ANSI codes off a terminal.
        assert!(rendered.contains('\n'));
        assert!(!rendered.contains('\u{1b}'));
    }

    #[test]
    fn semantic_helpers_lead_with_their_glyph() {
        // Tests don't run on a TTY, so color is suppressed and the glyph
        // stands alone — which is exactly the guarantee: meaning survives
        // without color.
        assert_eq!(success("matched"), "✓ matched");
        assert_eq!(warn("weak"), "⚠ weak");
        assert_eq!(fail("missing"), "✗ missing");
        assert_eq!(suggest("try this"), "→ try this");
        assert_eq!(info("note"), "ℹ note");
        assert_eq!(bullet("item"), "· item");
    }

    #[test]
    fn done_is_the_success_line() {
        assert_eq!(done("rendered"), success("rendered"));
    }

    #[test]
    fn grade_bands_by_ratio_but_always_shows_the_text() {
        // Color is suppressed off a TTY, so only the text remains — proving
        // the band is reinforcement, never the only signal. Each ratio lands
        // in a different band (green ≥0.80, yellow ≥0.60, red below).
        assert_eq!(grade(0.91, "0.91"), "0.91");
        assert_eq!(grade(0.72, "72%"), "72%");
        assert_eq!(grade(0.40, "score 0.40"), "score 0.40");
    }

    #[test]
    fn a_section_header_breathes_above() {
        // A blank line precedes every section so blocks don't run together.
        assert_eq!(section("Tiers"), "\nTiers");
    }

    #[test]
    fn display_width_ignores_ansi_color_codes() {
        // Three visible characters wrapped in a bold escape sequence: the
        // codes must not count toward the measured width, or every
        // width-aware layout would fold too early on colored lines.
        assert_eq!(display_width("\x1b[1mabc\x1b[0m"), 3);
        assert_eq!(display_width("plain"), 5);
    }

    #[test]
    fn term_width_is_unbounded_off_a_terminal() {
        // The test harness's stderr is not a TTY, so there's no margin to
        // wrap against and the single-line form should always be chosen.
        assert_eq!(term_width(), usize::MAX);
    }

    #[test]
    fn kv_pads_the_key_column_on_the_plain_text() {
        // "model:" is 6 chars; padded to 8 then a two-space gap before the
        // value. Padding the plain label (not the colored one) keeps columns
        // aligned regardless of ANSI codes.
        assert_eq!(kv("model", "opus", 8), "model:    opus");
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
        let reporter = StreamReporter::new(BTreeMap::new(), crate::config::Billing::Metered);
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
        let reporter = StreamReporter::new(BTreeMap::new(), crate::config::Billing::Metered);
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
    fn a_subscription_shows_tokens_but_no_dollars_even_on_a_priced_model() {
        let reporter = StreamReporter::new(BTreeMap::new(), crate::config::Billing::Subscription);
        let st = ReporterState {
            bar: None,
            label: "tailoring".to_string(),
            model: "claude-sonnet-4-6".to_string(), // priced, but the plan covers it
            chars: 4000,
            spent: 0.10,
        };
        let line = reporter.line(&st);
        assert!(line.contains("~1.0k tok"));
        assert!(!line.contains("$"));
    }

    #[test]
    fn a_local_run_shows_tokens_but_no_dollars_even_on_a_priced_name() {
        // A local model id containing a priced family name (community merges
        // named after opus or haiku exist) must not show live dollar figures:
        // the run is free regardless of what the id resembles.
        let reporter = StreamReporter::new(BTreeMap::new(), crate::config::Billing::Local);
        let st = ReporterState {
            bar: None,
            label: "tailoring".to_string(),
            model: "some-haiku-merge-8b".to_string(),
            chars: 4000,
            spent: 0.0,
        };
        let line = reporter.line(&st);
        assert!(line.contains("~1.0k tok"));
        assert!(!line.contains("$"));
    }

    #[test]
    fn end_accrues_real_cost_into_the_running_total() {
        let reporter = StreamReporter::new(BTreeMap::new(), crate::config::Billing::Metered);
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
