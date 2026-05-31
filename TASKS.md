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
- Axum web server, JWT auth (Argon2id + HS256), CORS
- Admin: playground, routing intel, users+permissions, API keys, provider keys, settings, audit logs
- User: my key+permissions, my providers, usage stats, profile
- Cyberpunk enterprise SPA (~75kb, zero external JS deps)
- Admin view-as-user toggle, click-to-detail on every user row

### 4-B  SQLite persistence ✅
- 12-table schema: +  +  (v0.2)

### 4-C  Onboarding wizard ✅ (4 steps)
- Step 2: admin API key shown once with copy button + integration snippet

### 4-D  API key system (v0.2 redesigned) ✅
- 1 API key per user, auto-generated at account creation
- Fine-grained permissions: can_use_router, can_use_proxy_bypass, allowed_models, rate_limit_per_hour
- Admin default: proxy_bypass=true; User default: proxy_bypass=false

### 4-E  Proxy pool ✅ (v0.2 enhanced)
- proxy_requests table tracks every proxied call (all-time per-proxy stats)
- Admin-only management; users get bypass via key permissions
- can_use_proxy_bypass=true: direct never used, proxy from first request

### 4-F  Permission enforcement ✅
- Router access gate, model allow-list, proxy bypass routing

---

## Phase 5 — Bayesian Routing ✅ Complete (v0.2)

### 5-A  Bayesian Agent Scoring ✅
- HealthStatus gains success_count (α) + failure_count (β)
- bayesian_competence() = α/(α+β), stability = inverse Beta variance
- Full Bayesian profile via /api/admin/models/health

### 5-B  Thompson Sampling ✅
- RouterEngine::select_thompson() — Gamma ratio sampler, Xorshift64 RNG
- omrp serve uses Thompson Sampling for production routing
- Routing Intelligence dashboard shows seed, selected model, all 5 scores

### 5-C  Wilson Score Garbage Detection ✅
- 95% CI lower bound, min 5 observations before exclusion

---

## Phase 6 — CI ✅ Complete

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
