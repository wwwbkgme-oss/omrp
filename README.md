# OMRP — Open Model Routing Protocol

> **Status:** Phase 1 — Kernel Bootstrapping (in progress)  
> **Architecture:** Event-sourced, deterministic, replay-safe LLM routing engine

## What Makes OMRP Special

### 🔒 **Deterministic Routing**
Unlike traditional LLM routers that use random selection or hidden state, OMRP guarantees:
- **Same ledger + same request → same routing decision**
- No randomness in scoring or selection
- No hidden external state in reducers
- Deterministic tiebreaking (lexicographic by model_id)

### 📜 **Tamper-Evident Ledger**
Every event is cryptographically chained:
- SHA-256 checksums link each entry to the previous
- Any modification is immediately detectable
- Append-only storage prevents data loss
- Full replay capability for recovery

### ⚡ **BKG-FMR Scoring Engine**
Best Known Garbage-Free Models Router:
- **5-factor scoring**: health (0.35), latency (0.20), success_rate (0.25), stability (0.10), load (0.10)
- Automatically excludes "garbage" models (too many failures)
- Window-based metrics (deterministic across replays)
- Capability matching bonus

### 🔄 **Event-Sourced Architecture**
Three-machine architecture for reliability:
1. **Ledger Machine** — Immutable source of truth (append-only)
2. **Reducer Machine** — Pure state projection (no IO, no randomness)
3. **Scheduler Machine** — Deterministic routing decisions

## Architecture

```
┌──────────────────────────────────────┐
│          Dioxus Dashboard           │
│   (Projection-only UI Layer)        │
└──────────────┬──────────────────────┘
               │ read-only
┌──────────────▼──────────────────────┐
│          OMRP Core Engine (Rust)     │
│                                      │
│  ┌────────────────────────────────┐│
│  │        Event Pipeline           ││
│  │  process(Event) → Ledger write  ││
│  └──────────┬─────────────────────┘│
│             │                      │
│  ┌──────────▼─────────────────────┐│
│  │    State Transition Layer       ││
│  │  StateTransitionFn<E> only      ││
│  └──────────┬─────────────────────┘│
│             │                      │
│  ┌──────────▼─────────────────────┐│
│  │    BKG-FMR Routing Engine       ││
│  │  - select model                  ││
│  │  - fallback chain                ││
│  │  - scoring                       ││
│  └──────────┬─────────────────────┘│
│             │                      │
│  ┌──────────▼─────────────────────┐│
│  │    Provider Adapters             ││
│  │  OpenRouter / Qwen / Kilo        ││
│  └──────────────────────────────────┘│
└──────────────────────────────────────┘
```

## Core Features

| Feature | Description |
|---------|-------------|
| **Event Pipeline** | Validate → Route → Persist → Apply → Project |
| **Deterministic Replay** | Same events always produce identical state |
| **Backpressure** | Inflight tracking prevents model overload |
| **Decision Trace** | Full score breakdown for explainability |
| **Circuit Breaker** | Fast-fail for failing providers |
| **Retry Policy** | Configurable retry with exponential backoff |

## Crates

- **omrp-types** — Shared types (Model, Clock, TaskType, RoutingDecision, SequencedInstant)
- **omrp-events** — Event definitions (Event enum, ErrorKind, ValidationError)
- **omrp-core** — Engine (State, reducers, pipeline, scorer, router, ledger, invariants)
- **omrp-runtime** — CLI binary entry point

## Quick Start

```bash
# Build all crates
cargo build

# Run tests
cargo test

# Run CLI
cargo run -- models
cargo run -- status
cargo run -- best code
```

## Phase 1 Status

- [x] omrp-types crate — SequencedInstant, Clock, Model, TaskType, RoutingDecision
- [ ] omrp-events crate — Event enum, ErrorKind, validation
- [ ] omrp-core — State + reducers + pipeline + scorer + router + ledger
- [ ] omrp-runtime CLI binary

## License

MIT