use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ErrorKind {
    RateLimited { retry_after: Option<u64> },
    Timeout { timeout_ms: u64 },
    AuthError,
    ModelNotAvailable,
    NetworkError(String),
    InternalError(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ProviderError {
    Network(String),
    Auth(String),
    RateLimited { retry_after: Option<u64> },
    ModelNotFound(String),
    Timeout(u64),
    Internal(String),
    CircuitBreakerOpen,
}

impl ProviderError {
    pub fn kind(&self) -> ErrorKind {
        match self {
            ProviderError::Network(s) => ErrorKind::NetworkError(s.clone()),
            ProviderError::Auth(_s) => ErrorKind::AuthError,
            ProviderError::RateLimited { retry_after } => ErrorKind::RateLimited { retry_after: *retry_after },
            ProviderError::ModelNotFound(_s) => ErrorKind::ModelNotAvailable,
            ProviderError::Timeout(ms) => ErrorKind::Timeout { timeout_ms: *ms },
            ProviderError::Internal(s) => ErrorKind::InternalError(s.clone()),
            ProviderError::CircuitBreakerOpen => ErrorKind::InternalError("circuit breaker open".into()),
        }
    }
}
