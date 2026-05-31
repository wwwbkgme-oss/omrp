//! SQLite database layer for OMRP's multi-user web application.
//!
//! ## Schema
//!
//! | Table              | Purpose                                              |
//! |--------------------|------------------------------------------------------|
//! | `users`            | User accounts with hashed passwords                  |
//! | `roles`            | Named role definitions                               |
//! | `user_roles`       | Many-to-many user ↔ role assignments                 |
//! | `permissions`      | Granular permission tokens (`admin:users:read`, …)   |
//! | `role_permissions` | Many-to-many role ↔ permission assignments           |
//! | `api_keys`         | Router keys clients use to authenticate with OMRP    |
//! | `provider_keys`    | LLM provider credentials (openrouter, kilo, …)       |
//! | `settings`         | System-wide key-value configuration                  |
//! | `audit_logs`       | Immutable audit trail of all mutating actions        |
//! | `request_logs`     | Per-request stats for dashboard analytics            |
//! | `proxies`          | HTTP/SOCKS proxy pool with health metadata           |
//!
//! ## Lifetime note
//!
//! All query methods use the `let result = expr; result` pattern to avoid
//! Rust's end-of-block temporary drop-order issue with `MutexGuard` + `?`.

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, Result as SqlResult};
use serde::{Deserialize, Serialize};

// ─── API Key permissions ──────────────────────────────────────────────────────

/// Fine-grained permissions stored with every API key (as JSON).
///
/// These are set by the admin at account-creation time and can be updated.
/// Each user has exactly **one** API key; this struct defines what they can do.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyPermissions {
    /// Can use the LLM router endpoint `/v1/chat/completions`.
    pub can_use_router: bool,
    /// Route every request through the proxy pool (bypasses per-IP rate limits
    /// entirely — the user never sees a 429).
    pub can_use_proxy_bypass: bool,
    /// Allowed model IDs.  Empty vec = all models permitted.
    pub allowed_models: Vec<String>,
    /// Max requests per hour.  0 = unlimited.
    pub rate_limit_per_hour: u32,
}

impl Default for ApiKeyPermissions {
    fn default() -> Self {
        Self {
            can_use_router:       true,
            can_use_proxy_bypass: false,
            allowed_models:       vec![],
            rate_limit_per_hour:  0,
        }
    }
}

impl ApiKeyPermissions {
    /// Admin default: full access with proxy bypass enabled.
    pub fn admin_default() -> Self {
        Self {
            can_use_router:       true,
            can_use_proxy_bypass: true,
            allowed_models:       vec![],
            rate_limit_per_hour:  0,
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    pub fn from_json(s: &str) -> Self {
        serde_json::from_str(s).unwrap_or_default()
    }
}

// ─── Database handle ──────────────────────────────────────────────────────────

/// Thread-safe SQLite connection wrapper.
#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    /// Open (or create) a SQLite database at the given path.
    pub fn open(path: &Path) -> SqlResult<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    /// Open an in-memory database (for tests).
    #[allow(dead_code)]
    pub fn open_in_memory() -> SqlResult<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    /// Run all DDL migrations (idempotent).
    pub fn migrate(&self) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(SCHEMA_SQL)?;
        // Additive column migrations (safe to run multiple times)
        let _ = conn.execute("ALTER TABLE api_keys ADD COLUMN permissions TEXT NOT NULL DEFAULT '{}'", []);
        let _ = conn.execute("ALTER TABLE request_logs ADD COLUMN proxy_id INTEGER", []);
        let _ = conn.execute("ALTER TABLE request_logs ADD COLUMN proxy_url TEXT", []);
        Ok(())
    }

    /// Insert built-in roles, permissions, and default settings.
    pub fn seed_defaults(&self) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(SEED_SQL)
    }

    /// Returns `true` if at least one user account exists.
    pub fn has_users(&self) -> bool {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM users", [], |r| r.get::<_, i64>(0))
            .unwrap_or(0) > 0
    }

    /// Returns `true` if the schema has been applied.
    #[allow(dead_code)]
    pub fn is_initialised(&self) -> bool {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='users'",
            [], |r| r.get::<_, i64>(0),
        ).unwrap_or(0) > 0
    }
}

// ─── Schema DDL ───────────────────────────────────────────────────────────────

static SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS users (
    id            TEXT    PRIMARY KEY,
    username      TEXT    UNIQUE NOT NULL,
    email         TEXT    UNIQUE,
    password_hash TEXT    NOT NULL,
    display_name  TEXT    NOT NULL DEFAULT '',
    is_active     INTEGER NOT NULL DEFAULT 1,
    is_admin      INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    last_login    INTEGER
);
CREATE TABLE IF NOT EXISTS roles (
    id          TEXT    PRIMARY KEY,
    name        TEXT    UNIQUE NOT NULL,
    description TEXT    NOT NULL DEFAULT '',
    created_at  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS user_roles (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role_id TEXT NOT NULL REFERENCES roles(id) ON DELETE CASCADE,
    PRIMARY KEY (user_id, role_id)
);
CREATE TABLE IF NOT EXISTS permissions (
    id          TEXT PRIMARY KEY,
    name        TEXT UNIQUE NOT NULL,
    description TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS role_permissions (
    role_id       TEXT NOT NULL REFERENCES roles(id) ON DELETE CASCADE,
    permission_id TEXT NOT NULL REFERENCES permissions(id) ON DELETE CASCADE,
    PRIMARY KEY (role_id, permission_id)
);
CREATE TABLE IF NOT EXISTS api_keys (
    id          TEXT    PRIMARY KEY,
    user_id     TEXT    REFERENCES users(id) ON DELETE CASCADE,
    key_hash    TEXT    NOT NULL,
    key_prefix  TEXT    NOT NULL,
    label       TEXT    NOT NULL DEFAULT 'default',
    is_active   INTEGER NOT NULL DEFAULT 1,
    created_at  INTEGER NOT NULL,
    last_used   INTEGER,
    expires_at  INTEGER,
    -- Fine-grained permissions JSON: ApiKeyPermissions struct
    permissions TEXT    NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_api_keys_user ON api_keys(user_id);
CREATE INDEX IF NOT EXISTS idx_api_keys_hash ON api_keys(key_hash);
CREATE UNIQUE INDEX IF NOT EXISTS idx_api_keys_user_unique ON api_keys(user_id) WHERE user_id IS NOT NULL;
CREATE TABLE IF NOT EXISTS provider_keys (
    id          TEXT    PRIMARY KEY,
    user_id     TEXT    REFERENCES users(id) ON DELETE CASCADE,
    provider    TEXT    NOT NULL,
    key_value   TEXT    NOT NULL,
    label       TEXT    NOT NULL DEFAULT '',
    is_active   INTEGER NOT NULL DEFAULT 1,
    is_global   INTEGER NOT NULL DEFAULT 0,
    created_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_provider_keys_user ON provider_keys(user_id);
CREATE TABLE IF NOT EXISTS settings (
    key         TEXT PRIMARY KEY,
    value       TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    updated_at  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS audit_logs (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id     TEXT,
    action      TEXT    NOT NULL,
    target_id   TEXT,
    target_type TEXT,
    metadata    TEXT,
    ip_addr     TEXT,
    created_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_audit_created ON audit_logs(created_at DESC);
CREATE TABLE IF NOT EXISTS request_logs (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id     TEXT,
    api_key_id  TEXT,
    model_id    TEXT    NOT NULL,
    provider    TEXT    NOT NULL,
    task_type   TEXT,
    tier        TEXT,
    tokens_in   INTEGER NOT NULL DEFAULT 0,
    tokens_out  INTEGER NOT NULL DEFAULT 0,
    latency_ms  INTEGER NOT NULL DEFAULT 0,
    success     INTEGER NOT NULL DEFAULT 1,
    error_msg   TEXT,
    created_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_req_created ON request_logs(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_req_user    ON request_logs(user_id);
-- Per-proxy request tracking: maps each LLM call to the proxy IP used
CREATE TABLE IF NOT EXISTS proxy_requests (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    proxy_id    INTEGER,              -- NULL = direct (no proxy)
    proxy_url   TEXT,                 -- URL at time of request (kept even if proxy deleted)
    model_id    TEXT    NOT NULL,
    provider    TEXT    NOT NULL,
    user_id     TEXT,
    api_key_id  TEXT,
    success     INTEGER NOT NULL DEFAULT 1,
    latency_ms  INTEGER NOT NULL DEFAULT 0,
    created_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_prx_proxy   ON proxy_requests(proxy_id);
CREATE INDEX IF NOT EXISTS idx_prx_user    ON proxy_requests(user_id);
CREATE INDEX IF NOT EXISTS idx_prx_created ON proxy_requests(created_at DESC);
CREATE TABLE IF NOT EXISTS proxies (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    url         TEXT    UNIQUE NOT NULL,
    protocol    TEXT    NOT NULL,
    country     TEXT,
    anonymity   TEXT,
    is_active   INTEGER NOT NULL DEFAULT 1,
    last_check  INTEGER,
    latency_ms  INTEGER,
    uptime_pct  REAL    NOT NULL DEFAULT 100.0,
    fail_count  INTEGER NOT NULL DEFAULT 0,
    added_at    INTEGER NOT NULL
);
";

// ─── Seed data ────────────────────────────────────────────────────────────────

static SEED_SQL: &str = "
INSERT OR IGNORE INTO roles (id, name, description, created_at) VALUES
    ('role_admin', 'admin', 'Full system access', 0),
    ('role_user',  'user',  'Standard user access', 0);
INSERT OR IGNORE INTO permissions (id, name, description) VALUES
    ('perm_admin_users_read',     'admin:users:read',     'List and view users'),
    ('perm_admin_users_write',    'admin:users:write',    'Create, edit, delete users'),
    ('perm_admin_roles_read',     'admin:roles:read',     'List and view roles'),
    ('perm_admin_roles_write',    'admin:roles:write',    'Create, edit, delete roles'),
    ('perm_admin_keys_read',      'admin:keys:read',      'View all API keys'),
    ('perm_admin_keys_write',     'admin:keys:write',     'Create and revoke any API key'),
    ('perm_admin_prov_read',      'admin:providers:read', 'View provider key config'),
    ('perm_admin_prov_write',     'admin:providers:write','Manage global provider keys'),
    ('perm_admin_settings_read',  'admin:settings:read',  'Read system settings'),
    ('perm_admin_settings_write', 'admin:settings:write', 'Update system settings'),
    ('perm_admin_logs_read',      'admin:logs:read',      'View audit and request logs'),
    ('perm_admin_proxies_write',  'admin:proxies:write',  'Manage proxy pool'),
    ('perm_user_keys_read',       'user:keys:read',       'List own API keys'),
    ('perm_user_keys_write',      'user:keys:write',      'Create/revoke own API keys'),
    ('perm_user_providers_write', 'user:providers:write', 'Add own provider keys'),
    ('perm_user_profile_write',   'user:profile:write',   'Edit own profile');
INSERT OR IGNORE INTO role_permissions (role_id, permission_id)
    SELECT 'role_admin', id FROM permissions;
INSERT OR IGNORE INTO role_permissions (role_id, permission_id)
    SELECT 'role_user', id FROM permissions WHERE name LIKE 'user:%';
INSERT OR IGNORE INTO settings (key, value, description, updated_at) VALUES
    ('app.name',              'OMRP',            'Application display name', 0),
    ('app.tagline',           'Open Model Routing Protocol', 'Tagline shown in UI', 0),
    ('app.registration_open', '0',               '1 = allow public self-registration', 0),
    ('proxy.enabled',         '0',               '1 = route LLM calls through proxy pool', 0),
    ('proxy.refresh_interval','3600',             'Proxy list refresh interval (seconds)', 0),
    ('proxy.source_url',      'https://api.proxyscrape.com/v4/free-proxy-list/get?request=display_proxies&proxy_format=protocolipport&format=json', 'ProxyScrape API URL', 0),
    ('jwt.expiry_secs',       '86400',            'JWT token lifetime in seconds (default 24h)', 0),
    ('routing.default_tier',  'medium',           'Default prompt tier when classifier is unsure', 0),
    ('routing.fallback_count','3',                'Max fallback models to try per request', 0);
";

// ─── Row types ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct UserRow {
    pub id:            String,
    pub username:      String,
    pub email:         Option<String>,
    pub password_hash: String,
    pub display_name:  String,
    pub is_active:     bool,
    pub is_admin:      bool,
    pub created_at:    i64,
    pub updated_at:    i64,
    pub last_login:    Option<i64>,
}

#[derive(Debug, Clone)]
pub struct ApiKeyRow {
    pub id:          String,
    pub user_id:     Option<String>,
    pub key_hash:    String,
    pub key_prefix:  String,
    pub label:       String,
    pub is_active:   bool,
    pub created_at:  i64,
    pub last_used:   Option<i64>,
    pub expires_at:  Option<i64>,
    /// JSON-encoded `ApiKeyPermissions`.
    pub permissions: String,
}

#[derive(Debug, Clone)]
pub struct ProviderKeyRow {
    pub id:         String,
    pub user_id:    Option<String>,
    pub provider:   String,
    pub key_value:  String,
    pub label:      String,
    pub is_active:  bool,
    pub is_global:  bool,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct AuditLogRow {
    pub id:          i64,
    pub user_id:     Option<String>,
    pub action:      String,
    pub target_id:   Option<String>,
    pub target_type: Option<String>,
    pub metadata:    Option<String>,
    pub ip_addr:     Option<String>,
    pub created_at:  i64,
}

// ─── Row mapping helpers ──────────────────────────────────────────────────────

fn map_user(r: &rusqlite::Row<'_>) -> SqlResult<UserRow> {
    Ok(UserRow {
        id:            r.get(0)?,
        username:      r.get(1)?,
        email:         r.get(2)?,
        password_hash: r.get(3)?,
        display_name:  r.get(4)?,
        is_active:     r.get::<_, i64>(5)? != 0,
        is_admin:      r.get::<_, i64>(6)? != 0,
        created_at:    r.get(7)?,
        updated_at:    r.get(8)?,
        last_login:    r.get(9)?,
    })
}

fn map_api_key(r: &rusqlite::Row<'_>) -> SqlResult<ApiKeyRow> {
    Ok(ApiKeyRow {
        id:          r.get(0)?,
        user_id:     r.get(1)?,
        key_hash:    r.get(2)?,
        key_prefix:  r.get(3)?,
        label:       r.get(4)?,
        is_active:   r.get::<_, i64>(5)? != 0,
        created_at:  r.get(6)?,
        last_used:   r.get(7)?,
        expires_at:  r.get(8)?,
        permissions: r.get::<_, Option<String>>(9)?.unwrap_or_else(|| "{}".into()),
    })
}

fn map_provider_key(r: &rusqlite::Row<'_>) -> SqlResult<ProviderKeyRow> {
    Ok(ProviderKeyRow {
        id:         r.get(0)?,
        user_id:    r.get(1)?,
        provider:   r.get(2)?,
        key_value:  r.get(3)?,
        label:      r.get(4)?,
        is_active:  r.get::<_, i64>(5)? != 0,
        is_global:  r.get::<_, i64>(6)? != 0,
        created_at: r.get(7)?,
    })
}

// ─── CRUD: Users ──────────────────────────────────────────────────────────────

impl Database {
    pub fn insert_user(&self, row: &UserRow) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO users
             (id,username,email,password_hash,display_name,is_active,is_admin,created_at,updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                row.id, row.username, row.email, row.password_hash, row.display_name,
                row.is_active as i64, row.is_admin as i64, row.created_at, row.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn find_user_by_username(&self, username: &str) -> SqlResult<Option<UserRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id,username,email,password_hash,display_name,
             is_active,is_admin,created_at,updated_at,last_login
             FROM users WHERE username=?1",
        )?;
        let mut rows: Vec<UserRow> = stmt
            .query_map(params![username], map_user)?
            .collect::<SqlResult<Vec<_>>>()?;
        // ^^^ .collect() result bound before function return avoids drop-order issue
        Ok(rows.pop().or_else(|| None))
    }

    pub fn find_user_by_id(&self, id: &str) -> SqlResult<Option<UserRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id,username,email,password_hash,display_name,
             is_active,is_admin,created_at,updated_at,last_login
             FROM users WHERE id=?1",
        )?;
        let mut rows: Vec<UserRow> = stmt
            .query_map(params![id], map_user)?
            .collect::<SqlResult<Vec<_>>>()?;
        Ok(rows.pop().or_else(|| None))
    }

    pub fn list_users(&self) -> SqlResult<Vec<UserRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id,username,email,password_hash,display_name,
             is_active,is_admin,created_at,updated_at,last_login
             FROM users ORDER BY created_at ASC",
        )?;
        let result: Vec<UserRow> = stmt
            .query_map([], map_user)?
            .collect::<SqlResult<Vec<_>>>()?;
        Ok(result)
    }

    pub fn set_user_last_login(&self, user_id: &str, ts: i64) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE users SET last_login=?1, updated_at=?1 WHERE id=?2",
            params![ts, user_id],
        )?;
        Ok(())
    }

    pub fn update_user_password(&self, user_id: &str, hash: &str, ts: i64) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE users SET password_hash=?1, updated_at=?2 WHERE id=?3",
            params![hash, ts, user_id],
        )?;
        Ok(())
    }

    pub fn set_user_active(&self, user_id: &str, active: bool, ts: i64) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE users SET is_active=?1, updated_at=?2 WHERE id=?3",
            params![active as i64, ts, user_id],
        )?;
        Ok(())
    }

    pub fn delete_user(&self, user_id: &str) -> SqlResult<bool> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute("DELETE FROM users WHERE id=?1", params![user_id])?;
        Ok(n > 0)
    }

    pub fn assign_role(&self, user_id: &str, role_id: &str) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO user_roles (user_id, role_id) VALUES (?1,?2)",
            params![user_id, role_id],
        )?;
        Ok(())
    }

    pub fn remove_role(&self, user_id: &str, role_id: &str) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM user_roles WHERE user_id=?1 AND role_id=?2",
            params![user_id, role_id],
        )?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn user_roles(&self, user_id: &str) -> SqlResult<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT r.name FROM roles r
             JOIN user_roles ur ON ur.role_id=r.id
             WHERE ur.user_id=?1",
        )?;
        let result: Vec<String> = stmt
            .query_map(params![user_id], |r| r.get(0))?
            .collect::<SqlResult<Vec<_>>>()?;
        Ok(result)
    }

    pub fn list_roles(&self) -> SqlResult<Vec<(String, String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, description FROM roles ORDER BY name",
        )?;
        let result: Vec<(String, String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<SqlResult<Vec<_>>>()?;
        Ok(result)
    }

    #[allow(dead_code)]
    pub fn user_permissions(&self, user_id: &str) -> SqlResult<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT p.name FROM permissions p
             JOIN role_permissions rp ON rp.permission_id=p.id
             JOIN user_roles ur ON ur.role_id=rp.role_id
             WHERE ur.user_id=?1",
        )?;
        let result: Vec<String> = stmt
            .query_map(params![user_id], |r| r.get(0))?
            .collect::<SqlResult<Vec<_>>>()?;
        Ok(result)
    }
}

