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
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, Request, StatusCode},
    response::{Html, IntoResponse, Json, Response},
    routing::{delete, get, post, put},
    Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::task;
use uuid::Uuid;

use crate::auth::{
    authenticate, auth_cookie, hash_password, logout_cookie, onboarding_needed,
    sha256_hex, AppState, AuthUser, LoginRequest, MaybeAuthUser,
};
use crate::config::Config;
use crate::db::{ApiKeyRow, Database, ProviderKeyRow, UserRow};
use crate::provider::CompatClient;
use crate::routing::{bootstrap_pipeline, select_for_tier, tier_from_str, tier_model_ids};
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

// ─── Router ───────────────────────────────────────────────────────────────────

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        // ── SPA ──────────────────────────────────────────────────────────────
        .route("/",          get(serve_spa))
        .route("/setup",     get(serve_spa))
        // ── Auth ─────────────────────────────────────────────────────────────
        .route("/api/auth/login",   post(auth_login))
        .route("/api/auth/logout",  post(auth_logout))
        .route("/api/auth/me",      get(auth_me))
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
        .route("/api/admin/proxies",            get(admin_list_proxies))
        .route("/api/admin/proxies/refresh",    post(admin_refresh_proxies))
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
    axum::serve(listener, app).await.unwrap();
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
    let expiry: u64 = state.db.get_setting("jwt.expiry_secs").ok().flatten()
        .and_then(|s| s.parse().ok()).unwrap_or(86400);
    match authenticate(&state.db, &req.username, &req.password, &state.jwt_secret, expiry) {
        Ok(resp) => {
            let cookie = auth_cookie(&resp.token, expiry);
            let mut headers = HeaderMap::new();
            headers.insert("Set-Cookie", cookie.parse().unwrap());
            (StatusCode::OK, headers, Json(json!({
                "token":    resp.token,
                "user_id":  resp.user_id,
                "username": resp.username,
                "is_admin": resp.is_admin,
            }))).into_response()
        }
        Err(e) => err(StatusCode::UNAUTHORIZED, e),
    }
}

async fn auth_logout() -> Response {
    let mut headers = HeaderMap::new();
    headers.insert("Set-Cookie", logout_cookie().parse().unwrap());
    (StatusCode::OK, headers, Json(json!({ "status": "ok" }))).into_response()
}

async fn auth_me(user: AuthUser) -> Response {
    ok(json!({
        "user_id":  user.user_id,
        "username": user.username,
        "is_admin": user.is_admin,
    }))
}

// ─── Onboarding / setup handlers ─────────────────────────────────────────────

async fn setup_status(State(state): State<Arc<AppState>>) -> Response {
    ok(json!({ "onboarding_needed": onboarding_needed(&state.db) }))
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
    let _ = state.db.audit(Some(&uid), "setup.init", Some(&uid), Some("user"), None, None, ts);
    ok(json!({ "status": "ok", "user_id": uid, "message": "Admin account created. Please log in." }))
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
    if req.password.len() < 8 {
        return err(StatusCode::BAD_REQUEST, "Password must be ≥ 8 chars");
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
    let _ = state.db.audit(Some(&user.user_id), "user.create", Some(&uid), Some("user"), None, None, ts);
    (StatusCode::CREATED, Json(json!({ "id": uid, "username": row.username }))).into_response()
}

#[derive(Deserialize)]
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
        if pw.len() < 8 { return err(StatusCode::BAD_REQUEST, "Password must be ≥ 8 chars"); }
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
        label:      req.label.unwrap_or_else(|| "default".into()),
        is_active:  true,
        created_at: ts,
        last_used:  None,
        expires_at: None,
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
    provider:  String,
    key_value: String,
    label:     Option<String>,
    is_global: Option<bool>,
    user_id:   Option<String>,
}

