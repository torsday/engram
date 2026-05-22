//! Configuration loading for engram.
//!
//! Two config files exist in every vault:
//!
//! - **`.engram/config.toml`** — top-level vault config (models, embeddings,
//!   privacy zones, cost limits, backup, etc.).  Loaded via [`EngramConfig::load`].
//! - **`agents/<name>/config.toml`** — per-agent config (schedule, permissions,
//!   autonomy thresholds, budget, etc.).  Loaded via [`AgentConfig::load`].
//!
//! Both use `#[serde(deny_unknown_fields)]` so typos fail loudly with a
//! descriptive error.  Defaults are defined via `#[serde(default)]` with named
//! functions — single source of truth, no scattered magic constants.
//!
//! # Hot-reload classification
//!
//! Each top-level section is classified as either **hot-reloadable** (can be
//! applied to a running daemon without restart) or **restart-required**.  The
//! classification is stored in [`HOT_RELOADABLE_SECTIONS`] and exposed by
//! [`EngramConfig::changed_sections`].  The daemon's config watcher calls this
//! to decide whether to apply changes in place or emit a restart notice.
//!
//! # Example
//!
//! ```no_run
//! use std::path::Path;
//! use engram_core::config::EngramConfig;
//!
//! let cfg = EngramConfig::load(Path::new("/vault")).unwrap();
//! println!("monthly cap: {}", cfg.cost.monthly_usd_cap);
//! ```

use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors from config loading or validation.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The config file could not be read from disk.
    #[error("cannot read config at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    /// The TOML could not be parsed; includes line/column context from the
    /// `toml` crate.
    #[error("invalid TOML in {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },

    /// A required field is absent.
    #[error("missing required field '{field}' in {path}")]
    MissingField { field: &'static str, path: String },
}

// ---------------------------------------------------------------------------
// Hot-reload classification
// ---------------------------------------------------------------------------

/// Top-level config sections that can be applied to a running daemon **without
/// restarting** the engram process.
///
/// Sections **not** in this list require a restart (`engram status` will surface
/// the notice after the next load).
pub const HOT_RELOADABLE_SECTIONS: &[&str] = &[
    "cost",      // cost caps and alert thresholds
    "backup",    // backup recency thresholds
    "scout",     // feed polling intervals
    "user",      // timezone, locale preferences
    "artifacts", // artifact retention
];

// ---------------------------------------------------------------------------
// EngramConfig — .engram/config.toml
// ---------------------------------------------------------------------------

/// Top-level vault configuration, loaded from `.engram/config.toml`.
///
/// All fields use `#[serde(default)]` with named functions so that a minimal
/// config (even an empty file) produces a valid, usable struct.
///
/// Unknown keys in the TOML cause a parse error (`deny_unknown_fields`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct EngramConfig {
    /// Model tier → provider+model mappings.
    #[serde(default)]
    pub models: ModelsConfig,

    /// Embedding model configuration.
    #[serde(default)]
    pub embeddings: EmbeddingsConfig,

    /// Privacy zone configuration.
    #[serde(default)]
    pub privacy: PrivacyConfig,

    /// Cost limits and alerting.
    ///
    /// **Hot-reloadable** — changes take effect without restart.
    #[serde(default)]
    pub cost: CostConfig,

    /// Backup recency monitoring.
    ///
    /// **Hot-reloadable.**
    #[serde(default)]
    pub backup: BackupConfig,

    /// User preferences (timezone, locale).
    ///
    /// **Hot-reloadable.**
    #[serde(default)]
    pub user: UserConfig,

    /// Scout agent feed polling configuration.
    ///
    /// **Hot-reloadable.**
    #[serde(default)]
    pub scout: ScoutConfig,

    /// Artifact retention policy.
    ///
    /// **Hot-reloadable.**
    #[serde(default)]
    pub artifacts: ArtifactsConfig,

    /// MCP server configuration.
    ///
    /// **Restart-required.**
    #[serde(default)]
    pub mcp: McpConfig,
}

impl EngramConfig {
    /// Load and validate the vault config from `<vault_root>/.engram/config.toml`.
    ///
    /// An absent file is treated as an empty file (all defaults apply).
    /// A present-but-invalid file is a hard error.
    pub fn load(vault_root: &Path) -> Result<Self, ConfigError> {
        let path = vault_root.join(".engram").join("config.toml");
        let path_str = path.display().to_string();

        let contents = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => {
                return Err(ConfigError::Io {
                    path: path_str,
                    source: e,
                })
            }
        };

