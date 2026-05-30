# OMRP — Open Model Routing Protocol

> **Status:** Phase 1 complete — kernel bootstrapped and usable  
> **Architecture:** Event-sourced, deterministic, replay-safe LLM routing engine

```
cargo run -p omrp-runtime -- best code
```

---

## What is OMRP?

OMRP is a local LLM routing daemon that selects the best available model for each request. Every routing decision is **deterministic**: given the same event history and the same request, OMRP always picks the same model. There is no randomness, no hidden state, and no wall-clock time in any reducer.

## Key Properties

### Deterministic Routing
- Same ledger + same request → same routing decision, always
- No `rand`, no `SystemTime::now()`, no external reads inside reducers
- Deterministic tiebreaking: lexicographic by `model_id`

### Tamper-Evident Ledger
- Every event is SHA-256 chained to the previous one
- Any modification to any entry is immediately detectable
- Append-only: history cannot be rewritten
- Full replay from any checkpoint

### BKG-FMR Scoring Engine
*Best Known Garbage-Free Models Router*
- 5-factor scoring: `health (0.35) + latency (0.20) + success_rate (0.25) + stability (0.10) + load (0.10)`
- EMA-based success ratio: degrades continuously under failures
- Garbage exclusion: models below threshold are removed from routing
- Capability matching bonus for task-specific suitability

### Event-Sourced Architecture
```
Events → Validate → LedgerStore → dispatch() → State → RouterEngine → RoutingDecision
```
Three logical machines, one data flow:
1. **Ledger Machine** — append-only, SHA-256 chained source of truth
2. **Reducer Machine** — pure `dispatch(&mut State, &Event)`, no IO
3. **Scheduler Machine** — `select(state, request)`, pure function

---

## Quick Start

```bash
# Build
cargo build

# Run all tests (34 tests, 0 failures)
cargo test

# CLI commands
cargo run -p omrp-runtime -- models           # list registered models
cargo run -p omrp-runtime -- status           # health + routing scores
cargo run -p omrp-runtime -- best code        # best model for coding tasks
cargo run -p omrp-runtime -- best reasoning   # best model for reasoning
cargo run -p omrp-runtime -- best vision      # best model for vision tasks
```

### Example output

```
$ cargo run -p omrp-runtime -- best code
Best model for "code": qwen/qwen-2-5-72b
Score: 1.076

Score breakdown:
  health          value=1.000  weight=0.35  contribution=0.350
  latency         value=1.038  weight=0.20  contribution=0.208
  success_rate    value=0.672  weight=0.25  contribution=0.168
  stability       value=1.000  weight=0.10  contribution=0.100
  load            value=1.000  weight=0.10  contribution=0.100

Fallback chain: openrouter/claude-3-5-sonnet → openrouter/gpt-4o
```

---

## Architecture

```
┌──────────────────────────────────────────────────────────┐
│                  omrp-runtime (CLI)                       │
│  models | status | best <task>                            │
└───────────────────────┬──────────────────────────────────┘
                        │ uses
┌───────────────────────▼──────────────────────────────────┐
│                   omrp-core                               │
│                                                           │
│  EventPipeline ──► LedgerStore (SHA-256 chain)           │
│       │                                                   │
│       ▼                                                   │
│  dispatch(&mut State, &Event)  ← pure reducer             │
│       │                                                   │
│       ▼                                                   │
│  State { models, health, inflight, routing_cache, diag }  │
│       │                                                   │
│       ▼                                                   │
│  RouterEngine::select(state, request)  ← pure function    │
│       │                                                   │
│       ▼                                                   │
│  RoutingDecision { model, score, factors, fallback_chain } │
└──────────────────────────────────────────────────────────┘
        depends on
┌───────────────────────────────────────────────────────────┐
│  omrp-events          │  omrp-types                       │
│  Event enum           │  Model, ModelCapabilities         │
│  ErrorKind            │  TaskType, RouteRequest           │
│  ValidationError      │  RoutingDecision, ScoreFactor     │
│  validate()           │  SequencedInstant, Clock          │
└───────────────────────────────────────────────────────────┘
```

---

## Crates

| Crate | Description |
|-------|-------------|
| `omrp-types` | Shared types: `Model`, `TaskType`, `RouteRequest`, `RoutingDecision`, `SequencedInstant`, `Clock` |
| `omrp-events` | Event definitions, `ErrorKind`, `ValidationError`, `validate()` |
| `omrp-core` | Engine: `State`, `dispatch()`, `EventPipeline`, `LedgerStore`, `Scorer`, `RouterEngine` |
| `omrp-runtime` | CLI binary (`omrp models \| status \| best <task>`) |

---

## Phase 1 Status

- [x] `omrp-types` — `SequencedInstant`, `Clock`, `Model`, `TaskType`, `RoutingDecision`
- [x] `omrp-events` — `Event` enum (14 variants), `ErrorKind`, `validate()`
- [x] `omrp-core/state` — `State`, `HealthStatus`, `Diagnostics`, `RoutingCache`
- [x] `omrp-core/reducers` — `dispatch()`, EMA success ratio, garbage detection
- [x] `omrp-core/pipeline` — `EventPipeline` with validate → persist → apply → replay
- [x] `omrp-core/scorer` — BKG-FMR 5-factor scoring with capability bonus
- [x] `omrp-core/router` — `RouterEngine` with deterministic selection + fallback chain
- [x] `omrp-core/ledger` — `LedgerStore` with SHA-256 chaining, JSON Lines persist/load
- [x] `omrp-runtime` — CLI: `models`, `status`, `best <task>`
- [x] Integration tests — determinism, replay identity, fuzz (34 tests total)

**Phase 2 tasks** → see [`TASKS.md`](TASKS.md)

---

## Documentation

| File | Description |
|------|-------------|
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | Crate graph, data flow, invariants, module details |
| [`docs/EVENTS.md`](docs/EVENTS.md) | All 14 event variants, state effects, validation rules |
| [`docs/ROUTING.md`](docs/ROUTING.md) | BKG-FMR scoring algorithm, garbage detection, fallback chain |
| [`TASKS.md`](TASKS.md) | Phase 2 and beyond: next implementation tasks |

---

## License

MIT
