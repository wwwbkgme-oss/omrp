# OMRP — Task Roadmap

---

## Phase 1 — Kernel ✅ Complete

All core machinery implemented and tested (34 tests, 0 failures).

```bash
cargo run -p omrp-runtime -- best code
```

---

## Phase 2 — Config + Persistence ✅ Complete

### 2-A  Config file loading ✅

- TOML config at `~/.config/omrp/config.toml`
- 5-provider built-in defaults (Cerebras, Groq, Kilo, OpenRouter, BUW)
- `omrp init` writes default config

### 2-B  Persistent LedgerStore ✅

- JSON Lines ledger at `~/.local/share/omrp/ledger.jsonl`
- SHA-256 chained entries, `verify_chain()` on load

---

## Phase 3 — First Real API Call ✅ Complete

### 3-A  ProviderAdapter + adapters ✅

- `CompatClient` for OpenRouter, Kilo, Cerebras, Groq, BUW
- HTTP status → `ProviderError` mapping
- `complete_with_retry` (429/network auto-retry)

### 3-B  `omrp route` CLI ✅

- `omrp route [--task T] [--tier T] [--max-tokens N] <prompt>`
- Stdin or argument prompt
- Tier classification + fallback chain

### 3-C  Retry policy + fallback ✅

- RateLimited → wait `retry_after`, retry once
- Network error → wait 1s, retry once
- Auth / ModelNotFound → skip to next fallback immediately

---

## Phase 4 — Web Application ✅ Complete (v0.2)

### 4-A  Multi-user dashboard ✅

- Axum web server at `omrp serve --host H --port N`
- JWT auth (Argon2id + HS256), cookie + Bearer header
- Admin dashboard: users, roles, API keys, provider keys, settings, audit logs
- User dashboard: personal keys, provider keys, usage stats, profile
- Cyberpunk enterprise SPA (zero external JS deps, 55kb)

### 4-B  SQLite persistence ✅

- 11-table schema: users, roles, permissions, api_keys, provider_keys,
  settings, audit_logs, request_logs, proxies
- Full CRUD layer in `src/db.rs`

### 4-C  Onboarding wizard ✅

- First-run detection (`/api/setup/status`)
- 3-step wizard in SPA: admin account → app name → done

### 4-D  API key system ✅

- Router keys (`omrp-sk-…`) for external tools (Cursor, Claude Desktop, Continue)
- Provider keys in DB (override env vars)
- Per-user and global scope

### 4-E  Proxy pool ✅

- ProxyScrape API integration (JSON + plain-text)
- SQLite-backed pool with health tracking
- Background refresh via `/api/admin/proxies/refresh`

---

## Phase 5 — Bayesian Routing ✅ Complete (v0.2)

### 5-A  Bayesian Agent Scoring ✅

- `HealthStatus` gains `success_count` (α) and `failure_count` (β)
- `bayesian_competence()` = α/(α+β) replaces EMA ratio in scorer
- `stability_score()` = inverse Beta variance (replaces binary last-event check)

### 5-B  Thompson Sampling ✅

- `RouterEngine::select_thompson()` — probabilistic exploration
- Beta(α,β) sampled via Gamma ratio (Marsaglia-Tsang, no external deps)
- Xorshift64 PRNG seeded from ledger sequence
- `select()` remains fully deterministic (CLI unchanged)

### 5-C  Wilson Score Garbage Detection ✅

- Replaces `success_ratio < 0.2` with Wilson Score lower bound (95% CI)
- Minimum 5 observations before garbage classification
- More principled: accounts for sample size uncertainty

---

## Phase 6 — CI/CD ✅ Complete

- GitHub Actions CI: `cargo build`, `cargo test`, `cargo clippy`
- Format check (advisory) — codebase uses column-aligned style

---

## Backlog

### Health Probes
- [ ] Background probe thread per model (Phase 4-A spec)
- [ ] `omrp status --watch` live table

### OpenAI Proxy Streaming
- [ ] True SSE streaming passthrough in axum server (currently batched)

### Roles & Permissions API
- [ ] Custom role creation via API
- [ ] Fine-grained permission assignment UI

### Cost-Aware Routing
- [ ] `cost_per_1k_tokens` in model config
- [ ] `max_cost` in RouteRequest

### Token Budget
- [ ] Pre-filter models whose `context_window < prompt_tokens`

### Circuit Breaker
- [ ] `CircuitState`: Closed → Open → HalfOpen → Closed
- [ ] Open after N consecutive failures
- [ ] `omrp status` shows circuit state

### Benchmarks
- [ ] `criterion` for `dispatch()` throughput and `select()` latency

### Dioxus Dashboard
- [ ] Read-only projection of State, live refresh (replaces TUI)

### Qwen Adapter
- [ ] Direct Qwen endpoint (different auth/endpoint than OpenRouter)

---

## Quick-Start Summary

```
Phase 1  kernel + routing                    ✅ (34 tests)
Phase 2  config + ledger persistence         ✅
Phase 3  live API calls + CLI routing        ✅
Phase 4  multi-user web app                  ✅ (v0.2)
Phase 5  Bayesian scoring + Thompson         ✅ (v0.2)
Phase 6  CI                                  ✅
─────────────────────────────────────────────────────────
Total: 119 tests, 0 failures
```
