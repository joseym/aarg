//! `aarg voice add|list` — capture and review the writing samples that
//! anchor voice rewrites (FR-3.3).
//!
//! A sample is just a piece of the user's own prose; the `VoiceRewriteAgent`
//! reads them to steer flagged resume lines toward how the person
//! actually writes. `add` reads the sample from stdin (so a file pipes in
//! cleanly, and an interactive paste-then-Ctrl-D works too); `list` shows
//! what's captured. Both are thin glue over the dataset store.

use std::io::{IsTerminal, Read};

use chrono::Utc;

use crate::commands::CliError;
use crate::dataset::store;
use crate::dataset::types::{SampleId, VoiceSample};

/// Capture a writing sample and append it to the dataset. Interactively
/// (a terminal with `$EDITOR` set) this opens an editor — write, save,
/// quit; otherwise it reads stdin, so `aarg voice add < sample.txt` and
/// scripted input still work.
pub async fn add(context: Option<String>) -> Result<(), CliError> {
    let interactive = std::io::stdin().is_terminal();
    let text = if interactive && crate::commands::editor_available() {
        read_via_editor()?
    } else {
        read_via_stdin(interactive)?
    };
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err(CliError::EmptyVoiceSample);
    }

    let mut dataset = store::load()?;
    let id = next_sample_id(&dataset);
    let chars = text.chars().count();
    dataset.voice_samples.push(VoiceSample {
        id: id.clone(),
        text,
        captured_at: Utc::now(),
        context: context.filter(|c| !c.trim().is_empty()),
    });
    dataset.metadata.updated_at = Utc::now();
    store::save(&dataset)?;

    println!(
        "captured {} ({chars} chars) · {} sample(s) total (previous version backed up)",
        id.0,
        dataset.voice_samples.len()
    );
    Ok(())
}

/// The instructional header an editor capture opens with. Stripped
/// before saving (git-commit style), so the user can write below it.
const EDITOR_TEMPLATE: &str = "\
# Write or paste a writing sample below, then save and quit.
# Lines in this leading block (starting with #) are ignored.

";

/// Open a scratch file in the user's editor, then read back the sample.
fn read_via_editor() -> Result<String, CliError> {
    let path = store::dir()?.join("voice.add.txt");
    std::fs::write(&path, EDITOR_TEMPLATE).map_err(|source| CliError::ReadInput {
        path: path.clone(),
        source,
    })?;
    crate::commands::launch_editor(&path)?;
    let raw = std::fs::read_to_string(&path).map_err(|source| CliError::ReadInput {
        path: path.clone(),
        source,
    })?;
    let _ = std::fs::remove_file(&path);
    Ok(strip_header(&raw))
}

/// Read a sample from stdin (a piped file, or an interactive paste ended
/// with Ctrl-D). The hint is the fix for the original "I pasted and
/// nothing happened" — stdin returns on EOF, not Enter.
fn read_via_stdin(interactive: bool) -> Result<String, CliError> {
    if interactive {
        eprintln!("Type or paste your sample, then press Ctrl-D on a blank line to finish:");
    }
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .map_err(|source| CliError::ReadInput {
            path: "<stdin>".into(),
            source,
        })?;
    Ok(text)
}

/// Drop the leading comment header (the template), keeping the body
/// verbatim — including any `#` lines the *sample itself* contains.
fn strip_header(text: &str) -> String {
    match text
        .lines()
        .position(|line| !line.trim_start().starts_with('#') && !line.trim().is_empty())
    {
        Some(start) => text.lines().skip(start).collect::<Vec<_>>().join("\n"),
        None => String::new(), // nothing but the header / blanks
    }
}

/// Print every captured sample: id, source label, and a one-line preview.
pub async fn list() -> Result<(), CliError> {
    let dataset = store::load()?;
    if dataset.voice_samples.is_empty() {
        println!("no voice samples yet — add one with `aarg voice add < sample.txt`");
        return Ok(());
    }
    for sample in &dataset.voice_samples {
        let label = sample.context.as_deref().unwrap_or("—");
        println!("{}  [{label}]  {}", sample.id.0, preview(&sample.text));
    }
    Ok(())
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
    fn strip_header_drops_the_template_but_keeps_body_hashes() {
        let raw = "# instructions\n# more\n\nMy real sample.\n# a heading I wrote\nmore text";
        assert_eq!(
            strip_header(raw),
            "My real sample.\n# a heading I wrote\nmore text"
        );
        // A file that is only the header yields nothing.
        assert_eq!(strip_header("# only\n# comments\n\n"), "");
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
