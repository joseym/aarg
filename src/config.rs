//! User configuration: where `config.toml` lives on disk and how it is
//! loaded and saved.
//!
//! Only non-secret settings belong here. API keys live in the OS keychain
//! (see the `secrets` module), never in the config file.

use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

/// The model used for Anthropic requests when the user has not picked one.
pub const DEFAULT_ANTHROPIC_MODEL: &str = "claude-opus-4-8";

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

/// Settings specific to the Anthropic provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnthropicConfig {
    /// Model ID sent with each request.
    pub model: String,
}

impl Default for AnthropicConfig {
    fn default() -> Self {
        Self {
            model: DEFAULT_ANTHROPIC_MODEL.to_string(),
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
            },
        };
        config.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path).unwrap(), config);
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
