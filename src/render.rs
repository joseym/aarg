//! Rendering the canonical resume to a PDF by shelling out to the
//! `typst` binary (FR-1.6, PRD §9.2).
//!
//! Everything typst needs is staged *inside the build directory*: the
//! payload JSON and a copy of the template. That keeps typst's project
//! root simple (a template resolves `json(sys.inputs.data)` relative to
//! itself, and typst refuses paths outside its root), and it makes every
//! build self-documenting — the exact template that produced the PDF
//! sits next to it.
//!
//! The payload is passed **by path**, never inlined into the command
//! line: real resumes would blow past OS argument-length limits and
//! invite shell-escaping bugs.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::tailor::TailoredResume;

/// The shipped ATS template, embedded at compile time so a fresh install
/// renders with zero setup. (User-editable template overrides are a
/// Phase 6 feature.)
const ATS_TEMPLATE: &str = include_str!("../templates/ats/classic.typ");

/// Everything that can go wrong while rendering.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("the `typst` binary was not found on PATH — aarg shells out to it to build PDFs")]
    TypstMissing,

    #[error("typst could not compile the resume:\n{stderr}")]
    TypstFailed { stderr: String },

    #[error("could not run typst")]
    Spawn(#[source] std::io::Error),

    #[error("could not write {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not serialize the resume payload")]
    Payload(#[from] serde_json::Error),
}

/// Render the ATS PDF into the build directory, returning the PDF path.
/// Stages `ats_payload.json` and `classic.typ` alongside it.
pub fn render_ats(build_dir: &Path, resume: &TailoredResume) -> Result<PathBuf, RenderError> {
    render_ats_with("typst", build_dir, resume)
}

/// The testable core: the typst program name is a parameter so tests can
/// point it at a stub script (or at nothing) without a real install.
fn render_ats_with(
    typst: &str,
    build_dir: &Path,
    resume: &TailoredResume,
) -> Result<PathBuf, RenderError> {
    let write = |name: &str, contents: &[u8]| -> Result<(), RenderError> {
        let path = build_dir.join(name);
        std::fs::write(&path, contents).map_err(|source| RenderError::Write { path, source })
    };
    write("ats_payload.json", &serde_json::to_vec_pretty(resume)?)?;
    write("classic.typ", ATS_TEMPLATE.as_bytes())?;

    let output = Command::new(typst)
        .args([
            "compile",
            "--input",
            "data=ats_payload.json",
            "classic.typ",
            "resume.ats.pdf",
        ])
        .current_dir(build_dir)
        .output();

    match output {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(RenderError::TypstMissing),
        Err(e) => Err(RenderError::Spawn(e)),
        Ok(out) if !out.status.success() => Err(RenderError::TypstFailed {
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }),
        Ok(_) => Ok(build_dir.join("resume.ats.pdf")),
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
    fn staging_writes_payload_and_template_next_to_the_output() {
        let dir = tempfile::tempdir().unwrap();
        let stub = stub_typst(dir.path(), "exit 0");

        let pdf = render_ats_with(&stub, dir.path(), &sample_resume()).unwrap();

        assert_eq!(pdf, dir.path().join("resume.ats.pdf"));
        let payload = std::fs::read_to_string(dir.path().join("ats_payload.json")).unwrap();
        assert!(payload.contains("Ada Lovelace"));
        // The target-title headline rides along in the payload.
        assert!(payload.contains("Senior Engineer"));
        let template = std::fs::read_to_string(dir.path().join("classic.typ")).unwrap();
        assert!(template.contains("json(sys.inputs.data)"));
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_compile_carries_typsts_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let stub = stub_typst(dir.path(), "echo 'error: unknown variable' >&2; exit 1");

        let err = render_ats_with(&stub, dir.path(), &sample_resume()).unwrap_err();
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
        let err = render_ats_with(
            "/definitely/not/a/real/typst-binary",
            dir.path(),
            &sample_resume(),
        )
        .unwrap_err();
        assert!(matches!(err, RenderError::TypstMissing));
    }
}
