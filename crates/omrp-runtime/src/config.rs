//! OMRP configuration — TOML file at `~/.config/omrp/config.toml`.
//!
//! If the file does not exist `Config::load_default()` returns built-in
//! sensible defaults with three OpenRouter models.

use std::path::{Path, PathBuf};

use omrp_events::event::{Event, ModelSource};
use omrp_types::model::{Model, ModelCapabilities};
use omrp_types::task::TaskType;
use serde::Deserialize;

// ─── TOML schema ─────────────────────────────────────────────────────────────

/// Root config struct — deserialised from `config.toml`.
#[derive(Debug, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub daemon: DaemonConfig,
    /// List of models, one `[[model]]` table per entry.
    #[serde(default)]
    pub model: Vec<ModelConfig>,
}

/// `[daemon]` section.
#[derive(Debug, Deserialize, Default)]
pub struct DaemonConfig {
    /// Path to the event-ledger file (JSON Lines).
    /// Tilde is expanded at load time.
    pub ledger_path: Option<String>,
}

/// One `[[model]]` table.
#[derive(Debug, Deserialize)]
pub struct ModelConfig {
    /// Model ID as used in API calls, e.g. `"anthropic/claude-3.5-sonnet"`.
    pub id: String,
    /// Provider name, e.g. `"openrouter"`.
    pub provider: String,
    /// Task-type strings supported by this model.
    #[serde(default)]
    pub tasks: Vec<String>,
    #[serde(default)]
    pub tool_use: bool,
    #[serde(default)]
    pub vision: bool,
    /// Maximum context window in tokens.
    #[serde(default = "default_ctx")]
    pub ctx: u32,
}

fn default_ctx() -> u32 { 4096 }

// ─── Paths ────────────────────────────────────────────────────────────────────

/// Returns `~/.config/omrp/config.toml` (XDG on Linux/macOS, AppData on Windows).
pub fn default_config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("omrp")
        .join("config.toml")
}

/// Returns `~/.local/share/omrp/ledger.jsonl`.
pub fn default_ledger_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("omrp")
        .join("ledger.jsonl")
}

// ─── Load / generate ─────────────────────────────────────────────────────────

impl Config {
    /// Load config from the given path.
    /// Returns an error string if the file exists but can't be parsed.
    pub fn load(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
        toml::from_str(&content)
            .map_err(|e| format!("Config parse error in {}: {e}", path.display()))
    }

    /// Load from the default XDG path, or return built-in defaults if missing.
    pub fn load_or_default() -> (Self, bool) {
        let path = default_config_path();
        if path.exists() {
            match Self::load(&path) {
                Ok(cfg) => (cfg, false),
                Err(e) => {
                    eprintln!("Warning: {e}  (using built-in defaults)");
                    (Self::builtin_defaults(), false)
                }
            }
        } else {
            (Self::builtin_defaults(), true) // true = file was missing
        }
    }

    /// Built-in model list — used when no config file exists.
    ///
    /// Leads with free-tier models (`:free` suffix on OpenRouter) so the
    /// router works out of the box even on a zero-credit account.
    /// Paid models are listed last as fallbacks when free slots are full.
    pub fn builtin_defaults() -> Self {
        Self {
            daemon: DaemonConfig { ledger_path: None },
            model: vec![
                // ── Free-tier models (no credits required) ──────────────────
                ModelConfig {
                    id: "openai/gpt-oss-20b:free".into(),
                    provider: "openrouter".into(),
                    tasks: vec!["chat".into(), "code".into(), "reasoning".into()],
                    tool_use: false,
                    vision: false,
                    ctx: 128_000,
                },
                ModelConfig {
                    id: "openai/gpt-oss-120b:free".into(),
                    provider: "openrouter".into(),
                    tasks: vec!["code".into(), "reasoning".into(), "chat".into(), "analysis".into()],
                    tool_use: false,
                    vision: false,
                    ctx: 128_000,
                },
                ModelConfig {
                    id: "meta-llama/llama-3.3-70b-instruct:free".into(),
                    provider: "openrouter".into(),
                    tasks: vec!["code".into(), "chat".into(), "analysis".into()],
                    tool_use: false,
                    vision: false,
                    ctx: 131_072,
                },
                // ── Paid models (fallback / higher quality) ─────────────────
                ModelConfig {
                    id: "anthropic/claude-3.5-haiku".into(),
                    provider: "openrouter".into(),
                    tasks: vec!["code".into(), "reasoning".into(), "chat".into(), "analysis".into()],
                    tool_use: true,
                    vision: false,
                    ctx: 200_000,
                },
                ModelConfig {
                    id: "openai/gpt-4o-mini".into(),
                    provider: "openrouter".into(),
                    tasks: vec!["chat".into(), "code".into(), "analysis".into()],
                    tool_use: true,
                    vision: true,
                    ctx: 128_000,
                },
                ModelConfig {
                    id: "qwen/qwen-2.5-72b-instruct".into(),
                    provider: "openrouter".into(),
                    tasks: vec!["code".into(), "chat".into(), "reasoning".into()],
                    tool_use: false,
                    vision: false,
                    ctx: 32_768,
                },
            ],
        }
    }

