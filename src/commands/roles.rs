//! `aarg roles enrich [id]` — the history copilot as a standalone
//! session. With an id it works one role; without, it sweeps every role
//! thin on detail. Thin glue over `crate::enrich`: load, interview, save
//! once on success.

use crate::agent::AgentContext;
use crate::commands::{CliError, configured_client};
use crate::dataset::store;
use crate::dataset::types::RoleId;
use crate::enrich;
use crate::terminal::auto_user;
use crate::trace::Tracer;

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
        println!("no thin roles to enrich — every role already has some detail");
        return Ok(());
    }

    let (client, config) = configured_client().await?;
    let tracer = Tracer::to_default_dir()?;
    let ctx = AgentContext {
        llm: &client,
        model: &config.anthropic,
        tracer: &tracer,
    };
    let user = auto_user();

    let outcome = enrich::enrich_roles(&mut dataset, &targets, user.as_ref(), &ctx).await?;

    if outcome.changed() {
        dataset.metadata.updated_at = chrono::Utc::now();
        store::save(&dataset)?;
    }
    println!(
        "added {} bullet(s) across {} role(s){}",
        outcome.bullets_added,
        outcome.roles_touched,
        if outcome.changed() {
            " · dataset saved (previous version backed up)"
        } else {
            " · dataset unchanged"
        }
    );
    Ok(())
}
