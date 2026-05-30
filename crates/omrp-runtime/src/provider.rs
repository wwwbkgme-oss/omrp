//! OpenRouter provider adapter.
//!
//! Reads the API key from `OPENROUTER_API_KEY` — never hard-code keys.
//! All calls are synchronous (no async / no tokio).

use std::time::{Duration, Instant};

use omrp_events::error::{ErrorKind, ProviderError};
use serde_json::{json, Value};

const BASE_URL: &str = "https://openrouter.ai/api/v1";
const HTTP_REFERER: &str = "https://github.com/wwwbkgme-oss/omrp";
const APP_TITLE: &str = "OMRP";
const DEFAULT_MAX_TOKENS: u32 = 1024;

// ─── Public types ─────────────────────────────────────────────────────────────

/// A single message in the conversation.
#[derive(Debug, Clone)]
pub struct Message {
    pub role: String,
    pub content: String,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: "user".into(), content: content.into() }
    }
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: "system".into(), content: content.into() }
    }
}

/// Successful completion result.
#[derive(Debug)]
pub struct CompletionResult {
    /// Response text from the model.
    pub text: String,
    /// Total tokens consumed (prompt + completion).
    pub tokens_used: u64,
    /// Wall-clock latency in milliseconds.
    pub latency_ms: u64,
    /// Actual model used (may differ from requested if OpenRouter rerouted).
    pub model_used: String,
}

// ─── Client ──────────────────────────────────────────────────────────────────

/// Blocking HTTP client for the OpenRouter chat/completions API.
pub struct OpenRouterClient {
    api_key: String,
}

impl OpenRouterClient {
    /// Create a client using `OPENROUTER_API_KEY` env var.
    ///
    /// Returns a clear error message if the variable is not set.
    pub fn from_env() -> Result<Self, String> {
        let key = std::env::var("OPENROUTER_API_KEY").map_err(|_| {
            concat!(
                "OPENROUTER_API_KEY is not set.\n",
                "\n",
                "  export OPENROUTER_API_KEY=sk-or-v1-...\n",
                "\n",
                "Get a free key at: https://openrouter.ai/keys"
            )
            .to_string()
        })?;
        Ok(Self { api_key: key })
    }

    /// Send a chat-completion request to the given `model_id`.
    ///
    /// On success returns a `CompletionResult`.
    /// On failure maps the HTTP error to a `ProviderError`.
    pub fn complete(
        &self,
        model_id: &str,
        messages: &[Message],
        max_tokens: Option<u32>,
    ) -> Result<CompletionResult, ProviderError> {
        let url = format!("{BASE_URL}/chat/completions");

        // Build request body.
        let msgs: Vec<Value> = messages
            .iter()
            .map(|m| json!({"role": m.role, "content": m.content}))
            .collect();

        let body = json!({
            "model": model_id,
            "messages": msgs,
            "max_tokens": max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        });
        let body_str = body.to_string();

        let started = Instant::now();

        let response = ureq::post(&url)
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .set("Content-Type", "application/json")
            .set("HTTP-Referer", HTTP_REFERER)
            .set("X-Title", APP_TITLE)
            .send_string(&body_str);

        let latency_ms = started.elapsed().as_millis() as u64;

        match response {
            Ok(resp) => {
                let raw = resp.into_string().map_err(|e| {
                    ProviderError::Internal(format!("Failed to read response body: {e}"))
                })?;
                parse_success(&raw, latency_ms)
            }
            Err(ureq::Error::Status(status, resp)) => {
                let raw = resp.into_string().unwrap_or_default();
                Err(map_http_error(status, &raw))
            }
            Err(ureq::Error::Transport(t)) => {
                Err(ProviderError::Network(t.to_string()))
            }
        }
    }

    /// Complete with automatic retry + fallback logic.
    ///
    /// - On `RateLimited`: waits `retry_after` seconds (capped at 60 s) then
    ///   retries the **same** model once.
    /// - On `Network` error: retries once after 1 s.
    /// - On any other error: returns immediately (caller decides fallback).
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

