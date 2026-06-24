//! Rendering a variant payload to a PDF by shelling out to the `typst`
//! binary (FR-1.6 / FR-5.2, PRD §9.2).
//!
//! Everything typst needs is staged *inside the build directory*: the
//! payload JSON, a copy of the template, and the shared template library.
//! That keeps typst's project root simple (a template resolves
//! `json(sys.inputs.data)` and `#import "aarg-template-lib.typ"` relative to
//! itself, and typst refuses paths outside its root), and it makes every
//! build self-documenting — the exact template that produced the PDF sits
//! next to it.
//!
//! The payload is passed **by path**, never inlined into the command line:
//! real resumes would blow past OS argument-length limits and invite
//! shell-escaping bugs.
//!
//! A template is a `Template`: a built-in embedded at compile time (the
//! zero-setup default) or a user-supplied `.typ` file. Both are staged the
//! same way, so a custom layout is a first-class input — the CLI surface to
//! point at one lands in Phase 6, but the renderer already handles it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::cover::CoverLetter;
use crate::variant::{Variant, VariantPayload};

/// The shipped templates and the shared library, embedded at compile time so
/// a fresh install renders with zero setup.
const ATS_TEMPLATE: &str = include_str!("../templates/ats/classic.typ");
const ATS_MINIMAL_TEMPLATE: &str = include_str!("../templates/ats/minimal.typ");
const HUMAN_TEMPLATE: &str = include_str!("../templates/human/modern.typ");
const HUMAN_TECHNICAL_TEMPLATE: &str = include_str!("../templates/human/technical.typ");
const HUMAN_EDITORIAL_TEMPLATE: &str = include_str!("../templates/human/editorial.typ");
const COVER_TEMPLATE: &str = include_str!("../templates/cover/standard.typ");
const SHARED_LIB: &str = include_str!("../templates/_shared/aarg-template-lib.typ");

/// The shared library's staged filename — templates import it by this name.
const SHARED_LIB_NAME: &str = "aarg-template-lib.typ";

/// A resolvable template: a built-in (embedded) or a user-supplied file.
#[derive(Debug)]
pub enum Template {
    Builtin {
        /// The filename to stage it under (templates import each other by name).
        filename: &'static str,
        source: &'static str,
    },
    User(PathBuf),
}

impl Template {
    /// The built-in ATS template ("classic").
    pub fn ats() -> Self {
        Template::Builtin {
            filename: "classic.typ",
            source: ATS_TEMPLATE,
        }
    }

    /// The built-in human template ("modern").
    pub fn human() -> Self {
        Template::Builtin {
            filename: "modern.typ",
            source: HUMAN_TEMPLATE,
        }
    }

    /// The built-in cover-letter template.
    pub fn cover() -> Self {
        Template::Builtin {
            filename: "cover.typ",
            source: COVER_TEMPLATE,
        }
    }

    /// The `(filename, source)` to stage. A user template is read from disk;
    /// a built-in is already in the binary.
    fn resolve(&self) -> Result<(String, String), RenderError> {
        match self {
            Template::Builtin { filename, source } => {
                Ok(((*filename).to_string(), (*source).to_string()))
            }
            Template::User(path) => {
                let source =
                    std::fs::read_to_string(path).map_err(|source| RenderError::TemplateRead {
                        path: path.clone(),
                        source,
                    })?;
                let filename = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("template.typ")
                    .to_string();
                Ok((filename, source))
            }
        }
    }
}

/// A shipped template: its public name, the variant it serves, and its
/// source embedded at compile time. This list is the single registry every
/// name-based lookup (the `templates` module, `aarg templates list`)
/// consults, so adding a built-in is one entry here plus its `.typ` file.
pub struct Builtin {
    pub name: &'static str,
    pub variant: Variant,
    pub filename: &'static str,
    pub source: &'static str,
}

impl Builtin {
    /// The renderable `Template` for this built-in.
    pub fn template(&self) -> Template {
        Template::Builtin {
            filename: self.filename,
            source: self.source,
        }
    }
}

