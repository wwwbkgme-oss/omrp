//! Authentication: Argon2id password hashing, JWT tokens, axum middleware.
//!
//! ## Flow
//!
//! 1. On first run `onboarding_needed()` returns `true` → redirect to `/setup`.
//! 2. `POST /api/auth/login` validates password → `issue_token()` → JWT cookie.
//! 3. Every protected axum handler extracts `AuthUser` via `FromRequestParts`.
//! 4. Admin-only handlers additionally check `AuthUser::is_admin`.
//! 5. API clients may send `Authorization: Bearer omrp-sk-…` instead of a
//!    cookie; the middleware resolves these against the `api_keys` table.

use std::sync::Arc;

use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    async_trait,
    extract::{FromRequestParts, State},
    http::{request::Parts, StatusCode},
    response::{IntoResponse, Json, Response},
};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

use crate::db::Database;

// ─── JWT Claims ───────────────────────────────────────────────────────────────

/// JWT payload stored in every access token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// Subject — the user's UUID.
    pub sub: String,
    /// Username (display convenience, not authoritative).
    pub username: String,
    /// Whether the user has admin role.
    pub is_admin: bool,
    /// Expiry (Unix seconds).
    pub exp: u64,
    /// Issued-at (Unix seconds).
    pub iat: u64,
}

// ─── Authenticated user extracted by middleware ───────────────────────────────

/// Carries the resolved identity of a request's caller.
///
/// Extracted by `FromRequestParts`; handlers that require auth list it as a
/// parameter — axum rejects requests that fail extraction with 401.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id:  String,
    pub username: String,
    pub is_admin: bool,
    /// The API-key ID if the request was authenticated via Bearer key.
    pub api_key_id: Option<String>,
}

// ─── Application state threaded through axum ─────────────────────────────────

/// Shared state injected into every axum handler.
#[derive(Clone)]
pub struct AppState {
    pub db:         Database,
    pub jwt_secret: String,
    pub cfg:        std::sync::Arc<crate::config::Config>,
}

impl AppState {
    pub fn new(db: Database, jwt_secret: impl Into<String>) -> Self {
        Self {
            db,
            jwt_secret: jwt_secret.into(),
            cfg: std::sync::Arc::new(crate::config::Config::builtin_defaults()),
        }
    }
}

// ─── Password hashing ─────────────────────────────────────────────────────────

/// Hash a plaintext password using Argon2id with a random salt.
pub fn hash_password(password: &str) -> Result<String, AuthError> {
    let salt   = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    argon2
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| AuthError::Internal(e.to_string()))
}

/// Verify a plaintext password against a stored Argon2id hash.
pub fn verify_password(password: &str, hash: &str) -> Result<bool, AuthError> {
    let parsed = PasswordHash::new(hash)
        .map_err(|e| AuthError::Internal(e.to_string()))?;
    Ok(Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
}

// ─── JWT helpers ─────────────────────────────────────────────────────────────

/// Mint a new JWT for the given user.
pub fn issue_token(
    user_id: &str, username: &str, is_admin: bool,
    expiry_secs: u64, secret: &str,
) -> Result<String, AuthError> {
    let now  = now_secs();
    let claims = Claims {
        sub:      user_id.to_string(),
        username: username.to_string(),
        is_admin,
        iat: now,
        exp: now + expiry_secs,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    ).map_err(|e| AuthError::Internal(e.to_string()))
}

/// Decode and validate a JWT.  Returns `Err(AuthError::Expired)` for stale
/// tokens and `Err(AuthError::Invalid)` for tampered ones.
pub fn decode_token(token: &str, secret: &str) -> Result<Claims, AuthError> {
    decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )
    .map(|d| d.claims)
    .map_err(|e| {
        use jsonwebtoken::errors::ErrorKind;
        match e.kind() {
            ErrorKind::ExpiredSignature => AuthError::Expired,
            _ => AuthError::Invalid,
        }
    })
}

// ─── axum FromRequestParts ────────────────────────────────────────────────────

