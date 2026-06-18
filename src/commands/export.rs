//! `aarg export [build] [--to <dir>]` — copy a build's rendered PDFs out of
//! the build directory under friendly, company-named filenames ready to
//! attach to an application.
//!
//! The build directory is the canonical, reproducible archive of a run (it's
//! what `render`, `history`, and `diff` read back), so export *copies* rather
//! than moves: the originals stay put with their machine names
//! (`resume.ats.pdf`), and the user gets `acme.ats.pdf` wherever they want it.
//! Pure file movement — no model call, no claim surface; whatever was already
//! rendered is what gets copied.

use std::path::{Path, PathBuf};

use crate::builds;
use crate::commands::CliError;
use crate::config::Config;
use crate::jd::JobRequirements;
use crate::style;
use crate::tailor::TailoredResume;
use crate::terminal::auto_user;
use crate::user::UserHandle;

/// The build artifacts export knows about, paired with the friendly suffix
/// each gets in the destination (`resume.ats.pdf` -> `<company>.ats.pdf`).
const EXPORTS: &[(&str, &str)] = &[
    ("resume.ats.pdf", "ats"),
    ("resume.human.pdf", "human"),
    ("cover_letter.pdf", "cover"),
];

/// What happened to one candidate file, so `run` can report it and total up.
enum Outcome {
    /// Copied to the destination (a new file, or an approved overwrite).
    Written(PathBuf),
    /// Already there and the user declined to overwrite — left untouched.
    Skipped(PathBuf),
}

pub async fn run(build: Option<String>, to: Option<PathBuf>) -> Result<(), CliError> {
    // Resolve which build: an explicit id is used as-is; with none we offer a
    // picker, and a piped/CI run gets a typed pointer instead of a hang.
    let build = match build {
        Some(id) => id,
        None => match super::pick_build("pick a build to export", "aarg export 029").await? {
            Some(id) => id,
            None => return Ok(()),
        },
    };
    let build_dir = builds::builds_root()?.join(&build);

    // Destination: --to wins, then the configured dir, then the cwd. Create it
    // so a fresh `export.dir` (or a nested `--to`) just works.
    let dest = resolve_dest(to)?;
    std::fs::create_dir_all(&dest).map_err(|source| CliError::ExportDir {
        path: dest.clone(),
        source,
    })?;
    let base = export_base(&build);

    eprintln!(
        "{}",
        style::section(format!("exporting build {build} to {}", dest.display()))
    );

    let user = auto_user();
    let outcomes = copy_exports(&build_dir, &base, &dest, user.as_ref()).await?;

    let mut written = 0usize;
    for outcome in &outcomes {
        match outcome {
            Outcome::Written(path) => {
                eprintln!("{}", style::success(path.display()));
                written += 1;
            }
            Outcome::Skipped(path) => {
                eprintln!(
                    "{}",
                    style::info(format!("kept existing {}", path.display()))
                );
            }
        }
    }

    if outcomes.is_empty() {
        // Every build renders at least one variant, so an empty set means the
        // PDFs were cleared (or never built) — point at how to make them.
        eprintln!(
            "{}",
            style::warn(format!(
                "build {build} has no rendered PDFs · run `aarg render {build}` first"
            ))
        );
    } else if written > 0 {
        eprintln!(
            "\n{}",
            style::done(style::bold(format!(
                "exported {written} file(s) to {}",
                dest.display()
            )))
        );
    }
    // (All-skipped prints nothing more: the per-file "kept existing" lines said it.)
    Ok(())
}

/// Copy each present export from `build_dir` into `dest` under `<base>.<suffix>.pdf`.
/// A variant that never rendered is silently absent (not every build makes
/// every PDF); an existing target is only overwritten with the user's say-so.
/// Split from `run` so the copy/overwrite logic is testable with a tempdir and
/// a `ScriptedUser`, without a real build or keychain.
async fn copy_exports(
    build_dir: &Path,
    base: &str,
    dest: &Path,
    user: &dyn UserHandle,
) -> Result<Vec<Outcome>, CliError> {
    let mut outcomes = Vec::new();
    for (artifact, suffix) in EXPORTS {
        let src = build_dir.join(artifact);
        if !src.is_file() {
            continue;
        }
        let target = dest.join(format!("{base}.{suffix}.pdf"));
        // Don't clobber unasked: confirm an overwrite (default yes, so a
        // re-export refreshes and a piped run follows that default).
        if target.exists()
            && !user
                .confirm(&format!("{} exists; overwrite?", target.display()), true)
                .await?
        {
            outcomes.push(Outcome::Skipped(target));
            continue;
        }
        std::fs::copy(&src, &target).map_err(|source| CliError::ExportCopy {
            from: src.clone(),
            to: target.clone(),
            source,
        })?;
        outcomes.push(Outcome::Written(target));
    }
    Ok(outcomes)
}

