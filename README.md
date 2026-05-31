# OMRP — Open Model Routing Protocol

> **Status:** v0.2.0 — multi-user web dashboard, 1-key-per-user API system, proxy bypass  
> **Architecture:** Event-sourced, deterministic, Bayesian LLM routing engine

```bash
omrp serve --host 0.0.0.0 --port 18800   # start the multi-user web server
```

Open **http://localhost:18800** in your browser. The setup wizard creates
the first admin account and shows your API key.

---

## Everything through the web

**No terminal access is required for daily use.** After the initial server
start, all configuration is done through the dashboard:

| Who | Can do via web |
|-----|---------------|
| **Admin** | Create/manage users, set permissions per user, add global provider API keys, manage proxy pool, view all stats + audit logs, configure app settings |
| **User** | View their API key + integration guide, add personal provider keys, view own usage stats, update profile/password |

---

## Quick Start

```bash
# 1. Start the server
omrp serve --host 0.0.0.0 --port 18800

# 2. Open browser → http://your-server:18800
# 3. Complete the 4-step setup wizard
# 4. Copy the admin API key (shown ONCE — save it!)
# 5. Configure Cursor / Claude Desktop / Continue with:
#      Base URL:  http://your-server:18800/v1
#      API Key:   omrp-sk-<your key>
#      Model:     omrp/auto
```

---

## API Key System (1 key per user)

Every account gets **exactly one** API key, auto-generated at account creation.
The key carries fine-grained permissions set by the admin:

| Permission | Description | Admin default | User default |
|-----------|-------------|---------------|-------------|
| `can_use_router` | Access `/v1/chat/completions` | `true` | `true` |
| `can_use_proxy_bypass` | Route through proxy pool — **zero rate limits** | `true` | `false` |
| `allowed_models` | Restrict to specific models | all | all |
| `rate_limit_per_hour` | Request cap | unlimited | unlimited |

To enable rate-limit bypass for a user: **Admin → Users → click user → Proxy Bypass → Save**.

If a user loses their key: **Admin → Router Keys → Reset** (old key immediately deactivated, new key generated).

---

## Proxy Rate-Limit Bypass

When `can_use_proxy_bypass = true`, **every** request from that user's key is
routed through a rotating pool of proxy IPs — their real IP never reaches
the LLM provider. Result: zero rate-limit errors.

Manage the proxy pool: **Admin → Proxies → Enable → Refresh Pool Now**.

---

## What is OMRP routing?

OMRP selects the best available model for each request using:
- **Bayesian Agent Scoring** — 5-factor: Competence α/(α+β), Speed, Skill Match, Load, Stability
- **Thompson Sampling** — probabilistic exploration balancing proven vs uncertain models
- **Wilson Score Garbage Detection** — excludes consistently failing models
- **SHA-256 Tamper-Evident Ledger** — every routing decision cryptographically chained

---

## CLI (advanced / server admin)

```bash
omrp route   [--task T] [--tier T] <prompt>   # one-shot routing
omrp status                                    # model health scores
omrp best <task>                               # best model for a task
omrp init                                      # create default config file
```

---

## Crates

| Crate | Description |
|-------|-------------|
| `omrp-types` | Shared types: Model, TaskType, RoutingDecision |
| `omrp-events` | Event enum, ErrorKind, validate() |
| `omrp-core` | Routing kernel: State, dispatch(), Scorer, RouterEngine |
| `omrp-runtime` | Web server + CLI binary |

---

## Documentation

| File | Description |
|------|-------------|
| `docs/WEB.md` | REST API reference, key system, proxy bypass |
| `docs/ROUTING.md` | Bayesian scoring, Thompson Sampling, Wilson Score |
| `docs/DATABASE.md` | SQLite schema (12 tables) |
| `docs/PROVIDERS.md` | All 5 LLM providers + setup |
| `docs/ARCHITECTURE.md` | Crate graph, data flow, invariants |

---

## License

MIT


> **Status:** v0.2.0 — multi-user web app, SQLite persistence, BUW provider  
> **Architecture:** Event-sourced, deterministic, replay-safe LLM routing engine

```bash
omrp serve                   # multi-user dashboard on :18800
omrp route --task code "…"   # one-shot routing from CLI
cargo run -p omrp-runtime -- best code
```

