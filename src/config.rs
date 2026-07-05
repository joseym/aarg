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

/// Where each local provider listens by default. LM Studio's server defaults
/// to 1234, Ollama's to 11434; both bind loopback.
pub const DEFAULT_LMSTUDIO_BASE_URL: &str = "http://127.0.0.1:1234";
pub const DEFAULT_OLLAMA_BASE_URL: &str = "http://127.0.0.1:11434";

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

/// Which LLM provider requests go to. Anthropic runs over the network with a
/// stored credential; LM Studio and Ollama run fully local with no credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[default]
    Anthropic,
    /// A local OpenAI-compatible server (LM Studio, by default).
    #[serde(rename = "lmstudio")]
    LmStudio,
    /// A local Ollama server speaking its native API.
    Ollama,
}

impl Provider {
    /// The stable lowercase name used for keychain entries and display. It
    /// matches the provider's config section name (`[anthropic]`, `[lmstudio]`,
    /// `[ollama]`).
    pub fn name(self) -> &'static str {
        match self {
            Provider::Anthropic => "anthropic",
            Provider::LmStudio => "lmstudio",
            Provider::Ollama => "ollama",
        }
    }

    /// Whether this provider runs on the local machine with no credential.
    /// Local providers skip the keychain and cost nothing per request.
    pub fn is_local(self) -> bool {
        matches!(self, Provider::LmStudio | Provider::Ollama)
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
/// - `Cli` — the bearer token is fetched per run by running a command, so no
///   secret is stored in aarg. The command defaults to the official Anthropic
///   CLI (`ant auth print-credentials --access-token`, which owns the OAuth
///   refresh) and is overridable per label via `credential_commands` — read a
///   0600 file, call `pass`, or hit a vault. This is the headless path for
///   when the OS keychain isn't reachable.
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

/// The default command a `Cli`-delegated key runs to fetch a fresh bearer
/// token: the official Anthropic CLI, which owns the OAuth refresh. A config
/// `credential_commands` entry overrides it per label.
pub const DEFAULT_CREDENTIAL_COMMAND: [&str; 4] =
    ["ant", "auth", "print-credentials", "--access-token"];

/// The environment variable AARG reads an API key from when `api_key_env`
/// isn't set. This is the same name the Anthropic SDK/CLI use; a config
/// override points AARG at a private name instead, so this conventional one
/// can be left unset (it otherwise overrides Claude Code's subscription login).
pub const DEFAULT_API_KEY_ENV: &str = "ANTHROPIC_API_KEY";

/// The environment variable AARG reads an OAuth/subscription bearer token
/// from when `auth_token_env` isn't set. A config override frees this
/// conventional name for Claude Code (a `claude setup-token` token lives here).
pub const DEFAULT_AUTH_TOKEN_ENV: &str = "ANTHROPIC_AUTH_TOKEN";

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
    /// Per-label override of the command a `Cli`-delegated key runs to fetch
    /// its bearer token. A label absent here (or mapped to an empty command)
    /// falls back to `DEFAULT_CREDENTIAL_COMMAND` (the official `ant` CLI), so
    /// existing delegated setups keep working. Only the command lives here;
    /// the secret stays wherever the command reads it from (a 0600 file,
    /// `pass`, a vault), never in this file.
    pub credential_commands: std::collections::BTreeMap<String, Vec<String>>,
    /// The environment variable AARG reads an API key from in the headless
    /// path. `None` (the default) uses the conventional `ANTHROPIC_API_KEY`;
    /// set it to a private name (e.g. `AARG_ANTHROPIC_API_KEY`) so leaving
    /// `ANTHROPIC_API_KEY` unset keeps Claude Code on its own subscription
    /// login. When set, AARG reads *only* that name — it does not also fall
    /// back to `ANTHROPIC_API_KEY`.
    pub api_key_env: Option<String>,
    /// The environment variable AARG reads an OAuth/subscription bearer token
    /// from in the headless path. `None` (the default) uses the conventional
    /// `ANTHROPIC_AUTH_TOKEN`; set a private name to free that var for Claude
    /// Code. When set, only that name is read.
    pub auth_token_env: Option<String>,
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
            credential_commands: std::collections::BTreeMap::new(),
            api_key_env: None,
            auth_token_env: None,
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

    /// The environment variable an API key is read from: the configured
    /// `api_key_env` if set, else the conventional `ANTHROPIC_API_KEY`.
    pub fn api_key_env(&self) -> &str {
        self.api_key_env.as_deref().unwrap_or(DEFAULT_API_KEY_ENV)
    }

    /// The environment variable an OAuth/subscription bearer token is read
    /// from: the configured `auth_token_env` if set, else the conventional
    /// `ANTHROPIC_AUTH_TOKEN`.
    pub fn auth_token_env(&self) -> &str {
        self.auth_token_env
            .as_deref()
            .unwrap_or(DEFAULT_AUTH_TOKEN_ENV)
    }

    /// The command a `Cli`-delegated `label` runs to fetch its token: an
    /// explicit `credential_commands` entry if non-empty, else the default
    /// `ant` CLI. Never returns an empty command, so callers always have a
    /// program to run.
    pub fn credential_command(&self, label: &str) -> Vec<String> {
        match self.credential_commands.get(label) {
            Some(argv) if !argv.is_empty() => argv.clone(),
            _ => DEFAULT_CREDENTIAL_COMMAND
                .iter()
                .map(|part| (*part).to_string())
                .collect(),
        }
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

/// The shared shape of a local provider's config: where the server listens,
/// the fallback model, per-tier and per-agent model overrides. Local providers
/// carry no credential fields; a local server needs no key. `model` is empty
/// by default because aarg ships no default local model name; a local provider
/// is unusable until the user names one (see [`Config::active_model`]).
///
/// LM Studio (`OpenAiCompatClient`) needs exactly these fields; Ollama adds two
/// of its own, so it has its own struct rather than reusing this one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LmStudioConfig {
    /// The server's base URL, e.g. `http://127.0.0.1:1234`.
    pub base_url: String,
    /// The fallback model id, used for any tier not pinned in `tiers`. Empty
    /// until the user sets one; there is no shipped default.
    pub model: String,
    /// Maps the three tiers to concrete model ids.
    pub tiers: ModelTiers,
    /// Per-agent overrides by agent id, the highest-priority resolution step.
    pub agents: std::collections::BTreeMap<String, String>,
}

impl Default for LmStudioConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_LMSTUDIO_BASE_URL.to_string(),
            model: String::new(),
            tiers: ModelTiers::default(),
            agents: std::collections::BTreeMap::new(),
        }
    }
}

