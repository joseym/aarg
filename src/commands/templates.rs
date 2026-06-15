//! `aarg templates list|use` — see the available resume templates and pick
//! the default each variant renders with.
//!
//! Templates resolve through the `templates` module: a shipped built-in or,
//! for the human variant, a `.typ` file under the workspace's
//! `templates/human/` directory. `use <name>` records the choice in
//! `[templates]` so it sticks across builds; `tailor --template` still wins
//! for a one-off.

use crate::commands::CliError;
use crate::config::Config;
use crate::templates::{self, Listed};
use crate::variant::Variant;

/// `aarg templates list` — show every template, grouped by variant and
/// marking the active one.
pub async fn list() -> Result<(), CliError> {
    let config = Config::load()?;
    let available = templates::available();

    println!("ATS templates (uploaded to applicant trackers — built-in only):");
    print_group(&available, Variant::Ats, config.templates.ats_name());
    println!("Human templates (shared with people — built-in or your own):");
    print_group(&available, Variant::Human, config.templates.human_name());

    println!();
    println!("set a default with `aarg templates use <name>`.");
    println!("custom human templates live at <workspace>/templates/human/<name>.typ.");
    Ok(())
}

/// Print one variant's templates, marking the active and tagging custom ones.
fn print_group(available: &[Listed], variant: Variant, active: &str) {
    for template in available.iter().filter(|t| t.variant == variant) {
        let active_marker = if template.name == active {
            " (active)"
        } else {
            ""
        };
        let custom_marker = if template.builtin { "" } else { " [custom]" };
        println!("  {}{custom_marker}{active_marker}", template.name);
    }
}

/// `aarg templates use <name>` — make a template the default. The name's
/// variant decides which `[templates]` field it sets, so the user doesn't
/// have to say which one.
pub async fn use_template(name: String) -> Result<(), CliError> {
    let mut config = Config::load()?;
    let variant = templates::variant_of(&name)
        .ok_or_else(|| CliError::UnknownTemplate { name: name.clone() })?;

    match variant {
        Variant::Ats => config.templates.ats = Some(name.clone()),
        Variant::Human => config.templates.human = Some(name.clone()),
    }
    config.save()?;

    let label = match variant {
        Variant::Ats => "ATS",
        Variant::Human => "Human",
    };
    println!("{label} template is now `{name}`.");
    Ok(())
}