/// Where to export to: the explicit `--to`, else the configured `export.dir`,
/// else the current directory (so the feature works with no setup).
fn resolve_dest(to: Option<PathBuf>) -> Result<PathBuf, CliError> {
    if let Some(dir) = to {
        return Ok(dir);
    }
    if let Some(dir) = Config::load()?.export.dir {
        return Ok(dir);
    }
    std::env::current_dir().map_err(CliError::CurrentDir)
}

/// The friendly filename stem for a build: the company it targeted, slugged.
/// Prefer the JD's company; fall back to the canonical draft's jd slug, then
/// the build id, so an older build with no `jd.json` still names sensibly.
fn export_base(build: &str) -> String {
    if let Ok(jd) = crate::history::read_artifact::<JobRequirements>(build, "jd.json") {
        let slug = slugify(&jd.company);
        if !slug.is_empty() {
            return slug;
        }
    }
    if let Ok(resume) = crate::history::read_artifact::<TailoredResume>(build, "canonical.json") {
        let slug = slugify(&resume.jd_id.0);
        if !slug.is_empty() {
            return slug;
        }
    }
    format!("build-{build}")
}

/// Lowercase, hyphen-separated, filesystem-safe stem of a label. Mirrors the
/// `slug` in `commands::tailor` (which joins company+title); kept separate
/// because it's six trivial lines and coupling two command modules to share
/// them buys nothing.
fn slugify(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::user::ScriptedUser;

    fn touch(path: &Path, contents: &[u8]) {
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn slugify_makes_a_filesystem_safe_company_stem() {
        assert_eq!(slugify("Amplo, Inc."), "amplo-inc");
        assert_eq!(slugify("ACME  Corp!"), "acme-corp");
        assert_eq!(slugify(""), "");
    }

    #[tokio::test]
    async fn copies_only_the_pdfs_that_exist_under_friendly_names() {
        let build = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        // ATS and cover rendered; the human variant was not.
        touch(&build.path().join("resume.ats.pdf"), b"%PDF ats");
        touch(&build.path().join("cover_letter.pdf"), b"%PDF cover");

        let user = ScriptedUser::new();
        let outcomes = copy_exports(build.path(), "acme", dest.path(), &user)
            .await
            .unwrap();

        assert_eq!(outcomes.len(), 2);
        assert!(dest.path().join("acme.ats.pdf").is_file());
        assert!(dest.path().join("acme.cover.pdf").is_file());
        // The variant that never rendered is simply absent, not an error.
        assert!(!dest.path().join("acme.human.pdf").exists());
    }

    #[tokio::test]
    async fn an_existing_target_is_kept_when_overwrite_is_declined() {
        let build = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        touch(&build.path().join("resume.ats.pdf"), b"new");
        // A file already sits at the destination name.
        let target = dest.path().join("acme.ats.pdf");
        touch(&target, b"old");

        let user = ScriptedUser::new();
        user.confirm_with(false); // decline the overwrite
        let outcomes = copy_exports(build.path(), "acme", dest.path(), &user)
            .await
            .unwrap();

        assert!(matches!(outcomes.as_slice(), [Outcome::Skipped(_)]));
        // The original file is untouched.
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "old");
    }

    #[tokio::test]
    async fn an_existing_target_is_overwritten_when_confirmed() {
        let build = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        touch(&build.path().join("resume.ats.pdf"), b"new");
        let target = dest.path().join("acme.ats.pdf");
        touch(&target, b"old");

        let user = ScriptedUser::new();
        user.confirm_with(true); // approve the overwrite
        let outcomes = copy_exports(build.path(), "acme", dest.path(), &user)
            .await
            .unwrap();

        assert!(matches!(outcomes.as_slice(), [Outcome::Written(_)]));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
    }
}
