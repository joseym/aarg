//! Template management: turn a template *name* into a renderable
//! [`render::Template`], and list what's available.
//!
//! A name resolves to a shipped built-in (embedded in the binary) or, for
//! the human variant, a user file at
//! `<config_dir>/templates/<variant>/<name>.typ` — found under the active
//! workspace's `.aarg/templates/` or the home config dir, the same way the
//! rest of aarg's files are located. The **ATS variant only accepts
//! built-ins**: that PDF is uploaded to applicant-tracking systems, and an
//! arbitrary layout risks breaking the parsers the ATS templates are kept
//! safe for.
//!
//! The built-in list lives in [`render::BUILTINS`]; this module adds the
//! name lookup, the on-disk discovery, and the listing the `templates`
//! command prints. The resolution core takes the templates root as a
//! parameter so it is unit-testable without touching the real filesystem.

use std::path::{Path, PathBuf};

use crate::render::{BUILTINS, Template};
use crate::variant::Variant;

/// Everything that can go wrong resolving a template name.
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum TemplateError {
    #[error("no {variant} template named {name:?}")]
    #[diagnostic(help("run `aarg templates list` to see what's available"))]
    NotFound { name: String, variant: &'static str },

    #[error(
        "the ATS variant only uses built-in templates (kept parser-safe for applicant trackers); {name:?} is a custom file"
    )]
    #[diagnostic(help(
        "pick a built-in ATS template (`classic` or `minimal`), or apply your custom layout to the human variant"
    ))]
    AtsMustBeBuiltin { name: String },
}

/// The human-facing label for a variant, for messages.
fn variant_label(variant: Variant) -> &'static str {
    match variant {
        Variant::Ats => "ATS",
        Variant::Human => "human",
    }
}

/// The `templates/` subdirectory a variant's user files live in.
fn variant_dir(variant: Variant) -> &'static str {
    match variant {
        Variant::Ats => "ats",
        Variant::Human => "human",
    }
}

/// The templates root for the active workspace (`<config_dir>/templates`).
fn templates_root() -> Option<PathBuf> {
    crate::workspace::config_dir().map(|dir| dir.join("templates"))
}

/// Resolve a template name for a variant to a renderable template, using the
/// active workspace's templates directory for user files.
pub fn resolve(name: &str, variant: Variant) -> Result<Template, TemplateError> {
    resolve_in(name, variant, templates_root().as_deref())
}

/// The testable core: `root` is the `templates/` directory to search for
/// user files (`None` = none available).
fn resolve_in(
    name: &str,
    variant: Variant,
    root: Option<&Path>,
) -> Result<Template, TemplateError> {
    // A shipped built-in wins — it's embedded and always available.
    if let Some(builtin) = BUILTINS
        .iter()
        .find(|builtin| builtin.name == name && builtin.variant == variant)
    {
        return Ok(builtin.template());
    }

    let on_disk = user_template_path(name, variant, root);
    match variant {
        // ATS never reads a user file: a custom layout could break a parser.
        Variant::Ats if on_disk.is_some() => Err(TemplateError::AtsMustBeBuiltin {
            name: name.to_string(),
        }),
        Variant::Ats => Err(TemplateError::NotFound {
            name: name.to_string(),
            variant: variant_label(variant),
        }),
        Variant::Human => match on_disk {
            Some(path) => Ok(Template::User(path)),
            None => Err(TemplateError::NotFound {
                name: name.to_string(),
                variant: variant_label(variant),
            }),
        },
    }
}

/// The path a user template *would* live at, if the file exists.
fn user_template_path(name: &str, variant: Variant, root: Option<&Path>) -> Option<PathBuf> {
    let path = root?.join(variant_dir(variant)).join(format!("{name}.typ"));
    path.is_file().then_some(path)
}

/// One available template, for `aarg templates list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub name: String,
    pub variant: Variant,
    pub builtin: bool,
}

/// Every template a user can pick: the built-ins plus user human templates
/// discovered under the workspace `templates/human/` directory.
pub fn available() -> Vec<Listed> {
    available_in(templates_root().as_deref())
}

