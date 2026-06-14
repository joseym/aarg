//! User configuration: where `config.toml` lives on disk and how it is
//! loaded and saved.
//!
//! Only non-secret settings belong here. API keys live in the OS keychain
//! (see the `secrets` module), never in the config file.

use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

/// The model used for Anthropic requests when the user has not picked one.
pub const DEFAULT_ANTHROPIC_MODEL: &str = "claude-haiku-4-5-20251001";

/// Everything that can go wrong while locating, reading, parsing, or
/// writing the config file.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not determine this user's home directory")]
    NoHomeDir,

    #[error("could not read {path}")]
    Read {
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

    #[error("{path} is not valid TOML")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("could not serialize the config to TOML")]
    Serialize(#[from] toml::ser::Error),
}

/// Which LLM provider requests go to. Anthropic is the only provider for
/// now; Ollama is planned as a fully local alternative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[default]
    Anthropic,
}

impl Provider {
    /// The stable lowercase name used for keychain entries and display.
    pub fn name(self) -> &'static str {
        match self {
            Provider::Anthropic => "anthropic",
        }
    }
}

/// Per-tier model overrides. A `None` falls back to the cheap default
/// (for `cheap`) or the legacy `model` (for `mid`/`smart`) — see the
/// `ModelResolver` impl. Resolved in code rather than via a `Default`
/// derive so a partial `[anthropic.tiers]` table still gets the fallback.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelTiers {
    pub cheap: Option<String>,
    pub mid: Option<String>,
    pub smart: Option<String>,
}

/// Settings specific to the Anthropic provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnthropicConfig {
    /// The fallback model — used for any tier not pinned in `tiers`, and
    /// the single model older configs (no `[tiers]`) ran everything on.
    pub model: String,
    /// Maps the three tiers to concrete model ids.
    pub tiers: ModelTiers,
    /// Per-agent overrides by agent id ("tailoring_v1" -> model), the
    /// highest-priority resolution step.
    pub agents: std::collections::BTreeMap<String, String>,
}

impl Default for AnthropicConfig {
    fn default() -> Self {
        Self {
            model: DEFAULT_ANTHROPIC_MODEL.to_string(),
            tiers: ModelTiers::default(),
            agents: std::collections::BTreeMap::new(),
        }
    }
}

impl crate::agent::ModelResolver for AnthropicConfig {
    fn resolve(&self, agent_id: &str, tier: crate::agent::ModelTier) -> &str {
        use crate::agent::ModelTier;
        // Highest priority: an explicit per-agent override.
        if let Some(model) = self.agents.get(agent_id) {
            return model;
        }
        // Then the tier mapping, with sensible fallbacks: cheap drops to
        // the cheap default (Haiku), mid/smart to the configured `model`.
        match tier {
            ModelTier::Cheap => self
                .tiers
                .cheap
                .as_deref()
                .unwrap_or(DEFAULT_ANTHROPIC_MODEL),
            ModelTier::Mid => self.tiers.mid.as_deref().unwrap_or(&self.model),
            ModelTier::Smart => self.tiers.smart.as_deref().unwrap_or(&self.model),
        }
    }
}

/// Tunable loop limits for `aarg tailor`. Each has a sensible default
/// (the PRD's), so an absent `[limits]` table changes nothing — the table
/// only exists for users who want a longer (or cheaper) loop. Resolved by
/// hand-written `Default` rather than serde so a partial `[limits]` table
/// still fills the rest from these values, not zeros.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    /// Hard cap on adversarial revision passes past the first draft
    /// (PRD §6.4's anti-oscillation cap; default 2).
    pub revisions: usize,
    /// A draft scoring at least this is accepted without revising — raise
    /// it to push the loop to keep trying, lower it to stop sooner.
    pub acceptable_score: f32,
    /// Max questions the strengthen interview may ask per bullet (one
    /// opening question plus follow-ups on thin answers; default 3).
    pub strengthen_questions: usize,
    /// Max times the user may ask for another strengthen rewrite before the
    /// loop offers only take-it-or-keep-mine (default 3).
    pub strengthen_revises: usize,
    /// Warn when a build's estimated cost exceeds this many dollars. `None`
    /// (the default) means no budget warning.
    pub budget_usd: Option<f64>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            revisions: 2,
            acceptable_score: 0.85,
            strengthen_questions: 3,
            strengthen_revises: 3,
            budget_usd: None,
        }
    }
}

/// The contents of `config.toml`. Any field missing from the file falls
/// back to its default, so an empty file is a valid config.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub provider: Provider,
    pub anthropic: AnthropicConfig,
    pub limits: Limits,
    /// Per-model price overrides (dollars per million tokens), keyed by
    /// model id. Absent models fall back to `pricing`'s built-in family
    /// rates; an empty table (the default) uses the built-ins for all.
    pub prices: std::collections::BTreeMap<String, crate::pricing::Price>,
}

impl Config {
    /// The directory holding aarg's configuration (e.g. `~/.config/aarg`
    /// on Linux), chosen per-OS by the `directories` crate.
    pub fn dir() -> Result<PathBuf, ConfigError> {
        ProjectDirs::from("", "", "aarg")
            .map(|dirs| dirs.config_dir().to_path_buf())
            .ok_or(ConfigError::NoHomeDir)
    }

    /// Full path of `config.toml`.
    pub fn path() -> Result<PathBuf, ConfigError> {
        Ok(Self::dir()?.join("config.toml"))
    }

