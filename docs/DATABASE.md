# OMRP Database Schema

OMRP v0.2 stores all persistent state in a SQLite database at
`~/.local/share/omrp/omrp.db` (XDG-compliant).

The database is opened with `PRAGMA journal_mode=WAL` (concurrent reads)
and `PRAGMA foreign_keys=ON` (cascade deletes).

---

## Tables

### `users`

User accounts with hashed passwords.

| Column | Type | Notes |
|--------|------|-------|
| `id` | `TEXT PK` | UUIDv4 |
| `username` | `TEXT UNIQUE NOT NULL` | Login name |
| `email` | `TEXT UNIQUE` | Optional |
| `password_hash` | `TEXT NOT NULL` | Argon2id PHC string |
| `display_name` | `TEXT NOT NULL DEFAULT ''` | UI name |
| `is_active` | `INTEGER NOT NULL DEFAULT 1` | 0 = disabled |
| `is_admin` | `INTEGER NOT NULL DEFAULT 0` | 1 = admin |
| `created_at` | `INTEGER NOT NULL` | Unix timestamp (secs) |
| `updated_at` | `INTEGER NOT NULL` | Unix timestamp (secs) |
| `last_login` | `INTEGER` | NULL until first login |

---

### `roles`

Named role definitions.

| Column | Type | Notes |
|--------|------|-------|
| `id` | `TEXT PK` | e.g. `role_admin`, `role_user` |
| `name` | `TEXT UNIQUE NOT NULL` | Display name |
| `description` | `TEXT NOT NULL DEFAULT ''` | |
| `created_at` | `INTEGER NOT NULL` | |

**Seeded rows:** `role_admin` (full access), `role_user` (user-scoped).

---

### `user_roles`

Many-to-many user ↔ role mapping.

| Column | Type | Notes |
|--------|------|-------|
| `user_id` | `TEXT NOT NULL` | → `users(id)` ON DELETE CASCADE |
| `role_id` | `TEXT NOT NULL` | → `roles(id)` ON DELETE CASCADE |

PK: `(user_id, role_id)`

---

### `permissions`

Granular permission tokens.

| Column | Type | Notes |
|--------|------|-------|
| `id` | `TEXT PK` | e.g. `perm_admin_users_write` |
| `name` | `TEXT UNIQUE NOT NULL` | e.g. `admin:users:write` |
| `description` | `TEXT NOT NULL DEFAULT ''` | |

**Seeded permission names:**

| Name | Description |
|------|-------------|
| `admin:users:read` | List and view users |
| `admin:users:write` | Create, edit, delete users |
| `admin:roles:read` | List and view roles |
| `admin:roles:write` | Create, edit, delete roles |
| `admin:keys:read` | View all API keys |
| `admin:keys:write` | Create and revoke any API key |
| `admin:providers:read` | View provider key config |
| `admin:providers:write` | Manage global provider keys |
| `admin:settings:read` | Read system settings |
| `admin:settings:write` | Update system settings |
| `admin:logs:read` | View audit and request logs |
| `admin:proxies:write` | Manage proxy pool |
| `user:keys:read` | List own API keys |
| `user:keys:write` | Create/revoke own API keys |
| `user:providers:write` | Add own provider keys |
| `user:profile:write` | Edit own profile |

---

### `role_permissions`

Many-to-many role ↔ permission mapping.

| Column | Type | Notes |
|--------|------|-------|
| `role_id` | `TEXT NOT NULL` | → `roles(id)` ON DELETE CASCADE |
| `permission_id` | `TEXT NOT NULL` | → `permissions(id)` ON DELETE CASCADE |

PK: `(role_id, permission_id)`

Seeded:
- `role_admin` → all permissions
- `role_user` → all `user:*` permissions

---

### `api_keys`

Router API keys — used by external tools to authenticate against the OMRP
proxy.  Only the SHA-256 hash of the raw key is stored; the raw key is
shown once on creation and never again.

| Column | Type | Notes |
|--------|------|-------|
| `id` | `TEXT PK` | e.g. `omrp_abc12345` |
| `user_id` | `TEXT` | → `users(id)` ON DELETE CASCADE, NULL = global |
| `key_hash` | `TEXT NOT NULL` | SHA-256 hex of `omrp-sk-<64hex>` |
| `key_prefix` | `TEXT NOT NULL` | First 16 chars (display only) |
| `label` | `TEXT NOT NULL DEFAULT 'default'` | Human label |
| `is_active` | `INTEGER NOT NULL DEFAULT 1` | 0 = revoked |
| `created_at` | `INTEGER NOT NULL` | |
| `last_used` | `INTEGER` | Updated on each authenticated request |
| `expires_at` | `INTEGER` | NULL = no expiry |

Indexes: `idx_api_keys_user(user_id)`, `idx_api_keys_hash(key_hash)`.

---

### `provider_keys`

Credentials for LLM providers (OpenRouter, Kilo, Cerebras, Groq, BUW).
Stored in plaintext (local tool, user-private directory).

| Column | Type | Notes |
|--------|------|-------|
| `id` | `TEXT PK` | UUIDv4 |
| `user_id` | `TEXT` | → `users(id)` ON DELETE CASCADE, NULL = global |
| `provider` | `TEXT NOT NULL` | `openrouter` \| `kilo` \| `cerebras` \| `groq` \| `buw` |
| `key_value` | `TEXT NOT NULL` | Raw API key |
| `label` | `TEXT NOT NULL DEFAULT ''` | Human label |
| `is_active` | `INTEGER NOT NULL DEFAULT 1` | |
| `is_global` | `INTEGER NOT NULL DEFAULT 0` | 1 = available to all users |
| `created_at` | `INTEGER NOT NULL` | |

