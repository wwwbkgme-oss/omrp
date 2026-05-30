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
    /// All models are free-tier.  Kilo Gateway models (provider = "kilo") use
    /// `KILO_API_KEY`; OpenRouter models use `OPENROUTER_API_KEY`.
    pub fn builtin_defaults() -> Self {
        Self {
            daemon: DaemonConfig { ledger_path: None },
            model: vec![
                // ── Kilo Gateway (KILO_API_KEY) ──────────────────────────────
                // kilo-auto/free is Kilo's smart router: it automatically picks
                // the best available free model on Kilo's network.
                ModelConfig {
                    id: "kilo-auto/free".into(),
                    provider: "kilo".into(),
                    tasks: vec!["code".into(), "reasoning".into(), "chat".into(), "analysis".into()],
                    tool_use: false,
                    vision: false,
                    ctx: 1_048_576,
                },
                ModelConfig {
                    id: "nvidia/nemotron-3-super-120b-a12b:free".into(),
                    provider: "kilo".into(),
                    tasks: vec!["reasoning".into(), "code".into(), "chat".into()],
                    tool_use: false,
                    vision: false,
                    ctx: 999_424,
                },
                ModelConfig {
                    id: "poolside/laguna-m.1:free".into(),
                    provider: "kilo".into(),
                    tasks: vec!["code".into(), "reasoning".into()],
                    tool_use: false,
                    vision: false,
                    ctx: 262_144,
                },

                // ── OpenRouter (OPENROUTER_API_KEY) ──────────────────────────
                // Large context — code and reasoning specialists
                ModelConfig {
                    id: "qwen/qwen3-coder:free".into(),
                    provider: "openrouter".into(),
                    tasks: vec!["code".into(), "reasoning".into(), "analysis".into()],
                    tool_use: false,
                    vision: false,
                    ctx: 1_048_576,
                },
                ModelConfig {
                    id: "deepseek/deepseek-v4-flash:free".into(),
                    provider: "openrouter".into(),
                    tasks: vec!["code".into(), "reasoning".into(), "chat".into()],
                    tool_use: false,
                    vision: false,
                    ctx: 1_048_576,
                },
                // General-purpose
                ModelConfig {
                    id: "openai/gpt-oss-120b:free".into(),
                    provider: "openrouter".into(),
                    tasks: vec!["code".into(), "reasoning".into(), "chat".into(), "analysis".into()],
                    tool_use: false,
                    vision: false,
                    ctx: 128_000,
                },
                ModelConfig {
                    id: "nousresearch/hermes-3-llama-3.1-405b:free".into(),
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
                ModelConfig {
                    id: "openai/gpt-oss-20b:free".into(),
                    provider: "openrouter".into(),
                    tasks: vec!["chat".into(), "code".into(), "reasoning".into()],
                    tool_use: false,
                    vision: false,
                    ctx: 128_000,
                },
                ModelConfig {
                    id: "google/gemma-4-31b-it:free".into(),
                    provider: "openrouter".into(),
                    tasks: vec!["chat".into(), "analysis".into(), "reasoning".into()],
                    tool_use: false,
                    vision: false,
                    ctx: 262_144,
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
# All models below are free-tier — no credits required.
# API keys are read from environment variables, never stored here.
#
# Quick start:
#   export KILO_API_KEY=...           # https://kilo.ai
#   export OPENROUTER_API_KEY=...     # https://openrouter.ai/keys  (optional)
#   omrp route "write a fibonacci function in Rust"
#
# Run `omrp dashboard` for a live TUI view of model health and routing scores.
# Browse free models: https://openrouter.ai/models?supported_parameters=free

[daemon]
# Ledger file — persists health scores across restarts.
# Defaults to ~/.local/share/omrp/ledger.jsonl
# ledger_path = "~/.local/share/omrp/ledger.jsonl"

# ─── Kilo Gateway (KILO_API_KEY) ──────────────────────────────────────────────
# kilo-auto/free is Kilo's smart auto-router: picks the best available free
# model on Kilo's network automatically.  Use it as your primary catch-all.

[[model]]
id       = "kilo-auto/free"
provider = "kilo"
tasks    = ["code", "reasoning", "chat", "analysis"]
ctx      = 1048576

[[model]]
id       = "nvidia/nemotron-3-super-120b-a12b:free"
provider = "kilo"
tasks    = ["reasoning", "code", "chat"]
ctx      = 999424

[[model]]
id       = "poolside/laguna-m.1:free"
provider = "kilo"
tasks    = ["code", "reasoning"]
ctx      = 262144

# ─── OpenRouter (OPENROUTER_API_KEY) ──────────────────────────────────────────

[[model]]
id       = "qwen/qwen3-coder:free"
provider = "openrouter"
tasks    = ["code", "reasoning", "analysis"]
ctx      = 1048576

[[model]]
id       = "deepseek/deepseek-v4-flash:free"
provider = "openrouter"
tasks    = ["code", "reasoning", "chat"]
ctx      = 1048576

[[model]]
id       = "openai/gpt-oss-120b:free"
provider = "openrouter"
tasks    = ["code", "reasoning", "chat", "analysis"]
ctx      = 128000

[[model]]
id       = "nousresearch/hermes-3-llama-3.1-405b:free"
provider = "openrouter"
tasks    = ["code", "reasoning", "chat", "analysis"]
ctx      = 128000

[[model]]
id       = "meta-llama/llama-3.3-70b-instruct:free"
provider = "openrouter"
tasks    = ["code", "chat", "analysis"]
ctx      = 131072

[[model]]
id       = "openai/gpt-oss-20b:free"
provider = "openrouter"
tasks    = ["chat", "code", "reasoning"]
ctx      = 128000

[[model]]
id       = "google/gemma-4-31b-it:free"
provider = "openrouter"
tasks    = ["chat", "analysis", "reasoning"]
ctx      = 262144
"#;

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_defaults_parses_models() {
        let cfg = Config::builtin_defaults();
        // 3 Kilo + 7 OpenRouter = 10 total, no duplicates
        assert_eq!(cfg.model.len(), 10, "expect 10 models (3 kilo + 7 openrouter)");
        let events = cfg.to_model_events();
        assert_eq!(events.len(), 10);
        // All models must be free-tier: either `:free` suffix OR `kilo-auto/*` OR provider="kilo"
        assert!(cfg.model.iter().all(|m| {
            m.id.ends_with(":free")
                || m.id.starts_with("kilo-auto/")
                || m.provider == "kilo"
        }), "every model must be free-tier");
        // Both providers must be present
        assert!(cfg.model.iter().any(|m| m.provider == "kilo"));
        assert!(cfg.model.iter().any(|m| m.provider == "openrouter"));
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
        let kilo_auto = cfg.model.iter().find(|m| m.id == "kilo-auto/free").unwrap();
        let model = kilo_auto.to_model();
        assert_eq!(model.provider, "kilo");
        assert_eq!(model.capabilities.context_window, 1_048_576);
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
