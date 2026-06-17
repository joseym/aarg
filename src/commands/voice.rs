//! `aarg voice add|list` — capture and review the writing samples that
//! anchor voice rewrites (FR-3.3).
//!
//! A sample is just a piece of the user's own prose; the `VoiceRewriteAgent`
//! reads them to steer flagged resume lines toward how the person
//! actually writes. `add` reads the sample from stdin (so a file pipes in
//! cleanly, and an interactive paste-then-Ctrl-D works too); `list` shows
//! what's captured. Both are thin glue over the dataset store.

use std::io::IsTerminal;

use chrono::Utc;
use inquire::Confirm;

use crate::commands::CliError;
use crate::dataset::store;
use crate::dataset::types::{ResumeDataset, SampleId, VoiceSample};
use crate::style;

/// Capture a writing sample and append it to the dataset. Interactively
/// (a terminal with `$EDITOR` set) this opens an editor — write, save,
/// quit; otherwise it reads stdin, so `aarg voice add < sample.txt` and
/// scripted input still work.
pub async fn add(context: Option<String>) -> Result<(), CliError> {
    let mut dataset = store::load()?;
    // An explicit `voice add` that yields nothing is a user error.
    let Some(id) = capture_into(&mut dataset, context)? else {
        return Err(CliError::EmptyVoiceSample);
    };
    store::save(&dataset)?;
    let chars = dataset
        .voice_samples
        .last()
        .map(|sample| sample.text.chars().count())
        .unwrap_or(0);
    eprintln!(
        "{}",
        style::success(format!(
            "captured {} ({chars} chars) · {} sample(s) total (previous version backed up)",
            id.0,
            dataset.voice_samples.len()
        ))
    );
    Ok(())
}

/// Read one writing sample (an editor when interactive with `$EDITOR`, else
/// stdin) and append it to `dataset`, returning the new sample's id.
/// `Ok(None)` means nothing was provided — the caller decides whether a
/// blank sample is an error (`add`) or simply nothing to do (onboarding).
/// Shared by `aarg voice add` and the onboarding offer below; the
/// editor-or-stdin capture itself lives in `commands::capture_free_text`
/// (also used by the JD paste flow).
fn capture_into(
    dataset: &mut ResumeDataset,
    context: Option<String>,
) -> Result<Option<SampleId>, CliError> {
    let text = crate::commands::capture_free_text(
        "voice.add.txt",
        EDITOR_TEMPLATE,
        "Type or paste your sample, then press Ctrl-D on a blank line to finish:",
    )?;
    if text.is_empty() {
        return Ok(None);
    }
    let id = next_sample_id(dataset);
    dataset.voice_samples.push(VoiceSample {
        id: id.clone(),
        text,
        captured_at: Utc::now(),
        context: context.filter(|c| !c.trim().is_empty()),
    });
    dataset.metadata.updated_at = Utc::now();
    Ok(Some(id))
}

/// Offer to capture a writing sample at onboarding (after `ingest`), so the
/// voice rewrite has an anchor from the first build. Interactive only: a
/// piped or CI run skips silently (the sample can be added later with
/// `aarg voice add`), so onboarding never blocks a script. Returns whether
/// a sample was added, so the caller knows to persist the dataset again.
pub(crate) fn offer_onboarding_sample(dataset: &mut ResumeDataset) -> Result<bool, CliError> {
    if !std::io::stdin().is_terminal() {
        return Ok(false);
    }
    let proceed = Confirm::new("Capture a writing voice sample now?")
        .with_help_message(
            "anchors voice rewrites to your own style; add more later with `aarg voice add`",
        )
        .with_default(true)
        .prompt()?;
    if !proceed {
        eprintln!(
            "{}",
            style::info("skipped · add one anytime with `aarg voice add`")
        );
        return Ok(false);
    }
    match capture_into(dataset, Some("onboarding".to_string()))? {
        Some(id) => {
            eprintln!(
                "{}",
                style::success(format!(
                    "captured {} · {} sample(s) total",
                    id.0,
                    dataset.voice_samples.len()
                ))
            );
            Ok(true)
        }
        None => {
            eprintln!(
                "{}",
                style::info("nothing captured · add one anytime with `aarg voice add`")
            );
            Ok(false)
        }
    }
}

/// The instructional header an editor capture opens with. Stripped
/// before saving (git-commit style), so the user can write below it.
const EDITOR_TEMPLATE: &str = "\
# Write or paste a writing sample below, then save and quit.
# Lines in this leading block (starting with #) are ignored.

";