/// Extract an `AuthUser` from the incoming request.
///
/// Resolution order:
/// 1. `Authorization: Bearer omrp-sk-…` header → SHA-256 hash → `api_keys` table
/// 2. `Authorization: Bearer <jwt>` header → JWT decode
/// 3. `omrp_token` cookie → JWT decode
///
/// Returns `401` if none of the above resolves.
#[async_trait]
impl FromRequestParts<Arc<AppState>> for AuthUser {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        // ── 1. Bearer header ─────────────────────────────────────────────────
        if let Some(bearer) = bearer_from_parts(parts) {
            // a) OMRP API key (omrp-sk- prefix)
            if bearer.starts_with("omrp-sk-") {
                let hash = sha256_hex(&bearer);
                if let Ok(Some(key)) = state.db.find_api_key_by_hash(&hash) {
                    if key.is_active {
                        // Update last_used non-blockingly (best-effort)
                        let db2 = state.db.clone();
                        let kid = key.id.clone();
                        let _  = std::thread::spawn(move || {
                            let _ = db2.touch_api_key(&kid, now_secs() as i64);
                        });
                        // Resolve user info from key.user_id
                        let (user_id, username, is_admin) =
                            if let Some(uid) = &key.user_id {
                                resolve_user_info(&state.db, uid)
                            } else {
                                // Orphan key (admin-created global key) → treat as admin
                                ("system".into(), "system".into(), true)
                            };
                        return Ok(AuthUser {
                            user_id,
                            username,
                            is_admin,
                            api_key_id: Some(key.id),
                        });
                    }
                }
                return Err(unauthorized("Invalid or revoked API key"));
            }

            // b) JWT bearer token
            return match decode_token(&bearer, &state.jwt_secret) {
                Ok(claims) => Ok(AuthUser {
                    user_id:    claims.sub,
                    username:   claims.username,
                    is_admin:   claims.is_admin,
                    api_key_id: None,
                }),
                Err(AuthError::Expired) => Err(unauthorized("Token expired")),
                Err(_)                  => Err(unauthorized("Invalid token")),
            };
        }

        // ── 2. Cookie ────────────────────────────────────────────────────────
        if let Some(token) = cookie_from_parts(parts, "omrp_token") {
            return match decode_token(&token, &state.jwt_secret) {
                Ok(claims) => Ok(AuthUser {
                    user_id:    claims.sub,
                    username:   claims.username,
                    is_admin:   claims.is_admin,
                    api_key_id: None,
                }),
                Err(AuthError::Expired) => Err(unauthorized("Session expired")),
                Err(_)                  => Err(unauthorized("Invalid session")),
            };
        }

        Err(unauthorized("Authentication required"))
    }
}

// ─── Optional auth (for endpoints that work with or without login) ────────────

/// Like `AuthUser` but returns `None` instead of 401 when unauthenticated.
pub struct MaybeAuthUser(pub Option<AuthUser>);

#[async_trait]
impl FromRequestParts<Arc<AppState>> for MaybeAuthUser {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let user = AuthUser::from_request_parts(parts, state).await.ok();
        Ok(MaybeAuthUser(user))
    }
}

// ─── Error type ───────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum AuthError {
    /// JWT signature invalid or malformed.
    Invalid,
    /// JWT exp claim is in the past.
    Expired,
    /// Internal crypto or DB error.
    Internal(String),
    /// Correct credentials but account is disabled.
    Disabled,
    /// Username/password mismatch.
    BadCredentials,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid       => write!(f, "Invalid token"),
            Self::Expired       => write!(f, "Token expired"),
            Self::Internal(e)   => write!(f, "Internal auth error: {e}"),
            Self::Disabled      => write!(f, "Account disabled"),
            Self::BadCredentials => write!(f, "Invalid credentials"),
        }
    }
}

impl std::error::Error for AuthError {}

// ─── Login / logout helpers ───────────────────────────────────────────────────

/// Request body for `POST /api/auth/login`.
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

/// Response body for a successful login.
#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub token:    String,
    pub user_id:  String,
    pub username: String,
    pub is_admin: bool,
}

/// Validate credentials, emit a JWT, and return a `Set-Cookie` response.
///
/// Returns `(token, user_id, username, is_admin)` or an `AuthError`.
pub fn authenticate(
    db: &Database,
    username: &str,
    password: &str,
    jwt_secret: &str,
    expiry_secs: u64,
) -> Result<LoginResponse, AuthError> {
    let user = db
        .find_user_by_username(username)
        .map_err(|e| AuthError::Internal(e.to_string()))?
        .ok_or(AuthError::BadCredentials)?;

    if !user.is_active {
        return Err(AuthError::Disabled);
    }

    if !verify_password(password, &user.password_hash)? {
        return Err(AuthError::BadCredentials);
    }

    // Update last_login
    let _ = db.set_user_last_login(&user.id, now_secs() as i64);

    let token = issue_token(&user.id, &user.username, user.is_admin, expiry_secs, jwt_secret)?;

    Ok(LoginResponse {
        token,
        user_id:  user.id,
        username: user.username,
        is_admin: user.is_admin,
    })
}