    /// Load the config from its default location. A missing file is not an
    /// error: first runs simply get the defaults.
    pub fn load() -> Result<Self, ConfigError> {
        Self::load_from(&Self::path()?)
    }

    fn load_from(path: &Path) -> Result<Self, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(e) => {
                return Err(ConfigError::Read {
                    path: path.to_path_buf(),
                    source: e,
                });
            }
        };
        toml::from_str(&text).map_err(|e| ConfigError::Parse {
            path: path.to_path_buf(),
            source: e,
        })
    }

    /// Write the config to its default location, creating the directory
    /// if needed.
    pub fn save(&self) -> Result<(), ConfigError> {
        self.save_to(&Self::path()?)
    }

    fn save_to(&self, path: &Path) -> Result<(), ConfigError> {
        let text = toml::to_string_pretty(self)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ConfigError::Write {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
        std::fs::write(path, text).map_err(|e| ConfigError::Write {
            path: path.to_path_buf(),
            source: e,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_loads_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load_from(&dir.path().join("config.toml")).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("config.toml");
        let config = Config {
            provider: Provider::Anthropic,
            anthropic: AnthropicConfig {
                model: "claude-haiku-4-5".to_string(),
                ..AnthropicConfig::default()
            },
            ..Config::default()
        };
        config.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path).unwrap(), config);
    }

    #[test]
    fn tiers_round_trip_through_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let config = Config {
            provider: Provider::Anthropic,
            anthropic: AnthropicConfig {
                model: "claude-sonnet-4-6".to_string(),
                tiers: ModelTiers {
                    cheap: Some("claude-haiku-4-5".to_string()),
                    mid: None,
                    smart: Some("claude-opus-4-8".to_string()),
                },
                agents: std::collections::BTreeMap::from([(
                    "tailoring_v1".to_string(),
                    "claude-opus-4-8".to_string(),
                )]),
            },
            ..Config::default()
        };
        config.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path).unwrap(), config);
    }

    #[test]
    fn prices_and_budget_round_trip_through_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut config = Config::default();
        config.limits.budget_usd = Some(1.50);
        config.prices.insert(
            "claude-opus-4-8".to_string(),
            crate::pricing::Price {
                input_per_mtok: 12.0,
                output_per_mtok: 60.0,
            },
        );
        config.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path).unwrap(), config);
    }

    #[test]
    fn limits_default_to_the_prd_values() {
        let limits = Limits::default();
        assert_eq!(limits.revisions, 2);
        assert_eq!(limits.strengthen_questions, 3);
        assert_eq!(limits.strengthen_revises, 3);
    }

    #[test]
    fn a_partial_limits_table_keeps_other_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // Only one limit set; the rest must fall back to their defaults,
        // not to zero.
        std::fs::write(&path, "[limits]\nrevisions = 5\n").unwrap();
        let config = Config::load_from(&path).unwrap();
        assert_eq!(config.limits.revisions, 5);
        assert_eq!(config.limits.strengthen_questions, 3);
        assert_eq!(config.limits.acceptable_score, 0.85);
    }

    #[test]
    fn resolve_falls_back_when_tiers_unset() {
        use crate::agent::{ModelResolver, ModelTier};
        let config = AnthropicConfig {
            model: "configured".to_string(),
            ..AnthropicConfig::default()
        };
        // Cheap drops to the cheap default; mid/smart to the configured model.
        assert_eq!(
            config.resolve("any", ModelTier::Cheap),
            DEFAULT_ANTHROPIC_MODEL
        );
        assert_eq!(config.resolve("any", ModelTier::Mid), "configured");
        assert_eq!(config.resolve("any", ModelTier::Smart), "configured");
    }

    #[test]
    fn resolve_honors_tier_overrides() {
        use crate::agent::{ModelResolver, ModelTier};
        let config = AnthropicConfig {
            model: "fallback".to_string(),
            tiers: ModelTiers {
                cheap: Some("cheap-model".to_string()),
                mid: Some("mid-model".to_string()),
                smart: Some("smart-model".to_string()),
            },
            ..AnthropicConfig::default()
        };
        assert_eq!(config.resolve("any", ModelTier::Cheap), "cheap-model");
        assert_eq!(config.resolve("any", ModelTier::Mid), "mid-model");
        assert_eq!(config.resolve("any", ModelTier::Smart), "smart-model");
    }

    #[test]
    fn per_agent_override_wins_over_tier() {
        use crate::agent::{ModelResolver, ModelTier};
        let config = AnthropicConfig {
            model: "fallback".to_string(),
            tiers: ModelTiers {
                smart: Some("smart-model".to_string()),
                ..ModelTiers::default()
            },
            agents: std::collections::BTreeMap::from([(
                "tailoring_v1".to_string(),
                "pinned".to_string(),
            )]),
        };
        // The per-agent pin beats the tier it would otherwise resolve to.
        assert_eq!(config.resolve("tailoring_v1", ModelTier::Smart), "pinned");
        // An agent without a pin still follows its tier.
        assert_eq!(config.resolve("other", ModelTier::Smart), "smart-model");
    }

    #[test]
    fn empty_file_is_a_valid_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "").unwrap();
        assert_eq!(Config::load_from(&path).unwrap(), Config::default());
    }

    #[test]
    fn garbage_reports_a_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "provider = [not toml").unwrap();
        let err = Config::load_from(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    // EXERCISE(EX-001)
    #[test]
    #[ignore = "exercise: unknown keys are currently skipped by the parser; make them an error"]
    fn unknown_keys_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "does_not_exist = true").unwrap();
        let err = Config::load_from(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }
}
