//! OpenAI-compatible provider adapter.
//!
//! All supported providers expose the same `/chat/completions` endpoint.
//! API keys are read from environment variables — never hard-coded:
//!   - OpenRouter : `OPENROUTER_API_KEY`   https://openrouter.ai/keys
//!   - Kilo       : `KILO_API_KEY`         https://kilo.ai
//!   - Cerebras   : `CEREBRAS_API_KEY`     https://cloud.cerebras.ai
//!   - Groq       : `GROQ_API_KEY`         https://console.groq.com

use std::time::{Duration, Instant};

use omrp_events::error::{ErrorKind, ProviderError};
use serde_json::{json, Value};

const HTTP_REFERER: &str = "https://github.com/wwwbkgme-oss/omrp";
const APP_TITLE: &str = "OMRP";
const DEFAULT_MAX_TOKENS: u32 = 1024;

// ─── Provider registry ────────────────────────────────────────────────────────

/// Known OpenAI-compatible provider backends.
/// BUW has been removed — `omrp/auto` provides the same smart-routing capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderKind {
    /// https://openrouter.ai — OPENROUTER_API_KEY
    OpenRouter,
    /// https://kilo.ai — KILO_API_KEY
    Kilo,
    /// https://cloud.cerebras.ai — CEREBRAS_API_KEY
    Cerebras,
    /// https://console.groq.com — GROQ_API_KEY
    Groq,
    /// https://api.sambanova.ai — SAMBANOVA_API_KEY
    SambaNova,
    /// https://codestral.mistral.ai — CODESTRAL_API_KEY (Mistral code model, free Experiment plan)
    Codestral,
    /// https://api.together.xyz — TOGETHER_API_KEY
    Together,
}

impl ProviderKind {
    /// Resolve from a provider slug or model-id prefix.
    /// Examples: `"groq"` → Groq; `"groq/llama-3.3-70b-versatile"` → Groq.
    pub fn from_str(s: &str) -> Option<Self> {
        let lower = s.to_lowercase();
        let prefix = lower.split('/').next().unwrap_or(&lower);
        match prefix {
            "openrouter"  => Some(Self::OpenRouter),
            "kilo"        => Some(Self::Kilo),
            "cerebras"    => Some(Self::Cerebras),
            "groq"        => Some(Self::Groq),
            "sambanova"   => Some(Self::SambaNova),
            "codestral"   => Some(Self::Codestral),
            "together"    => Some(Self::Together),
            // well-known bare model IDs → infer provider
            "llama-3.3-70b-versatile" | "llama-3.1-8b-instant"
            | "gemma2-9b-it" | "qwen-qwq-32b"
            | "compound-beta"
            | "mixtral-8x7b-32768"     => Some(Self::Groq),
            "llama-3.3-70b" | "llama3.1-8b" | "gpt-oss-120b" => Some(Self::Cerebras),
            _ => None,
        }
    }