        toml::from_str(&contents).map_err(|e| ConfigError::Parse {
            path: path_str,
            source: e,
        })
    }

    /// Return the names of top-level sections that differ between `self` and
    /// `other`.
    ///
    /// Used by the hot-reload watcher to decide which changes can be applied
    /// in place and which require a restart notice.
    pub fn changed_sections(&self, other: &EngramConfig) -> ChangedSections {
        let mut hot = Vec::new();
        let mut cold = Vec::new();

        macro_rules! check {
            ($field:ident, $name:literal) => {
                if self.$field != other.$field {
                    if HOT_RELOADABLE_SECTIONS.contains(&$name) {
                        hot.push($name);
                    } else {
                        cold.push($name);
                    }
                }
            };
        }

        check!(models, "models");
        check!(embeddings, "embeddings");
        check!(privacy, "privacy");
        check!(cost, "cost");
        check!(backup, "backup");
        check!(user, "user");
        check!(scout, "scout");
        check!(artifacts, "artifacts");

        ChangedSections { hot, cold }
    }
}

/// Output of [`EngramConfig::changed_sections`].
#[derive(Debug, Clone, PartialEq)]
pub struct ChangedSections {
    /// Sections that changed and can be applied without restarting.
    pub hot: Vec<&'static str>,
    /// Sections that changed and require a restart to take effect.
    pub cold: Vec<&'static str>,
}

impl ChangedSections {
    /// `true` if no sections changed.
    pub fn is_empty(&self) -> bool {
        self.hot.is_empty() && self.cold.is_empty()
    }
}

// ---------------------------------------------------------------------------
// ModelsConfig
// ---------------------------------------------------------------------------

/// Tier → provider+model mapping.
///
/// **Restart-required** — changing the model used by running agents requires
/// their task loops to restart with the new handle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelsConfig {
    /// Cloud model tiers.
    #[serde(default)]
    pub fast: ModelEntry,
    #[serde(default = "ModelEntry::default_standard")]
    pub standard: ModelEntry,
    #[serde(default = "ModelEntry::default_deep")]
    pub deep: ModelEntry,
    /// Local model overrides (used when `privacy: local-only` is set).
    #[serde(default)]
    pub local: LocalModelsConfig,
}

impl Default for ModelsConfig {
    fn default() -> Self {
        Self {
            fast: ModelEntry::default(),
            standard: ModelEntry::default_standard(),
            deep: ModelEntry::default_deep(),
            local: LocalModelsConfig::default(),
        }
    }
}

/// A provider + model pair.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelEntry {
    pub provider: String,
    pub model: String,
}

impl Default for ModelEntry {
    fn default() -> Self {
        Self {
            provider: "anthropic".to_owned(),
            model: "claude-haiku-4-5".to_owned(),
        }
    }
}

impl ModelEntry {
    fn default_standard() -> Self {
        Self {
            provider: "anthropic".to_owned(),
            model: "claude-sonnet-4-5".to_owned(),
        }
    }

    fn default_deep() -> Self {
        Self {
            provider: "anthropic".to_owned(),
            model: "claude-opus-4-5".to_owned(),
        }
    }
}

/// Local model tier mappings (used when `privacy: local-only`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalModelsConfig {
    #[serde(default = "LocalModelsConfig::default_fast")]
    pub fast: ModelEntry,
    #[serde(default = "LocalModelsConfig::default_standard")]
    pub standard: ModelEntry,
    #[serde(default = "LocalModelsConfig::default_deep")]
    pub deep: ModelEntry,
}

impl Default for LocalModelsConfig {
    fn default() -> Self {
        Self {
            fast: Self::default_fast(),
            standard: Self::default_standard(),
            deep: Self::default_deep(),
        }
    }
}

impl LocalModelsConfig {
    fn default_fast() -> ModelEntry {
        ModelEntry {
            provider: "ollama".to_owned(),
            model: "llama3.2:3b".to_owned(),
        }
    }
    fn default_standard() -> ModelEntry {
        ModelEntry {
            provider: "ollama".to_owned(),
            model: "llama3.2:8b".to_owned(),
        }
    }
    fn default_deep() -> ModelEntry {
        ModelEntry {
            provider: "ollama".to_owned(),
            model: "llama3.3:70b".to_owned(),
        }
    }
}

