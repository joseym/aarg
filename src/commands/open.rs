//! `aarg open [build]` — open a build's rendered PDFs in the system viewer.
//!
//! The convenience bookend to `aarg export`: where `export` copies a build's
//! PDFs out under friendly names, `open` just shows them. It prints each PDF's
//! path (the durable, scriptable output) and, on a terminal, hands each to the
//! OS default opener. Read-only — it generates nothing and has no claim
//! surface; the same `pick_build` picker the rest of the build family uses
//! resolves a missing id.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::builds;
use crate::commands::CliError;
use crate::style;
use crate::terminal::auto_user;

/// The rendered PDFs a build may hold, in the order they're opened.
const PDFS: &[&str] = &["resume.ats.pdf", "resume.human.pdf", "cover_letter.pdf"];

pub async fn run(build: Option<String>) -> Result<(), CliError> {
    // Resolve which build: an explicit id is used as-is; with none we offer a
    // picker, and a piped/CI run gets a typed pointer instead of a hang.
    let build = match build {
        Some(id) => id,
        None => match super::pick_build("pick a build to open", "aarg open 029").await? {
            Some(id) => id,
            None => return Ok(()),
        },
    };
    let build_dir = builds::builds_root()?.join(&build);
    let pdfs = present_pdfs(&build_dir);

    if pdfs.is_empty() {
        eprintln!(
            "{}",
            style::warn(format!(
                "build {build} has no rendered PDFs · run `aarg render {build}` first"
            ))
        );
        return Ok(());
    }

    eprintln!("{}", style::section(format!("opening build {build}")));
    // Launch a GUI viewer only on a terminal; a piped/CI run just gets the
    // paths (printed below regardless), so it never spawns a window it can't use.
    let interactive = auto_user().is_interactive();
    for pdf in &pdfs {
        eprintln!("{}", style::success(pdf.display()));
        if interactive && let Err(note) = launch(pdf) {
            eprintln!("{}", style::warn(note));
        }
    }
    if !interactive {
        eprintln!(
            "{}",
            style::dim("not a terminal · printed the paths instead of opening a viewer")
        );
    }
    Ok(())
}

/// The build's rendered PDFs that actually exist, in [`PDFS`] order. A build
/// made with `--variant ats` has no human PDF; one never given `aarg cover`
/// has no cover letter — those are simply absent, not errors.
fn present_pdfs(build_dir: &Path) -> Vec<PathBuf> {
    PDFS.iter()
        .map(|name| build_dir.join(name))
        .filter(|path| path.is_file())
        .collect()
}

/// Hand one file to the OS default opener, detached (the viewer outlives this
/// process) and silenced (its own stdout/stderr don't clutter the terminal).
/// Returns a human note on failure — the path was already printed, so a missing
/// opener is advisory, not fatal.
fn launch(path: &Path) -> Result<(), String> {
    let (program, pre_args) = opener();
    let result = Command::new(program)
        .args(pre_args)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match result {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(format!(
            "no `{program}` on PATH · open the path above manually"
        )),
        Err(e) => Err(format!("could not launch `{program}`: {e}")),
    }
}

/// The OS default-opener program and any arguments that precede the file.
/// `cfg!` picks at compile time, so each platform's binary builds in.
fn opener() -> (&'static str, &'static [&'static str]) {
    if cfg!(target_os = "macos") {
        ("open", &[])
    } else if cfg!(target_os = "windows") {
        // `start` is a `cmd` builtin; the empty "" is its window-title argument,
        // so the path isn't mistaken for the title.
        ("cmd", &["/C", "start", ""])
    } else {
        ("xdg-open", &[])
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn present_pdfs_lists_only_existing_files_in_order() {
        let dir = tempfile::tempdir().unwrap();
        // ATS and cover rendered; the human variant was not.
        std::fs::write(dir.path().join("resume.ats.pdf"), b"%PDF").unwrap();
        std::fs::write(dir.path().join("cover_letter.pdf"), b"%PDF").unwrap();

        let found = present_pdfs(dir.path());

        let names: Vec<&str> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(names, vec!["resume.ats.pdf", "cover_letter.pdf"]);
    }

    #[test]
    fn present_pdfs_is_empty_when_nothing_rendered() {
        let dir = tempfile::tempdir().unwrap();
        assert!(present_pdfs(dir.path()).is_empty());
    }

    #[test]
    fn opener_is_xdg_open_on_linux() {
        // The CI/dev target here is Linux; the macOS/Windows arms are cfg-gated.
        if cfg!(target_os = "linux") {
            assert_eq!(opener().0, "xdg-open");
        }
    }
}