    pub fn base_url(&self) -> &'static str {
        match self {
            Self::OpenRouter => "https://openrouter.ai/api/v1",
            Self::Kilo       => "https://api.kilo.ai/api/gateway",
            Self::Cerebras   => "https://api.cerebras.ai/v1",
            Self::Groq       => "https://api.groq.com/openai/v1",
            Self::SambaNova  => "https://api.sambanova.ai/v1",
            Self::Codestral  => "https://codestral.mistral.ai/v1",
            Self::Together   => "https://api.together.xyz/v1",
        }
    }

    pub fn api_key_env(&self) -> &'static str {
        match self {
            Self::OpenRouter => "OPENROUTER_API_KEY",
            Self::Kilo       => "KILO_API_KEY",
            Self::Cerebras   => "CEREBRAS_API_KEY",
            Self::Groq       => "GROQ_API_KEY",
            Self::SambaNova  => "SAMBANOVA_API_KEY",
            Self::Codestral  => "CODESTRAL_API_KEY",
            Self::Together   => "TOGETHER_API_KEY",
        }
    }

    pub fn to_str(&self) -> &'static str {
        match self {
            Self::OpenRouter => "openrouter",
            Self::Kilo       => "kilo",
            Self::Cerebras   => "cerebras",
            Self::Groq       => "groq",
            Self::SambaNova  => "sambanova",
            Self::Codestral  => "codestral",
            Self::Together   => "together",
        }
    }

    #[allow(dead_code)]
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::OpenRouter => "OpenRouter",
            Self::Kilo       => "Kilo Gateway",
            Self::Cerebras   => "Cerebras",
            Self::Groq       => "Groq",
            Self::SambaNova  => "SambaNova",
            Self::Codestral  => "Codestral (Mistral)",
            Self::Together   => "Together AI",
        }
    }

    /// Free / default models for this provider, shown in the playground.
    /// Tuple: (model_id_for_api, display_label_for_ui)
    pub fn free_models(&self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::OpenRouter => &[
                // explicit :free models — no credits required
                ("meta-llama/llama-3.3-70b-instruct:free",   "Meta — Llama 3.3 70B (free)"),
                ("deepseek/deepseek-chat-v3-0324:free",      "DeepSeek — DeepSeek V3 (free)"),
                ("google/gemma-3-27b-it:free",               "Google — Gemma 3 27B (free)"),
                ("qwen/qwen3-8b:free",                       "Qwen — Qwen 3 8B (free)"),
                ("mistralai/mistral-7b-instruct:free",       "Mistral AI — Mistral 7B (free)"),
                ("openrouter/quasar-alpha",                  "OpenRouter — Quasar Alpha (free)"),
            ],
            Self::Groq => &[
                ("llama-3.3-70b-versatile", "Meta — Llama 3.3 70B"),
                ("llama-3.1-8b-instant",    "Meta — Llama 3.1 8B (fast)"),
                ("gemma2-9b-it",            "Google — Gemma 2 9B"),
                ("qwen-qwq-32b",            "Qwen — QwQ 32B (reasoning)"),
                ("compound-beta",           "Groq — Compound Beta"),
            ],
            Self::Cerebras => &[
                ("llama-3.3-70b",  "Meta — Llama 3.3 70B (wafer-fast)"),
                ("llama3.1-8b",    "Meta — Llama 3.1 8B (wafer-fast)"),
            ],
            Self::Kilo => &[
                ("kilo-auto/free", "Kilo — auto-free router"),
            ],
            Self::SambaNova => &[
                ("Meta-Llama-3.3-70B-Instruct", "Meta — Llama 3.3 70B"),
                ("Meta-Llama-3.1-8B-Instruct",  "Meta — Llama 3.1 8B"),
                ("Qwen2.5-72B-Instruct",         "Qwen — Qwen 2.5 72B"),
            ],
            Self::Codestral => &[
                ("codestral-latest", "Mistral — Codestral (code)"),
                ("mistral-small-latest", "Mistral — Mistral Small"),
            ],
            Self::Together => &[
                ("meta-llama/Llama-3.3-70B-Instruct-Turbo-Free", "Meta — Llama 3.3 70B (free)"),
                ("meta-llama/Llama-3.2-11B-Vision-Instruct-Turbo", "Meta — Llama 3.2 11B Vision (free)"),
                ("deepseek-ai/DeepSeek-V3",                       "DeepSeek — DeepSeek V3"),
            ],
        }
    }

    /// All built-in providers in priority order for auto-routing fallback.
    pub fn all() -> &'static [ProviderKind] {
        &[
            ProviderKind::OpenRouter,
            ProviderKind::Groq,
            ProviderKind::Cerebras,
            ProviderKind::Kilo,
            ProviderKind::SambaNova,
            ProviderKind::Codestral,
            ProviderKind::Together,
        ]
    }

    /// Strip the `{provider}/` prefix from a model ID before sending to the provider API.
    ///
    /// Most providers use bare model IDs (e.g. Groq expects `llama-3.3-70b-versatile`
    /// not `groq/llama-3.3-70b-versatile`). OpenRouter is an exception — it expects
    /// the full `{creator}/{model}` format for its model routing.
    ///
    /// Rules:
    /// - OpenRouter: keep as-is (they use `creator/model[:free]` natively)
    /// - All others: if model starts with `{provider_slug}/`, strip it
    pub fn normalize_model_id<'a>(&self, model_id: &'a str) -> &'a str {
        if matches!(self, Self::OpenRouter) {
            return model_id; // OpenRouter uses full paths like meta-llama/...
        }
        let prefix = format!("{}/", self.to_str());
        if model_id.to_lowercase().starts_with(&prefix.to_lowercase()) {
            &model_id[prefix.len()..]
        } else {
            model_id
        }
    }
}

