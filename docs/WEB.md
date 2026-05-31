# OMRP Web Server — REST API Reference

`omrp serve` starts a full multi-user web application on port 18800
(default).  The same port serves the SPA dashboard, the REST API, and the
OpenAI-compatible LLM proxy.

---

## Starting the server

```bash
omrp serve                         # localhost:18800
omrp serve --host 0.0.0.0 --port 8080
```

On first run the server detects that no users exist and the SPA shows an
onboarding wizard to create the admin account.

---

## Authentication

### Login (obtain JWT)

```http
POST /api/auth/login
Content-Type: application/json

{ "username": "admin", "password": "s3cr3t" }
```

**Response 200:**
```json
{ "token": "eyJ...", "user_id": "uuid", "username": "admin", "is_admin": true }
```

The server also sets `Set-Cookie: omrp_token=<jwt>; HttpOnly; SameSite=Lax`.

### Using the JWT

Every protected endpoint requires **one** of:
1. `Authorization: Bearer <jwt>` header
2. `omrp_token` cookie (set automatically on login)
3. `Authorization: Bearer omrp-sk-<64hex>` (router API key)

### Logout

```http
POST /api/auth/logout
```

Clears the `omrp_token` cookie.

### Current user

```http
GET /api/auth/me
Authorization: Bearer <token>
```

---

## Onboarding / Setup

```http
GET /api/setup/status
```
Returns `{ "onboarding_needed": true|false }`.

```http
POST /api/setup
Content-Type: application/json

{
  "username":     "admin",
  "password":     "min8chars",
  "display_name": "Administrator",   // optional
  "app_name":     "OMRP"            // optional
}
```

Only works when `onboarding_needed == true` (no users exist yet).

---

## Admin Endpoints

All `/api/admin/*` endpoints require a valid JWT with `is_admin: true`.

### Users

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/admin/users` | List all users |
| `POST` | `/api/admin/users` | Create user |
| `PUT` | `/api/admin/users/:id` | Update user (password, is_active) |
| `DELETE` | `/api/admin/users/:id` | Delete user (cannot delete yourself) |
| `PUT` | `/api/admin/users/:id/roles` | Replace user's roles |

**Create user body:**
```json
{
  "username": "alice",
  "password": "min8chars",
  "display_name": "Alice",  // optional
  "is_admin": false          // optional, default false
}
```

### Roles

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/admin/roles` | List built-in roles |

Built-in roles: `role_admin` (full access), `role_user` (user-scoped access).

### Router API Keys

Keys that external tools (Cursor, Claude Desktop, Continue, etc.) use to
authenticate against the OMRP proxy.

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/admin/api-keys` | List all API keys |
| `POST` | `/api/admin/api-keys` | Generate new key (returned once) |
| `DELETE` | `/api/admin/api-keys/:id` | Revoke key |

**Generate body:**
```json
{ "label": "Cursor", "user_id": "uuid" }  // user_id optional
```

**Response (key shown once):**
```json
{
  "id":      "omrp_abc12345",
  "key":     "omrp-sk-<64hex>",
  "label":   "Cursor",
  "created_at": 1748000000,
  "note":    "Copy this key — it will not be shown again."
}
```

### Provider API Keys

Credentials for LLM providers (OpenRouter, Kilo, Cerebras, Groq, BUW).
DB keys take priority over environment variables.

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/admin/provider-keys` | List all provider keys |
| `POST` | `/api/admin/provider-keys` | Add provider key |
| `DELETE` | `/api/admin/provider-keys/:id` | Remove provider key |

**Add body:**
```json
{
  "provider":  "openrouter",
  "key_value": "sk-or-...",
  "label":     "Primary",
  "is_global": true,          // true = available to all users
  "user_id":   null           // optional: scope to one user
}
```

### Settings

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/admin/settings` | List all system settings |
| `PUT` | `/api/admin/settings/:key` | Update a setting |

**Update body:**
```json
{ "value": "MyOMRP" }
```

**Configurable settings:**

| Key | Default | Description |
|-----|---------|-------------|
| `app.name` | `OMRP` | Application display name |
| `app.tagline` | `Open Model Routing Protocol` | Tagline shown in UI |
| `app.registration_open` | `0` | `1` = allow public self-registration |
| `proxy.enabled` | `0` | `1` = route LLM calls through proxy pool |
| `proxy.refresh_interval` | `3600` | Proxy list refresh (seconds) |
| `proxy.source_url` | ProxyScrape URL | API URL for proxy list |
| `jwt.expiry_secs` | `86400` | JWT token lifetime (seconds) |
| `routing.default_tier` | `medium` | Default prompt tier |
| `routing.fallback_count` | `3` | Max fallback models per request |

### Statistics & Logs

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/admin/stats` | Usage stats (30 days), user count, key count, proxy count |
| `GET` | `/api/admin/audit-logs` | Last 100 audit log entries |

