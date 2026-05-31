# OMRP Architecture — v0.2

## Crate Dependency Graph

```
omrp-runtime  (binary: omrp)
    │
    ├── omrp-core
    │       ├── omrp-events
    │       │       └── omrp-types
    │       └── omrp-types
    │
    └── omrp-events
            └── omrp-types
```

All crates live in the same Cargo workspace (`resolver = "2"`).
`omrp-types` has no internal dependencies and forms the shared vocabulary
for the entire system.

---

## System Overview

OMRP v0.2 combines a **deterministic LLM routing kernel** with a
**multi-user web application**.  The web layer adds SQLite persistence,
JWT authentication, an admin/user dashboard, and a proxy pool — but the
routing kernel underneath remains pure and deterministic.

```
┌─────────────────────────────────────────────────────────────────────────┐
│  omrp-runtime                                                            │
│                                                                          │
│  omrp serve  ──► axum web server (web_server.rs)                        │
│               │     REST API, SPA (spa.html), LLM proxy passthrough      │
│               │                                                           │
│               ├──► auth.rs    JWT / Argon2id / onboarding               │
│               ├──► db.rs      SQLite via rusqlite (11 tables)            │
│               └──► proxy.rs   ProxyScrape fetch + pool                   │
│                                                                           │
│  omrp route  ──► routing.rs (bootstrap_pipeline, select_for_tier)       │
│  omrp status │                                                           │
│  omrp best   └──► uses omrp-core directly (deterministic)               │
│  omrp dashboard                                                          │
└────────────────────────────┬────────────────────────────────────────────┘
                             │ uses
┌────────────────────────────▼────────────────────────────────────────────┐
│  omrp-core                                                               │
│                                                                          │
│  EventPipeline ──► LedgerStore (SHA-256 chain)                          │
│       │                                                                  │
│       ▼                                                                  │
│  dispatch(&mut State, &Event)  ← pure reducer                            │
│       │                                                                  │
│       ▼                                                                  │
│  State { models, health { α,β,Wilson,EMA }, inflight, cache, diag }     │
│       │                                                                  │
│       ▼                                                                  │
│  RouterEngine::select()           ← deterministic BKG-FMR 5-factor     │
│  RouterEngine::select_thompson()  ← Thompson Sampling (probabilistic)  │
└──────────────────────────────────────────────────────────────────────────┘
        depends on
┌─────────────────────────────────────────────────────────────────────────┐
│  omrp-events │  omrp-types                                              │
│  Event enum  │  Model, TaskType, RouteRequest, RoutingDecision          │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## Data Flow

```
External input (HTTP or CLI)
      │
      ▼
 EventPipeline::process(event)
      │
      ├─ 1. validate(&event)          ← omrp-events::validate
      │
      ├─ 2. ledger.append(event)      ← SHA-256 chained to previous
      │
      └─ 3. dispatch(&mut state, &event)  ← pure reducer, no IO
                │
                └─ mutates State:
                   • success_count / failure_count (Bayesian α/β)
                   • success_ratio (EMA, backward compat)
                   • rolling_latency_avg_ms (EMA)
                   • garbage (Wilson Score lower bound)

      State
        │
        ├─ RouterEngine::select()          → deterministic RoutingDecision
        └─ RouterEngine::select_thompson() → Thompson Sample RoutingDecision
```

No reducer step may read wall-clock time, generate randomness, or perform IO.

---

## Three-Machine Model

```
┌─────────────────────────────────────────────────────────────────┐
│  1. Ledger Machine                                               │
│                                                                  │
│  Append-only, SHA-256 chained.  Tampering anywhere detectable.  │
│  LedgerStore { entries: Vec<LedgerEntry>, path, checksum }      │
└──────────────────────────────┬──────────────────────────────────┘
                               │ ordered slice of Events
                               ▼
┌─────────────────────────────────────────────────────────────────┐
│  2. Reducer Machine                                              │
│                                                                  │
│  Pure: no IO, no randomness, no SystemTime.                      │
│  Determinism guarantee: same events → identical State.           │
│  fn dispatch(state: &mut State, event: &Event)                  │
│  Updates: health (α/β/EMA), inflight, routing_cache, diag       │
└──────────────────────────────┬──────────────────────────────────┘
                               │ immutable State snapshot
                               ▼