// ─── Free-tier provider catalog ───────────────────────────────────────────────
//
// Curated list of publicly documented OpenAI-compatible APIs that offer a
// free tier. All data is sourced from each provider's public documentation.
// Rates and limits change — treat as informational, not authoritative.

/// A single free-tier model entry within a provider.
pub struct FreeModel {
    pub id:        &'static str,  // model ID sent verbatim to the provider API
    pub creator:   &'static str,  // organisation that trained the model
    pub context_k: u32,           // context window in K tokens
    pub label:     &'static str,  // short display label, e.g. "Llama 3.3 70B"
}

/// Metadata for one free-tier LLM API provider.
pub struct FreeProvider {
    pub id:            &'static str,
    pub name:          &'static str,
    pub country_emoji: &'static str,
    pub base_url:      &'static str,
    pub signup_url:    &'static str,
    pub key_env:       &'static str,
    pub free_note:     &'static str,
    pub models:        &'static [FreeModel],
}

/// All known free-tier OpenAI-compatible LLM API providers.
/// BUW removed — omrp/auto provides the same smart-routing capability.
pub static FREE_PROVIDERS: &[FreeProvider] = &[
    FreeProvider {
        id: "openrouter", name: "OpenRouter", country_emoji: "🇺🇸",
        base_url: "https://openrouter.ai/api/v1",
        signup_url: "https://openrouter.ai/keys",
        key_env: "OPENROUTER_API_KEY",
        free_note: "35+ :free models — no daily cap, no credit card required",
        models: &[
            FreeModel { id:"meta-llama/llama-3.3-70b-instruct:free", creator:"Meta",       context_k:131, label:"Llama 3.3 70B" },
            FreeModel { id:"deepseek/deepseek-chat-v3-0324:free",    creator:"DeepSeek",   context_k:164, label:"DeepSeek V3" },
            FreeModel { id:"google/gemma-3-27b-it:free",             creator:"Google",     context_k:128, label:"Gemma 3 27B" },
            FreeModel { id:"qwen/qwen3-8b:free",                     creator:"Qwen",       context_k:128, label:"Qwen 3 8B" },
            FreeModel { id:"mistralai/mistral-7b-instruct:free",     creator:"Mistral AI", context_k:32,  label:"Mistral 7B" },
            FreeModel { id:"openrouter/quasar-alpha",                creator:"OpenRouter", context_k:1000,label:"Quasar Alpha" },
        ],
    },
    FreeProvider {
        id: "groq", name: "Groq", country_emoji: "🇺🇸",
        base_url: "https://api.groq.com/openai/v1",
        signup_url: "https://console.groq.com/keys",
        key_env: "GROQ_API_KEY",
        free_note: "Ultra-low latency LPU inference — free tier, 1K–14K req/day",
        models: &[
            FreeModel { id:"llama-3.3-70b-versatile", creator:"Meta",   context_k:128, label:"Llama 3.3 70B" },
            FreeModel { id:"llama-3.1-8b-instant",    creator:"Meta",   context_k:128, label:"Llama 3.1 8B (fast)" },
            FreeModel { id:"gemma2-9b-it",            creator:"Google", context_k:8,   label:"Gemma 2 9B" },
            FreeModel { id:"qwen-qwq-32b",            creator:"Qwen",   context_k:128, label:"QwQ 32B (reasoning)" },
            FreeModel { id:"compound-beta",           creator:"Groq",   context_k:128, label:"Compound Beta" },
        ],
    },
    FreeProvider {
        id: "cerebras", name: "Cerebras", country_emoji: "🇺🇸",
        base_url: "https://api.cerebras.ai/v1",
        signup_url: "https://cloud.cerebras.ai/",
        key_env: "CEREBRAS_API_KEY",
        free_note: "Wafer-scale inference ~2600 tok/s — 1M tokens/day free",
        models: &[
            FreeModel { id:"llama-3.3-70b", creator:"Meta", context_k:128, label:"Llama 3.3 70B" },
            FreeModel { id:"llama3.1-8b",   creator:"Meta", context_k:128, label:"Llama 3.1 8B" },
        ],
    },
    FreeProvider {
        id: "kilo", name: "Kilo Code", country_emoji: "🇺🇸",
        base_url: "https://api.kilo.ai/api/gateway",
        signup_url: "https://kilo.ai",
        key_env: "KILO_API_KEY",
        free_note: "Free models, no credit card — kilo-auto/free smart router",
        models: &[
            FreeModel { id:"kilo-auto/free", creator:"Kilo", context_k:200, label:"Auto-free router" },
        ],
    },
    FreeProvider {
        id: "sambanova", name: "SambaNova", country_emoji: "🇺🇸",
        base_url: "https://api.sambanova.ai/v1",
        signup_url: "https://cloud.sambanova.ai/",
        key_env: "SAMBANOVA_API_KEY",
        free_note: "Free tier — fast inference, no credit card",
        models: &[
            FreeModel { id:"Meta-Llama-3.3-70B-Instruct", creator:"Meta",  context_k:128, label:"Llama 3.3 70B" },
            FreeModel { id:"Meta-Llama-3.1-8B-Instruct",  creator:"Meta",  context_k:16,  label:"Llama 3.1 8B" },
            FreeModel { id:"Qwen2.5-72B-Instruct",        creator:"Qwen",  context_k:128, label:"Qwen 2.5 72B" },
        ],
    },
    FreeProvider {
        id: "codestral", name: "Codestral (Mistral)", country_emoji: "🇫🇷",
        base_url: "https://codestral.mistral.ai/v1",
        signup_url: "https://console.mistral.ai/api-keys",
        key_env: "CODESTRAL_API_KEY",
        free_note: "Free Experiment plan — optimised for code, 2 req/min",
        models: &[
            FreeModel { id:"codestral-latest",     creator:"Mistral AI", context_k:256, label:"Codestral (code)" },
            FreeModel { id:"mistral-small-latest", creator:"Mistral AI", context_k:128, label:"Mistral Small" },
        ],
    },
    FreeProvider {
        id: "together", name: "Together AI", country_emoji: "🇺🇸",
        base_url: "https://api.together.xyz/v1",
        signup_url: "https://api.together.ai/",
        key_env: "TOGETHER_API_KEY",
        free_note: "Several permanently free models + $1 signup credit",
        models: &[
            FreeModel { id:"meta-llama/Llama-3.3-70B-Instruct-Turbo-Free",   creator:"Meta",     context_k:131, label:"Llama 3.3 70B (free)" },
            FreeModel { id:"meta-llama/Llama-3.2-11B-Vision-Instruct-Turbo", creator:"Meta",     context_k:128, label:"Llama 3.2 11B Vision (free)" },
            FreeModel { id:"deepseek-ai/DeepSeek-V3",                        creator:"DeepSeek", context_k:128, label:"DeepSeek V3" },
        ],
    },
    FreeProvider {
        id: "gemini", name: "Google Gemini", country_emoji: "🇺🇸",
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        signup_url: "https://aistudio.google.com/app/apikey",
        key_env: "GEMINI_API_KEY",
        free_note: "Free tier — 10 RPM / 250 RPD. Not in EU/UK/CH.",
        models: &[
            FreeModel { id:"gemini-2.5-flash",      creator:"Google", context_k:1000, label:"Gemini 2.5 Flash" },
            FreeModel { id:"gemini-2.5-flash-lite", creator:"Google", context_k:1000, label:"Gemini 2.5 Flash-Lite" },
        ],
    },
    FreeProvider {
        id: "mistral", name: "Mistral AI", country_emoji: "🇫🇷",
        base_url: "https://api.mistral.ai/v1",
        signup_url: "https://console.mistral.ai/api-keys",
        key_env: "MISTRAL_API_KEY",
        free_note: "Free Experiment plan — no credit card, ~1B tokens/month",
        models: &[
            FreeModel { id:"mistral-small-latest", creator:"Mistral AI", context_k:32, label:"Mistral Small" },
            FreeModel { id:"open-mistral-7b",      creator:"Mistral AI", context_k:32, label:"Mistral 7B (open)" },
        ],
    },
    FreeProvider {
        id: "github-models", name: "GitHub Models", country_emoji: "🇺🇸",
        base_url: "https://models.inference.ai.azure.com",
        signup_url: "https://github.com/marketplace/models",
        key_env: "GITHUB_TOKEN",
        free_note: "Free for all GitHub users — 45+ models",
        models: &[
            FreeModel { id:"gpt-4o-mini",                  creator:"OpenAI", context_k:128, label:"GPT-4o mini" },
            FreeModel { id:"meta-llama-3.3-70b-instruct",  creator:"Meta",   context_k:128, label:"Llama 3.3 70B" },
        ],
    },
    FreeProvider {
        id: "llm7", name: "LLM7.io", country_emoji: "🇬🇧",
        base_url: "https://api.llm7.io/v1",
        signup_url: "https://token.llm7.io",
        key_env: "LLM7_API_KEY",
        free_note: "Zero-friction — no registration for basic access, 30+ models",
        models: &[
            FreeModel { id:"gpt-4o",                     creator:"OpenAI",    context_k:128, label:"GPT-4o compat" },
            FreeModel { id:"claude-3-5-sonnet-20241022", creator:"Anthropic", context_k:200, label:"Claude Sonnet compat" },
        ],
    },
    FreeProvider {
        id: "nvidia-nim", name: "NVIDIA NIM", country_emoji: "🇺🇸",
        base_url: "https://integrate.api.nvidia.com/v1",
        signup_url: "https://build.nvidia.com/explore/discover",
        key_env: "NVIDIA_API_KEY",
        free_note: "Free with NVIDIA Developer Program — 100+ models",
        models: &[
            FreeModel { id:"meta/llama-3.3-70b-instruct",             creator:"Meta",  context_k:128, label:"Llama 3.3 70B" },
            FreeModel { id:"nvidia/llama-3.1-nemotron-ultra-253b-v1", creator:"NVIDIA",context_k:128, label:"Nemotron Ultra 253B" },
        ],
    },
];