impl crate::agent::ModelResolver for LmStudioConfig {
    fn resolve(&self, agent_id: &str, tier: crate::agent::ModelTier) -> &str {
        resolve_local(&self.agents, &self.tiers, &self.model, agent_id, tier)
    }
}

/// Ollama's config: the local shape plus the two knobs its native API exposes,
/// the context-window floor and how long the model stays resident.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OllamaConfig {
    /// The server's base URL, e.g. `http://127.0.0.1:11434`.
    pub base_url: String,
    /// The fallback model id. Empty until the user sets one.
    pub model: String,
    pub tiers: ModelTiers,
    pub agents: std::collections::BTreeMap<String, String>,
    /// The context-window floor in tokens. `None` uses the client default
    /// (8192); each request still grows the window above this when the prompt
    /// needs it (see the Ollama client), so this is a minimum, not a cap.
    pub num_ctx: Option<u32>,
    /// How long the model stays loaded after a request (Ollama duration syntax,
    /// e.g. `"5m"`, `"30s"`, `"0"` to unload at once). `None` uses the client
    /// default (`"5m"`).
    pub keep_alive: Option<String>,
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_OLLAMA_BASE_URL.to_string(),
            model: String::new(),
            tiers: ModelTiers::default(),
            agents: std::collections::BTreeMap::new(),
            num_ctx: None,
            keep_alive: None,
        }
    }
}

impl crate::agent::ModelResolver for OllamaConfig {
    fn resolve(&self, agent_id: &str, tier: crate::agent::ModelTier) -> &str {
        resolve_local(&self.agents, &self.tiers, &self.model, agent_id, tier)
    }
}

/// The tier resolution local providers share: a per-agent pin wins, then the
/// tier mapping, then the fallback `model`. Unlike Anthropic, no tier drops to
/// a shipped default, since a local build has no default model to reach for; an
/// unset tier and unset `model` resolve to the empty string, which the caller
/// rejects up front (see [`Config::active_model`]).
fn resolve_local<'a>(
    agents: &'a std::collections::BTreeMap<String, String>,
    tiers: &'a ModelTiers,
    model: &'a str,
    agent_id: &str,
    tier: crate::agent::ModelTier,
) -> &'a str {
    use crate::agent::ModelTier;
    if let Some(pinned) = agents.get(agent_id) {
        return pinned;
    }
    match tier {
        ModelTier::Cheap => tiers.cheap.as_deref().unwrap_or(model),
        ModelTier::Mid => tiers.mid.as_deref().unwrap_or(model),
        ModelTier::Smart => tiers.smart.as_deref().unwrap_or(model),
    }
}