**Resolution order** when making an LLM request:
1. User's personal key for this provider (`user_id = me`, `is_active = 1`)
2. Global key (`is_global = 1`, `is_active = 1`)
3. Environment variable (`PROVIDER_API_KEY`)

Index: `idx_provider_keys_user(user_id)`.

---

### `settings`

System-wide key-value configuration.

| Column | Type | Notes |
|--------|------|-------|
| `key` | `TEXT PK` | Setting name (e.g. `app.name`) |
| `value` | `TEXT NOT NULL` | String value |
| `description` | `TEXT NOT NULL DEFAULT ''` | Human description |
| `updated_at` | `INTEGER NOT NULL` | |

Uses `INSERT OR REPLACE` (upsert) for updates.

---

### `audit_logs`

Immutable audit trail of all mutating actions.

| Column | Type | Notes |
|--------|------|-------|
| `id` | `INTEGER PK AUTOINCREMENT` | |
| `user_id` | `TEXT` | NULL = system/anonymous |
| `action` | `TEXT NOT NULL` | e.g. `user.create`, `api_key.revoke` |
| `target_id` | `TEXT` | ID of affected object |
| `target_type` | `TEXT` | `user` \| `api_key` \| `role` \| `setting` \| … |
| `metadata` | `TEXT` | JSON blob with extra details |
| `ip_addr` | `TEXT` | Client IP (when available) |
| `created_at` | `INTEGER NOT NULL` | |

Index: `idx_audit_created(created_at DESC)`.

---

### `request_logs`

Per-request stats for dashboard analytics.

| Column | Type | Notes |
|--------|------|-------|
| `id` | `INTEGER PK AUTOINCREMENT` | |
| `user_id` | `TEXT` | NULL = anonymous/API-key request |
| `api_key_id` | `TEXT` | Which API key was used |
| `model_id` | `TEXT NOT NULL` | Actual model used |
| `provider` | `TEXT NOT NULL` | Provider name |
| `task_type` | `TEXT` | e.g. `code`, `reasoning` |
| `tier` | `TEXT` | e.g. `medium`, `complex` |
| `tokens_in` | `INTEGER NOT NULL DEFAULT 0` | Prompt tokens |
| `tokens_out` | `INTEGER NOT NULL DEFAULT 0` | Completion tokens |
| `latency_ms` | `INTEGER NOT NULL DEFAULT 0` | End-to-end latency |
| `success` | `INTEGER NOT NULL DEFAULT 1` | 0 = error |
| `error_msg` | `TEXT` | NULL on success |
| `created_at` | `INTEGER NOT NULL` | |

Indexes: `idx_req_created(created_at DESC)`, `idx_req_user(user_id)`.

---

### `proxies`

HTTP/SOCKS5 proxy pool with health tracking.

| Column | Type | Notes |
|--------|------|-------|
| `id` | `INTEGER PK AUTOINCREMENT` | |
| `url` | `TEXT UNIQUE NOT NULL` | e.g. `http://1.2.3.4:8080` |
| `protocol` | `TEXT NOT NULL` | `http` \| `https` \| `socks5` |
| `country` | `TEXT` | ISO country code |
| `anonymity` | `TEXT` | `transparent` \| `anonymous` \| `elite` |
| `is_active` | `INTEGER NOT NULL DEFAULT 1` | 0 = disabled (too many failures) |
| `last_check` | `INTEGER` | Timestamp of last health check |
| `latency_ms` | `INTEGER` | Last measured latency |
| `uptime_pct` | `REAL NOT NULL DEFAULT 100.0` | Running uptime percentage |
| `fail_count` | `INTEGER NOT NULL DEFAULT 0` | Consecutive failures |
| `added_at` | `INTEGER NOT NULL` | |

Proxies are automatically disabled after 5 consecutive failures
(`fail_count >= 5`). Active proxies are ordered by `latency_ms ASC,
uptime_pct DESC` when selected for rotation.

---

## Initialization

The database is initialized on first `omrp serve` start:

```rust
let db = Database::open(&db_path)?;
db.migrate()?;       // CREATE TABLE IF NOT EXISTS … (idempotent)
db.seed_defaults()?; // INSERT OR IGNORE built-in roles, permissions, settings
```

`migrate()` is safe to call on every startup — all DDL uses
`CREATE TABLE IF NOT EXISTS` and `CREATE INDEX IF NOT EXISTS`.

---

## CRUD Layer

All database operations go through `crates/omrp-runtime/src/db.rs`.
The `Database` struct wraps `Arc<Mutex<Connection>>` for thread safety.

Each method locks the connection for the duration of its operation and
releases it immediately.  The collect-then-return pattern avoids Rust's
end-of-block lifetime issue with `MutexGuard` + `?`:

```rust
// Correct pattern used throughout db.rs:
let conn = self.conn.lock().unwrap();
let mut stmt = conn.prepare("SELECT ...")?;
let result: Vec<T> = stmt.query_map(params, map_fn)?.collect::<SqlResult<Vec<T>>>()?;
Ok(result)  // conn dropped here, releasing the mutex
```