/// Serialise the catalog as JSON for the `/api/public/providers` endpoint.
pub fn free_providers_json() -> serde_json::Value {
    use serde_json::json;
    serde_json::Value::Array(FREE_PROVIDERS.iter().map(|p| json!({
        "id":         p.id,
        "name":       p.name,
        "country":    p.country_emoji,
        "base_url":   p.base_url,
        "signup_url": p.signup_url,
        "key_env":    p.key_env,
        "free_note":  p.free_note,
        "models": p.models.iter().map(|m| json!({
            "id":        m.id,
            "creator":   m.creator,
            "context_k": m.context_k,
            "label":     m.label,
            "display":   format!("{} — {}", m.creator, m.label),
        })).collect::<Vec<_>>(),
    })).collect())
}

// ─── Public types ─────────────────────────────────────────────────────────────

/// A single chat message.
#[derive(Debug, Clone)]
pub struct Message {
    pub role: String,
    pub content: String,
}

impl Message {
    #[allow(dead_code)]
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: "user".into(), content: content.into() }
    }
    #[allow(dead_code)]
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: "system".into(), content: content.into() }
    }
}

/// Successful completion result.
#[derive(Debug)]
pub struct CompletionResult {
    pub text: String,
    pub tokens_used: u64,
    pub latency_ms: u64,
    /// Actual model used (provider may have rerouted).
    pub model_used: String,
}