    /// Validate the API key by calling `GET /auth/key`.
    /// Returns `Ok(credits_remaining)` or an error string.
    pub fn validate_key(&self) -> Result<Option<f64>, String> {
        let url = format!("{BASE_URL}/auth/key");
        match ureq::get(&url)
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .call()
        {
            Ok(resp) => {
                let raw = resp.into_string().map_err(|e| e.to_string())?;
                let v: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
                let credits = v["data"]["limit_remaining"].as_f64();
                Ok(credits)
            }
            Err(ureq::Error::Status(401, _)) => {
                Err("Invalid API key — check OPENROUTER_API_KEY".into())
            }
            Err(e) => Err(e.to_string()),
        }
    }
}

// ─── Parsing helpers ──────────────────────────────────────────────────────────

fn parse_success(raw: &str, latency_ms: u64) -> Result<CompletionResult, ProviderError> {
    let v: Value = serde_json::from_str(raw)
        .map_err(|e| ProviderError::Internal(format!("JSON parse error: {e}")))?;

    // Check for an inline error object (OpenRouter sometimes returns 200 + error).
    if let Some(err) = v.get("error") {
        let code = err["code"].as_u64().unwrap_or(0) as u16;
        let msg = err["message"].as_str().unwrap_or("unknown error").to_string();
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
    let model_used = v["model"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();

    Ok(CompletionResult { text, tokens_used, latency_ms, model_used })
}

fn map_http_error(status: u16, body: &str) -> ProviderError {
    // Try to extract the message from OpenRouter's error envelope.
    let message: String = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v["error"]["message"]
                .as_str()
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| body.chars().take(200).collect());

    // Extract retry_after from the error metadata if present.
    let retry_after: Option<u64> = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v["error"]["metadata"]["headers"]["X-RateLimit-Reset-Requests"]
                .as_str()
                .and_then(|s| s.parse::<u64>().ok())
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

/// Map a `ProviderError` to a human-readable string for CLI output.
pub fn format_provider_error(e: &ProviderError) -> String {
    match e {
        ProviderError::Auth(msg) => {
            format!("Authentication failed: {msg}\n\nCheck your OPENROUTER_API_KEY.")
        }
        ProviderError::RateLimited { retry_after } => {
            let hint = retry_after
                .map(|s| format!(" (retry in {s}s)"))
                .unwrap_or_default();
            format!("Rate limited{hint}. Try again later or switch models.")
        }
        ProviderError::ModelNotFound(msg) => {
            format!("Model not found: {msg}")
        }
        ProviderError::Network(msg) => {
            format!("Network error: {msg}")
        }
        ProviderError::Timeout(ms) => {
            format!("Request timed out after {ms}ms.")
        }
        ProviderError::Internal(msg) => {
            format!("Provider error: {msg}")
        }
        ProviderError::CircuitBreakerOpen => {
            "Circuit breaker open — model temporarily excluded.".into()
        }
    }
}

/// Convert a `ProviderError` to the `ErrorKind` that gets stored in the ledger.
pub fn provider_error_to_kind(e: &ProviderError) -> ErrorKind {
    e.kind()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_success_response() {
        let raw = r#"{
            "model": "anthropic/claude-3.5-sonnet",
            "choices": [{"message": {"role": "assistant", "content": "Hello!"}}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8}
        }"#;
        let result = parse_success(raw, 500).unwrap();
        assert_eq!(result.text, "Hello!");
        assert_eq!(result.tokens_used, 8);
        assert_eq!(result.latency_ms, 500);
        assert_eq!(result.model_used, "anthropic/claude-3.5-sonnet");
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
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ProviderError::Auth(_)));
    }

    #[test]
    fn test_format_auth_error() {
        let e = ProviderError::Auth("bad key".into());
        let msg = format_provider_error(&e);
        assert!(msg.contains("Authentication failed"));
        assert!(msg.contains("OPENROUTER_API_KEY"));
    }
}