// ─── CRUD: API Keys ───────────────────────────────────────────────────────────

impl Database {
    pub fn insert_api_key(&self, row: &ApiKeyRow) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO api_keys
             (id,user_id,key_hash,key_prefix,label,is_active,created_at,expires_at,permissions)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                row.id, row.user_id, row.key_hash, row.key_prefix, row.label,
                row.is_active as i64, row.created_at, row.expires_at, row.permissions,
            ],
        )?;
        Ok(())
    }

    /// Get the single active API key for a user (each user has exactly one).
    pub fn get_user_api_key(&self, user_id: &str) -> SqlResult<Option<ApiKeyRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id,user_id,key_hash,key_prefix,label,is_active,created_at,last_used,expires_at,permissions
             FROM api_keys WHERE user_id=?1 AND is_active=1 ORDER BY created_at ASC LIMIT 1",
        )?;
        let mut result: Vec<ApiKeyRow> = stmt
            .query_map(params![user_id], map_api_key)?
            .collect::<SqlResult<Vec<_>>>()?;
        Ok(result.pop())
    }

    /// Create and return the API key for a new user (1 key per account).
    /// Returns (row, raw_key_shown_once).
    pub fn create_user_api_key(
        &self, user_id: &str, label: &str, perms: &ApiKeyPermissions,
    ) -> SqlResult<(ApiKeyRow, String)> {
        use rand::RngCore;
        let mut buf = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut buf);
        let raw_key: String = format!("omrp-sk-{}", buf.iter().map(|b| format!("{b:02x}")).collect::<String>());

        use std::time::{SystemTime, UNIX_EPOCH};
        let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;

        // SHA-256 hash for storage
        use sha2::Digest;
        let digest = sha2::Sha256::digest(raw_key.as_bytes());
        let hash: String = digest.iter().map(|b| format!("{b:02x}")).collect();

        // Short random id
        let mut id_buf = [0u8; 4];
        rand::thread_rng().fill_bytes(&mut id_buf);
        let id = format!("omrp_{}", id_buf.iter().map(|b| format!("{b:02x}")).collect::<String>());

        let row = ApiKeyRow {
            id,
            user_id:     Some(user_id.to_string()),
            key_hash:    hash,
            key_prefix:  raw_key.chars().take(16).collect(),
            label:       label.to_string(),
            is_active:   true,
            created_at:  ts,
            last_used:   None,
            expires_at:  None,
            permissions: perms.to_json(),
        };
        self.insert_api_key(&row)?;
        Ok((row, raw_key))
    }

    /// Update the permissions of a user's API key.
    pub fn update_api_key_permissions(&self, key_id: &str, perms: &ApiKeyPermissions) -> SqlResult<bool> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "UPDATE api_keys SET permissions=?1 WHERE id=?2",
            params![perms.to_json(), key_id],
        )?;
        Ok(n > 0)
    }

    pub fn list_api_keys_for_user(&self, user_id: &str) -> SqlResult<Vec<ApiKeyRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id,user_id,key_hash,key_prefix,label,is_active,created_at,last_used,expires_at,permissions
             FROM api_keys WHERE user_id=?1 ORDER BY created_at DESC",
        )?;
        let result: Vec<ApiKeyRow> = stmt
            .query_map(params![user_id], map_api_key)?
            .collect::<SqlResult<Vec<_>>>()?;
        Ok(result)
    }

    pub fn list_all_api_keys(&self) -> SqlResult<Vec<ApiKeyRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id,user_id,key_hash,key_prefix,label,is_active,created_at,last_used,expires_at,permissions
             FROM api_keys ORDER BY created_at DESC",
        )?;
        let result: Vec<ApiKeyRow> = stmt
            .query_map([], map_api_key)?
            .collect::<SqlResult<Vec<_>>>()?;
        Ok(result)
    }

    pub fn find_api_key_by_hash(&self, hash: &str) -> SqlResult<Option<ApiKeyRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id,user_id,key_hash,key_prefix,label,is_active,created_at,last_used,expires_at,permissions
             FROM api_keys WHERE key_hash=?1 AND is_active=1",
        )?;
        let mut result: Vec<ApiKeyRow> = stmt
            .query_map(params![hash], map_api_key)?
            .collect::<SqlResult<Vec<_>>>()?;
        Ok(result.pop())
    }

    pub fn deactivate_api_key(&self, id: &str) -> SqlResult<bool> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute("UPDATE api_keys SET is_active=0 WHERE id=?1", params![id])?;
        Ok(n > 0)
    }

    pub fn touch_api_key(&self, id: &str, ts: i64) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("UPDATE api_keys SET last_used=?1 WHERE id=?2", params![ts, id])?;
        Ok(())
    }
}