/// Build a `Set-Cookie` header value for the JWT.
pub fn auth_cookie(token: &str, expiry_secs: u64) -> String {
    format!(
        "omrp_token={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={expiry_secs}"
    )
}

/// Build a cookie that clears the session.
pub fn logout_cookie() -> &'static str {
    "omrp_token=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT"
}

// ─── Onboarding ───────────────────────────────────────────────────────────────

/// Returns `true` when the setup wizard should be shown (no users exist yet).
pub fn onboarding_needed(db: &Database) -> bool {
    !db.has_users()
}

// ─── Private helpers ──────────────────────────────────────────────────────────

fn bearer_from_parts(parts: &Parts) -> Option<String> {
    parts
        .headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|v| v.trim().to_string())
}

fn cookie_from_parts(parts: &Parts, name: &str) -> Option<String> {
    let cookie_hdr = parts.headers.get("cookie")?.to_str().ok()?;
    for part in cookie_hdr.split(';') {
        let part = part.trim();
        if let Some(val) = part.strip_prefix(&format!("{name}=")) {
            return Some(val.to_string());
        }
    }
    None
}

fn resolve_user_info(db: &Database, user_id: &str) -> (String, String, bool) {
    db.find_user_by_id(user_id)
        .ok()
        .flatten()
        .map(|u| (u.id, u.username, u.is_admin))
        .unwrap_or_else(|| (user_id.to_string(), "unknown".into(), false))
}

/// SHA-256 hex digest of a string — used to hash API keys before DB lookup.
pub fn sha256_hex(input: &str) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(input.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn unauthorized(msg: &'static str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({
            "error": { "message": msg, "type": "unauthorized", "code": 401 }
        })),
    )
        .into_response()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "test-secret-key-32-chars-minimum!!";

    #[test]
    fn test_password_hash_and_verify() {
        let hash = hash_password("hunter2").unwrap();
        assert!(hash.starts_with("$argon2id$"));
        assert!(verify_password("hunter2", &hash).unwrap());
        assert!(!verify_password("wrong", &hash).unwrap());
    }

    #[test]
    fn test_jwt_roundtrip() {
        let token = issue_token("user-uuid", "alice", false, 3600, SECRET).unwrap();
        let claims = decode_token(&token, SECRET).unwrap();
        assert_eq!(claims.sub, "user-uuid");
        assert_eq!(claims.username, "alice");
        assert!(!claims.is_admin);
    }

    #[test]
    fn test_jwt_wrong_secret() {
        let token = issue_token("uid", "bob", true, 3600, SECRET).unwrap();
        let result = decode_token(&token, "wrong-secret");
        assert!(matches!(result, Err(AuthError::Invalid)));
    }

    #[test]
    fn test_sha256_hex() {
        // Known SHA-256 of "hello"
        let h = sha256_hex("hello");
        assert_eq!(h, "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
    }

    #[test]
    fn test_onboarding_needed_when_no_users() {
        let db = Database::open_in_memory().unwrap();
        db.migrate().unwrap();
        db.seed_defaults().unwrap();
        assert!(onboarding_needed(&db));
    }

    #[test]
    fn test_authenticate_full_flow() {
        let db = Database::open_in_memory().unwrap();
        db.migrate().unwrap();
        db.seed_defaults().unwrap();

        let ts  = now_secs() as i64;
        let row = crate::db::UserRow {
            id:            "u-test".into(),
            username:      "testuser".into(),
            email:         None,
            password_hash: hash_password("s3cr3t").unwrap(),
            display_name:  "Test".into(),
            is_active:     true,
            is_admin:      true,
            created_at:    ts,
            updated_at:    ts,
            last_login:    None,
        };
        db.insert_user(&row).unwrap();
        db.assign_role("u-test", "role_admin").unwrap();

        // Correct credentials
        let resp = authenticate(&db, "testuser", "s3cr3t", SECRET, 3600).unwrap();
        assert_eq!(resp.username, "testuser");
        assert!(resp.is_admin);

        // Wrong password
        let err = authenticate(&db, "testuser", "wrong", SECRET, 3600);
        assert!(matches!(err, Err(AuthError::BadCredentials)));

        // Onboarding no longer needed
        assert!(!onboarding_needed(&db));
    }
}