/// Every shipped template. ATS templates must stay parser-safe (they are the
/// ones uploaded to applicant trackers); human templates are free to be
/// designed.
pub const BUILTINS: &[Builtin] = &[
    Builtin {
        name: "classic",
        variant: Variant::Ats,
        filename: "classic.typ",
        source: ATS_TEMPLATE,
    },
    Builtin {
        name: "minimal",
        variant: Variant::Ats,
        filename: "minimal.typ",
        source: ATS_MINIMAL_TEMPLATE,
    },
    Builtin {
        name: "modern",
        variant: Variant::Human,
        filename: "modern.typ",
        source: HUMAN_TEMPLATE,
    },
    Builtin {
        name: "technical",
        variant: Variant::Human,
        filename: "technical.typ",
        source: HUMAN_TECHNICAL_TEMPLATE,
    },
    Builtin {
        name: "editorial",
        variant: Variant::Human,
        filename: "editorial.typ",
        source: HUMAN_EDITORIAL_TEMPLATE,
    },
];

/// Everything that can go wrong while rendering.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error(
        "the `typst` binary was not found (looked on PATH and next to aarg) — aarg shells out to it to build PDFs"
    )]
    TypstMissing,

    #[error("typst could not compile the resume:\n{stderr}")]
    TypstFailed { stderr: String },

    #[error("could not run typst")]
    Spawn(#[source] std::io::Error),

    #[error("could not read the template {path}")]
    TemplateRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not write {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not serialize the resume payload")]
    Payload(#[from] serde_json::Error),
}

/// Resolve the `typst` program to invoke. Prefer the one on `PATH`, so the
/// behavior matches the CLI exactly. When `PATH` doesn't have it — as in a
/// stripped-down launch environment like an SSH-spawned MCP server, which
/// never sourced the shell rc files that put `~/.cargo/bin` on `PATH` — fall
/// back to well-known locations, **the directory holding aarg's own executable
/// first** (a `cargo install`ed typst sits next to a `cargo install`ed aarg).
/// So "typst works for the CLI" implies "typst works for the server" with no
/// PATH configuration.
fn typst_program() -> String {
    // An explicit override wins and is trusted as-is (a wrong path then fails
    // with the normal typst error). Otherwise resolve automatically.
    if let Some(configured) = configured_typst() {
        return configured;
    }
    resolve_typst(which("typst"), &fallback_dirs())
}

/// An explicitly configured typst path: the `AARG_TYPST` environment variable
/// (a per-run override, handy in a stripped-down launch like an SSH-spawned MCP
/// server), else the `[render] typst` config key. `None` when neither is set. A
/// leading `~/` is expanded to the home directory.
fn configured_typst() -> Option<String> {
    let from_env = std::env::var("AARG_TYPST")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let configured = from_env.or_else(|| {
        crate::config::Config::load()
            .ok()
            .and_then(|config| config.render.typst)
            .filter(|value| !value.trim().is_empty())
    });
    configured.map(expand_tilde)
}

/// Expand a leading `~/` to `$HOME`, so a config or env value can use it.
fn expand_tilde(path: String) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => match std::env::var_os("HOME") {
            Some(home) => format!("{}/{rest}", home.to_string_lossy()),
            None => path,
        },
        None => path,
    }
}

/// The testable core of [`typst_program`]: given whether `typst` is already on
/// `PATH` and the fallback directories to search, choose what to invoke.
fn resolve_typst(on_path: bool, fallback_dirs: &[PathBuf]) -> String {
    if on_path {
        return "typst".to_string();
    }
    for dir in fallback_dirs {
        let candidate = dir.join("typst");
        if is_executable(&candidate) {
            return candidate.to_string_lossy().into_owned();
        }
    }
    // Found nowhere: the bare name yields the normal "not found" install error.
    "typst".to_string()
}

