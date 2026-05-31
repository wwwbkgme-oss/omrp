//! OMRP axum web server — multi-user dashboard + OpenAI-compatible LLM proxy.
//!
//! ## Endpoint groups
//!
//! | Prefix           | Auth       | Purpose                               |
//! |------------------|------------|---------------------------------------|
//! `/api/auth/`      | varies     | Login, logout, current-user           |
//! `/api/setup`      | none       | First-run onboarding wizard           |
//! `/api/admin/`     | admin JWT  | User/key/settings management          |
//! `/api/user/`      | user JWT   | Personal keys, provider keys, stats   |
//! `/v1/`            | key or JWT | OpenAI-compatible LLM proxy           |
//! `/health`         | none       | Uptime + version                      |
//! `/`               | none       | SPA (admin/user dashboard)            |

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, HeaderName, HeaderValue, Request, StatusCode},
    middleware,
    response::{Html, IntoResponse, Json, Response},
    routing::{delete, get, post, put},
    Router,
};
use bytes::Bytes;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::task;
use tokio_stream::wrappers::ReceiverStream;
use tower_http::cors::{Any, CorsLayer};
use uuid::Uuid;

use crate::auth::{
    authenticate, auth_cookie, hash_password, logout_cookie, onboarding_needed,
    sha256_hex, AppState, AuthUser, LoginRequest, MaybeAuthUser,
};
use crate::config::Config;
use crate::db::{ApiKeyPermissions, ApiKeyRow, Database, MAX_FAILED_LOGINS, ProviderKeyRow, UserRow};
use crate::routing::{bootstrap_pipeline, select_for_tier, tier_model_ids};
use crate::validation::{
    validate_allowed_models, validate_api_key_value, validate_display_name,
    validate_email, validate_label, validate_password, validate_username,
};
use omrp_core::classifier::{classify_prompt, detect_mode_override};
use omrp_core::router::RouterEngine;
use omrp_types::task::RouteRequest;

// ─── JSON error helper ────────────────────────────────────────────────────────

fn err(status: StatusCode, msg: impl std::fmt::Display) -> Response {
    (status, Json(json!({ "error": { "message": msg.to_string(), "code": status.as_u16() } }))).into_response()
}

fn ok(body: Value) -> Response {
    Json(body).into_response()
}

// ─── Security headers middleware ──────────────────────────────────────────────
//
// Applied to every response via .layer(middleware::from_fn(security_headers)).
// These headers harden the SPA against common web attacks:
//
//  X-Frame-Options            — prevent clickjacking (older browsers)
//  X-Content-Type-Options     — prevent MIME-type sniffing
//  Referrer-Policy            — limit referrer info sent cross-origin
//  X-XSS-Protection           — disable legacy XSS auditor (causes issues)
//  Permissions-Policy         — restrict powerful browser APIs
//  Content-Security-Policy    — inline-friendly policy for the monolithic SPA:
//    default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self'
//    'unsafe-inline'; img-src 'self' data:; connect-src 'self';
//    frame-ancestors 'none'; base-uri 'self'; form-action 'self'

const SEC_HEADERS: &[(&str, &str)] = &[
    ("x-frame-options",          "DENY"),
    ("x-content-type-options",   "nosniff"),
    ("referrer-policy",          "strict-origin-when-cross-origin"),
    ("x-xss-protection",         "0"),
    ("permissions-policy",       "camera=(), microphone=(), geolocation=()"),
    ("content-security-policy",
        "default-src 'self'; \
         script-src 'self' 'unsafe-inline'; \
         style-src 'self' 'unsafe-inline'; \
         img-src 'self' data:; \
         connect-src 'self'; \
         frame-ancestors 'none'; \
         base-uri 'self'; \
         form-action 'self'"),
];

async fn security_headers_mw(req: Request<Body>, next: middleware::Next) -> Response {
    let mut resp = next.run(req).await;
    let hdrs = resp.headers_mut();
    for (name, value) in SEC_HEADERS {
        let n = HeaderName::from_static(name);
        let v = HeaderValue::from_static(value);
        hdrs.entry(n).or_insert(v);
    }
    resp
}

// ─── Router ───────────────────────────────────────────────────────────────────

pub fn build_router(state: Arc<AppState>) -> Router {
    // CORS: allow all origins so browser tools can call /v1/
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
        .expose_headers(Any);

    Router::new()
        // ── SPA ──────────────────────────────────────────────────────────────
        .route("/",          get(serve_spa))
        .route("/setup",     get(serve_spa))
        // ── Auth ─────────────────────────────────────────────────────────────
        .route("/api/auth/login",    post(auth_login))
        .route("/api/auth/logout",   post(auth_logout))
        .route("/api/auth/me",       get(auth_me))
        .route("/api/auth/register", post(auth_register))
        // ── Onboarding ───────────────────────────────────────────────────────
        .route("/api/setup",        post(setup_init))
        .route("/api/setup/status", get(setup_status))
        // ── Admin: users ─────────────────────────────────────────────────────
        .route("/api/admin/users",             get(admin_list_users).post(admin_create_user))
        .route("/api/admin/users/:id",         put(admin_update_user).delete(admin_delete_user))
        .route("/api/admin/users/:id/roles",   put(admin_set_user_roles))
        // ── Admin: roles ─────────────────────────────────────────────────────
        .route("/api/admin/roles", get(admin_list_roles))
        // ── Admin: API keys ───────────────────────────────────────────────────
        .route("/api/admin/api-keys",     get(admin_list_api_keys).post(admin_create_api_key))
        .route("/api/admin/api-keys/:id", delete(admin_revoke_api_key))
        // ── Admin: provider keys ──────────────────────────────────────────────
        .route("/api/admin/provider-keys",     get(admin_list_provider_keys).post(admin_create_provider_key))
        .route("/api/admin/provider-keys/:id", delete(admin_revoke_provider_key))
        // ── Admin: settings ───────────────────────────────────────────────────
        .route("/api/admin/settings",       get(admin_list_settings))
        .route("/api/admin/settings/:key",  put(admin_set_setting))
        // ── Admin: stats / logs / proxies ─────────────────────────────────────
        .route("/api/admin/stats",              get(admin_stats))
        .route("/api/admin/audit-logs",         get(admin_audit_logs))
        .route("/api/admin/proxies",              get(admin_list_proxies).post(admin_add_proxy))
        .route("/api/admin/proxies/:id",          delete(admin_delete_proxy).put(admin_update_proxy_status))
        .route("/api/admin/proxies/:id/activate", post(admin_activate_proxy))
        .route("/api/admin/proxies/refresh",      post(admin_refresh_proxies))
        // ── Admin: model health + routing intelligence ─────────────────────────
        .route("/api/admin/models/health",  get(admin_model_health))
        .route("/api/admin/routing/stats",  get(admin_routing_stats))
        // ── Admin: per-user stats ─────────────────────────────────────────────
        .route("/api/admin/users/:id/stats", get(admin_user_stats))
        // ── Admin: per-user API key + permissions ─────────────────────────────
        .route("/api/admin/users/:id/key",             get(admin_get_user_key))
        .route("/api/admin/users/:id/key/permissions", put(admin_update_user_key_permissions))
        .route("/api/admin/users/:id/key/reset",        post(admin_reset_user_key))
        // ── Admin: proxy usage stats ──────────────────────────────────────────
        .route("/api/admin/proxies/stats", get(admin_proxy_stats))
        // ── Admin: announcements / news ───────────────────────────────────────
        .route("/api/admin/news",     get(admin_list_news).post(admin_create_news))
        .route("/api/admin/news/:id", put(admin_update_news).delete(admin_delete_news))
        // ── Public: announcements (no auth) ───────────────────────────────────
        .route("/api/public/news",      get(public_news))
        .route("/api/public/providers", get(public_providers))
        // ── User: own key + permissions ────────────────────────────────────────
        .route("/api/user/key",         get(user_get_key))
        .route("/api/user/key/reset",   post(user_reset_own_key))
        .route("/api/user/permissions", get(user_get_permissions))
        // ── User: personal API keys ────────────────────────────────────────────
        .route("/api/user/api-keys",     get(user_list_api_keys).post(user_create_api_key))
        .route("/api/user/api-keys/:id", delete(user_revoke_api_key))
        // ── User: provider keys ───────────────────────────────────────────────
        .route("/api/user/provider-keys",     get(user_list_provider_keys).post(user_create_provider_key))
        .route("/api/user/provider-keys/:id", delete(user_revoke_provider_key))
        // ── User: stats & profile ──────────────────────────────────────────────
        .route("/api/user/stats", get(user_stats))
        .route("/api/user/me",    get(user_profile).put(user_update_profile))
        // ── OpenAI-compatible proxy ────────────────────────────────────────────
        .route("/v1/chat/completions", post(proxy_completions))
        .route("/chat/completions",    post(proxy_completions))
        .route("/v1/models",           get(proxy_models))
        .route("/models",              get(proxy_models))
        // ── Meta ──────────────────────────────────────────────────────────────
        .route("/health", get(health))
        .route("/stats",  get(server_stats))
        .with_state(state)
        .layer(middleware::from_fn(security_headers_mw))
        .layer(cors)
}

