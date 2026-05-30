# OMRP Architecture

## Three-Machine Architecture

```
┌──────────────────────────────────────┐
│           1. Ledger Machine            │
│           (immutable, append-only)     │
│                                        │
│   Event → validate → checksum → persist│
│                                        │
│   Source of Truth. Nothing else stores │
│   events.                              │
└──────────────────────────────────────┘
                │ events (ordered slice)
                ▼
┌──────────────────────────────────────┐
│           2. Reducer Machine           │
│           (deterministic, pure)        │
│                                        │
│   State = reduce(events)                 │
│   No IO. No randomness. No SystemTime.  │
│                                        │
│   State contains NO ledger reference.    │
└──────────────────────────────────────┘
                │ state (immutable snapshot)
                ▼
┌──────────────────────────────────────┐
│           3. Scheduler Machine         │
│           (BKG-FMR routing decision)   │
│                                        │
│   decision = select(state, request)    │
│   Pure function. Deterministic.        │
│                                        │
│   Returns: decision + trace + fallback   │
└──────────────────────────────────────┘
```

## Core Invariants

### Single Source of Truth
- The ledger is the **only** persistent truth
- State is always a derived projection
- No dual truth (state + ledger)

### Single Mutation Path
- All state changes go through `dispatch(&mut State, &Event)`
- No direct field mutation anywhere else
- Reducers are pure functions

### Replay Safety
- Same ledger + same reducers = identical state
- No randomness in reducers
- No hidden external state
- No SystemTime::now() in reducers

### Deterministic Time
- `SequencedInstant` replaces wall-clock time
- Single `Clock` instance per daemon
- Logical time derived from sequence position

## Module Structure

```
crates/
├── omrp-types/
│   ├── model.rs        # Model, ModelId, ModelCapabilities
│   ├── task.rs         # TaskType, RouteRequest
│   ├── routing.rs      # RoutingDecision, ScoreFactor, RoutingReason
│   └── time.rs         # SequencedInstant, Clock
│
├── omrp-events/
│   ├── event.rs        # Event enum (14 variants)
│   ├── error.rs        # ErrorKind, ProviderError
│   └── validate.rs     # Event validation
│
└── omrp-core/
    ├── state.rs        # State, HealthStatus, Diagnostics
    ├── reducers.rs     # dispatch() function
    ├── pipeline.rs     # EventPipeline
    ├── scorer.rs       # BKG-FMR scoring
    ├── router.rs       # RouterEngine
    ├── ledger.rs       # LedgerStore
    └── invariants.rs   # debug_assert! checks
```