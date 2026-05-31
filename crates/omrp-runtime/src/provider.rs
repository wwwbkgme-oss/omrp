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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderKind {
    /// https://openrouter.ai — OPENROUTER_API_KEY — 50-1000 free req/day
    OpenRouter,
    /// https://kilo.ai — KILO_API_KEY — kilo/auto-free smart router
    Kilo,
    /// https://cloud.cerebras.ai — CEREBRAS_API_KEY — 14,400 req/day, wafer-fast
    Cerebras,
    /// https://console.groq.com — GROQ_API_KEY — 1,000-14,400 req/day, ultra-low latency
    Groq,
    /// https://api.buw.xyz — BUW_API_KEY — BUW virtual model gateway
    Buw,
}

impl ProviderKind {
    /// Resolve from the string stored in config (`provider = "groq"`)
    /// or from a model id prefix (e.g. `"openrouter/auto"` → `openrouter`).
    pub fn from_str(s: &str) -> Option<Self> {
        // Try exact provider name first, then extract prefix from model IDs
        let lower = s.to_lowercase();
        let prefix = lower.split('/').next().unwrap_or(&lower);
        match prefix {
            "openrouter" => Some(Self::OpenRouter),
            "kilo"       => Some(Self::Kilo),
            "cerebras"   => Some(Self::Cerebras),
            "groq"       => Some(Self::Groq),
            "buw"        => Some(Self::Buw),
            // well-known model families → infer provider
            "llama-3.3-70b-versatile" | "llama-3.1-8b-instant"
            | "mixtral-8x7b-32768" | "qwen-qwq-32b"
            | "gemma2-9b-it" => Some(Self::Groq),
            "gpt-oss-120b" | "llama-3.3-70b" => Some(Self::Cerebras),
            _ => None,
        }
    }

    pub fn base_url(&self) -> &'static str {
        match self {
            Self::OpenRouter => "https://openrouter.ai/api/v1",
            Self::Kilo       => "https://api.kilo.ai/api/gateway",
            Self::Cerebras   => "https://api.cerebras.ai/v1",
            Self::Groq       => "https://api.groq.com/openai/v1",
            Self::Buw        => "https://api.buw.xyz/v1",
        }
    }

    pub fn api_key_env(&self) -> &'static str {
        match self {
            Self::OpenRouter => "OPENROUTER_API_KEY",
            Self::Kilo       => "KILO_API_KEY",
            Self::Cerebras   => "CEREBRAS_API_KEY",
            Self::Groq       => "GROQ_API_KEY",
            Self::Buw        => "BUW_API_KEY",
        }
    }

    pub fn to_str(&self) -> &'static str {
        match self {
            Self::OpenRouter => "openrouter",
            Self::Kilo       => "kilo",
            Self::Cerebras   => "cerebras",
            Self::Groq       => "groq",
            Self::Buw        => "buw",
        }
    }

    #[allow(dead_code)]
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::OpenRouter => "OpenRouter",
            Self::Kilo       => "Kilo Gateway",
            Self::Cerebras   => "Cerebras",
            Self::Groq       => "Groq",
            Self::Buw        => "BUW",
        }
    }

    /// Best free/default models to show in the playground when a key is configured.
    /// Returned as (model_id, display_hint).
    pub fn free_models(&self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::OpenRouter => &[
                ("openrouter/auto",                                    "OR smart router"),
                ("meta-llama/llama-3.3-70b-instruct:free",             "Llama 3.3 70B free"),
                ("mistralai/mistral-7b-instruct:free",                 "Mistral 7B free"),
                ("google/gemma-3-12b-it:free",                         "Gemma 3 12B free"),
                ("deepseek/deepseek-chat-v3-0324:free",                "DeepSeek v3 free"),
                ("qwen/qwen3-8b:free",                                 "Qwen 3 8B free"),
                ("openrouter/quasar-alpha",                            "Quasar Alpha free"),
            ],
            Self::Groq => &[
                ("llama-3.3-70b-versatile",  "Llama 3.3 70B"),
                ("llama-3.1-8b-instant",     "Llama 3.1 8B"),
                ("gemma2-9b-it",             "Gemma 2 9B"),
                ("qwen-qwq-32b",             "Qwen QwQ 32B"),
            ],
            Self::Cerebras => &[
                ("llama-3.3-70b",  "Llama 3.3 70B"),
                ("llama3.1-8b",    "Llama 3.1 8B"),
            ],
            Self::Kilo => &[
                ("kilo/auto-free", "Kilo auto-free"),
            ],
            Self::Buw => &[
                ("buw/auto", "BUW auto"),
            ],
        }
    }

    /// All known providers in priority order.
    pub fn all() -> &'static [ProviderKind] {
        &[
            ProviderKind::OpenRouter,
            ProviderKind::Groq,
            ProviderKind::Cerebras,
            ProviderKind::Kilo,
            ProviderKind::Buw,
        ]
    }
}