/// How the active provider's requests are billed, derived from `Config` alone
/// (no keychain read). Drives every cost surface: a per-token dollar figure for
/// a metered API key, and a plain note for the two cases where a dollar figure
/// would mislead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Billing {
    /// A Claude plan (OAuth or CLI-delegated), covered by the flat fee.
    Subscription,
    /// A pay-as-you-go Anthropic API key, priced per token.
    Metered,
    /// A local model; requests cost nothing.
    Local,
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

/// Where `aarg export` writes a build's PDFs when `--to` is omitted. A
/// `None` (the default) exports to the current directory, so the feature
/// works with no setup; set `dir` to point every export at one folder.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportConfig {
    pub dir: Option<PathBuf>,
}

/// Where to find the `typst` binary `aarg` shells out to. `None` (the default)
/// resolves typst automatically: on `PATH`, then next to aarg's own binary and
/// the usual `~/.cargo/bin` / `~/.local/bin` / `/usr/local/bin`. Set `typst` to
/// pin an explicit path — useful when aarg runs somewhere typst isn't on `PATH`
/// (e.g. an MCP server launched over SSH). The `AARG_TYPST` environment
/// variable overrides this for a single run.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RenderConfig {
    pub typst: Option<String>,
}

/// The contents of `config.toml`. Any field missing from the file falls
/// back to its default, so an empty file is a valid config.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub provider: Provider,
    /// The directory aarg uses as its workspace — the file-based equivalent of
    /// the `AARG_DIR` environment variable. Set this in the **global** config
    /// (the per-OS default location) to point aarg at a project directory whose
    /// `.aarg/` subfolder holds your config, dataset, and builds, so you don't
    /// need the env var. A leading `~/` is expanded. `None` (the default)
    /// leaves resolution to `AARG_DIR`, a discovered `.aarg/`, or the per-OS
    /// defaults. It is read during workspace resolution straight from the
    /// global file (see the `workspace` module, which can't depend on this type
    /// without a cycle); it lives here too so it round-trips and shows in
    /// `aarg config`.
    pub workspace: Option<PathBuf>,
    pub anthropic: AnthropicConfig,
    /// Settings for the LM Studio (OpenAI-compatible) local provider. Read only
    /// when `provider = "lmstudio"`; an absent `[lmstudio]` table is the default.
    pub lmstudio: LmStudioConfig,
    /// Settings for the Ollama local provider. Read only when
    /// `provider = "ollama"`; an absent `[ollama]` table is the default.
    pub ollama: OllamaConfig,
    pub limits: Limits,
    /// Which template each variant renders with.
    pub templates: TemplatesConfig,
    /// Where `aarg export` writes friendly-named PDFs by default.
    pub export: ExportConfig,
    /// Where to find the `typst` binary (auto-resolved when unset).
    pub render: RenderConfig,
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

    /// The model resolver for the active provider, so callers map an agent +
    /// tier to a model without reaching for a provider-specific config section.
    pub fn active_resolver(&self) -> &dyn crate::agent::ModelResolver {
        match self.provider {
            Provider::Anthropic => &self.anthropic,
            Provider::LmStudio => &self.lmstudio,
            Provider::Ollama => &self.ollama,
        }
    }

    /// The active provider's fallback model: what a single-model command (like
    /// `aarg llm ping`) sends. Empty when a local provider has no model set,
    /// which the caller treats as "not configured yet".
    pub fn active_model(&self) -> &str {
        match self.provider {
            Provider::Anthropic => &self.anthropic.model,
            Provider::LmStudio => &self.lmstudio.model,
            Provider::Ollama => &self.ollama.model,
        }
    }

    /// The active provider's base URL, for a local provider. `None` for
    /// Anthropic, which the client points at the hosted API itself. Trailing
    /// slashes are trimmed, matching the clients' own normalization, so every
    /// caller that joins a path onto this (the ping probes, the init model
    /// list) is safe against a hand-edited `base_url = "http://host:1234/"`.
    pub fn active_base_url(&self) -> Option<&str> {
        match self.provider {
            Provider::Anthropic => None,
            Provider::LmStudio => Some(self.lmstudio.base_url.trim_end_matches('/')),
            Provider::Ollama => Some(self.ollama.base_url.trim_end_matches('/')),
        }
    }

    /// How the active provider's requests are billed, from config alone. A
    /// local provider is always [`Billing::Local`]; Anthropic is a subscription
    /// when the resolved credential is a plan token (env override first, then
    /// the active label's kind), otherwise metered.
    pub fn billing(&self) -> Billing {
        if self.provider.is_local() {
            return Billing::Local;
        }
        if env_var_set(self.anthropic.auth_token_env()) {
            return Billing::Subscription; // the OAuth env var is always a plan token
        }
        if env_var_set(self.anthropic.api_key_env()) {
            return Billing::Metered; // the API-key env var is never a plan token
        }
        let override_label = std::env::var("AARG_KEY").ok();
        let label = override_label
            .as_deref()
            .unwrap_or_else(|| self.anthropic.active_label());
        match self.anthropic.kind_for(label) {
            AuthKind::Oauth | AuthKind::Cli => Billing::Subscription,
            AuthKind::ApiKey => Billing::Metered,
        }
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

/// Whether an environment variable is set to a non-empty value, the same
/// "unset or blank means absent" rule the credential resolution uses, so an
/// exported-but-empty var doesn't read as present.
fn env_var_set(var: &str) -> bool {
    std::env::var(var).is_ok_and(|value| !value.is_empty())
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
    fn credential_command_defaults_to_ant_and_honors_overrides() {
        let mut config = AnthropicConfig::default();
        // No override: the default `ant` CLI command, and never empty.
        assert_eq!(
            config.credential_command("subscription"),
            vec![
                "ant".to_string(),
                "auth".to_string(),
                "print-credentials".to_string(),
                "--access-token".to_string(),
            ]
        );

        // An explicit per-label command wins — this is what lets a headless
        // deployment point at a file / `pass` / vault instead of the keychain.
        config.credential_commands.insert(
            "subscription".to_string(),
            vec!["cat".to_string(), "/home/me/.config/aarg/token".to_string()],
        );
        assert_eq!(
            config.credential_command("subscription"),
            vec!["cat".to_string(), "/home/me/.config/aarg/token".to_string()]
        );

        // An empty override falls back to the default rather than yielding a
        // command with no program to run.
        config
            .credential_commands
            .insert("blank".to_string(), Vec::new());
        assert_eq!(config.credential_command("blank")[0], "ant");

        // And the override survives a TOML round trip.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let full = Config {
            anthropic: config.clone(),
            ..Config::default()
        };
        full.save_to(&path).unwrap();
        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(
            loaded.anthropic.credential_command("subscription"),
            vec!["cat".to_string(), "/home/me/.config/aarg/token".to_string()]
        );
    }

    #[test]
    fn auth_env_names_default_to_standard_then_honor_overrides() {
        // Unset by default: the conventional Anthropic SDK/CLI names, so an
        // existing config keeps reading the standard vars.
        let defaults = AnthropicConfig::default();
        assert_eq!(defaults.api_key_env(), "ANTHROPIC_API_KEY");
        assert_eq!(defaults.auth_token_env(), "ANTHROPIC_AUTH_TOKEN");

        // A private name decouples AARG from the standard vars, so they can be
        // left unset for Claude Code's own subscription login.
        let custom = AnthropicConfig {
            api_key_env: Some("AARG_ANTHROPIC_API_KEY".to_string()),
            auth_token_env: Some("AARG_ANTHROPIC_AUTH_TOKEN".to_string()),
            ..AnthropicConfig::default()
        };
        assert_eq!(custom.api_key_env(), "AARG_ANTHROPIC_API_KEY");
        assert_eq!(custom.auth_token_env(), "AARG_ANTHROPIC_AUTH_TOKEN");

        // The override survives a TOML round trip.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let full = Config {
            anthropic: custom.clone(),
            ..Config::default()
        };
        full.save_to(&path).unwrap();
        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded.anthropic, custom);
    }

    #[test]
    fn export_dir_round_trips_and_defaults_to_none() {
        // Unset by default — export then targets the current directory.
        assert_eq!(ExportConfig::default().dir, None);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let config = Config {
            export: ExportConfig {
                dir: Some(PathBuf::from("/home/me/applications")),
            },
            ..Config::default()
        };
        config.save_to(&path).unwrap();
        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded, config);
        assert_eq!(
            loaded.export.dir,
            Some(PathBuf::from("/home/me/applications"))
        );
    }

    #[test]
    fn workspace_redirect_round_trips_and_defaults_to_none() {
        // Unset by default — resolution falls to AARG_DIR / discovery / global.
        assert_eq!(Config::default().workspace, None);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let config = Config {
            workspace: Some(PathBuf::from("/home/me/aarg")),
            ..Config::default()
        };
        config.save_to(&path).unwrap();
        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded, config);
        assert_eq!(loaded.workspace, Some(PathBuf::from("/home/me/aarg")));
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

    #[test]
    fn provider_names_match_their_config_sections() {
        assert_eq!(Provider::Anthropic.name(), "anthropic");
        assert_eq!(Provider::LmStudio.name(), "lmstudio");
        assert_eq!(Provider::Ollama.name(), "ollama");
        assert!(!Provider::Anthropic.is_local());
        assert!(Provider::LmStudio.is_local());
        assert!(Provider::Ollama.is_local());
    }

    #[test]
    fn local_provider_round_trips_and_defaults_to_loopback() {
        // The two local sections default to their servers' loopback ports with
        // no model set, and are absent from a fresh config.
        let defaults = Config::default();
        assert_eq!(defaults.lmstudio.base_url, "http://127.0.0.1:1234");
        assert_eq!(defaults.ollama.base_url, "http://127.0.0.1:11434");
        assert!(defaults.lmstudio.model.is_empty());
        assert!(defaults.ollama.model.is_empty());
        assert_eq!(defaults.ollama.num_ctx, None);
        assert_eq!(defaults.ollama.keep_alive, None);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let config = Config {
            provider: Provider::Ollama,
            ollama: OllamaConfig {
                base_url: "http://127.0.0.1:9999".to_string(),
                model: "qwen3:8b".to_string(),
                num_ctx: Some(16384),
                keep_alive: Some("30m".to_string()),
                ..OllamaConfig::default()
            },
            ..Config::default()
        };
        config.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path).unwrap(), config);
    }

    #[test]
    fn a_lowercase_provider_name_parses() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "provider = \"lmstudio\"\n").unwrap();
        assert_eq!(
            Config::load_from(&path).unwrap().provider,
            Provider::LmStudio
        );
        std::fs::write(&path, "provider = \"ollama\"\n").unwrap();
        assert_eq!(Config::load_from(&path).unwrap().provider, Provider::Ollama);
    }

    #[test]
    fn local_resolve_falls_back_to_the_model_never_a_shipped_default() {
        use crate::agent::{ModelResolver, ModelTier};
        // No tiers pinned: every tier resolves to the fallback model, not to a
        // hardcoded Anthropic default.
        let config = LmStudioConfig {
            model: "qwen2.5-coder".to_string(),
            ..LmStudioConfig::default()
        };
        assert_eq!(config.resolve("any", ModelTier::Cheap), "qwen2.5-coder");
        assert_eq!(config.resolve("any", ModelTier::Smart), "qwen2.5-coder");
        // An unset model resolves to empty, which the caller rejects.
        assert_eq!(
            LmStudioConfig::default().resolve("any", ModelTier::Cheap),
            ""
        );
        // A per-agent pin still wins over the tier.
        let pinned = OllamaConfig {
            model: "base".to_string(),
            agents: std::collections::BTreeMap::from([(
                "tailoring_v1".to_string(),
                "big-model".to_string(),
            )]),
            ..OllamaConfig::default()
        };
        assert_eq!(
            pinned.resolve("tailoring_v1", ModelTier::Smart),
            "big-model"
        );
        assert_eq!(pinned.resolve("other", ModelTier::Smart), "base");
    }

    #[test]
    fn active_resolver_and_model_follow_the_provider() {
        use crate::agent::ModelTier;
        let config = Config {
            provider: Provider::LmStudio,
            lmstudio: LmStudioConfig {
                model: "local-model".to_string(),
                ..LmStudioConfig::default()
            },
            ..Config::default()
        };
        assert_eq!(config.active_model(), "local-model");
        assert_eq!(config.active_base_url(), Some("http://127.0.0.1:1234"));
        // A hand-edited trailing slash is trimmed, so path joins can't double
        // a slash (which LM Studio answers with a 200 error body).
        let slashed = Config {
            provider: Provider::Ollama,
            ollama: OllamaConfig {
                base_url: "http://127.0.0.1:11434/".to_string(),
                ..OllamaConfig::default()
            },
            ..Config::default()
        };
        assert_eq!(slashed.active_base_url(), Some("http://127.0.0.1:11434"));
        assert_eq!(
            config.active_resolver().resolve("any", ModelTier::Smart),
            "local-model"
        );
        // Anthropic has no base URL to show and keeps its own default model.
        let anthropic = Config::default();
        assert_eq!(anthropic.active_base_url(), None);
        assert_eq!(anthropic.active_model(), DEFAULT_ANTHROPIC_MODEL);
    }

    #[test]
    fn billing_is_local_for_local_providers() {
        // A local provider is free regardless of any Anthropic credential env
        // vars the test process happens to carry, so this holds unconditionally.
        for provider in [Provider::LmStudio, Provider::Ollama] {
            let config = Config {
                provider,
                ..Config::default()
            };
            assert_eq!(config.billing(), Billing::Local);
        }
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
