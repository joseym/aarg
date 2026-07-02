//! Readability checks on a rendered resume PDF (FR-5.4, PRD §9.4). A
//! **service** — pure code, no LLM (like `ats`) — that reads the finished
//! PDF and reports whether it is something a human can actually read at a
//! glance: short enough, not wall-to-wall text, and made of real selectable
//! text rather than an image of text.
//!
//! It deliberately checks the *rendered PDF*, not the payload, for the same
//! reason `ats` does: a template bug, an overstuffed role, or a human variant
//! that somehow rasterized to images would all pass a payload check and fail
//! a person — only the artifact tells the truth.
//!
//! Three of the four PRD checks are implemented here deterministically:
//! page count (vs the variant's `max_pages`), density (characters per page
//! and a too-many-bullets-on-one-role flag), and text-extraction sanity. The
//! fourth — font-size *hierarchy* (title > company > dates > bullets) — needs
//! per-glyph sizes, which the PRD gets from `pdfium-render`; that crate loads
//! a native `libpdfium` at runtime that isn't present in every environment,
//! so when it can't run the hierarchy check is reported as *unchecked*
//! (`hierarchy_ok: None`) rather than guessed. The three text-layer checks
//! are the must-haves and never depend on a native library.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::variant::VariantPayload;

/// Default page ceiling when the payload's layout hints don't set one.
const DEFAULT_MAX_PAGES: u8 = 2;

/// A role carrying more than this many bullets reads as a wall of text on the
/// page, regardless of total length. Matches the tailoring `MAX_BULLETS_PER_ROLE`
/// upper bound so the two agree on "too many".
const MAX_BULLETS_PER_ROLE: usize = 6;

/// Characters of extracted text per page above which a page is "dense" — a
/// rough proxy for too-little whitespace. A comfortable one-column resume page
/// runs well under this; a page crammed past it is hard to skim. A signal, not
/// a hard rule, so it only ever adds an `issues` note.
const DENSE_CHARS_PER_PAGE: f32 = 3500.0;

/// The verdict of a readability check, written alongside a build as
/// `readability_report.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadabilityReport {
    /// Number of pages in the rendered PDF.
    pub page_count: u8,
    /// Whether `page_count` exceeds the variant's page ceiling.
    pub over_page_limit: bool,
    /// A 0..1-ish density signal: mean characters per page scaled against
    /// `DENSE_CHARS_PER_PAGE` (1.0 ≈ at the dense threshold; higher = denser).
    pub density_score: f32,
    /// Whether the title > company > dates > bullets font hierarchy holds.
    /// `None` when the check couldn't run in this environment (no native
    /// `libpdfium`); see the module docs.
    pub hierarchy_ok: Option<bool>,
    /// Human-readable problems found; empty means the page reads cleanly.
    pub issues: Vec<String>,
}