/// Print every captured sample: id, source label, and a one-line preview.
pub async fn list() -> Result<(), CliError> {
    let dataset = store::load()?;
    if dataset.voice_samples.is_empty() {
        eprintln!(
            "{}",
            style::suggest("no voice samples yet · add one with `aarg voice add < sample.txt`")
        );
        return Ok(());
    }
    eprintln!(
        "{}",
        style::section(format!("Voice samples ({})", dataset.voice_samples.len()))
    );
    for sample in &dataset.voice_samples {
        let label = sample.context.as_deref().unwrap_or("-");
        eprintln!(
            "  {}",
            style::bullet(format!(
                "{}  [{label}]  {}",
                sample.id.0,
                preview(&sample.text)
            ))
        );
    }
    Ok(())
}

/// Remove a sample by id; errors if no sample carries that id.
pub async fn remove(id: String) -> Result<(), CliError> {
    let mut dataset = store::load()?;
    if !remove_sample(&mut dataset, &id) {
        return Err(CliError::VoiceSampleNotFound { id });
    }
    dataset.metadata.updated_at = Utc::now();
    store::save(&dataset)?;
    eprintln!(
        "{}",
        style::success(format!(
            "removed {id} · {} sample(s) remaining (previous version backed up)",
            dataset.voice_samples.len()
        ))
    );
    Ok(())
}

/// Drop the sample with this id from the dataset; returns whether one was
/// actually removed (so the caller can distinguish a bad id from a no-op).
fn remove_sample(dataset: &mut crate::dataset::types::ResumeDataset, id: &str) -> bool {
    let before = dataset.voice_samples.len();
    dataset.voice_samples.retain(|sample| sample.id.0 != id);
    dataset.voice_samples.len() != before
}

/// A single-line, length-capped preview: collapse whitespace, then clip.
fn preview(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX: usize = 70;
    if flat.chars().count() > MAX {
        let clipped: String = flat.chars().take(MAX - 1).collect();
        format!("{clipped}…")
    } else {
        flat
    }
}

/// New sample IDs continue the `sample-N` sequence.
fn next_sample_id(dataset: &crate::dataset::types::ResumeDataset) -> SampleId {
    let highest = dataset
        .voice_samples
        .iter()
        .filter_map(|s| s.id.0.strip_prefix("sample-")?.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    SampleId(format!("sample-{}", highest + 1))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::dataset::types::{Contact, ResumeDataset};

    fn empty_dataset() -> ResumeDataset {
        ResumeDataset::new(Contact {
            full_name: "Ada".into(),
            email: "ada@example.com".into(),
            phone: None,
            location: None,
            links: Vec::new(),
        })
    }

    #[test]
    fn sample_ids_continue_the_sequence() {
        let mut dataset = empty_dataset();
        assert_eq!(next_sample_id(&dataset), SampleId("sample-1".into()));
        dataset.voice_samples.push(VoiceSample {
            id: SampleId("sample-4".into()),
            text: "x".into(),
            captured_at: Utc::now(),
            context: None,
        });
        assert_eq!(next_sample_id(&dataset), SampleId("sample-5".into()));
    }

    #[test]
    fn onboarding_offer_is_a_noop_without_a_terminal() {
        // Under `cargo test` stdin is not a tty, so the offer must skip
        // cleanly and leave the dataset untouched — onboarding never blocks
        // a piped or CI run.
        let mut dataset = empty_dataset();
        assert!(!offer_onboarding_sample(&mut dataset).unwrap());
        assert!(dataset.voice_samples.is_empty());
    }

    #[test]
    fn remove_sample_drops_the_match_and_reports_whether_it_did() {
        let mut dataset = empty_dataset();
        for id in ["sample-1", "sample-2"] {
            dataset.voice_samples.push(VoiceSample {
                id: SampleId(id.into()),
                text: "x".into(),
                captured_at: Utc::now(),
                context: None,
            });
        }
        assert!(remove_sample(&mut dataset, "sample-1"));
        assert_eq!(dataset.voice_samples.len(), 1);
        assert_eq!(dataset.voice_samples[0].id, SampleId("sample-2".into()));
        // A miss changes nothing and reports false.
        assert!(!remove_sample(&mut dataset, "sample-9"));
        assert_eq!(dataset.voice_samples.len(), 1);
    }

    #[test]
    fn preview_collapses_whitespace_and_clips_long_text() {
        assert_eq!(preview("  hello\n  world  "), "hello world");
        let long = "word ".repeat(40);
        let p = preview(&long);
        assert!(p.chars().count() <= 70);
        assert!(p.ends_with('…'));
    }
}