// ─── CRUD: Provider Keys ──────────────────────────────────────────────────────

impl Database {
    pub fn insert_provider_key(&self, row: &ProviderKeyRow) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO provider_keys
             (id,user_id,provider,key_value,label,is_active,is_global,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                row.id, row.user_id, row.provider, row.key_value, row.label,
                row.is_active as i64, row.is_global as i64, row.created_at,
            ],
        )?;
        Ok(())
    }

    pub fn list_provider_keys_for_user(&self, user_id: &str) -> SqlResult<Vec<ProviderKeyRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id,user_id,provider,key_value,label,is_active,is_global,created_at
             FROM provider_keys WHERE user_id=?1 OR is_global=1
             ORDER BY created_at DESC",
        )?;
        let result: Vec<ProviderKeyRow> = stmt
            .query_map(params![user_id], map_provider_key)?
            .collect::<SqlResult<Vec<_>>>()?;
        Ok(result)
    }

    pub fn list_all_provider_keys(&self) -> SqlResult<Vec<ProviderKeyRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id,user_id,provider,key_value,label,is_active,is_global,created_at
             FROM provider_keys ORDER BY created_at DESC",
        )?;
        let result: Vec<ProviderKeyRow> = stmt
            .query_map([], map_provider_key)?
            .collect::<SqlResult<Vec<_>>>()?;
        Ok(result)
    }

    pub fn deactivate_provider_key(&self, id: &str) -> SqlResult<bool> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "UPDATE provider_keys SET is_active=0 WHERE id=?1", params![id],
        )?;
        Ok(n > 0)
    }

    /// Return the active key value for a provider (global first, then user-owned).
    pub fn resolve_provider_key(
        &self, provider: &str, user_id: Option<&str>,
    ) -> SqlResult<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let uid = user_id.unwrap_or("");
        let mut stmt = conn.prepare(
            "SELECT key_value FROM provider_keys
             WHERE provider=?1 AND is_active=1
               AND (is_global=1 OR (?2 != '' AND user_id=?2))
             ORDER BY is_global DESC, created_at ASC
             LIMIT 1",
        )?;
        let mut result: Vec<String> = stmt
            .query_map(params![provider, uid], |r| r.get(0))?
            .collect::<SqlResult<Vec<_>>>()?;
        Ok(result.pop())
    }
}