/// The testable core of [`available`].
fn available_in(root: Option<&Path>) -> Vec<Listed> {
    let mut listed: Vec<Listed> = BUILTINS
        .iter()
        .map(|builtin| Listed {
            name: builtin.name.to_string(),
            variant: builtin.variant,
            builtin: true,
        })
        .collect();

    // User templates are a human-variant feature (ATS stays built-in only).
    if let Some(dir) = root.map(|r| r.join(variant_dir(Variant::Human)))
        && let Ok(entries) = std::fs::read_dir(&dir)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("typ") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            // A user file that shadows a built-in name isn't listed twice.
            let shadows_builtin = BUILTINS
                .iter()
                .any(|b| b.name == stem && b.variant == Variant::Human);
            if !shadows_builtin {
                listed.push(Listed {
                    name: stem.to_string(),
                    variant: Variant::Human,
                    builtin: false,
                });
            }
        }
    }
    listed
}

/// The variant a known template name serves — so `aarg templates use <name>`
/// can set the right config field. `None` if no such template is available.
pub fn variant_of(name: &str) -> Option<Variant> {
    available()
        .into_iter()
        .find(|t| t.name == name)
        .map(|t| t.variant)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn builtins_resolve_by_name_and_variant() {
        assert!(matches!(
            resolve_in("classic", Variant::Ats, None).unwrap(),
            Template::Builtin { .. }
        ));
        assert!(matches!(
            resolve_in("minimal", Variant::Ats, None).unwrap(),
            Template::Builtin { .. }
        ));
        assert!(matches!(
            resolve_in("modern", Variant::Human, None).unwrap(),
            Template::Builtin { .. }
        ));
        assert!(matches!(
            resolve_in("technical", Variant::Human, None).unwrap(),
            Template::Builtin { .. }
        ));
    }

    #[test]
    fn a_builtin_name_is_variant_specific() {
        // `classic` is an ATS template, not a human one.
        assert!(matches!(
            resolve_in("classic", Variant::Human, None),
            Err(TemplateError::NotFound { .. })
        ));
    }

    #[test]
    fn an_unknown_name_is_not_found() {
        assert!(matches!(
            resolve_in("nope", Variant::Human, None),
            Err(TemplateError::NotFound { .. })
        ));
        assert!(matches!(
            resolve_in("nope", Variant::Ats, None),
            Err(TemplateError::NotFound { .. })
        ));
    }

    #[test]
    fn a_user_human_file_resolves_but_an_ats_one_is_refused() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("human")).unwrap();
        std::fs::create_dir_all(root.path().join("ats")).unwrap();
        std::fs::write(root.path().join("human").join("custom.typ"), "// x").unwrap();
        std::fs::write(root.path().join("ats").join("sneaky.typ"), "// x").unwrap();

        // A human user template is a first-class choice.
        match resolve_in("custom", Variant::Human, Some(root.path())).unwrap() {
            Template::User(path) => assert!(path.ends_with("human/custom.typ")),
            other => panic!("expected a user template, got {other:?}"),
        }
        // An ATS user file is refused even though it exists on disk.
        assert!(matches!(
            resolve_in("sneaky", Variant::Ats, Some(root.path())),
            Err(TemplateError::AtsMustBeBuiltin { .. })
        ));
    }

    #[test]
    fn available_lists_builtins_and_user_human_templates() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("human")).unwrap();
        std::fs::write(root.path().join("human").join("fancy.typ"), "// x").unwrap();
        // A user file shadowing a built-in name is not listed twice.
        std::fs::write(root.path().join("human").join("modern.typ"), "// x").unwrap();

        let listed = available_in(Some(root.path()));
        let names: Vec<&str> = listed.iter().map(|l| l.name.as_str()).collect();
        for builtin in ["classic", "minimal", "modern", "technical"] {
            assert!(names.contains(&builtin), "missing built-in {builtin}");
        }
        assert!(names.contains(&"fancy"));
        assert_eq!(
            listed.iter().filter(|l| l.name == "modern").count(),
            1,
            "the shadowing user file must not duplicate the built-in"
        );
    }
}