/// Start the axum server.  Blocks until the server stops.
pub async fn run(cfg: Config, host: &str, port: u16) {
    let db_path = crate::db::default_db_path();
    let db = Database::open(&db_path).expect("Cannot open database");
    db.migrate().expect("DB migration failed");
    db.seed_defaults().expect("DB seed failed");

    // Resolve or generate JWT secret
    let jwt_secret = match db.get_setting("auth.jwt_secret").unwrap_or(None) {
        Some(s) => s,
        None => {
            use rand::RngCore;
            let mut buf = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut buf);
            let secret: String = buf.iter().map(|b| format!("{b:02x}")).collect();
            db.set_setting("auth.jwt_secret", &secret, now_secs() as i64).ok();
            secret
        }
    };

    // Build proxy pool — load from DB if proxy.enabled = 1
    let refresh_interval: i64 = db.get_setting("proxy.refresh_interval")
        .unwrap_or(None).and_then(|s| s.parse().ok()).unwrap_or(3600);
    let proxy_pool = crate::proxy::ProxyPool::new(refresh_interval);
    let proxy_enabled = db.get_setting("proxy.enabled")
        .unwrap_or(None).map(|v| v == "1").unwrap_or(false);
    if proxy_enabled {
        proxy_pool.load(&db);
        eprintln!("[proxy] enabled — {} proxies loaded", proxy_pool.len());
    } else {
        eprintln!("[proxy] disabled (enable: Admin → Settings → proxy.enabled = 1)");
    }

    let state = Arc::new(AppState {
        db,
        jwt_secret,
        cfg: Arc::new(cfg),
        proxy_pool,
    });

    // If enabled but pool was empty, trigger a background refresh
    if proxy_enabled && state.proxy_pool.is_empty() {
        let state2 = state.clone();
        tokio::spawn(async move {
            if let Some(url) = state2.db.get_setting("proxy.source_url").unwrap_or(None) {
                let s = state2.clone();
                tokio::task::spawn_blocking(move || {
                    crate::proxy::refresh_proxy_pool(&s.db, &url);
                    s.proxy_pool.load(&s.db);
                }).await.ok();
            }
        });
    }

    let app  = build_router(state.clone());
    let addr = format!("{host}:{port}");

    println!("OMRP  \x1b[36mhttp://{addr}\x1b[0m");
    println!("  Dashboard   http://{addr}/");
    println!("  API         http://{addr}/v1/chat/completions");
    println!("  Setup       http://{addr}/setup  (if first run)");
    println!("  Proxy pool  {} active proxies", state.proxy_pool.len());
    println!();

    // ── Background proxy auto-refresh task ───────────────────────────────────
    // When proxy.enabled=1 the pool is refreshed every refresh_interval seconds
    // so the proxy list stays current.  Also triggers if pool drops to zero.
    if proxy_enabled {
        let state_bg = state.clone();
        tokio::spawn(async move {
            loop {
                // Sleep for the configured interval
                let interval = state_bg.db.get_setting("proxy.refresh_interval")
                    .unwrap_or(None).and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(3600);
                tokio::time::sleep(tokio::time::Duration::from_secs(interval)).await;

                // Check if still enabled
                let still_on = state_bg.db.get_setting("proxy.enabled")
                    .unwrap_or(None).map(|v| v == "1").unwrap_or(false);
                if !still_on { break; }

                let s = state_bg.clone();
                tokio::task::spawn_blocking(move || {
                    if let Some(url) = s.db.get_setting("proxy.source_url").unwrap_or(None) {
                        eprintln!("[proxy] auto-refresh starting…");
                        crate::proxy::refresh_proxy_pool(&s.db, &url);
                        s.proxy_pool.load(&s.db);
                        eprintln!("[proxy] auto-refresh done: {} proxies", s.proxy_pool.len());
                    }
                }).await.ok();
            }
        });
    }

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| { eprintln!("Cannot bind {addr}: {e}"); std::process::exit(1); });

    eprintln!("[omrp] listening on {addr}");

    // Graceful shutdown: wait for SIGTERM or SIGINT before draining connections.
    // axum::serve.with_graceful_shutdown() waits for in-flight requests to finish.
    let shutdown = async {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigterm = signal(SignalKind::terminate())
                .expect("Cannot install SIGTERM handler");
            let mut sigint = signal(SignalKind::interrupt())
                .expect("Cannot install SIGINT handler");
            tokio::select! {
                _ = sigterm.recv() => eprintln!("[omrp] SIGTERM received, shutting down…"),
                _ = sigint.recv()  => eprintln!("[omrp] SIGINT received, shutting down…"),
            }
        }
        #[cfg(not(unix))]
        {
            tokio::signal::ctrl_c().await.ok();
            eprintln!("[omrp] Ctrl-C received, shutting down…");
        }
    };

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .unwrap_or_else(|e| eprintln!("[omrp] serve error: {e}"));
    eprintln!("[omrp] shutdown complete.");
}

// ─── SPA (cyberpunk enterprise dashboard) ─────────────────────────────────────
// Loaded from spa.html at compile time via include_str!

static SPA_HTML: &str = include_str!("spa.html");

async fn serve_spa(_state: State<Arc<AppState>>) -> impl IntoResponse {
    Html(SPA_HTML)
}

// ─── Auth handlers ────────────────────────────────────────────────────────────

async fn auth_login(
    State(state): State<Arc<AppState>>,
    Json(req): Json<LoginRequest>,
) -> Response {
    // ── Brute-force guard ──────────────────────────────────────────────────────
    // Check BEFORE running Argon2 to prevent both enumeration and CPU exhaustion.
    if let Ok(Some(secs)) = state.db.login_lockout_secs(&req.username) {
        let mins = (secs + 59) / 60;
        return err(StatusCode::TOO_MANY_REQUESTS,
            format!("Account temporarily locked after {MAX_FAILED_LOGINS} failed attempts. \
                     Try again in {mins} minute(s)."));
    }

    let expiry: u64 = state.db.get_setting("jwt.expiry_secs").ok().flatten()
        .and_then(|s| s.parse().ok()).unwrap_or(86400);
    match authenticate(&state.db, &req.username, &req.password, &state.jwt_secret, expiry) {
        Ok(resp) => {
            // Successful login: clear any recorded failed attempts
            let _ = state.db.clear_login_attempts(&req.username);
            let _ = state.db.audit(Some(&resp.user_id), "auth.login", None, None, None, None, now_secs() as i64);
            let cookie = auth_cookie(&resp.token, expiry);
            let mut headers = HeaderMap::new();
            if let Ok(v) = cookie.parse::<HeaderValue>() { headers.insert("Set-Cookie", v); }
            (StatusCode::OK, headers, Json(json!({
                "token":    resp.token,
                "user_id":  resp.user_id,
                "username": resp.username,
                "is_admin": resp.is_admin,
            }))).into_response()
        }
        Err(e) => {
            // Failed login: record attempt (may lock the account)
            let _ = state.db.record_failed_login(&req.username);
            let _ = state.db.audit(None, "auth.login.failed", None, None, Some(&req.username), None, now_secs() as i64);
            err(StatusCode::UNAUTHORIZED, e)
        }
    }
}

async fn auth_logout(
    State(state): State<Arc<AppState>>,
    user: MaybeAuthUser,
) -> Response {
    if let Some(u) = &user.0 {
        let _ = state.db.audit(Some(&u.user_id), "auth.logout", None, None, None, None, now_secs() as i64);
    }
    let mut headers = HeaderMap::new();
    if let Ok(v) = logout_cookie().parse::<HeaderValue>() { headers.insert("Set-Cookie", v); }
    (StatusCode::OK, headers, Json(json!({ "status": "ok" }))).into_response()
}

async fn auth_me(user: AuthUser) -> Response {
    ok(json!({
        "user_id":  user.user_id,
        "username": user.username,
        "is_admin": user.is_admin,
    }))
}

// ─── Public self-registration ─────────────────────────────────────────────────

#[derive(Deserialize)]
struct RegisterRequest {
    username:     String,
    password:     String,
    display_name: Option<String>,
    email:        Option<String>,
}