// ---------------------------------------------------------------------------
// EmbeddingsConfig
// ---------------------------------------------------------------------------

/// Embedding model configuration.
///
/// **Restart-required** — changing the embedding model requires a re-embedding
/// migration pass; the daemon enforces this.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingsConfig {
    #[serde(default = "EmbeddingsConfig::default_provider")]
    pub provider: String,
    #[serde(default = "EmbeddingsConfig::default_model")]
    pub model: String,
    #[serde(default = "EmbeddingsConfig::default_dimensions")]
    pub dimensions: u32,
}

impl Default for EmbeddingsConfig {
    fn default() -> Self {
        Self {
            provider: Self::default_provider(),
            model: Self::default_model(),
            dimensions: Self::default_dimensions(),
        }
    }
}

impl EmbeddingsConfig {
    fn default_provider() -> String {
        "local".to_owned()
    }
    fn default_model() -> String {
        "bge-m3".to_owned()
    }
    fn default_dimensions() -> u32 {
        1024
    }
}

// ---------------------------------------------------------------------------
// PrivacyConfig
// ---------------------------------------------------------------------------

/// Privacy zone configuration.
///
/// **Restart-required** — privacy zones control which notes are excluded from
/// external MCP; changing them requires the MCP server to restart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivacyConfig {
    /// Note paths excluded from all external-facing surfaces (MCP, cloud LLM).
    #[serde(default = "PrivacyConfig::default_excluded_paths")]
    pub excluded_paths: Vec<String>,
}

impl Default for PrivacyConfig {
    fn default() -> Self {
        Self {
            excluded_paths: Self::default_excluded_paths(),
        }
    }
}

impl PrivacyConfig {
    fn default_excluded_paths() -> Vec<String> {
        vec![
            "notes/work/".to_owned(),
            "notes/medical/".to_owned(),
            "notes/journal/".to_owned(),
        ]
    }
}

// ---------------------------------------------------------------------------
// McpConfig
// ---------------------------------------------------------------------------

/// MCP server configuration.
///
/// **Restart-required** — controls whether the MCP stdio server starts
/// automatically when `engram serve` launches (without `--mcp-stdio`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpConfig {
    /// Whether the MCP stdio server is enabled when running `engram serve`.
    ///
    /// Defaults to `false` (opt-in). Pass `--mcp-stdio` on the CLI to
    /// override at runtime regardless of this value.
    #[serde(default)]
    pub enabled: bool,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self { enabled: false }
    }
}

// ---------------------------------------------------------------------------
// CostConfig
// ---------------------------------------------------------------------------

/// Cost limits and alerting.
///
/// **Hot-reloadable** — cost caps can be adjusted while the daemon is running.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CostConfig {
    /// Hard monthly USD cap.  The system pauses all agent work when exceeded.
    #[serde(default = "CostConfig::default_monthly_usd_cap")]
    pub monthly_usd_cap: f64,

    /// Fraction of cap at which a warning is emitted (0.0–1.0).
    #[serde(default = "CostConfig::default_warning_threshold")]
    pub warning_threshold: f64,

    /// Token-to-USD conversion source.
    #[serde(default = "CostConfig::default_provider_cost_table")]
    pub provider_cost_table: String,

    /// Alerting configuration.
    #[serde(default)]
    pub alert: CostAlertConfig,
}

impl Default for CostConfig {
    fn default() -> Self {
        Self {
            monthly_usd_cap: Self::default_monthly_usd_cap(),
            warning_threshold: Self::default_warning_threshold(),
            provider_cost_table: Self::default_provider_cost_table(),
            alert: CostAlertConfig::default(),
        }
    }
}

impl CostConfig {
    fn default_monthly_usd_cap() -> f64 {
        25.0
    }
    fn default_warning_threshold() -> f64 {
        0.75
    }
    fn default_provider_cost_table() -> String {
        "default".to_owned()
    }
}

/// Alerting thresholds for cost events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CostAlertConfig {
    #[serde(default = "bool_true")]
    pub notify_swift_app: bool,
    #[serde(default = "bool_true")]
    pub include_in_standup: bool,
}

