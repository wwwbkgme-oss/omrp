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
    /// Resolve from the string stored in config (`provider = "groq"`).
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "openrouter" => Some(Self::OpenRouter),
            "kilo"       => Some(Self::Kilo),
            "cerebras"   => Some(Self::Cerebras),
            "groq"       => Some(Self::Groq),
            "buw"        => Some(Self::Buw),
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
}

// ─── Public types ─────────────────────────────────────────────────────────────

/// A single chat message.
#[derive(Debug, Clone)]
pub struct Message {
    pub role: String,
    pub content: String,
}

impl Message {
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
pub struct CompatClient {
    base_url: String,
    api_key: String,
    #[allow(dead_code)]
    kind: ProviderKind,
}

impl CompatClient {
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
        Ok(Self { base_url: kind.base_url().into(), api_key, kind })
    }

    /// Build a client using an explicit API key (e.g. stored in the database).
    /// This bypasses the env-var lookup used by `for_provider`.
    pub fn from_key_and_provider(provider: &str, api_key: &str) -> Result<Self, String> {
        let kind = ProviderKind::from_str(provider)
            .ok_or_else(|| format!("Unknown provider: {provider:?}"))?;
        Ok(Self { base_url: kind.base_url().into(), api_key: api_key.to_string(), kind })
    }

    /// Provider name for display.
    #[allow(dead_code)]
    pub fn provider_name(&self) -> &str {
        self.kind.display_name()
    }

    /// Make a **streaming** chat request.
    ///
    /// Returns the raw SSE `Box<dyn Read>` body on success.
    /// The caller is responsible for forwarding chunks to the client.
    /// Errors that arrive as HTTP status codes (4xx/5xx) are mapped to
    /// `ProviderError` before returning so the caller can try a fallback.
    pub fn stream_request(
        &self,
        body: &Value,
    ) -> Result<Box<dyn std::io::Read + Send + 'static>, ProviderError> {
        let url = format!("{}/chat/completions", self.base_url);
        let body_str = body.to_string();

        match ureq::post(&url)
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

        let response = ureq::post(&url)
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
    /// - 429 RateLimited → wait `retry_after` (max 60 s), retry once.
    /// - Network error   → wait 1 s, retry once.
    /// - Any other error → return immediately.
    pub fn complete_with_retry(
        &self,
        model_id: &str,
        messages: &[Message],
        max_tokens: Option<u32>,
    ) -> Result<CompletionResult, ProviderError> {
        match self.complete(model_id, messages, max_tokens) {
            Err(ProviderError::RateLimited { retry_after }) => {
                let wait = retry_after.unwrap_or(5).min(60);
                eprintln!("  [rate-limited] waiting {wait}s before retry…");
                std::thread::sleep(Duration::from_secs(wait));
                self.complete(model_id, messages, max_tokens)
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