┌─────────────────────────────────────────────────────────────────┐
│  3. Scheduler Machine                                            │
│                                                                  │
│  select():          deterministic BKG-FMR scoring               │
│  select_thompson(): probabilistic Thompson Sampling              │
│  Returns: RoutingDecision { model, score, factors, fallback }   │
└─────────────────────────────────────────────────────────────────┘
```

---

## Crate Details

### `omrp-types`

Shared vocabulary. No dependencies other than `serde`.

| Module | Key types |
|--------|-----------|
| `time` | `SequencedInstant { seq, logical_time }`, `Clock` |
| `model` | `Model { id, provider, capabilities }`, `ModelCapabilities`, `ModelId = String` |
| `task` | `TaskType` (Code/Reasoning/Chat/Vision/Analysis), `RouteRequest` |
| `routing` | `RoutingDecision`, `ModelScore`, `ScoreFactor`, `RoutingCache`, `FallbackEntry` |

---

### `omrp-events`

Event definitions and validation.

| Module | Purpose |
|--------|---------|
| `event` | `Event` enum (14 variants), `ModelSource` |
| `error` | `ErrorKind`, `ProviderError` |
| `validate` | `validate(&Event) -> Result<(), ValidationError>` |

---

### `omrp-core`

The routing kernel. Pure, deterministic, no IO.

| Module | Key type / function | Role |
|--------|---------------------|------|
| `state` | `State`, `HealthStatus`, `Diagnostics` | Projected in-memory state |
| `reducers` | `dispatch(&mut State, &Event)` | Pure state transition; updates α/β and Wilson Score garbage flag |
| `pipeline` | `EventPipeline` | Orchestrates validate → persist → apply |
| `ledger` | `LedgerStore`, `LedgerEntry` | Tamper-evident append-only log |
| `scorer` | `Scorer`, `ScoringWeights` | BKG-FMR 5-factor Bayesian scoring |
| `router` | `RouterEngine` | Deterministic `select()` + probabilistic `select_thompson()` |
| `caveman` | `CavemanLevel`, `inject_caveman` | Prompt compression (lite/full/ultra) |
| `classifier` | `classify_prompt`, `detect_mode_override` | Tier classification |
| `rtk` | `compress_messages`, `format_rtk_log` | Tool output compression |

#### `state::HealthStatus` (v0.2)

```rust
pub struct HealthStatus {
    // Timestamps (deterministic SequencedInstant)
    pub last_success:          SequencedInstant,
    pub last_failure:          SequencedInstant,

    // Legacy EMA success ratio (backward compat, still updated)
    pub success_ratio:         f32,             // EMA α=0.1, init 0.5

    // Latency
    pub rolling_latency_avg_ms: f64,            // EMA, init 0.0

    // Garbage flag (set by Wilson Score)
    pub garbage:               bool,

    // NEW v0.2: Bayesian Beta distribution parameters
    pub success_count:         u64,             // α (successes)
    pub failure_count:         u64,             // β (failures)
}

// Derived methods:
impl HealthStatus {
    pub fn alpha()               -> f64  // success_count + 1  (Laplace smoothing)
    pub fn beta_param()          -> f64  // failure_count + 1
    pub fn bayesian_competence() -> f64  // α/(α+β) posterior mean
    pub fn wilson_lower()        -> f64  // Wilson Score 95% CI lower bound
    pub fn beta_variance()       -> f64  // αβ / ((α+β)²(α+β+1))
    pub fn stability_score()     -> f64  // 1 - normalised std-dev of Beta
}
```

#### `reducers::dispatch` (v0.2 changes)

- **`success_count` / `failure_count`** incremented on every
  `CompletionFinished`, `ModelFailed`, `ProbeUpdated`, `ProbeFailed`
- **Garbage detection** upgraded from simple EMA threshold to
  Wilson Score lower bound — see `docs/ROUTING.md` for details
- EMA `success_ratio` still maintained for backward-compatible ledger replay

#### `router::RouterEngine` (v0.2 additions)

```rust
// Deterministic (unchanged)
pub fn select(&self, state: &State, request: &RouteRequest) -> RoutingDecision