impl Default for CostAlertConfig {
    fn default() -> Self {
        Self {
            notify_swift_app: true,
            include_in_standup: true,
        }
    }
}

// ---------------------------------------------------------------------------
// BackupConfig
// ---------------------------------------------------------------------------

/// Backup recency monitoring thresholds.
///
/// **Hot-reloadable.**
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupConfig {
    /// Warn if the git remote hasn't been pushed to in this many hours.
    #[serde(default = "BackupConfig::default_remote_stale_hours")]
    pub remote_stale_hours: u32,

    /// Warn if the filesystem snapshot is older than this many days.
    #[serde(default = "BackupConfig::default_snapshot_stale_days")]
    pub snapshot_stale_days: u32,
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            remote_stale_hours: Self::default_remote_stale_hours(),
            snapshot_stale_days: Self::default_snapshot_stale_days(),
        }
    }
}

impl BackupConfig {
    fn default_remote_stale_hours() -> u32 {
        24
    }
    fn default_snapshot_stale_days() -> u32 {
        7
    }
}

// ---------------------------------------------------------------------------
// UserConfig
// ---------------------------------------------------------------------------

/// User preference settings.
///
/// **Hot-reloadable.**
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserConfig {
    /// IANA timezone string, e.g. `"America/New_York"`.
    #[serde(default = "UserConfig::default_timezone")]
    pub timezone: String,
}

impl Default for UserConfig {
    fn default() -> Self {
        Self {
            timezone: Self::default_timezone(),
        }
    }
}

impl UserConfig {
    fn default_timezone() -> String {
        "UTC".to_owned()
    }
}

// ---------------------------------------------------------------------------
// ScoutConfig
// ---------------------------------------------------------------------------

/// Scout agent feed-polling configuration.
///
/// **Hot-reloadable** — new feeds can be added or removed while the daemon
/// runs; the scout task manager picks up the diff on the next reload event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScoutConfig {
    /// Polling interval in seconds (applied as a staggered jitter across feeds).
    #[serde(default = "ScoutConfig::default_poll_interval_seconds")]
    pub poll_interval_seconds: u32,

    /// Maximum number of feeds to poll concurrently.
    #[serde(default = "ScoutConfig::default_max_concurrent")]
    pub max_concurrent: u32,
}

impl Default for ScoutConfig {
    fn default() -> Self {
        Self {
            poll_interval_seconds: Self::default_poll_interval_seconds(),
            max_concurrent: Self::default_max_concurrent(),
        }
    }
}

impl ScoutConfig {
    fn default_poll_interval_seconds() -> u32 {
        3600
    }
    fn default_max_concurrent() -> u32 {
        4
    }
}

// ---------------------------------------------------------------------------
// ArtifactsConfig
// ---------------------------------------------------------------------------

/// Artifact retention policy.
///
/// **Hot-reloadable** — retention policy changes take effect on the next
/// garbage-collection pass.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactsConfig {
    /// Number of days to retain deliberation transcripts.
    #[serde(default = "ArtifactsConfig::default_deliberation_retention_days")]
    pub deliberation_retention_days: u32,
}

impl Default for ArtifactsConfig {
    fn default() -> Self {
        Self {
            deliberation_retention_days: Self::default_deliberation_retention_days(),
        }
    }
}

impl ArtifactsConfig {
    fn default_deliberation_retention_days() -> u32 {
        90
    }
}

// ---------------------------------------------------------------------------
// AgentConfig — agents/<name>/config.toml
// ---------------------------------------------------------------------------

/// Per-agent configuration, loaded from `agents/<name>/config.toml`.
///
/// Mirrors the Linker example in `docs/design/01-agents-and-council.md`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    /// Core agent identity.
    pub agent: AgentIdentity,

    /// Scheduling and trigger configuration.
    #[serde(default)]
    pub schedule: AgentSchedule,

    /// Vault mutation permissions.
    #[serde(default)]
    pub permissions: AgentPermissions,

    /// Confidence-gated autonomy thresholds.
    #[serde(default)]
    pub autonomy: AgentAutonomy,

    /// Council participation.
    #[serde(default)]
    pub council: AgentCouncil,

    /// Agent memory settings.
    #[serde(default)]
    pub memory: AgentMemory,

    /// Trust level and promotion criteria.
    #[serde(default)]
    pub trust: AgentTrust,

    /// Token budget.
    #[serde(default)]
    pub budget: AgentBudget,

    /// Conversation capability.
    #[serde(default)]
    pub conversation: AgentConversation,
}