// ─── Free-tier provider catalog ───────────────────────────────────────────────
//
// Curated list of publicly documented OpenAI-compatible APIs that offer a
// free tier. All data is sourced from each provider's public documentation.
// Rates and limits change — treat as informational, not authoritative.

/// A single free-tier model entry within a provider.
pub struct FreeModel {
    pub id:          &'static str,
    pub context_k:   u32,     // context window in K tokens
    pub description: &'static str,
}

/// Metadata for one free-tier LLM API provider.
pub struct FreeProvider {
    pub id:            &'static str,  // slug, used in OMRP as `provider` field
    pub name:          &'static str,
    pub country_emoji: &'static str,
    pub base_url:      &'static str,
    pub signup_url:    &'static str,
    pub key_env:       &'static str,  // env var name for the API key
    pub free_note:     &'static str,  // one-liner describing the free tier
    pub compat:        &'static str,  // "openai" | "openai+anthropic" | "custom"
    pub models:        &'static [FreeModel],
}

/// All known free-tier OpenAI-compatible LLM API providers.
pub static FREE_PROVIDERS: &[FreeProvider] = &[
    FreeProvider {
        id: "openrouter", name: "OpenRouter", country_emoji: "🇺🇸",
        base_url: "https://openrouter.ai/api/v1",
        signup_url: "https://openrouter.ai/keys",
        key_env: "OPENROUTER_API_KEY",
        free_note: "35+ models with :free suffix — no daily token cap on free models",
        compat: "openai",
        models: &[
            FreeModel { id:"openrouter/auto",                                 context_k:200, description:"Smart routing" },
            FreeModel { id:"meta-llama/llama-3.3-70b-instruct:free",          context_k:131, description:"Llama 3.3 70B" },
            FreeModel { id:"deepseek/deepseek-chat-v3-0324:free",             context_k:164, description:"DeepSeek V3" },
            FreeModel { id:"google/gemma-3-27b-it:free",                      context_k:128, description:"Gemma 3 27B" },
            FreeModel { id:"qwen/qwen3-8b:free",                              context_k:128, description:"Qwen 3 8B" },
            FreeModel { id:"mistralai/mistral-7b-instruct:free",              context_k:32,  description:"Mistral 7B" },
            FreeModel { id:"openrouter/quasar-alpha",                         context_k:1000,description:"Quasar Alpha (1M ctx)" },
        ],
    },
    FreeProvider {
        id: "groq", name: "Groq", country_emoji: "🇺🇸",
        base_url: "https://api.groq.com/openai/v1",
        signup_url: "https://console.groq.com/keys",
        key_env: "GROQ_API_KEY",
        free_note: "Free tier — ultra-low latency LPU inference, 1K–14K req/day per model",
        compat: "openai",
        models: &[
            FreeModel { id:"llama-3.3-70b-versatile",  context_k:128, description:"Llama 3.3 70B" },
            FreeModel { id:"llama-3.1-8b-instant",      context_k:128, description:"Llama 3.1 8B (fast)" },
            FreeModel { id:"gemma2-9b-it",              context_k:8,   description:"Gemma 2 9B" },
            FreeModel { id:"qwen-qwq-32b",              context_k:128, description:"Qwen QwQ 32B (reasoning)" },
            FreeModel { id:"compound-beta",             context_k:128, description:"Groq Compound Beta" },
        ],
    },
    FreeProvider {
        id: "cerebras", name: "Cerebras", country_emoji: "🇺🇸",
        base_url: "https://api.cerebras.ai/v1",
        signup_url: "https://cloud.cerebras.ai/",
        key_env: "CEREBRAS_API_KEY",
        free_note: "Free tier — wafer-scale chips, ~2600 tok/s, 1M tokens/day cap",
        compat: "openai",
        models: &[
            FreeModel { id:"llama-3.3-70b",  context_k:128, description:"Llama 3.3 70B" },
            FreeModel { id:"llama3.1-8b",    context_k:128, description:"Llama 3.1 8B" },
        ],
    },
    FreeProvider {
        id: "kilo", name: "Kilo Code", country_emoji: "🇺🇸",
        base_url: "https://api.kilo.ai/api/gateway",
        signup_url: "https://kilo.ai",
        key_env: "KILO_API_KEY",
        free_note: "Free models with no credit card — kilo/auto-free smart router",
        compat: "openai",
        models: &[
            FreeModel { id:"kilo/auto-free", context_k:200, description:"Auto-router (free tier)" },
        ],
    },
    FreeProvider {
        id: "gemini", name: "Google Gemini", country_emoji: "🇺🇸",
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        signup_url: "https://aistudio.google.com/app/apikey",
        key_env: "GEMINI_API_KEY",
        free_note: "Free tier — not available in EU/UK/CH. Prompts may be used for improvement.",
        compat: "openai",
        models: &[
            FreeModel { id:"gemini-2.5-flash",      context_k:1000, description:"Gemini 2.5 Flash (10 RPM / 250 RPD)" },
            FreeModel { id:"gemini-2.5-flash-lite",  context_k:1000, description:"Gemini 2.5 Flash-Lite (15 RPM / 1K RPD)" },
        ],
    },
    FreeProvider {
        id: "mistral", name: "Mistral AI", country_emoji: "🇫🇷",
        base_url: "https://api.mistral.ai/v1",
        signup_url: "https://console.mistral.ai/api-keys",
        key_env: "MISTRAL_API_KEY",
        free_note: "Free Experiment plan — no credit card, ~1B tokens/month",
        compat: "openai",
        models: &[
            FreeModel { id:"mistral-small-latest",    context_k:32,  description:"Mistral Small" },
            FreeModel { id:"open-mistral-7b",         context_k:32,  description:"Mistral 7B (open)" },
            FreeModel { id:"open-mixtral-8x7b",       context_k:32,  description:"Mixtral 8x7B (open)" },
        ],
    },
    FreeProvider {
        id: "github-models", name: "GitHub Models", country_emoji: "🇺🇸",
        base_url: "https://models.inference.ai.azure.com",
        signup_url: "https://github.com/marketplace/models",
        key_env: "GITHUB_TOKEN",
        free_note: "Free for all GitHub users — 45+ models, per-request rate limits",
        compat: "openai",
        models: &[
            FreeModel { id:"gpt-4o-mini",                  context_k:128, description:"GPT-4o mini" },
            FreeModel { id:"meta-llama-3.3-70b-instruct",  context_k:128, description:"Llama 3.3 70B" },
            FreeModel { id:"mistral-large-latest",         context_k:128, description:"Mistral Large" },
        ],
    },
    FreeProvider {
        id: "llm7", name: "LLM7.io", country_emoji: "🇬🇧",
        base_url: "https://api.llm7.io/v1",
        signup_url: "https://token.llm7.io",
        key_env: "LLM7_API_KEY",
        free_note: "Zero-friction — no registration for basic access, 30+ models",
        compat: "openai",
        models: &[
            FreeModel { id:"gpt-4o",                        context_k:128, description:"GPT-4o compat" },
            FreeModel { id:"claude-3-5-sonnet-20241022",    context_k:200, description:"Claude Sonnet compat" },
        ],
    },
    FreeProvider {
        id: "cohere", name: "Cohere", country_emoji: "🇨🇦",
        base_url: "https://api.cohere.com/v2",
        signup_url: "https://dashboard.cohere.com/api-keys",
        key_env: "COHERE_API_KEY",
        free_note: "Trial key, no credit card — 1,000 API calls/month, non-commercial",
        compat: "openai",
        models: &[
            FreeModel { id:"command-a-03-2025",  context_k:256, description:"Command A 111B" },
            FreeModel { id:"command-r-plus",     context_k:128, description:"Command R+" },
            FreeModel { id:"command-r7b-12-2024",context_k:128, description:"Command R 7B" },
        ],
    },
    FreeProvider {
        id: "nvidia-nim", name: "NVIDIA NIM", country_emoji: "🇺🇸",
        base_url: "https://integrate.api.nvidia.com/v1",
        signup_url: "https://build.nvidia.com/explore/discover",
        key_env: "NVIDIA_API_KEY",
        free_note: "Free with NVIDIA Developer Program — 100+ models, no daily token cap",
        compat: "openai",
        models: &[
            FreeModel { id:"meta/llama-3.3-70b-instruct", context_k:128, description:"Llama 3.3 70B" },
            FreeModel { id:"nvidia/llama-3.1-nemotron-ultra-253b-v1",context_k:128,description:"Nemotron Ultra 253B" },
        ],
    },
    FreeProvider {
        id: "siliconflow", name: "SiliconFlow", country_emoji: "🇨🇳",
        base_url: "https://api.siliconflow.cn/v1",
        signup_url: "https://cloud.siliconflow.cn/account/ak",
        key_env: "SILICONFLOW_API_KEY",
        free_note: "Signup credits + permanently free models available",
        compat: "openai",
        models: &[
            FreeModel { id:"Qwen/Qwen2.5-7B-Instruct",           context_k:128, description:"Qwen 2.5 7B" },
            FreeModel { id:"THUDM/glm-4-9b-chat",                context_k:128, description:"GLM-4 9B" },
            FreeModel { id:"deepseek-ai/DeepSeek-V2.5",          context_k:128, description:"DeepSeek V2.5" },
        ],
    },
    FreeProvider {
        id: "cloudflare", name: "Cloudflare Workers AI", country_emoji: "🇺🇸",
        base_url: "https://api.cloudflare.com/client/v4/accounts/{account_id}/ai/v1",
        signup_url: "https://dash.cloudflare.com/profile/api-tokens",
        key_env: "CLOUDFLARE_API_KEY",
        free_note: "10,000 Neurons/day free — 50+ models, requires account_id in URL",
        compat: "openai",
        models: &[
            FreeModel { id:"@cf/meta/llama-3.3-70b-instruct-fp8-fast", context_k:128, description:"Llama 3.3 70B fast" },
            FreeModel { id:"@cf/google/gemma-3-12b-it",                 context_k:32,  description:"Gemma 3 12B" },
        ],
    },
    FreeProvider {
        id: "buw", name: "BUW Gateway", country_emoji: "🌐",
        base_url: "https://api.buw.xyz/v1",
        signup_url: "https://api.buw.xyz",
        key_env: "BUW_API_KEY",
        free_note: "BUW virtual model gateway",
        compat: "openai",
        models: &[
            FreeModel { id:"buw/auto", context_k:200, description:"Auto-routing" },
        ],
    },
];

/// Serialise the entire catalog to a JSON array for the `/api/public/providers` endpoint.
pub fn free_providers_json() -> serde_json::Value {
    use serde_json::json;
    serde_json::Value::Array(
        FREE_PROVIDERS.iter().map(|p| json!({
            "id":           p.id,
            "name":         p.name,
            "country":      p.country_emoji,
            "base_url":     p.base_url,
            "signup_url":   p.signup_url,
            "key_env":      p.key_env,
            "free_note":    p.free_note,
            "compat":       p.compat,
            "models": p.models.iter().map(|m| json!({
                "id":          m.id,
                "context_k":   m.context_k,
                "description": m.description,
            })).collect::<Vec<_>>(),
        })).collect()
    )
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
                    ProviderKind::Buw        => "https://api.buw.xyz",
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