/// Where to look for `typst` when it isn't on `PATH`, in priority order: next
/// to aarg's own binary (the cargo-install case), then the usual user/local
/// bin dirs.
fn fallback_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        dirs.push(parent.to_path_buf());
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.push(home.join(".cargo").join("bin"));
        dirs.push(home.join(".local").join("bin"));
    }
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs
}

/// Whether `name` resolves to an executable on `PATH` — a `which`-style search
/// that spawns nothing.
fn which(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| is_executable(&dir.join(name)))
}

/// Whether `path` is a file we can execute.
fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Render `payload` with `template` into the build directory, returning the
/// PDF path. The output filename and the payload JSON name come from the
/// payload's variant (`resume.ats.pdf` / `resume.human.pdf`).
pub fn render(
    build_dir: &Path,
    payload: &VariantPayload,
    template: &Template,
) -> Result<PathBuf, RenderError> {
    render_with(&typst_program(), build_dir, payload, template)
}

/// The testable core: the typst program name is a parameter so tests can
/// point it at a stub script (or at nothing) without a real install.
fn render_with(
    typst: &str,
    build_dir: &Path,
    payload: &VariantPayload,
    template: &Template,
) -> Result<PathBuf, RenderError> {
    stage_and_compile(
        typst,
        build_dir,
        payload.variant.payload_name(),
        &serde_json::to_vec_pretty(payload)?,
        payload.variant.pdf_name(),
        template,
    )
}

/// Render a cover letter to `cover_letter.pdf` in the build directory. Same
/// staging as a resume variant, a different payload and output name — the
/// `CoverLetter` is the documented JSON the template reads.
pub fn render_cover(
    build_dir: &Path,
    letter: &CoverLetter,
    template: &Template,
) -> Result<PathBuf, RenderError> {
    render_cover_with(&typst_program(), build_dir, letter, template)
}

/// The testable core of [`render_cover`]; the typst program name is a
/// parameter so tests can point it at a stub.
fn render_cover_with(
    typst: &str,
    build_dir: &Path,
    letter: &CoverLetter,
    template: &Template,
) -> Result<PathBuf, RenderError> {
    stage_and_compile(
        typst,
        build_dir,
        "cover_payload.json",
        &serde_json::to_vec_pretty(letter)?,
        "cover_letter.pdf",
        template,
    )
}

/// Check that the `typst` binary is on PATH, so a command can fail fast with
/// the install message *before* doing expensive work (a whole tailor loop, an
/// LLM cover-letter draft) that would otherwise only discover the missing
/// binary at render time. Returns the same [`RenderError::TypstMissing`] that
/// rendering would, so the diagnostic is identical wherever it surfaces.
pub fn ensure_available() -> Result<(), RenderError> {
    ensure_available_with(&typst_program())
}

/// The testable core of [`ensure_available`]; the program name is a parameter
/// so tests can point it at a stub or a path that doesn't exist.
fn ensure_available_with(typst: &str) -> Result<(), RenderError> {
    // `--version` is the cheapest invocation that proves the binary runs; its
    // output is discarded — only whether it could be spawned matters. A
    // non-zero exit still means the binary exists, so only a spawn failure is
    // treated as missing.
    match Command::new(typst)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(RenderError::TypstMissing),
        Err(e) => Err(RenderError::Spawn(e)),
    }
}