// NEW: Thompson Sampling (probabilistic)
pub fn select_thompson(&self, state: &State, request: &RouteRequest) -> RoutingDecision
```

`select_thompson` uses a Xorshift64 PRNG seeded from the ledger sequence,
samples Beta(α, β) via the Gamma ratio method (Marsaglia-Tsang), and
combines the sample with load and capability factors.

---

### `omrp-runtime` modules (v0.2)

| Module | Purpose |
|--------|---------|
| `main.rs` | CLI entry point: route, serve, models, status, best, dashboard, init |
| `config.rs` | TOML config at `~/.config/omrp/config.toml`; 5-provider built-in defaults |
| `provider.rs` | OpenAI-compatible HTTP clients for OpenRouter, Kilo, Cerebras, Groq, BUW |
| `routing.rs` | Shared helpers: `bootstrap_pipeline`, `select_for_tier`, `tier_from_str` |
| `server.rs` | Legacy tiny_http proxy (kept for `--rtk`/`--caveman` flags) |
| `web_server.rs` | Axum web server: REST API + SPA + LLM proxy passthrough |
| `auth.rs` | Argon2id passwords, HS256 JWT, axum `FromRequestParts` extractor |
| `db.rs` | SQLite CRUD layer (11 tables) via rusqlite |
| `keys.rs` | File-based API key store (used by legacy server.rs) |
| `proxy.rs` | ProxyScrape fetch, JSON + text ingest, DB upsert |
| `spa.html` | Cyberpunk enterprise SPA (admin + user dashboards, onboarding wizard) |
| `dashboard.rs` | Ratatui TUI live dashboard |

---

## Core Invariants

| Invariant | Description |
|-----------|-------------|
| **No wall-clock time in reducers** | `SystemTime` / `Instant` forbidden inside `dispatch`; time tracked via `SequencedInstant` |
| **Single mutation path** | All state changes flow through `dispatch(&mut State, &Event)` |
| **Replay safety** | `replay(events) == replay(events)` for any event sequence |
| **Ledger append-only** | `LedgerStore` has no remove/update/truncate methods |
| **Reducer never fails** | `dispatch` returns `()`; unknown-model events no-op silently |
| **Thompson randomness is external** | `select()` is pure; `select_thompson()` is explicitly probabilistic |

---

## File Map

```
omrp/
├── Cargo.toml                         workspace root (v0.2.0)
├── Cargo.lock
├── .gitignore
├── README.md
├── TASKS.md                           Phase roadmap
│
├── .github/workflows/ci.yml           GitHub Actions: build + test + clippy
│
├── crates/
│   ├── omrp-types/src/
│   │   ├── model.rs                   Model, ModelId, ModelCapabilities
│   │   ├── task.rs                    TaskType, RouteRequest
│   │   ├── routing.rs                 RoutingDecision, ScoreFactor, RoutingCache
│   │   └── time.rs                    SequencedInstant, Clock
│   │
│   ├── omrp-events/src/
│   │   ├── event.rs                   Event (14 variants), ModelSource
│   │   ├── error.rs                   ErrorKind, ProviderError
│   │   └── validate.rs                validate(), ValidationError
│   │
│   ├── omrp-core/
│   │   ├── tests/determinism.rs       integration: replay identity, fuzz
│   │   └── src/
│   │       ├── state.rs               State, HealthStatus (+ α/β v0.2), Diagnostics
│   │       ├── reducers.rs            dispatch(), EMA, Wilson Score garbage detection
│   │       ├── pipeline.rs            EventPipeline, ProjectionView
│   │       ├── ledger.rs              LedgerStore, LedgerEntry, SHA-256 chain
│   │       ├── scorer.rs              Scorer, ScoringWeights, Bayesian factors
│   │       ├── router.rs              RouterEngine: select() + select_thompson()
│   │       ├── caveman.rs             Prompt compression
│   │       ├── classifier.rs          Prompt tier classifier
│   │       └── rtk/                   Tool output compression (RTK)
│   │
│   └── omrp-runtime/src/
│       ├── main.rs                    CLI dispatcher
│       ├── config.rs                  TOML config, 5-provider defaults
│       ├── provider.rs                HTTP clients (OpenRouter/Kilo/Cerebras/Groq/BUW)
│       ├── routing.rs                 bootstrap_pipeline, select_for_tier helpers
│       ├── server.rs                  Legacy tiny_http proxy
│       ├── web_server.rs              Axum server (REST API + SPA)
│       ├── auth.rs                    JWT + Argon2id auth middleware
│       ├── db.rs                      SQLite 11-table CRUD layer
│       ├── keys.rs                    File-based API key store
│       ├── proxy.rs                   ProxyScrape proxy pool
│       ├── spa.html                   Cyberpunk enterprise SPA
│       └── dashboard.rs               TUI (ratatui)
│
└── docs/
    ├── ARCHITECTURE.md                ← this file
    ├── EVENTS.md                      event catalogue, state effects, validation
    ├── ROUTING.md                     Bayesian scoring, Thompson Sampling, Wilson Score
    ├── WEB.md                         web server, REST API reference, auth flow
    ├── DATABASE.md                    SQLite schema, table descriptions, CRUD patterns
    └── PROVIDERS.md                   provider setup (5 providers + BUW)
```