// ─── CRUD: Settings ───────────────────────────────────────────────────────────

impl Database {
    pub fn get_setting(&self, key: &str) -> SqlResult<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT value FROM settings WHERE key=?1")?;
        let mut result: Vec<String> = stmt
            .query_map(params![key], |r| r.get(0))?
            .collect::<SqlResult<Vec<_>>>()?;
        Ok(result.pop())
    }

    pub fn set_setting(&self, key: &str, value: &str, ts: i64) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO settings (key,value,description,updated_at) VALUES (?1,?2,'',?3)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",
            params![key, value, ts],
        )?;
        Ok(())
    }

    pub fn all_settings(&self) -> SqlResult<Vec<(String, String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT key, value, description FROM settings ORDER BY key",
        )?;
        let result: Vec<(String, String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<SqlResult<Vec<_>>>()?;
        Ok(result)
    }
}

// ─── CRUD: Audit Log ──────────────────────────────────────────────────────────

impl Database {
    #[allow(clippy::too_many_arguments)]
    pub fn audit(
        &self, user_id: Option<&str>, action: &str,
        target_id: Option<&str>, target_type: Option<&str>,
        metadata: Option<&str>, ip: Option<&str>, ts: i64,
    ) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO audit_logs
             (user_id,action,target_id,target_type,metadata,ip_addr,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![user_id, action, target_id, target_type, metadata, ip, ts],
        )?;
        Ok(())
    }

    pub fn recent_audit_logs(&self, limit: i64) -> SqlResult<Vec<AuditLogRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id,user_id,action,target_id,target_type,metadata,ip_addr,created_at
             FROM audit_logs ORDER BY created_at DESC LIMIT ?1",
        )?;
        let result: Vec<AuditLogRow> = stmt
            .query_map(params![limit], |r| Ok(AuditLogRow {
                id:          r.get(0)?,
                user_id:     r.get(1)?,
                action:      r.get(2)?,
                target_id:   r.get(3)?,
                target_type: r.get(4)?,
                metadata:    r.get(5)?,
                ip_addr:     r.get(6)?,
                created_at:  r.get(7)?,
            }))?
            .collect::<SqlResult<Vec<_>>>()?;
        Ok(result)
    }
}