/// Public self-registration endpoint.
///
/// Only active when the `app.registration_open` DB setting is `"1"`.
/// Creates a non-admin account with default API key permissions and
/// returns a JWT (same shape as `/api/auth/login`).
async fn auth_register(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RegisterRequest>,
) -> Response {
    // Check feature flag — default off
    let open = state.db.get_setting("app.registration_open")
        .ok().flatten().as_deref() == Some("1");
    if !open {
        return err(StatusCode::FORBIDDEN,
            "Registration is closed. Contact an administrator to create an account.");
    }
    // Validate inputs
    if let Err(e) = validate_username(req.username.trim()) { return err(StatusCode::BAD_REQUEST, e); }
    if let Err(e) = validate_password(&req.password)       { return err(StatusCode::BAD_REQUEST, e); }
    if let Err(e) = validate_display_name(req.display_name.as_deref().unwrap_or("")) {
        return err(StatusCode::BAD_REQUEST, e);
    }
    if let Err(e) = validate_email(req.email.as_deref().unwrap_or("")) {
        return err(StatusCode::BAD_REQUEST, e);
    }
    // Hash password and create user
    let hash = match hash_password(&req.password) {
        Ok(h) => h,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let ts  = now_secs() as i64;
    let uid = Uuid::new_v4().to_string();
    let row = UserRow {
        id: uid.clone(),
        username:      req.username.trim().to_string(),
        email:         req.email,
        password_hash: hash,
        display_name:  req.display_name.unwrap_or_default(),
        is_active:     true,
        is_admin:      false,  // self-registered users are never admins
        created_at:    ts,
        updated_at:    ts,
        last_login:    None,
    };
    if let Err(e) = state.db.insert_user(&row) {
        let msg = e.to_string();
        if msg.contains("UNIQUE") {
            return err(StatusCode::CONFLICT, "Username is already taken");
        }
        return err(StatusCode::INTERNAL_SERVER_ERROR, msg);
    }
    // Auto-generate an API key for the new user
    let key_plain = generate_api_key();
    let key_hash  = sha256_hex(&key_plain);
    let prefix    = key_plain.chars().take(16).collect::<String>();
    let _ = state.db.insert_api_key(&ApiKeyRow {
        id:          generate_key_id(),
        user_id:     Some(uid.clone()),
        key_hash,
        key_prefix:  prefix,
        label:       "Default".into(),
        is_active:   true,
        permissions: "{}".into(),
        created_at:  ts,
        last_used:   None,
        expires_at:  None,
    });
    // Audit log
    let _ = state.db.audit(Some(&uid), "auth.register", None, None, None, None, ts);
    // Issue JWT and log the user in
    let expiry: u64 = state.db.get_setting("jwt.expiry_secs").ok().flatten()
        .and_then(|s| s.parse().ok()).unwrap_or(86400);
    match crate::auth::authenticate(&state.db, &row.username, &req.password,
        &state.jwt_secret, expiry) {
        Ok(resp) => {
            let cookie = auth_cookie(&resp.token, expiry);
            let mut headers = HeaderMap::new();
            if let Ok(v) = cookie.parse::<HeaderValue>() { headers.insert("Set-Cookie", v); }
            (StatusCode::CREATED, headers, Json(json!({
                "token":    resp.token,
                "user_id":  resp.user_id,
                "username": resp.username,
                "is_admin": false,
                "api_key":  key_plain,
            }))).into_response()
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn setup_status(State(state): State<Arc<AppState>>) -> Response {
    let reg_open = state.db.get_setting("app.registration_open")
        .ok().flatten().as_deref() == Some("1");
    ok(json!({
        "onboarding_needed":   onboarding_needed(&state.db),
        "registration_open":   reg_open,
    }))
}

#[derive(Deserialize)]
struct SetupRequest {
    username:     String,
    password:     String,
    display_name: Option<String>,
    app_name:     Option<String>,
}

async fn setup_init(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SetupRequest>,
) -> Response {
    if !onboarding_needed(&state.db) {
        return err(StatusCode::CONFLICT, "Setup already completed");
    }
    if req.username.trim().is_empty() || req.password.len() < 8 {
        return err(StatusCode::BAD_REQUEST, "Username required, password must be ≥ 8 chars");
    }
    if let Err(e) = validate_username(req.username.trim()) {
        return err(StatusCode::BAD_REQUEST, e);
    }
    if let Err(e) = validate_password(&req.password) {
        return err(StatusCode::BAD_REQUEST, e);
    }
    if let Err(e) = validate_display_name(req.display_name.as_deref().unwrap_or("")) {
        return err(StatusCode::BAD_REQUEST, e);
    }
    let hash = match hash_password(&req.password) {
        Ok(h) => h,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let ts = now_secs() as i64;
    let uid = Uuid::new_v4().to_string();
    let row = UserRow {
        id:            uid.clone(),
        username:      req.username.trim().to_string(),
        email:         None,
        password_hash: hash,
        display_name:  req.display_name.unwrap_or_default(),
        is_active:     true,
        is_admin:      true,
        created_at:    ts,
        updated_at:    ts,
        last_login:    None,
    };
    if let Err(e) = state.db.insert_user(&row) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    let _ = state.db.assign_role(&uid, "role_admin");
    if let Some(name) = req.app_name {
        let _ = state.db.set_setting("app.name", &name, ts);
    }
    // Auto-generate admin's single API key with full permissions (proxy bypass enabled)
    let admin_key = match state.db.create_user_api_key(&uid, "admin-default", &ApiKeyPermissions::admin_default()) {
        Ok((_, raw)) => raw,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, format!("Key gen failed: {e}")),
    };
    let _ = state.db.audit(Some(&uid), "setup.init", Some(&uid), Some("user"), None, None, ts);
    ok(json!({
        "status":   "ok",
        "user_id":  uid,
        "api_key":  admin_key,
        "message":  "Admin account created. Save your API key — it will not be shown again.",
    }))
}

// ─── Admin: users ─────────────────────────────────────────────────────────────

fn require_admin(user: &AuthUser) -> Option<Response> {
    if !user.is_admin {
        Some(err(StatusCode::FORBIDDEN, "Admin access required"))
    } else {
        None
    }
}

async fn admin_list_users(
    State(state): State<Arc<AppState>>, user: AuthUser,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    match state.db.list_users() {
        Ok(users) => ok(json!({ "data": users.iter().map(|u| json!({
            "id": u.id, "username": u.username, "email": u.email,
            "display_name": u.display_name, "is_active": u.is_active,
            "is_admin": u.is_admin, "created_at": u.created_at, "last_login": u.last_login,
        })).collect::<Vec<_>>() })),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

#[derive(Deserialize)]
struct CreateUserRequest {
    username:     String,
    password:     String,
    email:        Option<String>,
    display_name: Option<String>,
    is_admin:     Option<bool>,
}

async fn admin_create_user(
    State(state): State<Arc<AppState>>, user: AuthUser,
    Json(req): Json<CreateUserRequest>,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    if let Err(e) = validate_username(req.username.trim()) {
        return err(StatusCode::BAD_REQUEST, e);
    }
    if let Err(e) = validate_password(&req.password) {
        return err(StatusCode::BAD_REQUEST, e);
    }
    if let Err(e) = validate_display_name(req.display_name.as_deref().unwrap_or("")) {
        return err(StatusCode::BAD_REQUEST, e);
    }
    if let Err(e) = validate_email(req.email.as_deref().unwrap_or("")) {
        return err(StatusCode::BAD_REQUEST, e);
    }
    let hash = match hash_password(&req.password) {
        Ok(h) => h,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let ts = now_secs() as i64;
    let uid = Uuid::new_v4().to_string();
    let row = UserRow {
        id: uid.clone(), username: req.username.trim().to_string(),
        email: req.email, password_hash: hash,
        display_name: req.display_name.unwrap_or_default(),
        is_active: true, is_admin: req.is_admin.unwrap_or(false),
        created_at: ts, updated_at: ts, last_login: None,
    };
    if let Err(e) = state.db.insert_user(&row) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    let role = if row.is_admin { "role_admin" } else { "role_user" };
    let _ = state.db.assign_role(&uid, role);

    // Auto-generate the user's single API key
    let default_perms = if row.is_admin {
        ApiKeyPermissions::admin_default()
    } else {
        ApiKeyPermissions::default()
    };
    let user_key = match state.db.create_user_api_key(&uid, "account-default", &default_perms) {
        Ok((_, raw)) => raw,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, format!("Key gen failed: {e}")),
    };

    let _ = state.db.audit(Some(&user.user_id), "user.create", Some(&uid), Some("user"), None, None, ts);
    (StatusCode::CREATED, Json(json!({
        "id":      uid,
        "username": row.username,
        "api_key": user_key,
        "note":    "Share this key with the user — it will not be shown again.",
    }))).into_response()
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct UpdateUserRequest {
    display_name: Option<String>,
    email:        Option<String>,
    is_active:    Option<bool>,
    password:     Option<String>,
}

async fn admin_update_user(
    State(state): State<Arc<AppState>>, user: AuthUser,
    Path(id): Path<String>, Json(req): Json<UpdateUserRequest>,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    let ts = now_secs() as i64;
    if let Some(active) = req.is_active {
        if let Err(e) = state.db.set_user_active(&id, active, ts) {
            return err(StatusCode::INTERNAL_SERVER_ERROR, e);
        }
    }
    if let Some(pw) = req.password {
        if let Err(e) = validate_password(&pw) { return err(StatusCode::BAD_REQUEST, e); }
        let hash = match hash_password(&pw) {
            Ok(h) => h,
            Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e),
        };
        if let Err(e) = state.db.update_user_password(&id, &hash, ts) {
            return err(StatusCode::INTERNAL_SERVER_ERROR, e);
        }
    }
    let _ = state.db.audit(Some(&user.user_id), "user.update", Some(&id), Some("user"), None, None, ts);
    ok(json!({ "status": "ok" }))
}

async fn admin_delete_user(
    State(state): State<Arc<AppState>>, user: AuthUser,
    Path(id): Path<String>,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    if id == user.user_id { return err(StatusCode::BAD_REQUEST, "Cannot delete yourself"); }
    let ts = now_secs() as i64;
    match state.db.delete_user(&id) {
        Ok(true)  => {
            let _ = state.db.audit(Some(&user.user_id), "user.delete", Some(&id), Some("user"), None, None, ts);
            ok(json!({ "status": "ok" }))
        }
        Ok(false) => err(StatusCode::NOT_FOUND, "User not found"),
        Err(e)    => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

#[derive(Deserialize)]
struct SetRolesRequest { roles: Vec<String> }

async fn admin_set_user_roles(
    State(state): State<Arc<AppState>>, user: AuthUser,
    Path(id): Path<String>, Json(req): Json<SetRolesRequest>,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    // Remove existing, then add new ones
    for role_name in ["role_admin", "role_user"] {
        let _ = state.db.remove_role(&id, role_name);
    }
    for role in &req.roles {
        let _ = state.db.assign_role(&id, role);
    }
    ok(json!({ "status": "ok" }))
}

async fn admin_list_roles(
    State(state): State<Arc<AppState>>, user: AuthUser,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    match state.db.list_roles() {
        Ok(roles) => ok(json!({ "data": roles.iter().map(|(id, name, desc)| json!({
            "id": id, "name": name, "description": desc
        })).collect::<Vec<_>>() })),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

// ─── Admin: API keys ──────────────────────────────────────────────────────────

async fn admin_list_api_keys(
    State(state): State<Arc<AppState>>, user: AuthUser,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    match state.db.list_all_api_keys() {
        Ok(keys) => ok(json!({ "data": keys.iter().map(serialize_api_key).collect::<Vec<_>>() })),
        Err(e)   => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

#[derive(Deserialize)]
struct CreateApiKeyRequest {
    label:   Option<String>,
    user_id: Option<String>,
}

async fn admin_create_api_key(
    State(state): State<Arc<AppState>>, user: AuthUser,
    Json(req): Json<CreateApiKeyRequest>,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    let raw_key = generate_api_key();
    let hash    = sha256_hex(&raw_key);
    let prefix  = raw_key.chars().take(16).collect::<String>();
    let ts = now_secs() as i64;
    let row = ApiKeyRow {
        id:         generate_key_id(),
        user_id:    req.user_id,
        key_hash:   hash,
        key_prefix: prefix,
        label:       req.label.unwrap_or_else(|| "default".into()),
        is_active:   true,
        created_at:  ts,
        last_used:   None,
        expires_at:  None,
        permissions: "{}".into(),
    };
    if let Err(e) = state.db.insert_api_key(&row) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    let _ = state.db.audit(Some(&user.user_id), "api_key.create", Some(&row.id), Some("api_key"), None, None, ts);
    // Return the raw key exactly once
    (StatusCode::CREATED, Json(json!({
        "id": row.id, "key": raw_key, "label": row.label,
        "created_at": row.created_at,
        "note": "Copy this key — it will not be shown again.",
    }))).into_response()
}

async fn admin_revoke_api_key(
    State(state): State<Arc<AppState>>, user: AuthUser,
    Path(id): Path<String>,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    let ts = now_secs() as i64;
    match state.db.deactivate_api_key(&id) {
        Ok(true)  => {
            let _ = state.db.audit(Some(&user.user_id), "api_key.revoke", Some(&id), Some("api_key"), None, None, ts);
            ok(json!({ "status": "ok" }))
        }
        Ok(false) => err(StatusCode::NOT_FOUND, "Key not found"),
        Err(e)    => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

// ─── Admin: provider keys ─────────────────────────────────────────────────────

async fn admin_list_provider_keys(
    State(state): State<Arc<AppState>>, user: AuthUser,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    match state.db.list_all_provider_keys() {
        Ok(keys) => ok(json!({ "data": keys.iter().map(serialize_provider_key).collect::<Vec<_>>() })),
        Err(e)   => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

#[derive(Deserialize)]
struct CreateProviderKeyRequest {
    provider:     String,
    key_value:    String,
    label:        Option<String>,
    is_global:    Option<bool>,
    user_id:      Option<String>,
    /// Custom base URL for non-standard / self-hosted providers.
    /// If omitted the built-in URL for `provider` is used.
    base_url:     Option<String>,
    display_name: Option<String>,
}

async fn admin_create_provider_key(
    State(state): State<Arc<AppState>>, user: AuthUser,
    Json(req): Json<CreateProviderKeyRequest>,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    if let Err(e) = validate_api_key_value(&req.key_value) {
        return err(StatusCode::BAD_REQUEST, e);
    }
    if let Err(e) = validate_label(req.label.as_deref().unwrap_or("")) {
        return err(StatusCode::BAD_REQUEST, e);
    }
    let ts = now_secs() as i64;
    let row = ProviderKeyRow {
        id:           Uuid::new_v4().to_string(),
        user_id:      req.user_id,
        provider:     req.provider.clone(),
        key_value:    req.key_value,
        label:        req.label.unwrap_or_default(),
        is_active:    true,
        is_global:    req.is_global.unwrap_or(false),
        created_at:   ts,
        base_url:     req.base_url,
        display_name: req.display_name,
    };
    if let Err(e) = state.db.insert_provider_key(&row) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    let _ = state.db.audit(Some(&user.user_id), "provider_key.create", Some(&row.id), Some("provider_key"), None, None, ts);
    (StatusCode::CREATED, Json(json!({ "id": row.id, "provider": row.provider }))).into_response()
}

async fn admin_revoke_provider_key(
    State(state): State<Arc<AppState>>, user: AuthUser,
    Path(id): Path<String>,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    let ts = now_secs() as i64;
    match state.db.deactivate_provider_key(&id) {
        Ok(true)  => {
            let _ = state.db.audit(Some(&user.user_id), "provider_key.revoke", Some(&id), Some("provider_key"), None, None, ts);
            ok(json!({ "status": "ok" }))
        }
        Ok(false) => err(StatusCode::NOT_FOUND, "Key not found"),
        Err(e)    => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

// ─── Admin: settings ──────────────────────────────────────────────────────────

async fn admin_list_settings(
    State(state): State<Arc<AppState>>, user: AuthUser,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    match state.db.all_settings() {
        Ok(settings) => ok(json!({ "data": settings.iter().map(|(k, v, d)| json!({
            "key": k, "value": v, "description": d
        })).collect::<Vec<_>>() })),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

#[derive(Deserialize)]
struct SetSettingRequest { value: String }

async fn admin_set_setting(
    State(state): State<Arc<AppState>>, user: AuthUser,
    Path(key): Path<String>, Json(req): Json<SetSettingRequest>,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    let ts = now_secs() as i64;
    // Protect auth.jwt_secret from being overwritten via API
    if key == "auth.jwt_secret" {
        return err(StatusCode::FORBIDDEN, "auth.jwt_secret is read-only");
    }
    if let Err(e) = state.db.set_setting(&key, &req.value, ts) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    let _ = state.db.audit(Some(&user.user_id), "settings.update", Some(&key), Some("setting"), Some(&req.value), None, ts);
    ok(json!({ "status": "ok" }))
}

// ─── Admin: stats / logs / proxies ────────────────────────────────────────────

async fn admin_stats(
    State(state): State<Arc<AppState>>, user: AuthUser,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    let stats = state.db.usage_stats(30).unwrap_or_default();
    let proxy_count = state.db.proxy_count().unwrap_or(0);
    let user_count: i64 = state.db.list_users().map(|u| u.len() as i64).unwrap_or(0);
    let key_count: i64  = state.db.list_all_api_keys().map(|k| k.len() as i64).unwrap_or(0);
    ok(json!({
        "daily":       stats.iter().map(|(d, r, e, t)| json!({ "date": d, "requests": r, "errors": e, "tokens": t })).collect::<Vec<_>>(),
        "proxy_count": proxy_count,
        "user_count":  user_count,
        "key_count":   key_count,
    }))
}

async fn admin_audit_logs(
    State(state): State<Arc<AppState>>, user: AuthUser,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    match state.db.recent_audit_logs(100) {
        Ok(logs) => ok(json!({ "data": logs.iter().map(|l| json!({
            "id": l.id, "user_id": l.user_id, "action": l.action,
            "target_id": l.target_id, "target_type": l.target_type,
            "metadata": l.metadata, "ip_addr": l.ip_addr, "created_at": l.created_at,
        })).collect::<Vec<_>>() })),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn admin_list_proxies(
    State(state): State<Arc<AppState>>, user: AuthUser,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    match state.db.full_proxies() {
        Ok(proxies) => ok(json!({ "data": proxies })),
        Err(e)      => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

#[derive(Deserialize)]
struct AddProxyRequest { url: String }

async fn admin_add_proxy(
    State(state): State<Arc<AppState>>, user: AuthUser,
    Json(req): Json<AddProxyRequest>,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    let url = req.url.trim().to_string();
    if url.is_empty() { return err(StatusCode::BAD_REQUEST, "url required"); }
    let protocol = url.split("://").next().unwrap_or("http").to_string();
    let ts = now_secs() as i64;
    match state.db.upsert_proxy(&url, &protocol, None, None, ts) {
        Ok(_)  => ok(json!({ "status": "ok", "url": url })),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn admin_delete_proxy(
    State(state): State<Arc<AppState>>, user: AuthUser,
    Path(id): Path<i64>,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    match state.db.delete_proxy(id) {
        Ok(true)  => { state.proxy_pool.load(&state.db); ok(json!({"status":"ok"})) }
        Ok(false) => err(StatusCode::NOT_FOUND, "proxy not found"),
        Err(e)    => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn admin_activate_proxy(
    State(state): State<Arc<AppState>>, user: AuthUser,
    Path(id): Path<i64>,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    match state.db.activate_proxy(id) {
        Ok(true)  => { state.proxy_pool.load(&state.db); ok(json!({"status":"ok"})) }
        Ok(false) => err(StatusCode::NOT_FOUND, "proxy not found"),
        Err(e)    => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

#[derive(Deserialize)]
struct UpdateProxyStatusRequest { is_active: Option<bool> }

/// `PUT /api/admin/proxies/:id` — activate or deactivate a proxy.
async fn admin_update_proxy_status(
    State(state): State<Arc<AppState>>, user: AuthUser,
    Path(id): Path<i64>, Json(req): Json<UpdateProxyStatusRequest>,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    let result = if req.is_active.unwrap_or(true) {
        state.db.activate_proxy(id)
    } else {
        // Deactivate: mark fail_count=5 which excludes from active pool
        let conn_result = state.db.deactivate_proxy(id);
        conn_result
    };
    match result {
        Ok(true)  => { state.proxy_pool.load(&state.db); ok(json!({"status":"ok"})) }
        Ok(false) => err(StatusCode::NOT_FOUND, "proxy not found"),
        Err(e)    => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn admin_refresh_proxies(
    State(state): State<Arc<AppState>>, user: AuthUser,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    // Kick off background refresh, then reload the in-memory pool
    let url = state.db.get_setting("proxy.source_url").ok().flatten()
        .unwrap_or_else(|| "https://api.proxyscrape.com/v4/free-proxy-list/get?request=display_proxies&proxy_format=protocolipport&format=json".into());
    let s = state.clone();
    task::spawn_blocking(move || {
        crate::proxy::refresh_proxy_pool(&s.db, &url);
        s.proxy_pool.load(&s.db);
        eprintln!("[proxy] manual refresh done: {} proxies", s.proxy_pool.len());
    });
    ok(json!({
        "status":  "ok",
        "message": "Proxy refresh started — pool will reload automatically",
        "count_before": state.proxy_pool.len()
    }))
}

// ─── Admin: model health / Bayesian routing intelligence ─────────────────────

/// `GET /api/admin/models/health`
///
/// Returns every registered model with its full Bayesian health profile:
/// - α/β counts, posterior competence, Wilson Score lower bound
/// - Beta variance stability, latency EMA, inflight
/// - Deterministic BKG-FMR score (all 5 factors) from the last ledger snapshot
/// - Thompson Sampling seed (ledger sequence) for reproducibility
async fn admin_model_health(
    State(state): State<Arc<AppState>>, user: AuthUser,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    let cfg = state.cfg.clone();
    let result = task::spawn_blocking(move || {
        let pipeline = crate::routing::bootstrap_pipeline(&cfg);
        let router   = omrp_core::router::RouterEngine::default();
        let request  = RouteRequest::default();

        let (models_data, selected, fallback_chain, ledger_len, ts_seed) =
            pipeline.state().read(|s| {
                let decision  = router.select(s, &request);
                let ledger_len = pipeline.event_log().len();
                // Thompson sampling seed (matches select_thompson logic)
                let ts_seed = s.diagnostics.total_completions
                    .wrapping_add(s.diagnostics.total_failures.wrapping_mul(2_654_435_761));

                let models_data: Vec<Value> = s.models.iter().map(|m| {
                    let h        = s.health.get(&m.id).cloned().unwrap_or_default();
                    let inflight = s.inflight.get(&m.id).copied().unwrap_or(0);
                    let score_e  = decision.scores.iter().find(|sc| sc.model_id == m.id);

                    let factors: Vec<Value> = score_e.map(|sc| sc.factors.iter().map(|f| json!({
                        "name":         f.name,
                        "value":        f.value,
                        "weight":       f.weight,
                        "contribution": f.contribution(),
                    })).collect()).unwrap_or_default();

                    json!({
                        "id":                  m.id,
                        "provider":            m.provider,
                        "context_window":      m.capabilities.context_window,
                        "tasks":               m.capabilities.task_suitability.iter().map(|t| t.as_str()).collect::<Vec<_>>(),
                        "score":               score_e.map(|sc| sc.total).unwrap_or(0.0),
                        "is_garbage":          h.garbage,
                        "inflight":            inflight,
                        "success_count":       h.success_count,
                        "failure_count":       h.failure_count,
                        "total_obs":           h.total_obs(),
                        "bayesian_competence": h.bayesian_competence(),
                        "wilson_lower":        h.wilson_lower(),
                        "beta_stability":      h.stability_score(),
                        "beta_variance":       h.beta_variance(),
                        "latency_ms":          h.rolling_latency_avg_ms,
                        "success_ratio":       h.success_ratio,
                        "factors":             factors,
                    })
                }).collect();

                (models_data,
                 decision.selected_model.clone(),
                 decision.fallback_chain.clone(),
                 ledger_len,
                 ts_seed)
            });

        json!({
            "models":          models_data,
            "selected":        selected,
            "fallback_chain":  fallback_chain,
            "ledger_events":   ledger_len,
            "ts_seed":         ts_seed,
        })
    }).await;

    match result {
        Ok(v)  => ok(v),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// `GET /api/admin/routing/stats`
///
/// Returns aggregate routing analytics: total completions, failures, fallbacks,
/// garbage models, ledger chain length, and 30-day request statistics.
async fn admin_routing_stats(
    State(state): State<Arc<AppState>>, user: AuthUser,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    let cfg = state.cfg.clone();

    // Get ledger/routing stats from the in-memory pipeline
    let routing = task::spawn_blocking(move || {
        let pipeline = crate::routing::bootstrap_pipeline(&cfg);
        pipeline.state().read(|s| {
            let garbage_count = s.health.values().filter(|h| h.garbage).count();
            json!({
                "total_completions": s.diagnostics.total_completions,
                "total_failures":    s.diagnostics.total_failures,
                "total_fallbacks":   s.diagnostics.total_fallbacks,
                "total_degradations": s.diagnostics.total_degradations,
                "garbage_count":     garbage_count,
                "model_count":       s.models.len(),
                "ledger_events":     pipeline.event_log().len(),
            })
        })
    }).await.unwrap_or_else(|_| json!({}));

    // Get DB request stats
    let daily = state.db.usage_stats(30).unwrap_or_default();
    let total_req: i64 = daily.iter().map(|d| d.1).sum();
    let total_err: i64 = daily.iter().map(|d| d.2).sum();
    let total_tok: i64 = daily.iter().map(|d| d.3).sum();
    let top_models: Vec<Value> = state.db.top_models(10).unwrap_or_default()
        .into_iter().map(|(id, reqs, toks)| json!({
            "model_id": id, "requests": reqs, "tokens": toks
        })).collect();

    ok(json!({
        "routing":     routing,
        "db_stats": {
            "requests_30d": total_req,
            "errors_30d":   total_err,
            "tokens_30d":   total_tok,
        },
        "top_models": top_models,
        "daily": daily.iter().map(|(d,r,e,t)| json!({
            "date":     d,
            "requests": r,
            "errors":   e,
            "tokens":   t,
        })).collect::<Vec<_>>(),
    }))
}

/// `GET /api/admin/users/:id/stats`
///
/// Returns per-user request statistics from request_logs.
async fn admin_user_stats(
    State(state): State<Arc<AppState>>, user: AuthUser,
    Path(uid): Path<String>,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    // Verify user exists
    let target = match state.db.find_user_by_id(&uid) {
        Ok(Some(u)) => u,
        Ok(None)    => return err(StatusCode::NOT_FOUND, "User not found"),
        Err(e)      => return err(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    // Get their API keys (for key count)
    let key_count = state.db.list_api_keys_for_user(&uid)
        .map(|k| k.iter().filter(|k| k.is_active).count())
        .unwrap_or(0);
    let prov_key_count = state.db.list_provider_keys_for_user(&uid)
        .map(|k| k.iter().filter(|k| !k.is_global).count())
        .unwrap_or(0);

    // Per-user daily stats from request_logs
    let daily = state.db.user_usage_stats(&uid, 30).unwrap_or_default();
    let total_req: i64 = daily.iter().map(|d| d.1).sum();
    let total_err: i64 = daily.iter().map(|d| d.2).sum();
    let total_tok: i64 = daily.iter().map(|d| d.3).sum();

    ok(json!({
        "user": {
            "id":           target.id,
            "username":     target.username,
            "display_name": target.display_name,
            "email":        target.email,
            "is_admin":     target.is_admin,
            "is_active":    target.is_active,
            "created_at":   target.created_at,
            "last_login":   target.last_login,
        },
        "api_keys":      key_count,
        "provider_keys": prov_key_count,
        "requests_30d":  total_req,
        "errors_30d":    total_err,
        "tokens_30d":    total_tok,
        "daily": daily.iter().map(|(d,r,e,t)| json!({
            "date": d, "requests": r, "errors": e, "tokens": t
        })).collect::<Vec<_>>(),
    }))
}

// ─── Admin: per-user key management ──────────────────────────────────────────

async fn admin_get_user_key(
    State(state): State<Arc<AppState>>, user: AuthUser,
    Path(uid): Path<String>,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    match state.db.get_user_api_key(&uid) {
        Ok(Some(k)) => ok(json!(serialize_api_key(&k))),
        Ok(None)    => err(StatusCode::NOT_FOUND, "No active API key for this user"),
        Err(e)      => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

#[derive(Deserialize)]
struct UpdatePermissionsRequest {
    can_use_router:       Option<bool>,
    can_use_proxy_bypass: Option<bool>,
    allowed_models:       Option<Vec<String>>,
    rate_limit_per_hour:  Option<u32>,
}

async fn admin_update_user_key_permissions(
    State(state): State<Arc<AppState>>, user: AuthUser,
    Path(uid): Path<String>,
    Json(req): Json<UpdatePermissionsRequest>,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    let key = match state.db.get_user_api_key(&uid) {
        Ok(Some(k)) => k,
        Ok(None)    => return err(StatusCode::NOT_FOUND, "No API key for this user"),
        Err(e)      => return err(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let mut perms = ApiKeyPermissions::from_json(&key.permissions);
    if let Some(v) = req.can_use_router       { perms.can_use_router = v; }
    if let Some(v) = req.can_use_proxy_bypass { perms.can_use_proxy_bypass = v; }
    if let Some(v) = req.allowed_models {
        if let Err(e) = validate_allowed_models(&v) { return err(StatusCode::BAD_REQUEST, e); }
        perms.allowed_models = v;
    }
    if let Some(v) = req.rate_limit_per_hour  { perms.rate_limit_per_hour = v; }
    let ts = now_secs() as i64;
    match state.db.update_api_key_permissions(&key.id, &perms) {
        Ok(_) => {
            let _ = state.db.audit(
                Some(&user.user_id), "api_key.permissions_update",
                Some(&key.id), Some("api_key"),
                Some(&perms.to_json()), None, ts,
            );
            ok(json!({ "status": "ok", "key_id": key.id, "permissions": perms }))
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// `POST /api/admin/users/:id/key/reset`
///
/// Deactivates the user's current API key and generates a fresh one,
/// preserving the existing permissions.  Use when a user has lost their key.
/// The new raw key is returned in the response (shown once).
async fn admin_reset_user_key(
    State(state): State<Arc<AppState>>, user: AuthUser,
    Path(uid): Path<String>,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }

    // Get existing key to preserve permissions
    let (old_id, perms) = match state.db.get_user_api_key(&uid) {
        Ok(Some(k)) => (Some(k.id), ApiKeyPermissions::from_json(&k.permissions)),
        Ok(None)    => (None, ApiKeyPermissions::default()),
        Err(e)      => return err(StatusCode::INTERNAL_SERVER_ERROR, e),
    };

    // Deactivate old key if it exists
    if let Some(ref kid) = old_id {
        let _ = state.db.deactivate_api_key(kid);
    }

    // Generate new key with same permissions
    let ts = now_secs() as i64;
    match state.db.create_user_api_key(&uid, "account-default", &perms) {
        Ok((row, raw_key)) => {
            let _ = state.db.audit(
                Some(&user.user_id), "api_key.reset",
                Some(&row.id), Some("api_key"), None, None, ts,
            );
            (StatusCode::CREATED, Json(json!({
                "status":       "ok",
                "new_key_id":   row.id,
                "api_key":      raw_key,
                "key_prefix":   row.key_prefix,
                "permissions":  perms,
                "note":         "Give this key to the user — it will not be shown again.",
            }))).into_response()
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn admin_proxy_stats(
    State(state): State<Arc<AppState>>, user: AuthUser,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    let stats = state.db.proxy_usage_stats().unwrap_or_default();
    let recent_1h  = state.db.proxy_requests_recent(1).unwrap_or(0);
    let recent_24h = state.db.proxy_requests_recent(24).unwrap_or(0);
    let pool_size  = state.proxy_pool.len();
    ok(json!({
        "pool_size":   pool_size,
        "requests_1h":  recent_1h,
        "requests_24h": recent_24h,
        "proxies": stats.iter().map(|(url, total, ok_c, users, last)| json!({
            "url":       url,
            "total":     total,
            "ok":        ok_c,
            "err":       total - ok_c,
            "success_pct": if *total > 0 { ok_c * 100 / total } else { 100 },
            "users":     users,
            "last_used": last,
        })).collect::<Vec<_>>(),
    }))
}

// ─── User: own key + permissions ─────────────────────────────────────────────

async fn user_get_key(
    State(state): State<Arc<AppState>>, user: AuthUser,
) -> Response {
    match state.db.get_user_api_key(&user.user_id) {
        Ok(Some(k)) => ok(json!({
            "id":          k.id,
            "key_prefix":  k.key_prefix,
            "label":       k.label,
            "is_active":   k.is_active,
            "created_at":  k.created_at,
            "last_used":   k.last_used,
            "permissions": ApiKeyPermissions::from_json(&k.permissions),
        })),
        Ok(None) => err(StatusCode::NOT_FOUND, "No active API key — contact your admin"),
        Err(e)   => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn user_get_permissions(
    State(state): State<Arc<AppState>>, user: AuthUser,
) -> Response {
    match state.db.get_user_api_key(&user.user_id) {
        Ok(Some(k)) => ok(json!(ApiKeyPermissions::from_json(&k.permissions))),
        Ok(None)    => ok(json!(ApiKeyPermissions::default())),
        Err(e)      => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// `POST /api/user/key/reset`
///
/// Invalidates the caller's current API key and generates a new one with the
/// same permissions.  The new key is returned **once** in plain text — it is
/// not stored; only the SHA-256 hash is kept in the database.
async fn user_reset_own_key(
    State(state): State<Arc<AppState>>, user: AuthUser,
) -> Response {
    let ts = now_secs() as i64;

    // Keep the existing permissions so the reset is non-destructive
    let old_perms = state.db.get_user_api_key(&user.user_id)
        .ok().flatten()
        .map(|k| ApiKeyPermissions::from_json(&k.permissions))
        .unwrap_or_default();

    // Revoke all existing keys for this user
    if let Ok(keys) = state.db.get_user_api_key(&user.user_id) {
        if let Some(k) = keys {
            let _ = state.db.deactivate_api_key(&k.id);
        }
    }

    // Issue a fresh key with the same permissions
    match state.db.create_user_api_key(&user.user_id, "account-default", &old_perms) {
        Ok((_, raw_key)) => {
            let _ = state.db.audit(Some(&user.user_id), "user.key.reset", None, None, None, None, ts);
            (StatusCode::CREATED, Json(json!({
                "api_key": raw_key,
                "note":    "Save this key — it will NOT be shown again.",
            }))).into_response()
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

// ─── User: personal API keys ──────────────────────────────────────────────────

async fn user_list_api_keys(
    State(state): State<Arc<AppState>>, user: AuthUser,
) -> Response {
    match state.db.list_api_keys_for_user(&user.user_id) {
        Ok(keys) => ok(json!({ "data": keys.iter().map(serialize_api_key).collect::<Vec<_>>() })),
        Err(e)   => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn user_create_api_key(
    State(state): State<Arc<AppState>>, user: AuthUser,
    Json(req): Json<CreateApiKeyRequest>,
) -> Response {
    let raw_key = generate_api_key();
    let hash    = sha256_hex(&raw_key);
    let prefix  = raw_key.chars().take(16).collect::<String>();
    let ts = now_secs() as i64;
    let row = ApiKeyRow {
        id:          generate_key_id(),
        user_id:     Some(user.user_id.clone()),
        key_hash:    hash,
        key_prefix:  prefix,
        label:       req.label.unwrap_or_else(|| "default".into()),
        is_active:   true,
        created_at:  ts, last_used: None, expires_at: None,
        permissions: "{}".into(),
    };
    if let Err(e) = state.db.insert_api_key(&row) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    (StatusCode::CREATED, Json(json!({
        "id": row.id, "key": raw_key, "label": row.label,
        "created_at": ts, "note": "Copy this key — it will not be shown again.",
    }))).into_response()
}

async fn user_revoke_api_key(
    State(state): State<Arc<AppState>>, user: AuthUser,
    Path(id): Path<String>,
) -> Response {
    // Only allow revoking own keys
    let keys = match state.db.list_api_keys_for_user(&user.user_id) {
        Ok(k) => k,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    if !keys.iter().any(|k| k.id == id) {
        return err(StatusCode::NOT_FOUND, "Key not found or not yours");
    }
    match state.db.deactivate_api_key(&id) {
        Ok(_)  => ok(json!({ "status": "ok" })),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

// ─── User: provider keys ──────────────────────────────────────────────────────

async fn user_list_provider_keys(
    State(state): State<Arc<AppState>>, user: AuthUser,
) -> Response {
    match state.db.list_provider_keys_for_user(&user.user_id) {
        Ok(keys) => ok(json!({ "data": keys.iter().map(serialize_provider_key).collect::<Vec<_>>() })),
        Err(e)   => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn user_create_provider_key(
    State(state): State<Arc<AppState>>, user: AuthUser,
    Json(req): Json<CreateProviderKeyRequest>,
) -> Response {
    if let Err(e) = validate_api_key_value(&req.key_value) {
        return err(StatusCode::BAD_REQUEST, e);
    }
    if let Err(e) = validate_label(req.label.as_deref().unwrap_or("")) {
        return err(StatusCode::BAD_REQUEST, e);
    }
    let ts = now_secs() as i64;
    let row = ProviderKeyRow {
        id:           Uuid::new_v4().to_string(),
        user_id:      Some(user.user_id.clone()),
        provider:     req.provider,
        key_value:    req.key_value,
        label:        req.label.unwrap_or_default(),
        is_active:    true,
        is_global:    false,   // users can't create global keys
        created_at:   ts,
        base_url:     req.base_url,
        display_name: req.display_name,
    };
    if let Err(e) = state.db.insert_provider_key(&row) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    (StatusCode::CREATED, Json(json!({ "id": row.id, "provider": row.provider }))).into_response()
}

async fn user_revoke_provider_key(
    State(state): State<Arc<AppState>>, user: AuthUser,
    Path(id): Path<String>,
) -> Response {
    let keys = match state.db.list_provider_keys_for_user(&user.user_id) {
        Ok(k) => k,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    if !keys.iter().any(|k| k.id == id && k.user_id.as_deref() == Some(&user.user_id)) {
        return err(StatusCode::NOT_FOUND, "Key not found or not yours");
    }
    match state.db.deactivate_provider_key(&id) {
        Ok(_)  => ok(json!({ "status": "ok" })),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

// ─── User: stats + profile ────────────────────────────────────────────────────

async fn user_stats(
    State(state): State<Arc<AppState>>, user: AuthUser,
) -> Response {
    // Must use user_usage_stats with the caller's user_id — NOT global usage_stats
    let daily = state.db.user_usage_stats(&user.user_id, 30).unwrap_or_default();
    let total_req: i64 = daily.iter().map(|d| d.1).sum();
    let total_err: i64 = daily.iter().map(|d| d.2).sum();
    let total_tok: i64 = daily.iter().map(|d| d.3).sum();
    ok(json!({
        "daily": daily.iter().map(|(d,r,e,t)| json!({
            "date": d, "requests": r, "errors": e, "tokens": t
        })).collect::<Vec<_>>(),
        "total_requests": total_req,
        "total_errors":   total_err,
        "total_tokens":   total_tok,
    }))
}

async fn user_profile(
    State(state): State<Arc<AppState>>, user: AuthUser,
) -> Response {
    match state.db.find_user_by_id(&user.user_id) {
        Ok(Some(u)) => ok(json!({
            "id": u.id, "username": u.username, "email": u.email,
            "display_name": u.display_name, "is_admin": u.is_admin,
            "created_at": u.created_at, "last_login": u.last_login,
        })),
        _ => err(StatusCode::NOT_FOUND, "User not found"),
    }
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct UpdateProfileRequest {
    display_name: Option<String>,
    email:        Option<String>,
    password:     Option<String>,
}

async fn user_update_profile(
    State(state): State<Arc<AppState>>, user: AuthUser,
    Json(req): Json<UpdateProfileRequest>,
) -> Response {
    let ts = now_secs() as i64;
    if let Some(pw) = req.password {
        if pw.len() < 8 { return err(StatusCode::BAD_REQUEST, "Password ≥ 8 chars required"); }
        let hash = match hash_password(&pw) {
            Ok(h) => h,
            Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e),
        };
        if let Err(e) = state.db.update_user_password(&user.user_id, &hash, ts) {
            return err(StatusCode::INTERNAL_SERVER_ERROR, e);
        }
    }
    ok(json!({ "status": "ok" }))
}

// ─── OpenAI-compatible proxy ──────────────────────────────────────────────────

/// `POST /v1/chat/completions` — classify prompt, select model, proxy to provider.
async fn proxy_completions(
    State(state): State<Arc<AppState>>,
    maybe_user: MaybeAuthUser,
    req: Request<Body>,
) -> Response {
    // ── Auth + permission check ────────────────────────────────────────────────
    // If any API keys exist in the DB, the request MUST carry a valid token.
    // We also extract the caller's permissions for model-allow-listing and
    // proxy-bypass decisions.
    let key_count = state.db.list_all_api_keys()
        .map(|k| k.iter().filter(|k| k.is_active).count())
        .unwrap_or(0);
    if key_count > 0 && maybe_user.0.is_none() {
        return err(StatusCode::UNAUTHORIZED, "Bearer token required");
    }

    // Resolve caller's API key permissions
    let caller_perms: ApiKeyPermissions = if let Some(ref u) = maybe_user.0 {
        match u.api_key_id.as_deref() {
            Some(_kid) => {
                // Find by key ID to get permissions
                match state.db.get_user_api_key(&u.user_id) {
                    Ok(Some(k)) => ApiKeyPermissions::from_json(&k.permissions),
                    _ => ApiKeyPermissions::default(),
                }
            }
            None => {
                // JWT session — look up their API key's permissions
                match state.db.get_user_api_key(&u.user_id) {
                    Ok(Some(k)) => ApiKeyPermissions::from_json(&k.permissions),
                    _ => ApiKeyPermissions::admin_default(), // JWT-only session = full access
                }
            }
        }
    } else {
        ApiKeyPermissions::admin_default() // No auth configured = open access
    };

    // Check router permission
    if !caller_perms.can_use_router {
        return err(StatusCode::FORBIDDEN, "Router access not permitted by your API key");
    }

    // Resolve caller key ID (needed for rate-limit check and request logging)
    let caller_key_id = maybe_user.0.as_ref().and_then(|u| u.api_key_id.clone())
        .or_else(|| maybe_user.0.as_ref().and_then(|u| {
            state.db.get_user_api_key(&u.user_id).ok()?.map(|k| k.id)
        }));

    // Enforce rate_limit_per_hour (0 = unlimited)
    if caller_perms.rate_limit_per_hour > 0 {
        let uid   = maybe_user.0.as_ref().map(|u| u.user_id.as_str());
        let kid   = caller_key_id.as_deref();
        let count = state.db.requests_last_hour(uid, kid).unwrap_or(0);
        if count >= caller_perms.rate_limit_per_hour as i64 {
            let mut hdrs = HeaderMap::new();
            if let Ok(v) = "60".parse() { hdrs.insert("Retry-After", v); }
            return (
                StatusCode::TOO_MANY_REQUESTS,
                hdrs,
                Json(json!({ "error": { "message":
                    format!("Rate limit exceeded: {} req/hr. Retry after window resets.",
                        caller_perms.rate_limit_per_hour),
                    "type": "rate_limit_exceeded", "code": 429 }})),
            ).into_response();
        }
    }

    // ── Parse request body ────────────────────────────────────────────────────
    let body_bytes = match axum::body::to_bytes(req.into_body(), 4 * 1024 * 1024).await {
        Ok(b)  => b,
        Err(_) => return err(StatusCode::BAD_REQUEST, "Cannot read request body"),
    };
    let body: Value = match serde_json::from_slice(&body_bytes) {
        Ok(v)  => v,
        Err(e) => return err(StatusCode::BAD_REQUEST, format!("JSON parse error: {e}")),
    };

    let model_hint = body["model"].as_str().unwrap_or("auto").to_string();
    let max_tokens = body["max_tokens"].as_u64().map(|n| n as u32);
    let streaming  = body["stream"].as_bool().unwrap_or(false);
    let msgs = body["messages"].as_array().cloned().unwrap_or_default();
    if msgs.is_empty() {
        return err(StatusCode::BAD_REQUEST, "messages array required");
    }

    let user_text: String = msgs.iter()
        .filter(|m| m["role"].as_str() == Some("user"))
        .last()
        .and_then(|m| m["content"].as_str())
        .unwrap_or("")
        .to_string();

    // ── Route via OMRP classifier ─────────────────────────────────────────────
    let cfg = state.cfg.clone();
    let is_auto = matches!(model_hint.as_str(), "auto" | "omrp/auto" | "omrp/auto-free");
    let (routed_model, tier_str) = if is_auto {
        let (effective, forced) = match detect_mode_override(&user_text) {
            Some(ov) => (ov.cleaned_prompt, Some(ov.tier)),
            None     => (user_text.clone(), None),
        };
        let cls  = classify_prompt(&effective, None);
        let tier = forced.unwrap_or_else(|| cls.tier.unwrap_or(omrp_core::classifier::PromptTier::Medium));
        let pipeline = bootstrap_pipeline(&cfg);
        let router   = RouterEngine::default();
        let req2     = RouteRequest { max_inflight_per_model: Some(3), ..Default::default() };
        let tids     = tier_model_ids(&cfg, tier);
        let dec = pipeline.state().read(|s| select_for_tier(s, &req2, tier, &tids, &router));

        if !dec.selected_model.is_empty() {
            (dec.selected_model, tier.as_str().to_string())
        } else {
            // ── Config pipeline empty → fall back to first available DB key ──
            // Check providers in priority order; pick the first with an active key.
            use crate::provider::ProviderKind;
            let uid_ref = maybe_user.0.as_ref().map(|u| u.user_id.as_str());
            let fallback = ProviderKind::all().iter().find_map(|prov| {
                let has_key = state.db
                    .resolve_provider_key(prov.to_str(), uid_ref)
                    .unwrap_or(None)
                    .is_some()
                    || std::env::var(prov.api_key_env()).is_ok();
                if has_key {
                    let model = prov.free_models().first().map(|(m,_)| *m).unwrap_or("auto");
                    Some((model.to_string(), prov.to_str().to_string()))
                } else { None }
            });
            match fallback {
                Some((model, _prov)) => (model, tier.as_str().to_string()),
                None => return err(StatusCode::SERVICE_UNAVAILABLE,
                    "No provider keys configured. Add a key in Admin → Provider Keys."),
            }
        }
    } else {
        (model_hint.clone(), "explicit".to_string())
    };

    // Check allowed_models permission
    if !caller_perms.allowed_models.is_empty()
        && !caller_perms.allowed_models.iter().any(|m| m == &routed_model || m == "*")
    {
        return err(StatusCode::FORBIDDEN,
            format!("Model '{}' not permitted by your API key", routed_model));
    }

    // ── Find provider name ────────────────────────────────────────────────────
    // Priority: config file entry → model-id prefix → fallback to openrouter
    let prov_name = cfg.model.iter()
        .find(|m| m.id == routed_model)
        .map(|m| m.provider.clone())
        .or_else(|| {
            use crate::provider::ProviderKind;
            ProviderKind::from_str(&routed_model).map(|p| p.to_str().to_string())
        })
        .unwrap_or_else(|| "openrouter".into());

    // ── Resolve provider API key (DB first, then env) ─────────────────────────
    let user_id = maybe_user.0.as_ref().map(|u| u.user_id.as_str()).unwrap_or("").to_string();
    let db_resolved = state.db
        .resolve_provider_key(&prov_name, if user_id.is_empty() { None } else { Some(&user_id) })
        .unwrap_or(None);
    // db_key_value = the API key string; custom_base_url = optional custom endpoint
    let (db_key_value, custom_base_url) = match db_resolved {
        Some((k, u)) => (Some(k), u),
        None         => (None, None),
    };

    let routed       = routed_model.clone();
    let prov         = prov_name.clone();
    let tier_out     = tier_str.clone();
    let db2          = state.db.clone();
    let uid_opt      = maybe_user.0.as_ref().map(|u| u.user_id.clone());
    let pool         = state.proxy_pool.clone();
    let use_bypass   = caller_perms.can_use_proxy_bypass && !pool.is_empty()
        && state.db.get_setting("proxy.enabled").ok().flatten().map(|v| v=="1").unwrap_or(false);
    let the_key_id   = caller_key_id.clone();

    // Resolve the API key string once, before entering spawn_blocking.
    // Resolve the API key string + optional custom base URL once, before entering
    // spawn_blocking — lets us rebuild CompatClient for every proxy retry without
    // re-querying the DB or re-reading env vars.
    let api_key_str: String = db_key_value.unwrap_or_else(|| {
        crate::provider::ProviderKind::from_str(&prov)
            .and_then(|k| std::env::var(k.api_key_env()).ok())
            .unwrap_or_default()
    });
    let custom_url = custom_base_url; // rename for clarity in closures

    // ── STREAMING PATH (SSE) ─────────────────────────────────────────────────
    // When client sends "stream":true, pipe SSE bytes directly from provider.
    // Proxy rotation applies: on 429 we switch proxy immediately.
    if streaming {
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(256);
        let pool2 = pool.clone(); let db3 = db2.clone();
        let api3 = api_key_str.clone(); let prov3 = prov.clone();
        let routed3 = routed.clone(); let _tier3 = tier_out.clone();
        let uid3 = uid_opt.clone(); let the_key3 = the_key_id.clone();
        let cust3 = custom_url.clone();
        let ubody = json!({
            "model": routed, "messages": msgs,
            "max_tokens": max_tokens.unwrap_or(2048), "stream": true,
        });
        task::spawn_blocking(move || {
            use std::io::Read;
            use omrp_events::error::ProviderError;
            use crate::provider::{CompatClient, format_provider_error};
            let pa = use_bypass && !pool2.is_empty();
            let tot = if pa { 1 + 12usize.min(pool2.len()) } else { 1 };
            let mut pu = 0i64;
            for attempt in (if pa {1} else {0})..tot {
                let c = if api3.is_empty() {
                    match CompatClient::for_provider(&prov3) { Ok(x)=>x, Err(_)=>break }
                } else if let Some(ref url) = cust3 {
                    CompatClient::from_key_custom(&api3, url)
                } else {
                    match CompatClient::from_key_and_provider(&prov3, &api3) { Ok(x)=>x, Err(_)=>break }
                };
                let c = if attempt > 0 || pa {
                    match pool2.nth(if pa { attempt-1 } else { attempt }) {
                        Some(ref p) => { pu=p.id; c.with_proxy(&p.url) }
                        None => break,
                    }
                } else { pu=0; c };
                match c.stream_request(&ubody) {
                    Ok(mut rdr) => {
                        if pu>0 { pool2.mark_success(pu,&db3); pool2.advance(attempt); }
                        let _ = db3.log_proxy_request(if pu>0{Some(pu)}else{None},None,&routed3,&prov3,uid3.as_deref(),the_key3.as_deref(),true,0,now_secs() as i64);
                        let mut buf=[0u8;8192];
                        loop {
                            match rdr.read(&mut buf) {
                                Ok(0)  => break,
                                Ok(n)  => { if tx.blocking_send(Ok(Bytes::copy_from_slice(&buf[..n]))).is_err(){break;} }
                                Err(e) => { let _=tx.blocking_send(Err(e)); break; }
                            }
                        }
                        return;
                    }
                    Err(ProviderError::RateLimited{..}) => {}
                    Err(ProviderError::Auth(_)) => break,
                    Err(e) => { if pu>0{pool2.mark_failure(pu,&db3);} eprintln!("[stream] {}", format_provider_error(&e)); }
                }
            }
            let _ = pu; // suppress unused-assignment warning on loop exit
        });
        let stream = ReceiverStream::new(rx);
        return axum::response::Response::builder()
            .status(200)
            .header("content-type", "text/event-stream")
            .header("cache-control", "no-cache")
            .header("connection", "keep-alive")
            .header("x-accel-buffering", "no")
            .header("access-control-allow-origin", "*")
            .header("x-omrp-model", &routed)
            .header("x-omrp-tier",  &tier_str)
            .body(Body::from_stream(stream))
            .unwrap_or_else(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "stream error"));
    }

    // ── NON-STREAMING DISPATCH ───────────────────────────────────────────────
    //
    // Attempt 0 — direct (no proxy)
    // Attempt N — route through pool[cursor + N-1] to present a new source IP
    //
    // On 429: advance to next proxy immediately (zero sleep), because the
    //   rate limit is per-IP — a different IP gets a fresh quota.
    // On network/timeout: mark_failure(proxy) so it's removed from the pool,
    //   then try the next entry.
    // On auth / model-not-found: break immediately (rotating IPs won't help).

    const MAX_PROXY_ATTEMPTS: usize = 12;
    let cust_url = custom_url.clone();

    let result: Result<Result<crate::provider::CompletionResult, String>, _> =
        task::spawn_blocking(move || -> Result<crate::provider::CompletionResult, String> {

        use omrp_events::error::ProviderError;
        use crate::provider::{CompatClient, format_provider_error};

        let msgs_typed: Vec<crate::provider::Message> = msgs.iter().map(|m| {
            crate::provider::Message {
                role:    m["role"].as_str().unwrap_or("user").to_string(),
                content: m["content"].as_str().unwrap_or("").to_string(),
            }
        }).collect();

        let proxy_available = use_bypass && !pool.is_empty();
        // When bypass is ON: start at proxy[0] immediately (no direct attempt).
        // When bypass is OFF: only attempt 0 (direct), no proxy fallback.
        let (start_attempt, total_attempts) = if proxy_available {
            (1usize, 1 + MAX_PROXY_ATTEMPTS.min(pool.len())) // skip direct (attempt 0)
        } else {
            (0usize, 1) // direct only
        };

        let mut last_err   = String::from("no attempt made");
        let mut proxy_used = 0i64;
        let mut proxy_url_used: Option<String> = None;

        for attempt in start_attempt..total_attempts {
            // ── Build client for this attempt ─────────────────────────────────
            let client = if api_key_str.is_empty() {
                match CompatClient::for_provider(&prov) {
                    Ok(c)  => c,
                    Err(e) => return Err(e),
                }
            } else if let Some(ref url) = cust_url {
                CompatClient::from_key_custom(&api_key_str, url)
            } else {
                match CompatClient::from_key_and_provider(&prov, &api_key_str) {
                    Ok(c)  => c,
                    Err(e) => return Err(e),
                }
            };

            // Attach proxy (always for bypass mode; only for attempt>0 in fallback mode)
            let client = if attempt > 0 || proxy_available {
                let proxy_idx = if proxy_available { attempt - 1 } else { attempt };
                match pool.nth(proxy_idx) {
                    Some(ref p) => {
                        proxy_used    = p.id;
                        proxy_url_used = Some(p.url.clone());
                        eprintln!("[proxy] {} via {}", &routed[..routed.len().min(30)], p.url);
                        client.with_proxy(&p.url)
                    }
                    None => break, // pool exhausted
                }
            } else {
                proxy_used    = 0;
                proxy_url_used = None;
                client
            };

            // ── Make the request ──────────────────────────────────────────────
            let started = std::time::Instant::now();
            let res     = client.complete_with_retry(&routed, &msgs_typed, max_tokens);
            let latency = started.elapsed().as_millis() as i64;

            match res {
                Ok(cr) => {
                    if proxy_used > 0 {
                        pool.mark_success(proxy_used, &db2);
                        pool.advance(if proxy_available { attempt } else { attempt });
                    }
                    let _ = db2.log_request(
                        uid_opt.as_deref(), None,
                        &routed, &prov, None, Some(&tier_out),
                        0, cr.tokens_used as i64, latency, true, None,
                        now_secs() as i64,
                    );
                    // Log proxy usage
                    if proxy_used > 0 || proxy_available {
                        let _ = db2.log_proxy_request(
                            if proxy_used > 0 { Some(proxy_used) } else { None },
                            proxy_url_used.as_deref(),
                            &routed, &prov,
                            uid_opt.as_deref(),
                            the_key_id.as_deref(),
                            true, latency,
                            now_secs() as i64,
                        );
                    }
                    return Ok(cr);
                }

                Err(ProviderError::RateLimited { .. }) => {
                    if proxy_used > 0 {
                        eprintln!("[proxy] 429 on proxy id={proxy_used}, trying next");
                    } else {
                        eprintln!("[proxy] 429 on direct");
                    }
                    last_err = "rate_limited".into();
                    // continue to next attempt
                }

                Err(ProviderError::Network(_)) | Err(ProviderError::Timeout(_)) => {
                    // Network/timeout — this proxy is broken; remove it.
                    if attempt > 0 {
                        eprintln!("[proxy] network/timeout on proxy id={proxy_used}");
                        pool.mark_failure(proxy_used, &db2);
                    }
                    last_err = format_provider_error(res.as_ref().unwrap_err());
                    // continue to next attempt
                }

                Err(ref e @ (ProviderError::Auth(_) | ProviderError::ModelNotFound(_))) => {
                    // Auth/model errors are not proxy-related — break immediately.
                    last_err = format_provider_error(e);
                    break;
                }

                Err(ref e) => {
                    last_err = format_provider_error(e);
                    // Other errors: try one more proxy, then give up
                    if attempt >= 2 { break; }
                }
            }
        }
        // Discard any loop-final values that weren't consumed on success
        let _ = proxy_used;
        let _ = proxy_url_used;

        // All attempts failed
        let _ = db2.log_request(
            uid_opt.as_deref(), None,
            &routed, &prov, None, Some(&tier_out),
            0, 0, 0, false, Some(&last_err),
            now_secs() as i64,
        );
        Err(last_err)
    }).await;

    match result {
        Ok(Ok(cr)) => {
            let resp_body = json!({
                "id":      format!("omrp-{}", now_millis()),
                "object":  "chat.completion",
                "model":   cr.model_used,
                "choices": [{ "index": 0, "message": { "role": "assistant", "content": cr.text }, "finish_reason": "stop" }],
                "usage":   { "prompt_tokens": 0, "completion_tokens": cr.tokens_used, "total_tokens": cr.tokens_used },
            });
            let mut headers = HeaderMap::new();
            if let Ok(v) = cr.model_used.parse() { headers.insert("X-OMRP-Model", v); }
            if let Ok(v) = tier_str.parse()      { headers.insert("X-OMRP-Tier",  v); }
            (StatusCode::OK, headers, Json(resp_body)).into_response()
        }
        Ok(Err(msg)) => err(StatusCode::BAD_GATEWAY, msg),
        Err(_)       => err(StatusCode::INTERNAL_SERVER_ERROR, "Routing task failed"),
    }
}

/// `GET /v1/models` — list available models in OpenAI format.
async fn proxy_models(State(state): State<Arc<AppState>>) -> Response {
    let now = now_secs();
    let uid: Option<String> = None; // /v1/models is public; no user-scoping needed

    // ── Always include omrp routing aliases ──────────────────────────────────
    let mut models = vec![
        json!({"id":"omrp/auto",      "object":"model","created":now,"owned_by":"omrp"}),
        json!({"id":"omrp/auto-free", "object":"model","created":now,"owned_by":"omrp"}),
    ];

    // ── Config-file models (may be empty on fresh installs) ──────────────────
    let mut seen = std::collections::HashSet::<String>::new();
    for m in &state.cfg.model {
        if seen.insert(m.id.clone()) {
            models.push(json!({"id": m.id, "object": "model", "created": now, "owned_by": m.provider}));
        }
    }

    // ── DB provider keys → add each provider's known free models ─────────────
    // Only include models for providers that actually have an active key
    // (either in the DB or as an environment variable).
    use crate::provider::ProviderKind;
    for prov in ProviderKind::all() {
        let has_key = state.db
            .resolve_provider_key(prov.to_str(), uid.as_deref())
            .unwrap_or(None)
            .is_some()
            || std::env::var(prov.api_key_env()).is_ok();

        if has_key {
            for (model_id, _hint) in prov.free_models() {
                if seen.insert(model_id.to_string()) {
                    models.push(json!({
                        "id":       model_id,
                        "object":   "model",
                        "created":  now,
                        "owned_by": prov.to_str(),
                    }));
                }
            }
        }
    }

    ok(json!({ "object": "list", "data": models }))
}

// ─── Announcements / News ────────────────────────────────────────────────────

/// `GET /api/public/providers` — full free-tier provider catalog (no auth).
async fn public_providers() -> Response {
    ok(json!({ "data": crate::provider::free_providers_json() }))
}
async fn public_news(State(state): State<Arc<AppState>>) -> Response {
    match state.db.list_announcements(true) {
        Ok(news) => ok(json!({ "data": news.iter().map(|n| json!({
            "id":         n.id,
            "title":      n.title,
            "body":       n.body,
            "is_pinned":  n.is_pinned,
            "created_at": n.created_at,
        })).collect::<Vec<_>>() })),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// `GET /api/admin/news` — all announcements (admin auth required).
async fn admin_list_news(
    State(state): State<Arc<AppState>>, user: AuthUser,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    match state.db.list_announcements(false) {
        Ok(news) => ok(json!({ "data": news.iter().map(|n| json!({
            "id":           n.id,
            "title":        n.title,
            "body":         n.body,
            "is_pinned":    n.is_pinned,
            "is_published": n.is_published,
            "author_id":    n.author_id,
            "created_at":   n.created_at,
            "updated_at":   n.updated_at,
        })).collect::<Vec<_>>() })),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

#[derive(Deserialize)]
struct NewsRequest {
    title:        String,
    body:         Option<String>,
    is_pinned:    Option<bool>,
    is_published: Option<bool>,
}

/// `POST /api/admin/news`
async fn admin_create_news(
    State(state): State<Arc<AppState>>, user: AuthUser,
    Json(req): Json<NewsRequest>,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    if req.title.trim().is_empty() { return err(StatusCode::BAD_REQUEST, "title required"); }
    match state.db.create_announcement(
        req.title.trim(), req.body.as_deref().unwrap_or(""),
        Some(&user.user_id), req.is_pinned.unwrap_or(false),
    ) {
        Ok(id) => (StatusCode::CREATED, Json(json!({ "id": id }))).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// `PUT /api/admin/news/:id`
async fn admin_update_news(
    State(state): State<Arc<AppState>>, user: AuthUser,
    Path(id): Path<i64>, Json(req): Json<NewsRequest>,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    match state.db.update_announcement(
        id, req.title.trim(), req.body.as_deref().unwrap_or(""),
        req.is_pinned.unwrap_or(false), req.is_published.unwrap_or(true),
    ) {
        Ok(true)  => ok(json!({ "status": "ok" })),
        Ok(false) => err(StatusCode::NOT_FOUND, "announcement not found"),
        Err(e)    => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// `DELETE /api/admin/news/:id`
async fn admin_delete_news(
    State(state): State<Arc<AppState>>, user: AuthUser,
    Path(id): Path<i64>,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    match state.db.delete_announcement(id) {
        Ok(true)  => ok(json!({ "status": "ok" })),
        Ok(false) => err(StatusCode::NOT_FOUND, "announcement not found"),
        Err(e)    => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

// ─── Health ───────────────────────────────────────────────────────────────────

async fn health(State(state): State<Arc<AppState>>) -> Response {
    // Ping the DB by running a trivial query
    let db_ok = state.db.get_setting("app.name").is_ok();
    let db_status = if db_ok { "ok" } else { "error" };
    let status_code = if db_ok { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };
    (status_code, Json(json!({
        "status":     db_status,
        "db":         db_status,
        "version":    env!("CARGO_PKG_VERSION"),
        "proxy_pool": state.proxy_pool.len(),
    }))).into_response()
}

async fn server_stats(State(state): State<Arc<AppState>>) -> Response {
    let proxy_count = state.db.proxy_count().unwrap_or(0);
    let user_count  = state.db.list_users().map(|u| u.len()).unwrap_or(0);
    ok(json!({
        "proxy_count": proxy_count,
        "user_count":  user_count,
        "version":     env!("CARGO_PKG_VERSION"),
    }))
}

// ─── Serialization helpers ────────────────────────────────────────────────────

fn serialize_api_key(k: &ApiKeyRow) -> Value {
    json!({
        "id":          k.id,
        "user_id":     k.user_id,
        "key_prefix":  k.key_prefix,
        "label":       k.label,
        "is_active":   k.is_active,
        "created_at":  k.created_at,
        "last_used":   k.last_used,
        "expires_at":  k.expires_at,
        "permissions": ApiKeyPermissions::from_json(&k.permissions),
    })
}

fn serialize_provider_key(k: &ProviderKeyRow) -> Value {
    json!({
        "id":           k.id,
        "user_id":      k.user_id,
        "provider":     k.provider,
        "display_name": k.display_name,
        "base_url":     k.base_url,
        "label":        k.label,
        "is_active":    k.is_active,
        "is_global":    k.is_global,
        "created_at":   k.created_at,
        // key_value intentionally omitted — never exposed over the API
    })
}

// ─── Key generation helpers ───────────────────────────────────────────────────

fn generate_api_key() -> String {
    use rand::RngCore;
    let mut buf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut buf);
    format!("omrp-sk-{}", buf.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

fn generate_key_id() -> String {
    use rand::RngCore;
    let mut buf = [0u8; 4];
    rand::thread_rng().fill_bytes(&mut buf);
    format!("omrp_{}", buf.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

// ─── Timestamp helpers ────────────────────────────────────────────────────────

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

fn now_millis() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis()
}