/// Everything that can go wrong while checking readability.
#[derive(Debug, thiserror::Error)]
pub enum ReadabilityError {
    /// The PDF could not be read or parsed into a text layer.
    #[error("could not read text from {path}")]
    Extract {
        path: std::path::PathBuf,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// Check a rendered resume PDF against `payload`'s layout intent.
///
/// Reuses the same text-extraction path as `ats` (the `pdf-extract` crate),
/// here in its page-aware form so the page count and the per-page density
/// signal come from one parse. The font-hierarchy check is deferred (see the
/// module docs): `hierarchy_ok` is `None` and an `issues` note says so.
pub fn check(pdf: &Path, payload: &VariantPayload) -> Result<ReadabilityReport, ReadabilityError> {
    let pages = extract_pages(pdf)?;
    let mut issues = Vec::new();

    // --- page count ---------------------------------------------------
    let page_count = u8::try_from(pages.len()).unwrap_or(u8::MAX);
    let max_pages = if payload.layout_hints.max_pages == 0 {
        DEFAULT_MAX_PAGES
    } else {
        payload.layout_hints.max_pages
    };
    let over_page_limit = page_count > max_pages;
    if over_page_limit {
        issues.push(format!(
            "{page_count} pages, over the {max_pages}-page limit"
        ));
    }

    // --- text-extraction sanity --------------------------------------
    // Real, selectable text is itself a check: a human variant that somehow
    // rasterized to an image of text would extract (next to) nothing.
    let total_chars: usize = pages.iter().map(|p| p.trim().chars().count()).sum();
    if total_chars < MIN_REAL_TEXT_CHARS {
        issues.push(format!(
            "the PDF extracted only {total_chars} characters of text — \
             it may have rendered as an image instead of selectable text"
        ));
    }

    // --- density ------------------------------------------------------
    // Mean characters per page, scaled so 1.0 sits at the dense threshold.
    let density_score = if page_count == 0 {
        0.0
    } else {
        (total_chars as f32 / page_count as f32) / DENSE_CHARS_PER_PAGE
    };
    for (i, page) in pages.iter().enumerate() {
        let chars = page.trim().chars().count();
        if chars as f32 > DENSE_CHARS_PER_PAGE {
            issues.push(format!(
                "page {} is dense ({chars} characters) — consider trimming for whitespace",
                i + 1
            ));
        }
    }
    // A single role with too many bullets reads as a wall of text even when
    // the page totals look fine. Read from the payload, which is the source of
    // what got laid out.
    for role in &payload.roles {
        if role.bullets.len() > MAX_BULLETS_PER_ROLE {
            issues.push(format!(
                "{} at {} has {} bullets (over {MAX_BULLETS_PER_ROLE}) — \
                 the role reads heavy",
                role.title,
                role.company,
                role.bullets.len()
            ));
        }
    }

    // --- font hierarchy (deferred) ------------------------------------
    // Needs per-glyph sizes (pdfium); unavailable here, so report it as
    // unchecked rather than guessing a pass/fail.
    let hierarchy_ok = None;
    issues.push("font hierarchy check unavailable (pdfium)".to_string());

    Ok(ReadabilityReport {
        page_count,
        over_page_limit,
        density_score,
        hierarchy_ok,
        issues,
    })
}

/// Below this many extracted characters a resume PDF almost certainly failed
/// to produce a real text layer (an empty or image-only render).
const MIN_REAL_TEXT_CHARS: usize = 50;

/// Extract the PDF's text one entry per page, reusing the `pdf-extract` crate
/// that `ats::extract_pdf_text` uses — here in its page-aware form so page
/// count and per-page density come from one parse.
fn extract_pages(pdf: &Path) -> Result<Vec<String>, ReadabilityError> {
    pdf_extract::extract_text_by_pages(pdf).map_err(|source| ReadabilityError::Extract {
        path: pdf.to_path_buf(),
        source: Box::new(source),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::dataset::types::{BulletId, Contact, RoleId, YearMonth};
    use crate::render::{Template, render};
    use crate::tailor::{
        BuildId, JdId, SkillsSection, TailoredBullet, TailoredResume, TailoredRole,
    };
    use crate::variant::ats_payload;

    fn contact() -> Contact {
        Contact {
            full_name: "Ada Lovelace".into(),
            email: "ada@example.com".into(),
            phone: None,
            location: None,
            links: Vec::new(),
        }
    }

    fn role(title: &str, n_bullets: usize, words_per_bullet: usize) -> TailoredRole {
        let bullets = (0..n_bullets)
            .map(|i| TailoredBullet {
                source_id: BulletId(format!("bullet-{i}")),
                text: format!(
                    "Delivered measurable platform improvements across teams {}",
                    "and stakeholders ".repeat(words_per_bullet)
                ),
            })
            .collect();
        TailoredRole {
            id: RoleId("role-1".into()),
            company: "Acme".into(),
            title: title.into(),
            start: YearMonth {
                year: 2020,
                month: 1,
            },
            end: None,
            location: None,
            bullets,
        }
    }

    fn resume(roles: Vec<TailoredRole>) -> TailoredResume {
        TailoredResume {
            build_id: BuildId("test".into()),
            jd_id: JdId("acme".into()),
            generated_at: chrono::Utc::now(),
            contact: contact(),
            target_title: Some("Senior Engineer".into()),
            summary: "Engineering leader who ships durable systems.".into(),
            roles,
            education: Vec::new(),
            skills_section: SkillsSection {
                skills: vec!["Rust".into(), "Kubernetes".into()],
            },
            projects: Vec::new(),
            achievements: Vec::new(),
            certifications: Vec::new(),
        }
    }

    /// Render a payload to a real PDF in a tempdir and check it. Hermetic:
    /// each test gets its own tempdir. Rendering shells out to `typst`, which
    /// is not guaranteed present (e.g. a CI runner without it); every caller
    /// gates on `render::ensure_available()` and skips before reaching here, so
    /// this helper assumes typst is available.
    fn render_and_check(draft: &TailoredResume) -> (tempfile::TempDir, ReadabilityReport) {
        let dir = tempfile::tempdir().unwrap();
        let payload = ats_payload(draft);
        let pdf = render(dir.path(), &payload, &Template::ats()).unwrap();
        let report = check(&pdf, &payload).unwrap();
        // Keep `dir` alive (the PDF lives under it) by returning it.
        (dir, report)
    }

    #[test]
    fn a_normal_resume_extracts_real_text_and_stays_in_limit() {
        if crate::render::ensure_available().is_err() {
            eprintln!("skipping: typst not installed");
            return;
        }
        let draft = resume(vec![role("Engineer", 3, 4)]);
        let (_dir, report) = render_and_check(&draft);

        assert!(report.page_count >= 1);
        assert!(!report.over_page_limit, "issues: {:?}", report.issues);
        // The PDF must contain real, selectable text — the extraction-sanity
        // check must not have fired.
        assert!(
            !report
                .issues
                .iter()
                .any(|i| i.contains("rendered as an image")),
            "issues: {:?}",
            report.issues
        );
    }

    #[test]
    fn an_over_long_resume_trips_over_page_limit() {
        if crate::render::ensure_available().is_err() {
            eprintln!("skipping: typst not installed");
            return;
        }
        // Many roles, each with many long bullets, well past two pages.
        let roles = (0..16)
            .map(|i| {
                let mut r = role("Engineer", 6, 16);
                r.id = RoleId(format!("role-{i}"));
                r.bullets = r
                    .bullets
                    .into_iter()
                    .enumerate()
                    .map(|(j, mut b)| {
                        b.source_id = BulletId(format!("bullet-{i}-{j}"));
                        b
                    })
                    .collect();
                r
            })
            .collect();
        let draft = resume(roles);
        let (_dir, report) = render_and_check(&draft);

        assert!(
            report.over_page_limit,
            "{} pages should exceed the limit; issues: {:?}",
            report.page_count, report.issues
        );
        assert!(
            report
                .issues
                .iter()
                .any(|i| i.contains("over the") && i.contains("page limit"))
        );
    }

    #[test]
    fn text_extraction_yields_real_text() {
        if crate::render::ensure_available().is_err() {
            eprintln!("skipping: typst not installed");
            return;
        }
        let draft = resume(vec![role("Engineer", 2, 3)]);
        let dir = tempfile::tempdir().unwrap();
        let payload = ats_payload(&draft);
        let pdf = render(dir.path(), &payload, &Template::ats()).unwrap();

        // Extraction itself returns real, non-empty per-page text.
        let pages = extract_pages(&pdf).unwrap();
        assert!(!pages.is_empty());
        let joined: String = pages.join(" ");
        assert!(joined.contains("Ada Lovelace"));
        assert!(joined.contains("Engineer"));

        let report = check(&pdf, &payload).unwrap();
        assert!(report.density_score > 0.0);
    }

    #[test]
    fn a_role_with_too_many_bullets_is_flagged() {
        if crate::render::ensure_available().is_err() {
            eprintln!("skipping: typst not installed");
            return;
        }
        let draft = resume(vec![role("Engineer", MAX_BULLETS_PER_ROLE + 2, 3)]);
        let (_dir, report) = render_and_check(&draft);

        assert!(
            report
                .issues
                .iter()
                .any(|i| i.contains("bullets") && i.contains("reads heavy")),
            "issues: {:?}",
            report.issues
        );
    }

    #[test]
    fn the_hierarchy_check_is_reported_as_unavailable() {
        if crate::render::ensure_available().is_err() {
            eprintln!("skipping: typst not installed");
            return;
        }
        let draft = resume(vec![role("Engineer", 2, 3)]);
        let (_dir, report) = render_and_check(&draft);

        // Deferred in this environment: reported as unchecked, with a note,
        // never a guessed pass/fail.
        assert_eq!(report.hierarchy_ok, None);
        assert!(report.issues.iter().any(|i| i.contains("font hierarchy")));
    }

    #[test]
    fn a_missing_pdf_is_a_typed_error_not_a_panic() {
        let draft = resume(vec![role("Engineer", 1, 1)]);
        let payload = ats_payload(&draft);
        let err = check(Path::new("/no/such/file.pdf"), &payload).unwrap_err();
        assert!(matches!(err, ReadabilityError::Extract { .. }));
    }
}