---

## What is OMRP?

OMRP is a local LLM routing daemon that selects the best available model for
each request. Every routing decision is **deterministic**: given the same event
history and the same request, OMRP always picks the same model.

The v0.2 web server adds:
- **Multi-user dashboard** — admin and user roles, JWT auth, onboarding wizard
- **SQLite persistence** — users, API keys, provider keys, audit logs, request
  stats, proxy pool
- **Database-backed provider keys** — set API keys per user or globally, no
  env-var required once configured
- **API key management** — generate `omrp-sk-…` tokens for tools like Cursor,
  Claude Desktop, or Continue to authenticate against the proxy
- **Proxy pool** — automatic fetch and rotation from ProxyScrape-compatible
  sources
- **BUW provider** — new gateway (`BUW_API_KEY`) with `buw/omrp-auto` and
  `buw/auto-kilo` virtual models

---

## Quick Start

```bash
# Build
cargo build

# Start multi-user server (first run shows onboarding wizard)
cargo run -p omrp-runtime -- serve --host 0.0.0.0 --port 18800

# Or: one-shot routing (no server)
export OPENROUTER_API_KEY=sk-…
omrp route --task code "write a fibonacci in Rust"
```

Open **http://localhost:18800** — the setup wizard creates your admin account on
first run.

---

## CLI commands

```bash
omrp route   [--task T] [--tier T] [--max-tokens N] <prompt|stdin>
omrp serve   [--port N] [--host H]
omrp models                          # list registered models
omrp status                          # health + routing scores
omrp best <task>                     # best model for a task (no API call)
omrp dashboard                       # live TUI (q to quit)
omrp init                            # create default config
```

---

## Providers

| Provider    | Env Var              | Free models           |
|-------------|----------------------|-----------------------|
| Cerebras    | `CEREBRAS_API_KEY`   | 14,400 req/day        |
| Groq        | `GROQ_API_KEY`       | 1k–14k req/day        |
| Kilo        | `KILO_API_KEY`       | kilo/auto-free router |
| OpenRouter  | `OPENROUTER_API_KEY` | 50–1000 req/day       |
| BUW         | `BUW_API_KEY`        | Virtual gateway       |

Keys can also be set via the web dashboard → Provider Keys (no restart needed).

---

## Key Properties

### Deterministic Routing
- Same ledger + same request → same routing decision, always
- No `rand`, no `SystemTime::now()`, no external reads inside reducers

### Tamper-Evident Ledger
- Every event is SHA-256 chained to the previous one
- Append-only: history cannot be rewritten

### BKG-FMR Scoring Engine
*Best Known Garbage-Free Models Router*
- 5-factor scoring: `health (0.35) + latency (0.20) + success_rate (0.25) + stability (0.10) + load (0.10)`

### Multi-User Web Application (v0.2+)
- SQLite-backed: users, roles, permissions, API keys, request logs, proxy pool
- JWT authentication with Argon2id password hashing
- Admin dashboard: user management, role-based access, key management, audit logs
- User dashboard: personal API keys, provider keys, usage statistics

---

## Architecture

```
┌──────────────────────────────────────────────────────────────┐
│                  omrp-runtime                                  │
│  serve (axum) | route | models | status | best | dashboard    │
└───────────────────────┬──────────────────────────────────────┘
                        │ uses
┌───────────────────────▼──────────────────────────────────────┐
│                   omrp-core                                    │
│  EventPipeline ──► LedgerStore (SHA-256 chain)               │
│  dispatch(&mut State, &Event)  ← pure reducer                 │
│  RouterEngine::select(state, request)  ← pure function        │
└──────────────────────────────────────────────────────────────┘
        depends on
┌───────────────────────────────────────────────────────────────┐
│  omrp-events          │  omrp-types                           │
└───────────────────────────────────────────────────────────────┘
```

---

## Documentation

| File | Description |
|------|-------------|
| `docs/ARCHITECTURE.md` | Crate graph, data flow, invariants |
| `docs/EVENTS.md`       | All 14 event variants, state effects |
| `docs/ROUTING.md`      | BKG-FMR scoring, garbage detection |
| `TASKS.md`             | Phase roadmap and next tasks |

---

## License

MIT
