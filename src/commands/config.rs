//! `aarg config` — show the effective configuration and where it comes
//! from. Read-only: the file is edited by hand or via `aarg init`.

use crate::agent::{ModelResolver, ModelTier};
use crate::commands::CliError;
use crate::config::Config;
use crate::secrets;
use crate::style;

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
    // by running the configured command at request time, so don't probe the
    // keychain for it. Show the actual command (the default `ant …` or a
    // per-label override) rather than assuming `ant`.
    let key_status = if kind == crate::config::AuthKind::Cli {
        let command = config.anthropic.credential_command(label).join(" ");
        style::success(format!(
            "delegated to `{command}` {}",
            style::dim(format!("(label: {label}, {kind_str})"))
        ))
    } else {
        match secrets::load_api_key(config.provider.name(), label).await {
            Ok(Some(_)) => style::success(format!(
                "stored in the OS keychain {}",
                style::dim(format!("(label: {label}, {kind_str})"))
            )),
            // Nothing under the active label; a legacy bare-slot key may still
            // be in play for users who haven't re-run init.
            Ok(None) if config.anthropic.keys.is_empty() => {
                match secrets::load_legacy_key(config.provider.name()).await {
                    Ok(Some(_)) => style::warn(
                        "stored in the OS keychain (legacy slot; run `aarg init` to label it)",
                    ),
                    Ok(None) => style::suggest("not set · run `aarg init`"),
                    Err(error) => style::warn(format!("unknown ({error})")),
                }
            }
            Ok(None) => style::suggest(format!(
                "not set for label `{label}` · run `aarg key add {label}`"
            )),
            Err(error) => style::warn(format!("unknown ({error})")),
        }
    };

    // Human status report on stderr (the stream the color helpers detect on);
    // this is a read-only display command with no machine mode.
    eprintln!("{}", style::section("Workspace"));
    // Width fits the longest label in this block ("config file") so the two
    // value columns line up.
    eprintln!(
        "{}",
        style::kv("workspace", crate::workspace::locate().describe(), 12)
    );
    eprintln!(
        "{}",
        style::kv(
            "config file",
            format!(
                "{}{}",
                path.display(),
                if file_exists {
                    String::new()
                } else {
                    style::dim(" (not created yet; showing defaults)")
                }
            ),
            12
        )
    );
    // Surface what is steering resolution, so a surprising location is
    // debuggable: the `AARG_DIR` env var and/or a `workspace` set in the
    // global config (the file-based equivalent of the env var).
    if let Some(env_dir) = std::env::var_os(crate::workspace::DIR_ENV)
        && !env_dir.is_empty()
    {
        eprintln!("{}", style::kv("AARG_DIR", env_dir.to_string_lossy(), 12));
    }
    if let Some(configured) = crate::workspace::configured_workspace() {
        eprintln!(
            "{}",
            style::kv(
                "configured",
                format!("{} (from global config)", configured.display()),
                12
            )
        );
    }

    eprintln!("{}", style::section("Provider"));
    eprintln!("{}", style::kv("provider", config.provider.name(), 9));
    eprintln!(
        "{}",
        style::kv(
            "model",
            format!(
                "{} {}",
                config.anthropic.model,
                style::dim("(fallback for unpinned tiers)")
            ),
            9
        )
    );
    eprintln!("{}", style::kv("api key", key_status, 9));
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
        eprintln!("{}", style::kv("keys", labels.join(", "), 9));
    }
    // The headless path overrides everything above; say so if it's in effect.
    // The var names are configurable, so report the names actually checked.
    let auth_token_env = config.anthropic.auth_token_env();
    let api_key_env = config.anthropic.api_key_env();
    if std::env::var_os(auth_token_env).is_some() {
        eprintln!(
            "{}",
            style::info(format!(
                "{auth_token_env} is set · requests use that OAuth token, not the keychain."
            ))
        );
    } else if std::env::var_os(api_key_env).is_some() {
        eprintln!(
            "{}",
            style::info(format!(
                "{api_key_env} is set · requests use that key, not the keychain."
            ))
        );
    }

    // Each agent runs on a tier; the tier resolves to a model here. The
    // `agent_id` passed to `resolve` only matters when a per-agent pin
    // exists, so a representative id per tier is enough to show the model.
    let anthropic = &config.anthropic;
    eprintln!("{}", style::section("Model tiers"));
    eprintln!(
        "{}",
        style::kv(
            "cheap",
            format!(
                "{} {}",
                anthropic.resolve("jd_parser_v1", ModelTier::Cheap),
                style::dim("(parse/match)")
            ),
            7
        )
    );
    eprintln!(
        "{}",
        style::kv(
            "mid",
            format!(
                "{} {}",
                anthropic.resolve("metric_interview_v1", ModelTier::Mid),
                style::dim("(interview)")
            ),
            7
        )
    );
    eprintln!(
        "{}",
        style::kv(
            "smart",
            format!(
                "{} {}",
                anthropic.resolve("tailoring_v1", ModelTier::Smart),
                style::dim("(tailor/review)")
            ),
            7
        )
    );
    if !anthropic.agents.is_empty() {
        eprintln!("{}", style::section("Per-agent overrides"));
        for (agent_id, model) in &anthropic.agents {
            eprintln!("  {}", style::bullet(format!("{agent_id}: {model}")));
        }
    }

    let limits = &config.limits;
    eprintln!("{}", style::section("Limits"));
    eprintln!("{}", style::kv("revisions", limits.revisions, 21));
    eprintln!(
        "{}",
        style::kv(
            "acceptable score",
            format!("{:.2}", limits.acceptable_score),
            21
        )
    );
    eprintln!(
        "{}",
        style::kv("strengthen questions", limits.strengthen_questions, 21)
    );
    eprintln!(
        "{}",
        style::kv("strengthen revises", limits.strengthen_revises, 21)
    );
    eprintln!(
        "{}",
        style::kv(
            "budget",
            match limits.budget_usd {
                Some(b) => format!("${b:.2} per build"),
                None => "none".to_string(),
            },
            21
        )
    );

    eprintln!("{}", style::section("Export"));
    eprintln!(
        "{}",
        style::kv(
            "destination",
            match &config.export.dir {
                Some(dir) => dir.display().to_string(),
                None => "current directory (set `export.dir`, or pass `--to`)".to_string(),
            },
            11
        )
    );

    eprintln!("{}", style::section("Render"));
    eprintln!(
        "{}",
        style::kv(
            "typst",
            match &config.render.typst {
                Some(path) => path.clone(),
                None => "auto (PATH, then next to aarg)".to_string(),
            },
            7
        )
    );
    Ok(())
}