/// Stage a payload, its template, and the shared library into the build
/// directory, then shell out to typst. Shared by the variant renderer and
/// the cover-letter renderer — they differ only in the payload JSON and the
/// output filename, so everything else (the staging, the missing-binary and
/// stderr paths) lives here once.
fn stage_and_compile(
    typst: &str,
    build_dir: &Path,
    payload_name: &str,
    payload: &[u8],
    out_name: &str,
    template: &Template,
) -> Result<PathBuf, RenderError> {
    let write = |name: &str, contents: &[u8]| -> Result<(), RenderError> {
        let path = build_dir.join(name);
        std::fs::write(&path, contents).map_err(|source| RenderError::Write { path, source })
    };

    let (template_file, template_src) = template.resolve()?;
    write(payload_name, payload)?;
    write(&template_file, template_src.as_bytes())?;
    // Staged for any template that imports it; harmless for those that don't.
    write(SHARED_LIB_NAME, SHARED_LIB.as_bytes())?;

    let output = Command::new(typst)
        .args([
            "compile",
            "--input",
            &format!("data={payload_name}"),
            &template_file,
            out_name,
        ])
        .current_dir(build_dir)
        .output();

    match output {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(RenderError::TypstMissing),
        Err(e) => Err(RenderError::Spawn(e)),
        Ok(out) if !out.status.success() => Err(RenderError::TypstFailed {
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }),
        Ok(_) => Ok(build_dir.join(out_name)),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::dataset::types::{Contact, YearMonth};
    use crate::tailor::{
        BuildId, JdId, SkillsSection, TailoredBullet, TailoredResume, TailoredRole,
    };
    use crate::variant::ats_payload;

    fn sample_resume() -> TailoredResume {
        TailoredResume {
            build_id: BuildId("001".into()),
            jd_id: JdId("acme-engineer".into()),
            generated_at: chrono::Utc::now(),
            contact: Contact {
                full_name: "Ada Lovelace".into(),
                email: "ada@example.com".into(),
                phone: None,
                location: None,
                links: Vec::new(),
            },
            target_title: Some("Senior Engineer".into()),
            summary: "Engineering leader.".into(),
            roles: vec![TailoredRole {
                id: crate::dataset::types::RoleId("role-1".into()),
                company: "Acme".into(),
                title: "Engineer".into(),
                start: YearMonth {
                    year: 2020,
                    month: 3,
                },
                end: None,
                location: None,
                bullets: vec![TailoredBullet {
                    source_id: crate::dataset::types::BulletId("bullet-1".into()),
                    text: "Did the thing".into(),
                }],
            }],
            education: Vec::new(),
            skills_section: SkillsSection {
                skills: vec!["Rust".into()],
            },
            projects: Vec::new(),
            achievements: Vec::new(),
            certifications: Vec::new(),
        }
    }

    #[cfg(unix)]
    fn stub_typst(dir: &Path, script_body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("typst-stub");
        std::fs::write(&path, format!("#!/bin/sh\n{script_body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.display().to_string()
    }

    #[cfg(unix)]
    #[test]
    fn staging_writes_payload_template_and_lib_next_to_the_output() {
        let dir = tempfile::tempdir().unwrap();
        let stub = stub_typst(dir.path(), "exit 0");

        let pdf = render_with(
            &stub,
            dir.path(),
            &ats_payload(&sample_resume()),
            &Template::ats(),
        )
        .unwrap();

        assert_eq!(pdf, dir.path().join("resume.ats.pdf"));
        let payload = std::fs::read_to_string(dir.path().join("ats_payload.json")).unwrap();
        assert!(payload.contains("Ada Lovelace"));
        // The target-title headline rides along in the payload.
        assert!(payload.contains("Senior Engineer"));
        let template = std::fs::read_to_string(dir.path().join("classic.typ")).unwrap();
        assert!(template.contains("json(sys.inputs.data)"));
        // The shared lib is staged for templates that import it.
        assert!(dir.path().join("aarg-template-lib.typ").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_user_template_is_a_first_class_input() {
        let dir = tempfile::tempdir().unwrap();
        let stub = stub_typst(dir.path(), "exit 0");
        // A user-supplied template read from an arbitrary path.
        let custom = dir.path().join("my-layout.typ");
        std::fs::write(&custom, "#let data = json(sys.inputs.data)\n").unwrap();

        let pdf = render_with(
            &stub,
            dir.path(),
            &ats_payload(&sample_resume()),
            &Template::User(custom),
        )
        .unwrap();

        assert_eq!(pdf, dir.path().join("resume.ats.pdf"));
        // The user's template was staged under its own filename.
        assert!(dir.path().join("my-layout.typ").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_cover_letter_stages_its_payload_and_renders() {
        let dir = tempfile::tempdir().unwrap();
        let stub = stub_typst(dir.path(), "exit 0");
        let letter = CoverLetter {
            contact: Contact {
                full_name: "Ada Lovelace".into(),
                email: "ada@example.com".into(),
                phone: None,
                location: None,
                links: Vec::new(),
            },
            company: "Acme".into(),
            title: "Staff Engineer".into(),
            greeting: "Dear Acme hiring team,".into(),
            paragraphs: vec!["I would welcome a conversation.".into()],
            signoff: "Ada Lovelace".into(),
        };

        let pdf = render_cover_with(&stub, dir.path(), &letter, &Template::cover()).unwrap();

        assert_eq!(pdf, dir.path().join("cover_letter.pdf"));
        let payload = std::fs::read_to_string(dir.path().join("cover_payload.json")).unwrap();
        assert!(payload.contains("Dear Acme hiring team,"));
        assert!(payload.contains("Ada Lovelace"));
        // The cover template is staged under its own name.
        assert!(dir.path().join("cover.typ").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_compile_carries_typsts_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let stub = stub_typst(dir.path(), "echo 'error: unknown variable' >&2; exit 1");

        let err = render_with(
            &stub,
            dir.path(),
            &ats_payload(&sample_resume()),
            &Template::ats(),
        )
        .unwrap_err();
        match err {
            RenderError::TypstFailed { stderr } => {
                assert!(stderr.contains("unknown variable"));
            }
            other => panic!("expected TypstFailed, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_binary_is_the_install_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let err = render_with(
            "/definitely/not/a/real/typst-binary",
            dir.path(),
            &ats_payload(&sample_resume()),
            &Template::ats(),
        )
        .unwrap_err();
        assert!(matches!(err, RenderError::TypstMissing));
    }

    #[test]
    fn ensure_available_reports_typst_missing_when_the_binary_is_absent() {
        let err = ensure_available_with("/definitely/not/a/real/typst-binary").unwrap_err();
        assert!(matches!(err, RenderError::TypstMissing));
    }

    #[cfg(unix)]
    #[test]
    fn ensure_available_passes_when_the_binary_runs() {
        let dir = tempfile::tempdir().unwrap();
        let stub = stub_typst(dir.path(), "exit 0");
        assert!(ensure_available_with(&stub).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn resolve_typst_prefers_path_then_a_known_dir_else_the_bare_name() {
        use std::os::unix::fs::PermissionsExt;

        // On PATH: the bare name, matching the CLI exactly.
        assert_eq!(resolve_typst(true, &[]), "typst");

        let dir = tempfile::tempdir().unwrap();
        let dirs = vec![dir.path().to_path_buf()];
        // Not on PATH and not in the dir: the bare name (which yields the
        // normal install error downstream), never a false positive.
        assert_eq!(resolve_typst(false, &dirs), "typst");

        // Drop an executable `typst` into the dir: it's found by absolute path,
        // so a stripped-PATH server (e.g. over SSH) still renders.
        let typst = dir.path().join("typst");
        std::fs::write(&typst, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&typst, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(resolve_typst(false, &dirs), typst.to_string_lossy());
    }

    #[test]
    fn expand_tilde_only_touches_a_leading_home_shortcut() {
        // Non-tilde paths pass through unchanged.
        assert_eq!(
            expand_tilde("/usr/local/bin/typst".into()),
            "/usr/local/bin/typst"
        );
        assert_eq!(expand_tilde("typst".into()), "typst");
        // A leading `~/` expands against the real $HOME for this run.
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(
                expand_tilde("~/.cargo/bin/typst".into()),
                format!("{}/.cargo/bin/typst", home.to_string_lossy())
            );
        }
    }

    #[test]
    fn a_missing_user_template_is_a_typed_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = render_with(
            "typst",
            dir.path(),
            &ats_payload(&sample_resume()),
            &Template::User("/no/such/template.typ".into()),
        )
        .unwrap_err();
        assert!(matches!(err, RenderError::TemplateRead { .. }));
    }
}