// ─── CRUD: Request Logs ───────────────────────────────────────────────────────

impl Database {
    #[allow(clippy::too_many_arguments)]
    pub fn log_request(
        &self, user_id: Option<&str>, api_key_id: Option<&str>,
        model_id: &str, provider: &str, task_type: Option<&str>, tier: Option<&str>,
        tokens_in: i64, tokens_out: i64, latency_ms: i64,
        success: bool, error_msg: Option<&str>, ts: i64,
    ) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO request_logs
             (user_id,api_key_id,model_id,provider,task_type,tier,
              tokens_in,tokens_out,latency_ms,success,error_msg,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                user_id, api_key_id, model_id, provider, task_type, tier,
                tokens_in, tokens_out, latency_ms, success as i64, error_msg, ts,
            ],
        )?;
        Ok(())
    }

    /// Aggregate stats per day for the last `days` days.
    /// Returns `(date_str, request_count, error_count, total_tokens)`.
    pub fn usage_stats(&self, days: i64) -> SqlResult<Vec<(String, i64, i64, i64)>> {
        let since = now_secs() - days * 86400;
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT date(created_at, 'unixepoch') as day,
                    COUNT(*) as reqs,
                    SUM(CASE WHEN success=0 THEN 1 ELSE 0 END) as errs,
                    SUM(tokens_in + tokens_out) as tokens
             FROM request_logs WHERE created_at >= ?1
             GROUP BY day ORDER BY day ASC",
        )?;
        let result: Vec<(String, i64, i64, i64)> = stmt
            .query_map(params![since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<SqlResult<Vec<_>>>()?;
        Ok(result)
    }

    /// Per-user request statistics for the last `days` days.
    /// Returns `(date, requests, errors, tokens)`.
    pub fn user_usage_stats(&self, user_id: &str, days: i64)
        -> SqlResult<Vec<(String, i64, i64, i64)>>
    {
        let since = now_secs() - days * 86400;
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT date(created_at, 'unixepoch') as day,
                    COUNT(*) as reqs,
                    SUM(CASE WHEN success=0 THEN 1 ELSE 0 END) as errs,
                    SUM(tokens_in + tokens_out) as tokens
             FROM request_logs
             WHERE user_id=?1 AND created_at >= ?2
             GROUP BY day ORDER BY day ASC",
        )?;
        let result: Vec<(String, i64, i64, i64)> = stmt
            .query_map(params![user_id, since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<SqlResult<Vec<_>>>()?;
        Ok(result)
    }

    /// Top-N models by request count (all-time) from request_logs.
    pub fn top_models(&self, limit: i64) -> SqlResult<Vec<(String, i64, i64)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT model_id, COUNT(*) as reqs, SUM(tokens_in+tokens_out) as tokens
             FROM request_logs GROUP BY model_id ORDER BY reqs DESC LIMIT ?1",
        )?;
        let result: Vec<(String, i64, i64)> = stmt
            .query_map(params![limit], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<SqlResult<Vec<_>>>()?;
        Ok(result)
    }
}