impl AgentConfig {
    /// Load an agent config from `<agents_dir>/<agent_name>/config.toml`.
    ///
    /// Unlike `EngramConfig::load`, a **missing file is a hard error** — every
    /// agent must have an explicit config.
    pub fn load(agents_dir: &Path, agent_name: &str) -> Result<Self, ConfigError> {
        let path = agents_dir.join(agent_name).join("config.toml");
        let path_str = path.display().to_string();

        let contents = std::fs::read_to_string(&path).map_err(|e| ConfigError::Io {
            path: path_str.clone(),
            source: e,
        })?;

        toml::from_str(&contents).map_err(|e| ConfigError::Parse {
            path: path_str,
            source: e,
        })
    }
}

/// Core agent identity fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentIdentity {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "AgentIdentity::default_model_tier")]
    pub model_tier: ModelTier,
}

/// Model tier selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    Fast,
    Standard,
    Deep,
}

impl AgentIdentity {
    fn default_model_tier() -> ModelTier {
        ModelTier::Fast
    }
}

/// Agent scheduling configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSchedule {
    #[serde(default = "AgentSchedule::default_trigger")]
    pub trigger: AgentTrigger,
    /// Only used when `trigger = "cron"`.
    #[serde(default)]
    pub cron: String,
    /// Milliseconds to wait for edits to settle before triggering.
    #[serde(default = "AgentSchedule::default_debounce_seconds")]
    pub debounce_seconds: u32,
}

impl Default for AgentSchedule {
    fn default() -> Self {
        Self {
            trigger: Self::default_trigger(),
            cron: String::new(),
            debounce_seconds: Self::default_debounce_seconds(),
        }
    }
}

impl AgentSchedule {
    fn default_trigger() -> AgentTrigger {
        AgentTrigger::OnDemand
    }
    fn default_debounce_seconds() -> u32 {
        30
    }
}

/// What causes an agent to run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTrigger {
    FileChange,
    Cron,
    OnDemand,
    CouncilOnly,
}

/// Vault mutation permissions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentPermissions {
    #[serde(default)]
    pub may_create_notes: bool,
    #[serde(default = "bool_true")]
    pub may_modify_notes: bool,
    #[serde(default)]
    pub may_delete_notes: bool,
    #[serde(default = "AgentPermissions::default_note_types")]
    pub note_types: Vec<String>,
    #[serde(default)]
    pub max_invasiveness: InvasivenessLevel,
}

impl Default for AgentPermissions {
    fn default() -> Self {
        Self {
            may_create_notes: false,
            may_modify_notes: true,
            may_delete_notes: false,
            note_types: Self::default_note_types(),
            max_invasiveness: InvasivenessLevel::default(),
        }
    }
}

impl AgentPermissions {
    fn default_note_types() -> Vec<String> {
        vec![
            "fleeting".to_owned(),
            "literature".to_owned(),
            "evergreen".to_owned(),
            "moc".to_owned(),
        ]
    }
}

/// How invasive an agent's autonomous actions can be.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum InvasivenessLevel {
    /// Only mechanical changes (whitespace, broken link fix).
    Mechanical,
    /// Add content without removing existing content.
    #[default]
    Additive,
    /// May rewrite or restructure existing content.
    Editorial,
    /// May create/delete notes, restructure vault layout.
    Structural,
}

/// Confidence-gated autonomy thresholds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentAutonomy {
    /// Below this confidence the change becomes a proposal; above, the agent
    /// writes to the working tree (unstaged).
    #[serde(default = "AgentAutonomy::default_auto_land_min_confidence")]
    pub auto_land_min_confidence: f64,
    #[serde(default = "bool_true")]
    pub trust_modulates_threshold: bool,
}

impl Default for AgentAutonomy {
    fn default() -> Self {
        Self {
            auto_land_min_confidence: Self::default_auto_land_min_confidence(),
            trust_modulates_threshold: true,
        }
    }
}

impl AgentAutonomy {
    fn default_auto_land_min_confidence() -> f64 {
        0.85
    }
}