### Proxy Pool

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/admin/proxies` | List active proxies |
| `POST` | `/api/admin/proxies/refresh` | Trigger background refresh from ProxyScrape |

---

## User Endpoints

All `/api/user/*` endpoints require a valid JWT (any role).

### Personal API Keys

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/user/api-keys` | List own API keys |
| `POST` | `/api/user/api-keys` | Generate own key |
| `DELETE` | `/api/user/api-keys/:id` | Revoke own key |

### Personal Provider Keys

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/user/provider-keys` | List own + global provider keys |
| `POST` | `/api/user/provider-keys` | Add personal provider key |
| `DELETE` | `/api/user/provider-keys/:id` | Remove own provider key |

### Profile & Stats

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/user/me` | Own profile |
| `PUT` | `/api/user/me` | Update password |
| `GET` | `/api/user/stats` | Own usage stats (30 days) |

---

## OpenAI-Compatible Proxy

The proxy endpoint is compatible with any OpenAI client.  Just change the
`base_url` to `http://localhost:18800/v1` and set the API key to an
`omrp-sk-…` token (or leave empty if no keys are configured).

```bash
curl http://localhost:18800/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer omrp-sk-<your-key>" \
  -d '{
    "model": "omrp/auto",
    "messages": [{"role":"user","content":"write a fibonacci in Rust"}]
  }'
```

### Endpoint

```http
POST /v1/chat/completions
POST /chat/completions      (alias)
```

### Special model names

| Model | Behaviour |
|-------|-----------|
| `omrp/auto` | OMRP selects best model via Thompson Sampling |
| `omrp/auto-free` | Same as above |
| `auto` | Same as above |
| Any registered model ID | Route directly to that model |

### Auth behaviour

- If **no API keys** are configured: all requests accepted (open mode)
- If **any key exists**: `Authorization: Bearer omrp-sk-…` required

### Request routing flow

```
POST /v1/chat/completions
  │
  ├─ Auth check (omrp-sk- key → SHA-256 lookup in api_keys table
  │              JWT token   → validate signature)
  │
  ├─ Parse body: model, messages, max_tokens, stream
  │
  ├─ Is model "auto"?
  │   ├─ YES: classify prompt → tier → select_thompson() → routed_model
  │   └─ NO:  use requested model directly
  │
  ├─ Resolve provider API key:
  │   1. DB (user's personal key for this provider)
  │   2. DB (global key for this provider)
  │   3. Environment variable (PROVIDER_API_KEY)
  │
  ├─ spawn_blocking: CompatClient → POST /chat/completions to provider
  │
  ├─ Log request to request_logs table
  │
  └─ Return response + X-OMRP-Model + X-OMRP-Tier headers
```

### Response headers

| Header | Value |
|--------|-------|
| `X-OMRP-Model` | Actual model used (may differ from requested if auto-routed) |
| `X-OMRP-Tier` | Routing tier (`simple`, `medium`, `complex`, `reasoning`, `explicit`) |

### List models

```http
GET /v1/models
GET /models     (alias)
```

Returns OpenAI-format model list including `omrp/auto`, `omrp/auto-free`,
and all models from the config file.

---

## Meta Endpoints

```http
GET /health   → { "status":"ok", "version":"0.2.0" }
GET /stats    → { "proxy_count":N, "user_count":N, "version":"0.2.0" }
```

---

## Tool Configuration Examples

### Cursor

- **Base URL**: `http://localhost:18800/v1`
- **API Key**: `omrp-sk-<your-key>` (generate in dashboard)
- **Model**: `omrp/auto` (auto-routing) or any registered model ID

### Claude Desktop

```json
{
  "mcpServers": {},
  "apiKey": "omrp-sk-<your-key>",
  "baseURL": "http://localhost:18800/v1"
}
```

### Continue (VS Code extension)

```json
{
  "models": [{
    "title": "OMRP Auto",
    "provider": "openai",
    "model": "omrp/auto",
    "apiBase": "http://localhost:18800/v1",
    "apiKey": "omrp-sk-<your-key>"
  }]
}
```
