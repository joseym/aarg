//! `aarg config` — show the effective configuration and where it comes
//! from. Read-only: the file is edited by hand or via `aarg init`.

use crate::agent::{ModelResolver, ModelTier};
use crate::commands::CliError;
use crate::config::Config;
use crate::secrets;

pub async fn run() -> Result<(), CliError> {
    let path = Config::path()?;
    let config = Config::load()?;
    let file_exists = path.exists();

    // Only presence is reported — the key itself never leaves the
    // keychain for display. An unreachable keychain (e.g. no Secret
    // Service daemon on a headless Linux box) downgrades to a note here
    // instead of failing the whole command: this command is read-only
    // status, and the rest of the config is still worth showing.
    let label = config.anthropic.active_label();
    let kind = config.anthropic.kind_for(label);
    let kind_str = match kind {
        crate::config::AuthKind::ApiKey => "API key",
        crate::config::AuthKind::Oauth => "OAuth / subscription",
        crate::config::AuthKind::Cli => "CLI-delegated",
    };
    // A CLI-delegated credential has no stored secret — its token is fetched
    // from `ant` at request time, so don't probe the keychain for it.
    let key_status = if kind == crate::config::AuthKind::Cli {
        format!("delegated to `ant auth print-credentials` (label: {label}, {kind_str})")
    } else {
        match secrets::load_api_key(config.provider.name(), label).await {
            Ok(Some(_)) => format!("stored in the OS keychain (label: {label}, {kind_str})"),
            // Nothing under the active label; a legacy bare-slot key may still
            // be in play for users who haven't re-run init.
            Ok(None) if config.anthropic.keys.is_empty() => {
                match secrets::load_legacy_key(config.provider.name()).await {
                    Ok(Some(_)) => {
                        "stored in the OS keychain (legacy slot; run `aarg init` to label it)"
                            .to_string()
                    }
                    Ok(None) => "not set (run `aarg init`)".to_string(),
                    Err(error) => format!("unknown ({error})"),
                }
            }
            Ok(None) => format!("not set for label `{label}` (run `aarg key add {label}`)"),
            Err(error) => format!("unknown ({error})"),
        }
    };

    println!("workspace:   {}", crate::workspace::locate().describe());
    println!(
        "config file: {}{}",
        path.display(),
        if file_exists {
            ""
        } else {
            " (not created yet; showing defaults)"
        }
    );
    println!("provider:    {}", config.provider.name());
    println!(
        "model:       {} (fallback for unpinned tiers)",
        config.anthropic.model
    );
    println!("api key:     {key_status}");
    if !config.anthropic.keys.is_empty() {
        // List the labels (never the secrets), marking the active one and
        // tagging non-API-key kinds.
        let active = config.anthropic.active_label();
        let labels: Vec<String> = config
            .anthropic
            .keys
            .iter()
            .map(|label| {
                let kind_tag = match config.anthropic.kind_for(label) {
                    crate::config::AuthKind::ApiKey => "",
                    crate::config::AuthKind::Oauth => " (oauth)",
                    crate::config::AuthKind::Cli => " (cli)",
                };
                let active_marker = if label == active { " (active)" } else { "" };
                format!("{label}{kind_tag}{active_marker}")
            })
            .collect();
        println!("keys:        {}", labels.join(", "));
    }
    // The headless path overrides everything above; say so if it's in effect.
    if std::env::var_os("ANTHROPIC_AUTH_TOKEN").is_some() {
        println!(
            "note:        ANTHROPIC_AUTH_TOKEN is set — requests use that OAuth token, not the keychain."
        );
    } else if std::env::var_os("ANTHROPIC_API_KEY").is_some() {
        println!(
            "note:        ANTHROPIC_API_KEY is set — requests use that key, not the keychain."
        );
    }

    // Each agent runs on a tier; the tier resolves to a model here. The
    // `agent_id` passed to `resolve` only matters when a per-agent pin
    // exists, so a representative id per tier is enough to show the model.
    let anthropic = &config.anthropic;
    println!("tiers:");
    println!(
        "  cheap (parse/match):   {}",
        anthropic.resolve("jd_parser_v1", ModelTier::Cheap)
    );
    println!(
        "  mid   (interview):     {}",
        anthropic.resolve("metric_interview_v1", ModelTier::Mid)
    );
    println!(
        "  smart (tailor/review): {}",
        anthropic.resolve("tailoring_v1", ModelTier::Smart)
    );
    if !anthropic.agents.is_empty() {
        println!("per-agent overrides:");
        for (agent_id, model) in &anthropic.agents {
            println!("  {agent_id}: {model}");
        }
    }

    let limits = &config.limits;
    println!("limits:");
    println!("  revisions:            {}", limits.revisions);
    println!("  acceptable score:     {:.2}", limits.acceptable_score);
    println!("  strengthen questions: {}", limits.strengthen_questions);
    println!("  strengthen revises:   {}", limits.strengthen_revises);
    println!(
        "  budget:               {}",
        match limits.budget_usd {
            Some(b) => format!("${b:.2} per build"),
            None => "none".to_string(),
        }
    );
    Ok(())
}
