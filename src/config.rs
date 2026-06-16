//! User configuration: where `config.toml` lives on disk and how it is
//! loaded and saved.
//!
//! Only non-secret settings belong here. API keys live in the OS keychain
//! (see the `secrets` module), never in the config file.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The model used for Anthropic requests when the user has not picked one.
pub const DEFAULT_ANTHROPIC_MODEL: &str = "claude-haiku-4-5-20251001";

/// The label a key is filed under when the user never names one — the
/// single-key case, and what a legacy (pre-labels) key is adopted as.
pub const DEFAULT_KEY_LABEL: &str = "default";

/// The template each variant renders with when the user hasn't picked one.
pub const DEFAULT_ATS_TEMPLATE: &str = "classic";
pub const DEFAULT_HUMAN_TEMPLATE: &str = "modern";

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

/// How a stored credential authenticates:
/// - `ApiKey` — a pay-as-you-go key sent as `x-api-key`.
/// - `Oauth` — a Claude-plan token (from `claude setup-token`) sent as a
///   bearer token; the secret lives in the keychain.
/// - `Cli` — a Claude plan delegated to the official Anthropic CLI: no
///   secret is stored, and a fresh bearer token is fetched per run via
///   `ant auth print-credentials` (the official client owns refresh).
///
/// `ApiKey` is the default so a config written before OAuth — where every
/// key is implicitly an API key — keeps working unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthKind {
    #[default]
    ApiKey,
    Oauth,
    Cli,
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
    /// Labels of the API keys stored for this provider, e.g. `["personal",
    /// "work"]`. The secrets themselves live in the OS keychain (see the
    /// `secrets` module); this is only the registry of which labels exist,
    /// because the keychain can't be enumerated portably. Empty means no
    /// labeled key yet — a legacy single key may still live in the bare
    /// keychain slot (see `secrets::load_legacy_key`).
    pub keys: Vec<String>,
    /// Which stored key (a `keys` label) requests use by default. `None`
    /// resolves to the sole key when there is exactly one, otherwise to the
    /// conventional `DEFAULT_KEY_LABEL`. See `active_label`.
    pub active_key: Option<String>,
    /// The auth kind of each label that isn't a plain API key. A label
    /// absent here is an `ApiKey` (the default and the only kind older
    /// configs knew), so old `config.toml`s keep working; OAuth keys record
    /// their kind so the client sends bearer + the oauth beta header.
    pub key_kinds: std::collections::BTreeMap<String, AuthKind>,
}

impl Default for AnthropicConfig {
    fn default() -> Self {
        Self {
            model: DEFAULT_ANTHROPIC_MODEL.to_string(),
            tiers: ModelTiers::default(),
            agents: std::collections::BTreeMap::new(),
            keys: Vec::new(),
            active_key: None,
            key_kinds: std::collections::BTreeMap::new(),
        }
    }
}

impl AnthropicConfig {
    /// The key label requests use by default: an explicit `active_key` if
    /// set, else the sole stored key, else the conventional default label.
    /// (A one-off `AARG_KEY` env override is applied above this, at the
    /// call site in `configured_client`.)
    pub fn active_label(&self) -> &str {
        if let Some(active) = &self.active_key {
            return active;
        }
        match self.keys.as_slice() {
            [only] => only,
            _ => DEFAULT_KEY_LABEL,
        }
    }

    /// The auth kind of a stored label — `ApiKey` unless recorded otherwise,
    /// so unknown and legacy labels behave like the API keys they are.
    pub fn kind_for(&self, label: &str) -> AuthKind {
        self.key_kinds.get(label).copied().unwrap_or_default()
    }

    /// Record that a key labeled `label` of `kind` now exists (idempotent).
    /// The secret is stored separately, in the keychain. `ApiKey` is the
    /// implicit default, so it is never written to the map (re-registering an
    /// existing label as `ApiKey` clears any prior OAuth tag).
    pub fn register_key(&mut self, label: &str, kind: AuthKind) {
        if !self.keys.iter().any(|existing| existing == label) {
            self.keys.push(label.to_string());
        }
        match kind {
            // The implicit default: kept out of the map so legacy/untagged
            // labels and API keys read back identically.
            AuthKind::ApiKey => {
                self.key_kinds.remove(label);
            }
            AuthKind::Oauth | AuthKind::Cli => {
                self.key_kinds.insert(label.to_string(), kind);
            }
        }
    }

