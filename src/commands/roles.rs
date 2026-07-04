//! `aarg roles enrich [id]` — the history copilot as a standalone
//! session. With an id it works one role; without, it sweeps every role
//! thin on detail. Thin glue over `crate::enrich`: load, interview, save
//! once on success.

use crate::agent::AgentContext;
use crate::commands::{CliError, configured_client};
use crate::dataset::store;
use crate::dataset::types::RoleId;
use crate::enrich;
use crate::style;
use crate::terminal::auto_user;

pub async fn enrich(id: Option<String>) -> Result<(), CliError> {
    let mut dataset = store::load()?;

    // Resolve the targets before spending anything on the model.
    let targets: Vec<RoleId> = match id {
        Some(id) => {
            let role_id = RoleId(id.clone());
            if !dataset.roles.iter().any(|role| role.id == role_id) {
                return Err(CliError::RoleNotFound { id });
            }
            vec![role_id]
        }
        None => enrich::thin_roles(&dataset),
    };
    if targets.is_empty() {
        eprintln!(
            "{}",
            style::info("no thin roles to enrich · every role already has some detail")
        );
        return Ok(());
    }

    let (client, config) = configured_client().await?;
    let tracer = super::default_tracer()?;
    let ctx = AgentContext {
        llm: &*client,
        model: config.active_resolver(),
        tracer: &tracer,
        sink: None,
    };
    let user = auto_user();

    let outcome = enrich::enrich_roles(&mut dataset, &targets, user.as_ref(), &ctx).await?;

    if outcome.changed() {
        dataset.metadata.updated_at = chrono::Utc::now();
        store::save(&dataset)?;
    }
    let tail = if outcome.changed() {
        style::dim("· dataset saved (previous version backed up)")
    } else {
        style::dim("· dataset unchanged")
    };
    eprintln!(
        "{}",
        style::success(format!(
            "added {} bullet(s) across {} role(s) {tail}",
            outcome.bullets_added, outcome.roles_touched
        ))
    );
    Ok(())
}