/// Council participation flags.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCouncil {
    #[serde(default = "bool_true")]
    pub participates: bool,
    #[serde(default)]
    pub may_convene: bool,
}

impl Default for AgentCouncil {
    fn default() -> Self {
        Self {
            participates: true,
            may_convene: false,
        }
    }
}

/// Agent memory settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentMemory {
    #[serde(default = "bool_true")]
    pub enabled: bool,
    #[serde(default = "AgentMemory::default_rejection_ttl_days")]
    pub rejection_ttl_days: u32,
    #[serde(default = "AgentMemory::default_max_entries")]
    pub max_entries: u32,
}

impl Default for AgentMemory {
    fn default() -> Self {
        Self {
            enabled: true,
            rejection_ttl_days: Self::default_rejection_ttl_days(),
            max_entries: Self::default_max_entries(),
        }
    }
}

impl AgentMemory {
    fn default_rejection_ttl_days() -> u32 {
        90
    }
    fn default_max_entries() -> u32 {
        10_000
    }
}

/// Trust level and promotion criteria.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTrust {
    #[serde(default)]
    pub initial_level: TrustLevel,
    #[serde(default = "AgentTrust::default_min_decisions_for_promotion")]
    pub min_decisions_for_promotion: u32,
}

impl Default for AgentTrust {
    fn default() -> Self {
        Self {
            initial_level: TrustLevel::default(),
            min_decisions_for_promotion: Self::default_min_decisions_for_promotion(),
        }
    }
}

impl AgentTrust {
    fn default_min_decisions_for_promotion() -> u32 {
        30
    }
}

/// Trust level of an agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    Low,
    #[default]
    Medium,
    High,
}

/// Token budget for an agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentBudget {
    #[serde(default = "AgentBudget::default_monthly_tokens")]
    pub monthly_tokens: u64,
    #[serde(default = "bool_true")]
    pub auto_pause: bool,
}

impl Default for AgentBudget {
    fn default() -> Self {
        Self {
            monthly_tokens: Self::default_monthly_tokens(),
            auto_pause: true,
        }
    }
}

impl AgentBudget {
    fn default_monthly_tokens() -> u64 {
        500_000
    }
}