// ─── CompatClient ─────────────────────────────────────────────────────────────

/// Blocking OpenAI-compatible HTTP client.
///
/// Works with any provider that speaks the `/v1/chat/completions` protocol.
/// Optionally routes requests through a proxy to bypass per-IP rate limits —
/// call `.with_proxy("http://ip:port")` after construction.
pub struct CompatClient {
    base_url:  String,
    api_key:   String,
    /// Optional proxy URL, e.g. `http://1.2.3.4:8080` or `socks5://…`.
    proxy_url: Option<String>,
    #[allow(dead_code)]
    kind: ProviderKind,
}

impl CompatClient {
    /// Attach a proxy to this client.  Returns a new client that routes all
    /// requests through the given proxy URL.
    pub fn with_proxy(mut self, proxy_url: impl Into<String>) -> Self {
        self.proxy_url = Some(proxy_url.into());
        self
    }

    /// Build a `ureq::Agent` optionally configured with a proxy.
    ///
    /// When `proxy_url` is `None` the agent behaves like a plain `ureq::post`.
    fn build_agent(&self) -> ureq::Agent {
        let builder = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(10))
            .timeout_read(std::time::Duration::from_secs(60));
        if let Some(ref proxy) = self.proxy_url {
            match ureq::Proxy::new(proxy) {
                Ok(p)  => return builder.proxy(p).build(),
                Err(e) => eprintln!("[proxy] invalid proxy URL {proxy:?}: {e}"),
            }
        }
        builder.build()
    }
    /// Build a client for a known provider, reading the API key from the
    /// appropriate environment variable.
    pub fn for_provider(provider: &str) -> Result<Self, String> {
        let kind = ProviderKind::from_str(provider).ok_or_else(|| {
            format!(
                "Unknown provider: {provider:?}. Supported: openrouter, kilo"
            )
        })?;
        Self::from_kind(kind)
    }

    /// Build a client from a `ProviderKind`.
    pub fn from_kind(kind: ProviderKind) -> Result<Self, String> {
        let env_var = kind.api_key_env();
        let api_key = std::env::var(env_var).map_err(|_| {
            format!(
                "{env_var} is not set.\n\
                 \n\
                 \t export {env_var}=<your-key>\n\
                 \n\
                 Get a free key at: {}",
                match kind {
                    ProviderKind::OpenRouter => "https://openrouter.ai/keys",
                    ProviderKind::Kilo       => "https://kilo.ai",
                    ProviderKind::Cerebras   => "https://cloud.cerebras.ai",
                    ProviderKind::Groq       => "https://console.groq.com",
                    ProviderKind::SambaNova  => "https://cloud.sambanova.ai/",
                    ProviderKind::Codestral  => "https://console.mistral.ai/",
                    ProviderKind::Together   => "https://api.together.ai/",
                }
            )
        })?;
        Ok(Self { base_url: kind.base_url().into(), api_key, kind, proxy_url: None })
    }

    /// Build a client using an explicit API key (e.g. stored in the database).
    /// This bypasses the env-var lookup used by `for_provider`.
    pub fn from_key_and_provider(provider: &str, api_key: &str) -> Result<Self, String> {
        let kind = ProviderKind::from_str(provider)
            .ok_or_else(|| format!("Unknown provider: {provider:?}"))?;
        Ok(Self { base_url: kind.base_url().into(), api_key: api_key.to_string(), kind, proxy_url: None })
    }

    /// Build a client with a fully custom base URL (for self-hosted / custom providers).
    /// `provider` is used only for display; it may be an arbitrary string.
    pub fn from_key_custom(api_key: &str, base_url: &str) -> Self {
        Self {
            base_url:  base_url.trim_end_matches('/').to_string(),
            api_key:   api_key.to_string(),
            kind:      ProviderKind::OpenRouter, // structural placeholder; base_url overrides
            proxy_url: None,
        }
    }

    /// Override the base URL after construction — useful when a DB key has a custom endpoint.
    #[allow(dead_code)]
    pub fn with_base_url(mut self, url: &str) -> Self {
        self.base_url = url.trim_end_matches('/').to_string();
        self
    }

    /// Provider name for display.
    #[allow(dead_code)]
    pub fn provider_name(&self) -> &str {
        self.kind.display_name()
    }

    /// Make a **streaming** chat request.
    ///
    /// Returns the raw SSE body reader on success.  When a proxy is configured
    /// the request exits through that proxy IP (rate-limit bypass).
    pub fn stream_request(
        &self,
        body: &Value,
    ) -> Result<Box<dyn std::io::Read + Send + 'static>, ProviderError> {
        let url      = format!("{}/chat/completions", self.base_url);
        let body_str = body.to_string();
        let agent    = self.build_agent();

        match agent.post(&url)
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .set("Content-Type", "application/json")
            .set("HTTP-Referer", HTTP_REFERER)
            .set("X-Title", APP_TITLE)
            .send_string(&body_str)
        {
            Ok(resp)  => Ok(Box::new(resp.into_reader())),
            Err(ureq::Error::Status(status, resp)) => {
                let raw = resp.into_string().unwrap_or_default();
                Err(map_http_error(status, &raw))
            }
            Err(ureq::Error::Transport(t)) => Err(ProviderError::Network(t.to_string())),
        }
    }

    /// Send a single chat-completion request.
    ///
    /// When a proxy is configured every byte exits through that proxy IP.
    pub fn complete(
        &self,
        model_id: &str,
        messages: &[Message],
        max_tokens: Option<u32>,
    ) -> Result<CompletionResult, ProviderError> {
        let url = format!("{}/chat/completions", self.base_url);

        let msgs: Vec<Value> = messages
            .iter()
            .map(|m| json!({"role": m.role, "content": m.content}))
            .collect();

        let body = json!({
            "model": model_id,
            "messages": msgs,
            "max_tokens": max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        });

        let started = Instant::now();
        let agent   = self.build_agent();

        let response = agent.post(&url)
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .set("Content-Type", "application/json")
            .set("HTTP-Referer", HTTP_REFERER)
            .set("X-Title", APP_TITLE)
            .send_string(&body.to_string());

        let latency_ms = started.elapsed().as_millis() as u64;

        match response {
            Ok(resp) => {
                let raw = resp.into_string().map_err(|e| {
                    ProviderError::Internal(format!("Failed to read response: {e}"))
                })?;
                parse_success(&raw, latency_ms)
            }
            Err(ureq::Error::Status(status, resp)) => {
                let raw = resp.into_string().unwrap_or_default();
                Err(map_http_error(status, &raw))
            }
            Err(ureq::Error::Transport(t)) => Err(ProviderError::Network(t.to_string())),
        }
    }

    /// Complete with automatic retry.
    ///
    /// - 429 RateLimited → **no wait**, signals caller to switch proxy.
    ///   The `complete_with_proxy_rotation` method in web_server.rs handles
    ///   the actual proxy swap; this method simply returns the error immediately
    ///   so the caller can react as fast as possible.
    /// - Network error → wait 1 s, retry once on the same proxy.
    /// - Any other error → return immediately.
    pub fn complete_with_retry(
        &self,
        model_id: &str,
        messages: &[Message],
        max_tokens: Option<u32>,
    ) -> Result<CompletionResult, ProviderError> {
        match self.complete(model_id, messages, max_tokens) {
            Err(ProviderError::RateLimited { retry_after }) => {
                // Do NOT sleep here — the proxy rotation layer will immediately
                // retry via a different IP.  Only sleep as last resort.
                Err(ProviderError::RateLimited { retry_after })
            }
            Err(ProviderError::Network(msg)) => {
                eprintln!("  [network error] {msg} — retrying in 1s…");
                std::thread::sleep(Duration::from_secs(1));
                self.complete(model_id, messages, max_tokens)
            }
            other => other,
        }
    }
}