// ─── CRUD: Proxies ────────────────────────────────────────────────────────────

impl Database {
    pub fn upsert_proxy(
        &self, url: &str, protocol: &str,
        country: Option<&str>, anonymity: Option<&str>, ts: i64,
    ) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO proxies (url,protocol,country,anonymity,added_at)
             VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(url) DO UPDATE SET
               protocol=excluded.protocol, country=excluded.country,
               anonymity=excluded.anonymity, is_active=1, fail_count=0",
            params![url, protocol, country, anonymity, ts],
        )?;
        Ok(())
    }

    pub fn active_proxies(&self) -> SqlResult<Vec<(i64, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, url FROM proxies WHERE is_active=1 AND fail_count < 5
             ORDER BY latency_ms ASC, uptime_pct DESC LIMIT 200",
        )?;
        let result: Vec<(i64, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<SqlResult<Vec<_>>>()?;
        Ok(result)
    }

    pub fn proxy_count(&self) -> SqlResult<i64> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM proxies WHERE is_active=1", [], |r| r.get(0))
    }

    pub fn mark_proxy_result(
        &self, id: i64, success: bool, latency_ms: Option<i64>, ts: i64,
    ) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        if success {
            conn.execute(
                "UPDATE proxies SET last_check=?2, latency_ms=?3,
                   uptime_pct=MIN(100.0, uptime_pct*0.9 + 10.0),
                   fail_count=0, is_active=1 WHERE id=?1",
                params![id, ts, latency_ms.unwrap_or(0)],
            )?;
        } else {
            conn.execute(
                "UPDATE proxies SET last_check=?2,
                   uptime_pct=MAX(0.0, uptime_pct*0.9),
                   fail_count=fail_count+1,
                   is_active=CASE WHEN fail_count >= 4 THEN 0 ELSE is_active END
                 WHERE id=?1",
                params![id, ts],
            )?;
        }
        Ok(())
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Default database path: `~/.local/share/omrp/omrp.db`.
pub fn default_db_path() -> std::path::PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("omrp")
        .join("omrp.db")
}

// ─── CRUD: Proxy Request Tracking ────────────────────────────────────────────

