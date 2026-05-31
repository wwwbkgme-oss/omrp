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
**onboarding wizard** that:
1. Creates the admin account
2. Sets the application name
3. Shows the admin's **API key once** — copy it and store it securely

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
3. `Authorization: Bearer omrp-sk-<64hex>` (router API key — the primary
   way external tools authenticate)

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

## API Key System

### One key per account

Every user account has **exactly one** API key, automatically generated
when the account is created.  The raw key is shown **once** and never
again.  No additional keys can be generated.

The API key carries **fine-grained permissions** (set/changed by admin):

| Permission | Default (user) | Default (admin) |
|-----------|----------------|-----------------|
| `can_use_router` | `true` | `true` |
| `can_use_proxy_bypass` | `false` | `true` |
| `allowed_models` | `[]` (all) | `[]` (all) |
| `rate_limit_per_hour` | `0` (unlimited) | `0` (unlimited) |

### Proxy bypass permission

When `can_use_proxy_bypass = true`:
- **Every** request from this key is routed through the proxy pool
  from the start — the user's real IP never reaches the LLM provider
- Zero rate limit issues (each request uses a different proxy IP)
- Admin enables this per-user in: Dashboard → Users → click user → Permissions

When `can_use_proxy_bypass = false`:
- Requests go direct — standard rate limits apply

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
  "app_name":     "OMRP"             // optional
}
```

**Response (includes admin API key — save immediately):**
```json
{
  "status":   "ok",
  "user_id":  "uuid",
  "api_key":  "omrp-sk-<64hex>",
  "message":  "Admin account created. Save your API key — it will not be shown again."
}
```

---

## Admin Endpoints

All `/api/admin/*` endpoints require `is_admin: true`.

### Users

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/admin/users` | List all users (includes last_login, created_at) |
| `POST` | `/api/admin/users` | Create user + auto-generate API key |
| `PUT` | `/api/admin/users/:id` | Update user (password, is_active, display_name, email) |
| `DELETE` | `/api/admin/users/:id` | Delete user |
| `PUT` | `/api/admin/users/:id/roles` | Replace user's roles |
| `GET` | `/api/admin/users/:id/stats` | Per-user usage stats (30d daily breakdown) |
| `GET` | `/api/admin/users/:id/key` | User's API key info + permissions |
| `PUT` | `/api/admin/users/:id/key/permissions` | Update API key permissions |

**Create user response (key shown once):**
```json
{
  "id": "uuid", "username": "alice",
  "api_key": "omrp-sk-<64hex>",
  "note": "Share this key with the user — it will not be shown again."
}
```

**Update permissions:**
```json
{
  "can_use_router":       true,
  "can_use_proxy_bypass": true,
  "allowed_models":       ["openrouter/claude-3-5-sonnet"],
  "rate_limit_per_hour":  100
}
```

### Roles & Settings

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/admin/roles` | List built-in roles |
| `GET` | `/api/admin/settings` | List all system settings |
| `PUT` | `/api/admin/settings/:key` | Update a setting |

### Router & Provider Keys

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/admin/api-keys` | List all API keys |
| `DELETE` | `/api/admin/api-keys/:id` | Revoke key (emergency only) |
| `GET` | `/api/admin/provider-keys` | List all provider keys |
| `POST` | `/api/admin/provider-keys` | Add provider key |
| `DELETE` | `/api/admin/provider-keys/:id` | Remove provider key |

### Stats & Analytics

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/admin/stats` | Usage stats (30d), user count, key count, proxy count |
| `GET` | `/api/admin/audit-logs` | Last 100 audit log entries |
| `GET` | `/api/admin/models/health` | Full Bayesian health profile per model |
| `GET` | `/api/admin/routing/stats` | Ledger events, completions, failures, top models |

### Proxy Pool

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/admin/proxies` | List active proxies |
| `GET` | `/api/admin/proxies/stats` | Per-proxy usage stats (total, success%, users, last_used) |
| `POST` | `/api/admin/proxies/refresh` | Trigger background refresh from ProxyScrape |

---

## User Endpoints

### Key & Permissions

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/user/key` | Own key info (prefix, is_active, permissions) |
| `GET` | `/api/user/permissions` | Own permissions object |

### Provider Keys

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/user/provider-keys` | Own + global provider keys |
| `POST` | `/api/user/provider-keys` | Add personal provider key |
| `DELETE` | `/api/user/provider-keys/:id` | Remove own provider key |

### Stats & Profile

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/user/me` | Own profile |
| `PUT` | `/api/user/me` | Update password, display_name, email |
| `GET` | `/api/user/stats` | Own usage stats (30 days) |

---

## OpenAI-Compatible Proxy

```bash
curl http://localhost:18800/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer omrp-sk-<your-key>" \
  -d '{ "model": "omrp/auto", "messages": [{"role":"user","content":"hello"}] }'
```

### Permission enforcement

| Check | Condition | Error |
|-------|-----------|-------|
| Router access | `can_use_router = false` | 403 Forbidden |
| Model allow-list | Model not in `allowed_models` | 403 Forbidden |
| Auth required | Keys exist but no Bearer token | 401 Unauthorized |

### Proxy bypass behaviour

- `can_use_proxy_bypass = true`: request routed through proxy pool **from the first attempt** — no direct IP
- `can_use_proxy_bypass = false`: direct only — standard provider rate limits apply

### Special model names

| Model | Behaviour |
|-------|-----------|
| `omrp/auto` | Thompson Sampling routing |
| `omrp/auto-free` | Same as above |
| `auto` | Same as above |
| Any registered model ID | Direct to that model |

### GET /v1/models

Returns OpenAI-format model list including virtual models and all registered models.

---

## Meta Endpoints

```http
GET /health   → { "status":"ok", "version":"0.2.0" }
GET /stats    → { "proxy_count":N, "user_count":N, "version":"0.2.0" }
```

---

## Tool Configuration

| Tool | Base URL | API Key field | Model |
|------|----------|--------------|-------|
| Cursor | `http://localhost:18800/v1` | Settings → Models → API Key | `omrp/auto` |
| Claude Desktop | `http://localhost:18800/v1` | `apiKey` in config | `omrp/auto` |
| Continue | `http://localhost:18800/v1` | `apiKey` | `omrp/auto` |
| Open WebUI | `http://localhost:18800/v1` | Admin → Connections | `omrp/auto` |
| Any OpenAI SDK | `http://localhost:18800/v1` | `api_key=` | `omrp/auto` |

The user dashboard (My API Key page) shows these instructions automatically
with the correct server URL and key prefix for reference.