    /// Write the default config file to disk (creates parent dirs).
    pub fn write_default(path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Cannot create config dir: {e}"))?;
        }
        std::fs::write(path, DEFAULT_CONFIG_TOML)
            .map_err(|e| format!("Cannot write config: {e}"))
    }

    /// Resolve the ledger path: config value (with ~ expanded) or XDG default.
    pub fn ledger_path(&self) -> PathBuf {
        self.daemon
            .ledger_path
            .as_deref()
            .map(expand_tilde)
            .unwrap_or_else(default_ledger_path)
    }

    /// Convert the model list to `ModelAdded` events ready for the pipeline.
    pub fn to_model_events(&self) -> Vec<Event> {
        self.model
            .iter()
            .map(|m| Event::ModelAdded {
                model: m.to_model(),
                source: ModelSource::LocalConfig,
            })
            .collect()
    }
}

impl ModelConfig {
    /// Convert this config entry to an `omrp-types::Model`.
    pub fn to_model(&self) -> Model {
        let task_suitability = self
            .tasks
            .iter()
            .filter_map(|s| parse_task_type(s))
            .collect();
        Model {
            id: self.id.clone(),
            provider: self.provider.clone(),
            capabilities: ModelCapabilities {
                task_suitability,
                supports_vision: self.vision,
                supports_tool_use: self.tool_use,
                context_window: self.ctx,
            },
        }
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Parse a task-type string (case-insensitive) into a `TaskType`.
pub fn parse_task_type(s: &str) -> Option<TaskType> {
    match s.trim().to_lowercase().as_str() {
        "code" => Some(TaskType::Code),
        "reasoning" => Some(TaskType::Reasoning),
        "chat" => Some(TaskType::Chat),
        "vision" => Some(TaskType::Vision),
        "analysis" => Some(TaskType::Analysis),
        _ => None,
    }
}

/// Expand a leading `~` to the home directory.
fn expand_tilde(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/") {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(rest)
    } else {
        PathBuf::from(s)
    }
}

// ─── Default config template ─────────────────────────────────────────────────

/// Written to disk on first run if no config exists.
static DEFAULT_CONFIG_TOML: &str = r#"# OMRP configuration
# API keys are read from environment variables — never put them here.
#
# Quick start:
#   export OPENROUTER_API_KEY=sk-or-v1-...
#   omrp route "write a hello world in Rust"
#
# Free-tier models (marked :free) work without spending credits.
# Paid models deliver higher quality when you have a funded account.

[daemon]
# Path to the event ledger — persists model health scores across restarts.
# Defaults to ~/.local/share/omrp/ledger.jsonl if not set.
# ledger_path = "~/.local/share/omrp/ledger.jsonl"

# ─── Free-tier models (no credits required) ───────────────────────────────────

[[model]]
id       = "openai/gpt-oss-20b:free"
provider = "openrouter"
tasks    = ["chat", "code", "reasoning"]
tool_use = false
vision   = false
ctx      = 128000

[[model]]
id       = "openai/gpt-oss-120b:free"
provider = "openrouter"
tasks    = ["code", "reasoning", "chat", "analysis"]
tool_use = false
vision   = false
ctx      = 128000

[[model]]
id       = "meta-llama/llama-3.3-70b-instruct:free"
provider = "openrouter"
tasks    = ["code", "chat", "analysis"]
tool_use = false
vision   = false
ctx      = 131072

# ─── Paid models (higher quality, require credits) ────────────────────────────

[[model]]
id       = "anthropic/claude-3.5-haiku"
provider = "openrouter"
tasks    = ["code", "reasoning", "chat", "analysis"]
tool_use = true
vision   = false
ctx      = 200000

[[model]]
id       = "openai/gpt-4o-mini"
provider = "openrouter"
tasks    = ["chat", "code", "analysis"]
tool_use = true
vision   = true
ctx      = 128000

[[model]]
id       = "qwen/qwen-2.5-72b-instruct"
provider = "openrouter"
tasks    = ["code", "chat", "reasoning"]
tool_use = false
vision   = false
ctx      = 32768
"#;

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_defaults_parses_models() {
        let cfg = Config::builtin_defaults();
        // 3 free-tier + 3 paid models
        assert_eq!(cfg.model.len(), 6);
        let events = cfg.to_model_events();
        assert_eq!(events.len(), 6);
        // At least one free-tier model should be present
        assert!(cfg.model.iter().any(|m| m.id.ends_with(":free")));
    }

    #[test]
    fn test_parse_task_types() {
        assert_eq!(parse_task_type("code"), Some(TaskType::Code));
        assert_eq!(parse_task_type("REASONING"), Some(TaskType::Reasoning));
        assert_eq!(parse_task_type("unknown"), None);
    }

    #[test]
    fn test_model_capabilities_set_correctly() {
        let cfg = Config::builtin_defaults();
        let claude = cfg.model.iter().find(|m| m.id.contains("claude")).unwrap();
        let model = claude.to_model();
        assert!(model.capabilities.supports_tool_use);
        assert!(!model.capabilities.supports_vision);
        assert_eq!(model.capabilities.context_window, 200_000);
        assert!(model.capabilities.task_suitability.contains(&TaskType::Code));
    }

    #[test]
    fn test_parse_toml() {
        let toml = r#"
[[model]]
id = "test/model"
provider = "openrouter"
tasks = ["code", "chat"]
tool_use = true
ctx = 8192
"#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.model.len(), 1);
        assert_eq!(cfg.model[0].ctx, 8192);
    }
}