impl Database {
    /// Log a single LLM request routed through a proxy (or direct if proxy_id=None).
    #[allow(clippy::too_many_arguments)]
    pub fn log_proxy_request(
        &self,
        proxy_id:  Option<i64>,
        proxy_url: Option<&str>,
        model_id:  &str,
        provider:  &str,
        user_id:   Option<&str>,
        api_key_id: Option<&str>,
        success:   bool,
        latency_ms: i64,
        ts:        i64,
    ) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO proxy_requests
             (proxy_id,proxy_url,model_id,provider,user_id,api_key_id,success,latency_ms,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![proxy_id, proxy_url, model_id, provider, user_id, api_key_id,
                    success as i64, latency_ms, ts],
        )?;
        Ok(())
    }

    /// Per-proxy aggregate stats: (proxy_url, total_requests, successes, unique_users, last_used).
    pub fn proxy_usage_stats(&self) -> SqlResult<Vec<(String, i64, i64, i64, i64)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT
               COALESCE(proxy_url, 'direct') as url,
               COUNT(*) as total,
               SUM(success) as ok,
               COUNT(DISTINCT user_id) as users,
               MAX(created_at) as last_used
             FROM proxy_requests
             GROUP BY COALESCE(proxy_url, 'direct')
             ORDER BY total DESC
             LIMIT 100",
        )?;
        let result: Vec<(String, i64, i64, i64, i64)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
            .collect::<SqlResult<Vec<_>>>()?;
        Ok(result)
    }

    /// Per-proxy detail: top models and users for a specific proxy URL.
    #[allow(dead_code)]
    pub fn proxy_detail_stats(&self, proxy_url: &str)
        -> SqlResult<(Vec<(String, i64)>, Vec<(String, i64)>)>
    {
        let conn = self.conn.lock().unwrap();
        // Top models
        let mut stmt = conn.prepare(
            "SELECT model_id, COUNT(*) as c FROM proxy_requests
             WHERE COALESCE(proxy_url,'direct')=?1 GROUP BY model_id ORDER BY c DESC LIMIT 5",
        )?;
        let models: Vec<(String, i64)> = stmt
            .query_map(params![proxy_url], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<SqlResult<Vec<_>>>()?;
        // Top users
        let mut stmt2 = conn.prepare(
            "SELECT COALESCE(user_id,'anon'), COUNT(*) as c FROM proxy_requests
             WHERE COALESCE(proxy_url,'direct')=?1 GROUP BY user_id ORDER BY c DESC LIMIT 5",
        )?;
        let users: Vec<(String, i64)> = stmt2
            .query_map(params![proxy_url], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<SqlResult<Vec<_>>>()?;
        Ok((models, users))
    }

    /// Total requests through the proxy pool in the last `hours`.
    pub fn proxy_requests_recent(&self, hours: i64) -> SqlResult<i64> {
        let since = now_secs() - hours * 3600;
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM proxy_requests WHERE proxy_id IS NOT NULL AND created_at >= ?1",
            params![since], |r| r.get(0),
        )
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_db() -> Database {
        let db = Database::open_in_memory().unwrap();
        db.migrate().unwrap();
        db.seed_defaults().unwrap();
        db
    }

    #[test]
    fn test_schema_creates_tables() {
        assert!(mem_db().is_initialised());
    }

    #[test]
    fn test_seed_roles() {
        let db = mem_db();
        let roles = db.list_roles().unwrap();
        assert_eq!(roles.len(), 2);
    }

    #[test]
    fn test_seed_settings() {
        let db = mem_db();
        assert_eq!(db.get_setting("app.name").unwrap().as_deref(), Some("OMRP"));
    }

    #[test]
    fn test_insert_and_find_user() {
        let db = mem_db();
        let ts = now_secs();
        let row = UserRow {
            id: "u1".into(), username: "alice".into(),
            email: Some("alice@example.com".into()),
            password_hash: "$argon2id$v=19$m=65536$...".into(),
            display_name: "Alice".into(),
            is_active: true, is_admin: false,
            created_at: ts, updated_at: ts, last_login: None,
        };
        db.insert_user(&row).unwrap();
        let found = db.find_user_by_username("alice").unwrap().unwrap();
        assert_eq!(found.email.as_deref(), Some("alice@example.com"));
        assert!(db.has_users());
    }

    #[test]
    fn test_api_key_lifecycle() {
        let db = mem_db();
        let ts = now_secs();
        let row = ApiKeyRow {
            id: "omrp_test1234".into(), user_id: None,
            key_hash: "deadbeef".into(), key_prefix: "omrp-sk-dead".into(),
            label: "Test".into(), is_active: true,
            created_at: ts, last_used: None, expires_at: None,
            permissions: "{}".into(),
        };
        db.insert_api_key(&row).unwrap();
        let found = db.find_api_key_by_hash("deadbeef").unwrap().unwrap();
        assert_eq!(found.label, "Test");
        assert!(db.deactivate_api_key("omrp_test1234").unwrap());
        assert!(db.find_api_key_by_hash("deadbeef").unwrap().is_none());
    }

    #[test]
    fn test_provider_key_resolution() {
        let db = mem_db();
        let ts = now_secs();
        db.insert_provider_key(&ProviderKeyRow {
            id: "pk1".into(), user_id: None, provider: "openrouter".into(),
            key_value: "sk-or-test".into(), label: "Global".into(),
            is_active: true, is_global: true, created_at: ts,
        }).unwrap();
        let v = db.resolve_provider_key("openrouter", None).unwrap();
        assert_eq!(v.as_deref(), Some("sk-or-test"));
    }

    #[test]
    fn test_setting_roundtrip() {
        let db = mem_db();
        db.set_setting("proxy.enabled", "1", now_secs()).unwrap();
        assert_eq!(db.get_setting("proxy.enabled").unwrap().as_deref(), Some("1"));
    }

    #[test]
    fn test_audit_log() {
        let db = mem_db();
        db.audit(None, "test.action", None, None, None, None, now_secs()).unwrap();
        let logs = db.recent_audit_logs(10).unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].action, "test.action");
    }

    #[test]
    fn test_proxy_upsert_and_count() {
        let db = mem_db();
        let ts = now_secs();
        db.upsert_proxy("http://1.2.3.4:8080", "http", Some("US"), Some("elite"), ts).unwrap();
        assert_eq!(db.proxy_count().unwrap(), 1);
        db.mark_proxy_result(1, false, None, ts).unwrap();
        // Still active after 1 failure
        assert_eq!(db.proxy_count().unwrap(), 1);
    }
}