// ─── Parsing ─────────────────────────────────────────────────────────────────

fn parse_success(raw: &str, latency_ms: u64) -> Result<CompletionResult, ProviderError> {
    let v: Value = serde_json::from_str(raw)
        .map_err(|e| ProviderError::Internal(format!("JSON parse error: {e}")))?;

    // Inline error (some providers return 200 + error body).
    if let Some(err) = v.get("error") {
        let code = err["code"].as_u64().unwrap_or(0) as u16;
        let msg = err["message"].as_str().unwrap_or("unknown").to_string();
        return Err(map_http_error(code, &msg));
    }

    let text = v["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();

    if text.is_empty() {
        return Err(ProviderError::Internal("Empty response content".into()));
    }

    let tokens_used = v["usage"]["total_tokens"].as_u64().unwrap_or(0);
    let model_used = v["model"].as_str().unwrap_or("unknown").to_string();

    Ok(CompletionResult { text, tokens_used, latency_ms, model_used })
}

fn map_http_error(status: u16, body: &str) -> ProviderError {
    let message: String = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| body.chars().take(200).collect());

    let retry_after: Option<u64> = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v["error"]["metadata"]["headers"]["X-RateLimit-Reset-Requests"]
                .as_str()
                .and_then(|s| s.parse().ok())
        });

    match status {
        401 => ProviderError::Auth(message),
        402 => ProviderError::Internal(format!("Insufficient credits: {message}")),
        429 => ProviderError::RateLimited { retry_after },
        404 => ProviderError::ModelNotFound(message),
        408 | 504 => ProviderError::Timeout(30_000),
        500..=599 => ProviderError::Internal(format!("Server error {status}: {message}")),
        _ => ProviderError::Internal(format!("HTTP {status}: {message}")),
    }
}