/// Conversation capability.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct AgentConversation {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub max_rounds: u32,
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Default function for `#[serde(default)]` on bool fields that default `true`.
fn bool_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // -----------------------------------------------------------------------
    // EngramConfig
    // -----------------------------------------------------------------------

    fn write_config(dir: &TempDir, rel: &str, content: &str) {
        let path = dir.path().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn empty_file_loads_with_defaults() {
        let dir = TempDir::new().unwrap();
        write_config(&dir, ".engram/config.toml", "");
        let cfg = EngramConfig::load(dir.path()).expect("should load");
        assert_eq!(cfg.cost.monthly_usd_cap, 25.0);
        assert_eq!(cfg.cost.warning_threshold, 0.75);
        assert_eq!(cfg.embeddings.dimensions, 1024);
        assert_eq!(cfg.user.timezone, "UTC");
    }

    #[test]
    fn missing_file_loads_with_defaults() {
        let dir = TempDir::new().unwrap();
        // Don't create the file at all.
        let cfg = EngramConfig::load(dir.path()).expect("should load");
        assert_eq!(cfg.cost.monthly_usd_cap, 25.0);
    }

    #[test]
    fn minimal_valid_config() {
        let dir = TempDir::new().unwrap();
        write_config(
            &dir,
            ".engram/config.toml",
            r#"
[cost]
monthly_usd_cap = 50.0
"#,
        );
        let cfg = EngramConfig::load(dir.path()).expect("should load");
        assert_eq!(cfg.cost.monthly_usd_cap, 50.0);
        // Other fields retain defaults.
        assert_eq!(cfg.cost.warning_threshold, 0.75);
    }

    #[test]
    fn full_config_round_trips() {
        let dir = TempDir::new().unwrap();
        write_config(
            &dir,
            ".engram/config.toml",
            r#"
[models]
[models.fast]
provider = "anthropic"
model = "claude-haiku"

[models.standard]
provider = "anthropic"
model = "claude-sonnet"

[models.deep]
provider = "anthropic"
model = "claude-opus"

[models.local]
[models.local.fast]
provider = "ollama"
model = "llama3.2:3b"

[models.local.standard]
provider = "ollama"
model = "llama3.2:8b"

[models.local.deep]
provider = "ollama"
model = "llama3.3:70b"

[embeddings]
provider = "openai"
model = "text-embedding-3-small"
dimensions = 1536

[privacy]
excluded_paths = ["notes/work/", "notes/personal/"]

[cost]
monthly_usd_cap = 30.0
warning_threshold = 0.80
provider_cost_table = "default"

[cost.alert]
notify_swift_app = true
include_in_standup = true

[backup]
remote_stale_hours = 48
snapshot_stale_days = 14

[user]
timezone = "America/New_York"

[scout]
poll_interval_seconds = 1800
max_concurrent = 8

[artifacts]
deliberation_retention_days = 180
"#,
        );
        let cfg = EngramConfig::load(dir.path()).expect("should parse full config");
        assert_eq!(cfg.models.fast.model, "claude-haiku");
        assert_eq!(cfg.models.standard.model, "claude-sonnet");
        assert_eq!(cfg.embeddings.provider, "openai");
        assert_eq!(cfg.embeddings.dimensions, 1536);
        assert_eq!(
            cfg.privacy.excluded_paths,
            vec!["notes/work/", "notes/personal/"]
        );
        assert_eq!(cfg.cost.monthly_usd_cap, 30.0);
        assert_eq!(cfg.cost.warning_threshold, 0.80);
        assert_eq!(cfg.backup.remote_stale_hours, 48);
        assert_eq!(cfg.user.timezone, "America/New_York");
        assert_eq!(cfg.scout.poll_interval_seconds, 1800);
        assert_eq!(cfg.artifacts.deliberation_retention_days, 180);
    }

    #[test]
    fn unknown_field_error() {
        let dir = TempDir::new().unwrap();
        write_config(
            &dir,
            ".engram/config.toml",
            r#"[cost]
monthly_usd_cap = 25.0
xxx_unknown_field = "oops"
"#,
        );
        let err = EngramConfig::load(dir.path()).unwrap_err();
        assert!(
            matches!(err, ConfigError::Parse { .. }),
            "expected parse error for unknown field, got: {err}"
        );
        assert!(
            err.to_string().contains("xxx_unknown_field") || err.to_string().contains("unknown"),
            "error should mention the unknown field: {err}"
        );
    }

    #[test]
    fn type_mismatch_error() {
        let dir = TempDir::new().unwrap();
        write_config(
            &dir,
            ".engram/config.toml",
            r#"[cost]
monthly_usd_cap = "not-a-number"
"#,
        );
        let err = EngramConfig::load(dir.path()).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    // -----------------------------------------------------------------------
    // Hot-reload classification
    // -----------------------------------------------------------------------

    #[test]
    fn no_changes_produces_empty_diff() {
        let cfg = EngramConfig::default();
        let diff = cfg.changed_sections(&cfg.clone());
        assert!(diff.is_empty());
    }

    #[test]
    fn cost_change_is_hot_reloadable() {
        let mut before = EngramConfig::default();
        let mut after = EngramConfig::default();
        after.cost.monthly_usd_cap = 999.0;
        before.cost.monthly_usd_cap = 25.0;
        let diff = before.changed_sections(&after);
        assert!(diff.hot.contains(&"cost"));
        assert!(diff.cold.is_empty());
    }

    #[test]
    fn models_change_requires_restart() {
        let mut before = EngramConfig::default();
        let mut after = EngramConfig::default();
        after.models.fast.model = "something-new".to_owned();
        before.models.fast.model = "claude-haiku-4-5".to_owned();
        let diff = before.changed_sections(&after);
        assert!(diff.cold.contains(&"models"));
        assert!(!diff.hot.contains(&"models"));
    }

    #[test]
    fn user_change_is_hot_reloadable() {
        let before = EngramConfig::default();
        let mut after = EngramConfig::default();
        after.user.timezone = "America/Los_Angeles".to_owned();
        let diff = before.changed_sections(&after);
        assert!(diff.hot.contains(&"user"));
        assert!(diff.cold.is_empty());
    }

    // -----------------------------------------------------------------------
    // AgentConfig
    // -----------------------------------------------------------------------

    fn write_agent_config(dir: &TempDir, agent: &str, content: &str) {
        write_config(dir, &format!("agents/{agent}/config.toml"), content);
    }

    #[test]
    fn minimal_agent_config() {
        let dir = TempDir::new().unwrap();
        write_agent_config(
            &dir,
            "linker",
            r#"
[agent]
name = "linker"
description = "Discovers wikilinks"
model_tier = "fast"
"#,
        );
        let cfg = AgentConfig::load(&dir.path().join("agents"), "linker").expect("should parse");
        assert_eq!(cfg.agent.name, "linker");
        assert_eq!(cfg.agent.model_tier, ModelTier::Fast);
        // Check defaults applied.
        assert_eq!(cfg.autonomy.auto_land_min_confidence, 0.85);
        assert_eq!(cfg.budget.monthly_tokens, 500_000);
        assert!(cfg.trust.initial_level == TrustLevel::Medium);
    }

    #[test]
    fn full_agent_config_matches_linker_example() {
        let dir = TempDir::new().unwrap();
        write_agent_config(
            &dir,
            "linker",
            r#"
[agent]
name = "linker"
description = "Discovers and proposes wikilinks between notes"
model_tier = "fast"

[schedule]
trigger = "file_change"
cron = ""
debounce_seconds = 30

[permissions]
may_create_notes = false
may_modify_notes = true
may_delete_notes = false
note_types = ["fleeting", "literature", "evergreen", "moc"]
max_invasiveness = "additive"

[autonomy]
auto_land_min_confidence = 0.85
trust_modulates_threshold = true

[council]
participates = true
may_convene = false

[memory]
enabled = true
rejection_ttl_days = 90
max_entries = 10000

[trust]
initial_level = "medium"
min_decisions_for_promotion = 30

[budget]
monthly_tokens = 500000
auto_pause = true

[conversation]
enabled = false
max_rounds = 0
"#,
        );
        let cfg = AgentConfig::load(&dir.path().join("agents"), "linker").expect("should parse");
        assert_eq!(cfg.agent.name, "linker");
        assert_eq!(cfg.schedule.trigger, AgentTrigger::FileChange);
        assert_eq!(cfg.schedule.debounce_seconds, 30);
        assert!(!cfg.permissions.may_create_notes);
        assert!(cfg.permissions.may_modify_notes);
        assert_eq!(
            cfg.permissions.max_invasiveness,
            InvasivenessLevel::Additive
        );
        assert_eq!(cfg.autonomy.auto_land_min_confidence, 0.85);
        assert!(cfg.council.participates);
        assert!(!cfg.council.may_convene);
        assert!(cfg.memory.enabled);
        assert_eq!(cfg.memory.rejection_ttl_days, 90);
        assert_eq!(cfg.trust.initial_level, TrustLevel::Medium);
        assert_eq!(cfg.trust.min_decisions_for_promotion, 30);
        assert_eq!(cfg.budget.monthly_tokens, 500_000);
        assert!(cfg.budget.auto_pause);
        assert!(!cfg.conversation.enabled);
    }

    #[test]
    fn agent_unknown_field_error() {
        let dir = TempDir::new().unwrap();
        write_agent_config(
            &dir,
            "bad-agent",
            r#"
[agent]
name = "bad"
model_tier = "fast"
xxx_invalid_key = true
"#,
        );
        let err = AgentConfig::load(&dir.path().join("agents"), "bad-agent").unwrap_err();
        assert!(
            matches!(err, ConfigError::Parse { .. }),
            "expected parse error: {err}"
        );
    }

    #[test]
    fn agent_missing_file_is_error() {
        let dir = TempDir::new().unwrap();
        let err = AgentConfig::load(&dir.path().join("agents"), "nonexistent").unwrap_err();
        assert!(
            matches!(err, ConfigError::Io { .. }),
            "expected IO error for missing file, got: {err}"
        );
    }

    #[test]
    fn agent_invalid_enum_variant() {
        let dir = TempDir::new().unwrap();
        write_agent_config(
            &dir,
            "bad-trigger",
            r#"
[agent]
name = "x"
model_tier = "fast"

[schedule]
trigger = "INVALID_TRIGGER"
"#,
        );
        let err = AgentConfig::load(&dir.path().join("agents"), "bad-trigger").unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }
}
