# OMRP — Open Model Routing Protocol

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