// ─── Helpers (used by main.rs) ────────────────────────────────────────────────

/// Human-readable description of a `ProviderError` for CLI output.
pub fn format_provider_error(e: &ProviderError) -> String {
    match e {
        ProviderError::Auth(msg) => {
            format!("Authentication failed: {msg}\nCheck your API key environment variable.")
        }
        ProviderError::RateLimited { retry_after } => {
            let hint = retry_after
                .map(|s| format!(" (retry in {s}s)"))
                .unwrap_or_default();
            format!("Rate limited{hint}.")
        }
        ProviderError::ModelNotFound(msg) => format!("Model not found: {msg}"),
        ProviderError::Network(msg) => format!("Network error: {msg}"),
        ProviderError::Timeout(ms) => format!("Timed out after {ms}ms."),
        ProviderError::Internal(msg) => format!("Provider error: {msg}"),
        ProviderError::CircuitBreakerOpen => "Circuit breaker open.".into(),
    }
}

/// Convert `ProviderError` to the `ErrorKind` stored in the ledger.
pub fn provider_error_to_kind(e: &ProviderError) -> ErrorKind {
    e.kind()
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_kind_from_str() {
        assert_eq!(ProviderKind::from_str("openrouter"), Some(ProviderKind::OpenRouter));
        assert_eq!(ProviderKind::from_str("kilo"),       Some(ProviderKind::Kilo));
        assert_eq!(ProviderKind::from_str("Kilo"),       Some(ProviderKind::Kilo));
        assert_eq!(ProviderKind::from_str("cerebras"),   Some(ProviderKind::Cerebras));
        assert_eq!(ProviderKind::from_str("groq"),       Some(ProviderKind::Groq));
        assert_eq!(ProviderKind::from_str("Groq"),       Some(ProviderKind::Groq));
        assert_eq!(ProviderKind::from_str("unknown"),    None);
    }

    #[test]
    fn test_with_proxy_sets_proxy_url() {
        let client = CompatClient::from_key_and_provider("openrouter", "sk-test-key").unwrap();
        assert!(client.proxy_url.is_none());
        let proxied = client.with_proxy("http://1.2.3.4:8080");
        assert_eq!(proxied.proxy_url.as_deref(), Some("http://1.2.3.4:8080"));
    }

    #[test]
    fn test_build_agent_no_proxy() {
        let client = CompatClient::from_key_and_provider("groq", "gsk-test").unwrap();
        // Should build without panic even without a proxy
        let _agent = client.build_agent();
    }

    #[test]
    fn test_provider_kind_urls() {
        assert!(ProviderKind::OpenRouter.base_url().contains("openrouter"));
        assert!(ProviderKind::Kilo.base_url().contains("kilo.ai"));
        assert!(ProviderKind::Cerebras.base_url().contains("cerebras"));
        assert!(ProviderKind::Groq.base_url().contains("groq"));
    }

    #[test]
    fn test_parse_success_response() {
        let raw = r#"{
            "model": "openai/gpt-oss-120b:free",
            "choices": [{"message": {"role": "assistant", "content": "Hello!"}}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8}
        }"#;
        let result = parse_success(raw, 500).unwrap();
        assert_eq!(result.text, "Hello!");
        assert_eq!(result.tokens_used, 8);
        assert_eq!(result.model_used, "openai/gpt-oss-120b:free");
    }

    #[test]
    fn test_map_401_to_auth_error() {
        let err = map_http_error(401, r#"{"error":{"message":"Invalid key"}}"#);
        assert!(matches!(err, ProviderError::Auth(_)));
    }

    #[test]
    fn test_map_429_to_rate_limited() {
        let err = map_http_error(429, r#"{"error":{"message":"Too many requests"}}"#);
        assert!(matches!(err, ProviderError::RateLimited { .. }));
    }

    #[test]
    fn test_inline_error_in_200_response() {
        let raw = r#"{"error": {"code": 401, "message": "Invalid API key"}}"#;
        let result = parse_success(raw, 100);
        assert!(matches!(result.unwrap_err(), ProviderError::Auth(_)));
    }

    #[test]
    fn test_format_auth_error() {
        let e = ProviderError::Auth("bad key".into());
        let msg = format_provider_error(&e);
        assert!(msg.contains("Authentication failed"));
        assert!(msg.contains("API key"));
    }
}
