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
    /// Model ID as used in API calls.
    pub id: String,
    /// Provider name: openrouter | kilo | cerebras | groq
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
    /// Routing tier: simple | medium | complex | reasoning.
    /// The classifier picks a tier; models in that tier are preferred.
    #[serde(default = "default_tier")]
    pub tier: String,
}

fn default_ctx() -> u32 { 4096 }
fn default_tier() -> String { "medium".into() }

/// Shorthand constructor for a free-tier `ModelConfig`.
fn mc(id: &str, provider: &str, tasks: &[&str], ctx: u32, tier: &str) -> ModelConfig {
    ModelConfig {
        id: id.into(),
        provider: provider.into(),
        tasks: tasks.iter().map(|s| s.to_string()).collect(),
        tool_use: false,
        vision: false,
        ctx,
        tier: tier.into(),
    }
}

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
    /// All models are permanently free-tier (no credits required).
    /// Covers five providers; set the corresponding env var for each:
    ///   Kilo      → KILO_API_KEY       (kilo/auto-free smart router)
    ///   Cerebras  → CEREBRAS_API_KEY   (wafer-fast, 14k req/day)
    ///   Groq      → GROQ_API_KEY       (ultra-low latency, 1k-14k req/day)
    ///   OpenRouter→ OPENROUTER_API_KEY (50-1000 req/day, many models)
    ///   BUW       → BUW_API_KEY        (virtual model gateway)
    pub fn builtin_defaults() -> Self {
        Self {
            daemon: DaemonConfig { ledger_path: None },
            model: vec![
                // ── Cerebras (CEREBRAS_API_KEY) — wafer-scale, 14,400 req/day ──
                // Ideal SIMPLE tier: fastest possible inference
                mc("llama3.1-8b",  "cerebras", &["chat","code"],                    128_000, "simple"),
                mc("gpt-oss-120b", "cerebras", &["code","reasoning","chat","analysis"], 128_000, "complex"),

                // ── Groq (GROQ_API_KEY) — ultra-low latency ─────────────────────
                // llama-3.1-8b-instant: 14,400 req/day  → fast SIMPLE fallback
                // llama-3.3-70b:         1,000 req/day  → MEDIUM general
                // llama-4-scout:         1,000 req/day, 30K tok/min → COMPLEX
                mc("llama-3.1-8b-instant",             "groq", &["chat","code"],                     131_072, "simple"),
                mc("llama-3.3-70b-versatile",          "groq", &["code","chat","analysis"],           131_072, "medium"),
                mc("llama-4-scout-17b-16e-instruct",   "groq", &["code","reasoning","chat"],          131_072, "complex"),
                mc("qwen/qwen3-32b",                   "groq", &["reasoning","code"],                 131_072, "reasoning"),

                // ── Kilo Gateway (KILO_API_KEY) ──────────────────────────────────
                // kilo/auto-free: Kilo's smart router — picks best free model.
                // Previously listed as kilo-auto/free (see Kilo-Org/kilocode#6686).
                mc("kilo/auto-free",                      "kilo", &["code","reasoning","chat","analysis"], 1_048_576, "medium"),
                mc("nvidia/nemotron-3-super-120b-a12b:free","kilo", &["reasoning","code","chat"],         999_424,   "reasoning"),
                mc("poolside/laguna-m.1:free",            "kilo", &["code","reasoning"],                  262_144,   "complex"),

                // ── OpenRouter (OPENROUTER_API_KEY) — 50-1000 req/day ────────────
                mc("qwen/qwen3-coder:free",                      "openrouter", &["code","reasoning","analysis"], 1_048_576, "complex"),
                mc("deepseek/deepseek-v4-flash:free",            "openrouter", &["code","reasoning","chat"],     1_048_576, "reasoning"),
                mc("openai/gpt-oss-120b:free",                   "openrouter", &["code","reasoning","chat","analysis"], 128_000, "complex"),
                mc("nousresearch/hermes-3-llama-3.1-405b:free",  "openrouter", &["code","reasoning","chat","analysis"], 128_000, "complex"),
                mc("meta-llama/llama-3.3-70b-instruct:free",     "openrouter", &["code","chat","analysis"],      131_072, "medium"),
                mc("openai/gpt-oss-20b:free",                    "openrouter", &["chat","code","reasoning"],     128_000, "simple"),
                mc("google/gemma-4-31b-it:free",                 "openrouter", &["chat","analysis","reasoning"], 262_144, "medium"),
                mc("moonshotai/kimi-k2.6:free",                  "openrouter", &["reasoning","code","chat"],     262_144, "reasoning"),

                // ── BUW Gateway (BUW_API_KEY) ────────────────────────────────────
                // Virtual model endpoints on the BUW gateway.
                // buw/omrp-auto: OMRP-compatible auto-routing virtual model.
                // buw/auto-kilo: Kilo-aware auto-routing virtual model.
                mc("buw/omrp-auto", "buw", &["code","reasoning","chat","analysis"], 1_048_576, "medium"),
                mc("buw/auto-kilo", "buw", &["code","reasoning","chat"],            1_048_576, "reasoning"),
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
static DEFAULT_CONFIG_TOML: &str = r#"# OMRP configuration — all models are permanently free-tier.
# API keys are read from environment variables, never stored here.
# Set keys for the providers you want to use (you don't need all of them):
#
#   export CEREBRAS_API_KEY=...     # https://cloud.cerebras.ai  (fastest, 14k req/day)
#   export GROQ_API_KEY=...         # https://console.groq.com   (ultra-low latency)
#   export KILO_API_KEY=...         # https://kilo.ai            (smart auto-router)
#   export OPENROUTER_API_KEY=...   # https://openrouter.ai/keys (50-1000 req/day)
#   export BUW_API_KEY=...          # https://api.buw.xyz        (virtual model gateway)
#
#   omrp route "write a fibonacci function in Rust"
#   omrp serve                      # OpenAI-compat proxy on :18800
#   omrp dashboard                  # live TUI health view
#
# tier: simple | medium | complex | reasoning
#   The prompt classifier picks a tier; models in that tier are tried first.

[daemon]
# Ledger file — persists health scores across restarts.
# ledger_path = "~/.local/share/omrp/ledger.jsonl"

# ─── Cerebras (CEREBRAS_API_KEY) — wafer-scale speed, 14,400 req/day ──────────

[[model]]
id    = "llama3.1-8b"
provider = "cerebras"
tasks = ["chat", "code"]
ctx   = 128000
tier  = "simple"

[[model]]
id    = "gpt-oss-120b"
provider = "cerebras"
tasks = ["code", "reasoning", "chat", "analysis"]
ctx   = 128000
tier  = "complex"

# ─── Groq (GROQ_API_KEY) — ultra-low latency inference ─────────────────────────

[[model]]
id    = "llama-3.1-8b-instant"
provider = "groq"
tasks = ["chat", "code"]
ctx   = 131072
tier  = "simple"

[[model]]
id    = "llama-3.3-70b-versatile"
provider = "groq"
tasks = ["code", "chat", "analysis"]
ctx   = 131072
tier  = "medium"

[[model]]
id    = "llama-4-scout-17b-16e-instruct"
provider = "groq"
tasks = ["code", "reasoning", "chat"]
ctx   = 131072
tier  = "complex"

[[model]]
id    = "qwen/qwen3-32b"
provider = "groq"
tasks = ["reasoning", "code"]
ctx   = 131072
tier  = "reasoning"

# ─── Kilo Gateway (KILO_API_KEY) ────────────────────────────────────────────────
# kilo/auto-free auto-picks the best free model on Kilo's network.
# (Note: previously listed as kilo-auto/free — fixed per Kilo-Org/kilocode#6686)

[[model]]
id    = "kilo/auto-free"
provider = "kilo"
tasks = ["code", "reasoning", "chat", "analysis"]
ctx   = 1048576
tier  = "medium"

[[model]]
id    = "nvidia/nemotron-3-super-120b-a12b:free"
provider = "kilo"
tasks = ["reasoning", "code", "chat"]
ctx   = 999424
tier  = "reasoning"

[[model]]
id    = "poolside/laguna-m.1:free"
provider = "kilo"
tasks = ["code", "reasoning"]
ctx   = 262144
tier  = "complex"

# ─── OpenRouter (OPENROUTER_API_KEY) ────────────────────────────────────────────

[[model]]
id    = "qwen/qwen3-coder:free"
provider = "openrouter"
tasks = ["code", "reasoning", "analysis"]
ctx   = 1048576
tier  = "complex"

[[model]]
id    = "deepseek/deepseek-v4-flash:free"
provider = "openrouter"
tasks = ["code", "reasoning", "chat"]
ctx   = 1048576
tier  = "reasoning"

[[model]]
id    = "openai/gpt-oss-120b:free"
provider = "openrouter"
tasks = ["code", "reasoning", "chat", "analysis"]
ctx   = 128000
tier  = "complex"

[[model]]
id    = "nousresearch/hermes-3-llama-3.1-405b:free"
provider = "openrouter"
tasks = ["code", "reasoning", "chat", "analysis"]
ctx   = 128000
tier  = "complex"

[[model]]
id    = "meta-llama/llama-3.3-70b-instruct:free"
provider = "openrouter"
tasks = ["code", "chat", "analysis"]
ctx   = 131072
tier  = "medium"

[[model]]
id    = "openai/gpt-oss-20b:free"
provider = "openrouter"
tasks = ["chat", "code", "reasoning"]
ctx   = 128000
tier  = "simple"

[[model]]
id    = "google/gemma-4-31b-it:free"
provider = "openrouter"
tasks = ["chat", "analysis", "reasoning"]
ctx   = 262144
tier  = "medium"

[[model]]
id    = "moonshotai/kimi-k2.6:free"
provider = "openrouter"
tasks = ["reasoning", "code", "chat"]
ctx   = 262144
tier  = "reasoning"

# ─── BUW Gateway (BUW_API_KEY) ──────────────────────────────────────────────────
# Virtual model endpoints on the BUW gateway (https://api.buw.xyz).
# buw/omrp-auto: OMRP-compatible auto-routing virtual model.
# buw/auto-kilo: Kilo-aware auto-routing virtual model.

[[model]]
id    = "buw/omrp-auto"
provider = "buw"
tasks = ["code", "reasoning", "chat", "analysis"]
ctx   = 1048576
tier  = "medium"

[[model]]
id    = "buw/auto-kilo"
provider = "buw"
tasks = ["code", "reasoning", "chat"]
ctx   = 1048576
tier  = "reasoning"
"#;

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_defaults_parses_models() {
        let cfg = Config::builtin_defaults();
        // 2 Cerebras + 4 Groq + 3 Kilo + 8 OpenRouter + 2 BUW = 19 total
        assert_eq!(cfg.model.len(), 19, "got {}", cfg.model.len());
        let events = cfg.to_model_events();
        assert_eq!(events.len(), 19);
        // All models must be free-tier: either :free suffix OR kilo/auto* OR Cerebras/Groq/BUW (always free)
        for m in &cfg.model {
            let is_free = m.id.ends_with(":free")
                || m.id.starts_with("kilo/auto")
                || m.id.starts_with("buw/")
                || m.provider == "cerebras"
                || m.provider == "groq"
                || m.provider == "buw";
            assert!(is_free, "model {} is not free-tier", m.id);
        }
        // All five providers must be present
        assert!(cfg.model.iter().any(|m| m.provider == "cerebras"));
        assert!(cfg.model.iter().any(|m| m.provider == "groq"));
        assert!(cfg.model.iter().any(|m| m.provider == "kilo"));
        assert!(cfg.model.iter().any(|m| m.provider == "openrouter"));
        assert!(cfg.model.iter().any(|m| m.provider == "buw"));
        // Kilo slug fixed (Kilo-Org/kilocode#6686)
        assert!(cfg.model.iter().any(|m| m.id == "kilo/auto-free"),
            "kilo/auto-free must be present (not kilo-auto/free)");
        assert!(!cfg.model.iter().any(|m| m.id == "kilo-auto/free"),
            "stale kilo-auto/free slug must not be present");
        // BUW virtual models must be present
        assert!(cfg.model.iter().any(|m| m.id == "buw/omrp-auto"),
            "buw/omrp-auto must be present");
        assert!(cfg.model.iter().any(|m| m.id == "buw/auto-kilo"),
            "buw/auto-kilo must be present");
        // All models must have tier assigned
        assert!(cfg.model.iter().all(|m| !m.tier.is_empty()));
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
        let kilo = cfg.model.iter().find(|m| m.id == "kilo/auto-free").unwrap();
        let model = kilo.to_model();
        assert_eq!(model.provider, "kilo");
        assert_eq!(model.capabilities.context_window, 1_048_576);
        assert!(model.capabilities.task_suitability.contains(&TaskType::Code));
        // Cerebras is present
        let cerberas = cfg.model.iter().find(|m| m.provider == "cerebras").unwrap();
        assert_eq!(cerberas.tier, "simple");
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