    /// Forget a key label and its kind, clearing `active_key` if it pointed
    /// at it. The caller deletes the secret from the keychain.
    pub fn forget_key(&mut self, label: &str) {
        self.keys.retain(|existing| existing != label);
        self.key_kinds.remove(label);
        if self.active_key.as_deref() == Some(label) {
            self.active_key = None;
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

/// Which template each variant renders with. A `None` falls back to the
/// shipped default (`classic` for ATS, `modern` for human); names resolve
/// through the `templates` module (a built-in, or a user file under the
/// workspace `templates/` directory).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TemplatesConfig {
    pub ats: Option<String>,
    pub human: Option<String>,
}

impl TemplatesConfig {
    /// The ATS template name to render with (configured, else the default).
    pub fn ats_name(&self) -> &str {
        self.ats.as_deref().unwrap_or(DEFAULT_ATS_TEMPLATE)
    }

    /// The human template name to render with (configured, else the default).
    pub fn human_name(&self) -> &str {
        self.human.as_deref().unwrap_or(DEFAULT_HUMAN_TEMPLATE)
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
    /// Which template each variant renders with.
    pub templates: TemplatesConfig,
    /// Per-model price overrides (dollars per million tokens), keyed by
    /// model id. Absent models fall back to `pricing`'s built-in family
    /// rates; an empty table (the default) uses the built-ins for all.
    pub prices: std::collections::BTreeMap<String, crate::pricing::Price>,
}

impl Config {
    /// The directory holding aarg's configuration — the active workspace's
    /// `.aarg/` when one is in use, otherwise the per-OS config directory
    /// (e.g. `~/.config/aarg` on Linux). Resolved by the `workspace` module.
    pub fn dir() -> Result<PathBuf, ConfigError> {
        crate::workspace::config_dir().ok_or(ConfigError::NoHomeDir)
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

    /// Load the config from an explicit path. Used by `init` to target a
    /// workspace it is creating, before discovery would find it.
    pub fn load_from(path: &Path) -> Result<Self, ConfigError> {
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

    /// Write the config to an explicit path, creating parent directories.
    /// Used by `init` to write into a workspace it is creating.
    pub fn save_to(&self, path: &Path) -> Result<(), ConfigError> {
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
                ..AnthropicConfig::default()
            },
            ..Config::default()
        };
        config.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path).unwrap(), config);
    }

    #[test]
    fn keys_and_active_key_round_trip_through_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let config = Config {
            anthropic: AnthropicConfig {
                keys: vec!["personal".to_string(), "work".to_string()],
                active_key: Some("work".to_string()),
                ..AnthropicConfig::default()
            },
            ..Config::default()
        };
        config.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path).unwrap(), config);
    }

    #[test]
    fn active_label_prefers_explicit_then_sole_then_default() {
        // No keys, no active: the conventional default label.
        let none = AnthropicConfig::default();
        assert_eq!(none.active_label(), DEFAULT_KEY_LABEL);

        // Exactly one key and no explicit active: that sole key.
        let sole = AnthropicConfig {
            keys: vec!["personal".to_string()],
            ..AnthropicConfig::default()
        };
        assert_eq!(sole.active_label(), "personal");

        // Several keys, no explicit active: fall back to the default label
        // rather than guess which of several the user meant.
        let several = AnthropicConfig {
            keys: vec!["personal".to_string(), "work".to_string()],
            ..AnthropicConfig::default()
        };
        assert_eq!(several.active_label(), DEFAULT_KEY_LABEL);

        // An explicit active wins over everything.
        let explicit = AnthropicConfig {
            keys: vec!["personal".to_string(), "work".to_string()],
            active_key: Some("work".to_string()),
            ..AnthropicConfig::default()
        };
        assert_eq!(explicit.active_label(), "work");
    }

    #[test]
    fn templates_round_trip_and_default_to_the_shipped_names() {
        // Defaults when nothing is configured.
        let defaults = TemplatesConfig::default();
        assert_eq!(defaults.ats_name(), "classic");
        assert_eq!(defaults.human_name(), "modern");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let config = Config {
            templates: TemplatesConfig {
                ats: Some("minimal".to_string()),
                human: Some("technical".to_string()),
            },
            ..Config::default()
        };
        config.save_to(&path).unwrap();
        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded, config);
        assert_eq!(loaded.templates.ats_name(), "minimal");
        assert_eq!(loaded.templates.human_name(), "technical");
    }

    #[test]
    fn register_key_is_idempotent_and_forget_clears_active() {
        let mut config = AnthropicConfig::default();
        config.register_key("work", AuthKind::ApiKey);
        config.register_key("work", AuthKind::ApiKey); // no duplicate
        config.register_key("personal", AuthKind::ApiKey);
        assert_eq!(
            config.keys,
            vec!["work".to_string(), "personal".to_string()]
        );

        config.active_key = Some("work".to_string());
        config.forget_key("work");
        assert_eq!(config.keys, vec!["personal".to_string()]);
        // Forgetting the active key clears the pointer so it can't dangle.
        assert_eq!(config.active_key, None);
    }

    #[test]
    fn key_kinds_default_to_api_key_and_round_trip() {
        let mut config = AnthropicConfig::default();
        // An unregistered label is an API key (the legacy default).
        assert_eq!(config.kind_for("anything"), AuthKind::ApiKey);

        config.register_key("personal", AuthKind::ApiKey);
        config.register_key("plan", AuthKind::Oauth);
        config.register_key("delegated", AuthKind::Cli);
        // API-key labels stay implicit (out of the map); OAuth and CLI are tagged.
        assert_eq!(config.kind_for("personal"), AuthKind::ApiKey);
        assert_eq!(config.kind_for("plan"), AuthKind::Oauth);
        assert_eq!(config.kind_for("delegated"), AuthKind::Cli);
        assert!(!config.key_kinds.contains_key("personal"));

        // Re-registering an OAuth label as an API key clears the tag.
        config.register_key("plan", AuthKind::ApiKey);
        assert_eq!(config.kind_for("plan"), AuthKind::ApiKey);

        // The kind survives a TOML round trip.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let full = Config {
            anthropic: AnthropicConfig {
                keys: vec!["plan".to_string()],
                key_kinds: std::collections::BTreeMap::from([(
                    "plan".to_string(),
                    AuthKind::Oauth,
                )]),
                ..AnthropicConfig::default()
            },
            ..Config::default()
        };
        full.save_to(&path).unwrap();
        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded, full);
        assert_eq!(loaded.anthropic.kind_for("plan"), AuthKind::Oauth);
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
            ..AnthropicConfig::default()
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