async fn admin_create_provider_key(
    State(state): State<Arc<AppState>>, user: AuthUser,
    Json(req): Json<CreateProviderKeyRequest>,
) -> Response {
    if let Some(e) = require_admin(&user) { return e; }
    let ts = now_secs() as i64;
    let row = ProviderKeyRow {
        id:         Uuid::new_v4().to_string(),
        user_id:    req.user_id,
        provider:   req.provider.clone(),
        key_value:  req.key_value,
        label:      req.label.unwrap_or_default(),
        is_active:  true,
        is_global:  req.is_global.unwrap_or(false),
        created_at: ts,
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
    match state.db.active_proxies() {
        Ok(proxies) => ok(json!({ "data": proxies.iter().map(|(id, url)| json!({ "id": id, "url": url })).collect::<Vec<_>>() })),
        Err(e)      => err(StatusCode::INTERNAL_SERVER_ERROR, e),
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
        id:         generate_key_id(),
        user_id:    Some(user.user_id.clone()),
        key_hash:   hash,
        key_prefix: prefix,
        label:      req.label.unwrap_or_else(|| "default".into()),
        is_active:  true,
        created_at: ts, last_used: None, expires_at: None,
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
    let ts = now_secs() as i64;
    let row = ProviderKeyRow {
        id:         Uuid::new_v4().to_string(),
        user_id:    Some(user.user_id.clone()),
        provider:   req.provider,
        key_value:  req.key_value,
        label:      req.label.unwrap_or_default(),
        is_active:  true,
        is_global:  false,   // users can't create global keys
        created_at: ts,
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
    let stats = state.db.usage_stats(30).unwrap_or_default();
    ok(json!({ "daily": stats.iter().map(|(d,r,e,t)| json!({
        "date": d, "requests": r, "errors": e, "tokens": t
    })).collect::<Vec<_>>() }))
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
    // ── Auth check (if any keys are configured) ───────────────────────────────
    // (auth is checked in FromRequestParts for MaybeAuthUser; if keys exist
    //  and the request has no valid token, we reject it here)
    let key_count = state.db.list_all_api_keys()
        .map(|k| k.iter().filter(|k| k.is_active).count())
        .unwrap_or(0);
    if key_count > 0 && maybe_user.0.is_none() {
        return err(StatusCode::UNAUTHORIZED, "Bearer token required");
    }

    // ── Parse request body ────────────────────────────────────────────────────
    let body_bytes = match axum::body::to_bytes(req.into_body(), 4 * 1024 * 1024).await {
        Ok(b)  => b,
        Err(_) => return err(StatusCode::BAD_REQUEST, "Cannot read request body"),
    };
    let mut body: Value = match serde_json::from_slice(&body_bytes) {
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
        (dec.selected_model, tier.as_str().to_string())
    } else {
        (model_hint.clone(), "explicit".to_string())
    };

    if routed_model.is_empty() {
        return err(StatusCode::SERVICE_UNAVAILABLE, "No models available");
    }

    // ── Find provider name ────────────────────────────────────────────────────
    let prov_name = cfg.model.iter()
        .find(|m| m.id == routed_model)
        .map(|m| m.provider.clone())
        .unwrap_or_else(|| "openrouter".into());

    // ── Resolve provider API key (DB first, then env) ─────────────────────────
    let user_id = maybe_user.0.as_ref().map(|u| u.user_id.as_str()).unwrap_or("").to_string();
    let db_key  = state.db
        .resolve_provider_key(&prov_name, if user_id.is_empty() { None } else { Some(&user_id) })
        .unwrap_or(None);

    let routed = routed_model.clone();
    let prov      = prov_name.clone();
    let tier_out  = tier_str.clone();
    let db2       = state.db.clone();
    let uid_opt   = maybe_user.0.as_ref().map(|u| u.user_id.clone());
    let pool      = state.proxy_pool.clone();

    // Resolve the API key string once, before entering spawn_blocking.
    // This lets us rebuild CompatClient for every proxy retry without
    // re-querying the DB or re-reading env vars.
    let api_key_str: String = db_key.unwrap_or_else(|| {
        crate::provider::ProviderKind::from_str(&prov)
            .and_then(|k| std::env::var(k.api_key_env()).ok())
            .unwrap_or_default()
    });

    // ── Dispatch with transparent proxy rotation on 429 ───────────────────────
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

        let proxy_available = !pool.is_empty();
        // Attempt 0 = direct; 1..=MAX = via proxy
        let total_attempts = if proxy_available {
            1 + MAX_PROXY_ATTEMPTS.min(pool.len())
        } else {
            1
        };

        let mut last_err   = String::from("no attempt made");
        let mut proxy_used = 0i64; // DB id of the proxy used in current attempt

        for attempt in 0..total_attempts {
            // ── Build client for this attempt ─────────────────────────────────
            let client = if api_key_str.is_empty() {
                match CompatClient::for_provider(&prov) {
                    Ok(c)  => c,
                    Err(e) => return Err(e),
                }
            } else {
                match CompatClient::from_key_and_provider(&prov, &api_key_str) {
                    Ok(c)  => c,
                    Err(e) => return Err(e),
                }
            };

            // Attach proxy for attempts 1+
            let client = if attempt > 0 {
                match pool.nth(attempt - 1) {
                    Some(ref p) => {
                        proxy_used = p.id;
                        eprintln!("[proxy] attempt {attempt}: {}", p.url);
                        client.with_proxy(&p.url)
                    }
                    None => break, // pool exhausted
                }
            } else {
                proxy_used = 0;
                client
            };

            // ── Make the request ──────────────────────────────────────────────
            let started = std::time::Instant::now();
            let res     = client.complete_with_retry(&routed, &msgs_typed, max_tokens);
            let latency = started.elapsed().as_millis() as i64;

            match res {
                Ok(cr) => {
                    // Success — advance pool cursor so the next request doesn't
                    // re-use the same proxy batch that hit rate limits.
                    if attempt > 0 {
                        pool.mark_success(proxy_used, &db2);
                        pool.advance(attempt);
                    }
                    let _ = db2.log_request(
                        uid_opt.as_deref(), None,
                        &routed, &prov, None, Some(&tier_out),
                        0, cr.tokens_used as i64, latency, true, None,
                        now_secs() as i64,
                    );
                    return Ok(cr);
                }

                Err(ProviderError::RateLimited { .. }) => {
                    // 429 — this IP is rate-limited.  Don't mark the proxy as
                    // failed (it works, just not for us right now).  Move on.
                    if attempt > 0 {
                        eprintln!("[proxy] 429 on proxy id={proxy_used}, trying next");
                    } else {
                        eprintln!("[proxy] 429 on direct, trying proxy pool");
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
    let mut models = vec![
        json!({"id":"auto",           "object":"model","created":now,"owned_by":"omrp"}),
        json!({"id":"omrp/auto",      "object":"model","created":now,"owned_by":"omrp"}),
        json!({"id":"omrp/auto-free", "object":"model","created":now,"owned_by":"omrp"}),
    ];
    for m in &state.cfg.model {
        models.push(json!({"id": m.id, "object": "model", "created": now, "owned_by": m.provider}));
    }
    ok(json!({ "object": "list", "data": models }))
}

// ─── Health ───────────────────────────────────────────────────────────────────

async fn health() -> Response {
    ok(json!({
        "status":  "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
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
        "id":         k.id,
        "user_id":    k.user_id,
        "key_prefix": k.key_prefix,
        "label":      k.label,
        "is_active":  k.is_active,
        "created_at": k.created_at,
        "last_used":  k.last_used,
        "expires_at": k.expires_at,
    })
}

fn serialize_provider_key(k: &ProviderKeyRow) -> Value {
    json!({
        "id":         k.id,
        "user_id":    k.user_id,
        "provider":   k.provider,
        "label":      k.label,
        "is_active":  k.is_active,
        "is_global":  k.is_global,
        "created_at": k.created_at,
        // key_value intentionally omitted
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
