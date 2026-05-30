# OMRP Architecture

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

## Data Flow

```
External input
      │
      ▼
 EventPipeline::process(event)
      │
      ├─ 1. validate(&event)          ← omrp-events::validate
      │         │
      │         └─ Err → PipelineError::Validation (caller handles)
      │
      ├─ 2. ledger.append(event)      ← LedgerStore (in-memory, Phase 1)
      │         │
      │         └─ computes SHA-256 checksum, chains to previous entry
      │
      └─ 3. dispatch(&mut state, &event) ← pure reducer, no IO
                │
                └─ mutates State in place

      State
        │
        └─ RouterEngine::select(&state, &request)
                │
                └─ RoutingDecision { selected_model, score, factors,
                                     fallback_chain, timestamp }
```

No step may read wall-clock time, generate randomness, or perform IO.
The reducer (`dispatch`) and the router (`select`) are pure functions.

---

## Three-Machine Model

OMRP separates concerns into three logical machines.
In Phase 1 all three run in the same process; in later phases they may
be separated.

```
┌─────────────────────────────────────────────────────────┐
│  1. Ledger Machine                                       │
│                                                          │
│  Source of truth. Append-only. Never mutated.            │
│  Each entry is SHA-256 chained to the previous one.      │
│  Tampering anywhere in the chain is immediately          │
│  detectable by verify_chain().                           │
│                                                          │
│  LedgerStore { entries: Vec<LedgerEntry>, ... }         │
│  LedgerEntry { seq, logical_time, event, checksum }      │
└────────────────────────┬────────────────────────────────┘
                         │ ordered slice of Events
                         ▼
┌─────────────────────────────────────────────────────────┐
│  2. Reducer Machine                                      │
│                                                          │
│  Derives State from the event stream.                    │
│  Pure: no IO, no randomness, no SystemTime.              │
│  Determinism guarantee: same events → identical State.   │
│                                                          │
│  fn dispatch(state: &mut State, event: &Event)           │
│  State { models, health, inflight,                       │
│          routing_cache, diagnostics }                    │
└────────────────────────┬────────────────────────────────┘
                         │ immutable State snapshot
                         ▼
┌─────────────────────────────────────────────────────────┐
│  3. Scheduler Machine                                    │
│                                                          │
│  Selects the best model for a task request.              │
│  Pure: no IO, no side effects, same inputs → same        │
│  output always.                                          │
│                                                          │
│  fn select(&self, state: &State,                         │
│            request: &RouteRequest) -> RoutingDecision    │
└─────────────────────────────────────────────────────────┘
```

---

## Crate Details

### `omrp-types`

Shared vocabulary. No dependencies other than `serde` and `serde_json`.

| Module | Key types |
|--------|-----------|
| `time` | `SequencedInstant { seq: u64, logical_time: u64 }`, `Clock` |
| `model` | `Model { id, provider, capabilities }`, `ModelCapabilities`, `ModelId = String` |
| `task` | `TaskType` (Code/Reasoning/Chat/Vision/Analysis), `RouteRequest` |
| `routing` | `RoutingDecision`, `ModelScore`, `ScoreFactor`, `RoutingCache`, `FallbackEntry` |

**`SequencedInstant`** replaces wall-clock time everywhere inside reducers.
It is deterministic (derived from the count of processed events), totally
ordered (implements `Ord`), and replay-safe.

```rust
pub struct SequencedInstant {
    pub seq: u64,          // monotonically increasing, never resets
    pub logical_time: u64, // same as seq in Phase 1
}

impl State {
    pub fn current_time(&self) -> SequencedInstant {
        SequencedInstant {
            seq: self.diagnostics.total_completions
               + self.diagnostics.total_failures + 1,
            logical_time: self.diagnostics.total_completions
                        + self.diagnostics.total_failures,
        }
    }
}
```

---

### `omrp-events`

Event definitions and validation. Depends on `omrp-types`.

| Module | Purpose |
|--------|---------|
| `event` | `Event` enum (14 variants), `ModelSource` |
| `error` | `ErrorKind`, `ProviderError` |
| `validate` | `validate(&Event) -> Result<(), ValidationError>` |

The `Event` enum is the only type that flows into the ledger and the
reducer. Nothing else is persisted.

---

### `omrp-core`

The engine. Depends on both `omrp-types` and `omrp-events`.

| Module | Key type / function | Role |
|--------|---------------------|------|
| `state` | `State`, `HealthStatus`, `Diagnostics` | Projected in-memory state |
| `reducers` | `dispatch(&mut State, &Event)` | Pure state transition |
| `pipeline` | `EventPipeline` | Orchestrates validate → persist → apply |
| `ledger` | `LedgerStore`, `LedgerEntry` | Tamper-evident append-only log |
| `scorer` | `Scorer`, `ScoringWeights` | BKG-FMR 5-factor scoring |
| `router` | `RouterEngine` | Deterministic model selection |

#### `state::State`

```
State
├── models: Vec<Model>                  — registered models
├── health: HashMap<ModelId, HealthStatus>
│       ├── last_success: SequencedInstant
│       ├── last_failure: SequencedInstant
│       ├── success_ratio: f32          — EMA (α=0.1), init 0.5
│       ├── rolling_latency_avg_ms: f64 — EMA (window 10–20)
│       └── garbage: bool               — excluded from routing
├── inflight: HashMap<ModelId, u32>     — current in-flight requests
├── routing_cache: RoutingCache
│       ├── last_selected: Option<RoutingCacheEntry>
│       └── last_fallback: Option<FallbackEntry>
└── diagnostics: Diagnostics
        ├── total_completions: u64
        ├── total_failures: u64
        ├── total_fallbacks: u64
        └── total_degradations: u64
```

`State` is fully serialisable (`serde::Serialize + Deserialize`) so that
replay identity can be asserted with `serde_json::to_value`.

#### `reducers::dispatch`

Single-dispatch table for all events. Signature:

```rust
pub fn dispatch(state: &mut State, event: &Event)
```

Returns `()` — errors are not surfaced; unknown-model events silently
skip the mutation. This is intentional: the ledger is the source of
truth; reducers never fail.

**EMA update rule** (used for `success_ratio` and `rolling_latency_avg_ms`):

```
success_ratio' = success_ratio × (1 − α) + outcome × α
    where α = 0.1, outcome ∈ {0.0, 1.0}

rolling_latency' = latency × (1 − 1/W) + new_ms × (1/W)
    where W = 10 for ProbeUpdated, 20 for CompletionFinished
```

**Garbage detection** (computed after every `CompletionFinished`,
`ModelFailed`, `ReportReceived`):

```rust
fn is_garbage(health: &HealthStatus) -> bool {
    health.success_ratio < 0.2
        && health.last_failure > health.last_success
}
```

Starting from the neutral prior `success_ratio = 0.5`, a model reaches
`< 0.2` after approximately 11 consecutive failures with no successes
(or fewer if it had prior failures).

#### `pipeline::EventPipeline`

```rust
pub struct EventPipeline {
    state: ProjectionView<State>,  // Arc<RwLock<State>>
    event_log: Vec<Event>,
}
```

Processing order:
1. `validate(&event)` — reject invalid events before they touch the ledger
2. `event_log.push(event.clone())` — in-memory log (replaces LedgerStore in Phase 1)
3. `dispatch(&mut state, &event)` — mutate projected state

`verify_replay()` replays the entire log from scratch and asserts equality
with the live state via `serde_json::to_value` comparison.

> **Phase 2**: `event_log` will be replaced by `LedgerStore::append`.

#### `ledger::LedgerStore`

```rust
pub struct LedgerStore {
    path: PathBuf,
    entries: Vec<LedgerEntry>,
    last_checksum: [u8; 32],
}
```

Each entry's checksum is computed as:

```
checksum = SHA-256(
    previous_checksum   // [0u8; 32] for genesis
  ‖ seq.to_le_bytes()
  ‖ logical_time.to_le_bytes()
  ‖ serde_json::to_vec(&event)
)
```

The checksum is serialised as a 64-character lowercase hex string in
JSON Lines files. `load()` calls `verify_chain()` before returning;
any integrity violation returns `Err(ChainIntegrityViolation)`.

#### `scorer::Scorer`

```
score(model, request) =
    health_score    × 0.35
  + latency_score   × 0.20
  + success_rate    × 0.25
  + stability_score × 0.10
  + load_score      × 0.10
  + capability_bonus          ← not weighted, flat bonus
```

| Sub-score | Formula |
|-----------|---------|
| `health_score` | `0.0` if garbage, `0.5` if no data, `1.0` if healthy |
| `latency_score` | `max(0, 1 − (avg_ms − 500) / 9500)` — 500 ms = 1.0, 10 s = 0.0 |
| `success_rate` | `0.5` if no data, else `health.success_ratio as f64` |
| `stability_score` | `1.0` if `last_success > last_failure`, else `0.0` |
| `load_score` | `max(0, 1 − inflight / max_inflight)` |
| `capability_bonus` | `+0.15` task match, `+0.10` vision, `+0.10` tool use, `+0.05` ctx window |

#### `router::RouterEngine`

```rust
pub fn select(&self, state: &State, request: &RouteRequest) -> RoutingDecision
```

1. Filter out garbage models
2. Score every remaining model
3. Sort descending by score; tiebreak ascending by `model_id` (lexicographic)
4. Return the first model as `selected_model`; the full sorted list as `fallback_chain`

The sort is fully deterministic: `f64::partial_cmp` with
`Ordering::Equal` as the NaN fallback, then `str::cmp`.

---

## Core Invariants

### No Wall-Clock Time in Reducers
`SystemTime`, `Instant`, `chrono::Utc::now()` are forbidden inside
`dispatch`. Time is tracked exclusively via `State::current_time()` which
derives `SequencedInstant` from the deterministic `diagnostics` counters.

### Single Mutation Path
All state changes flow through `dispatch(&mut State, &Event)`.
No module outside `reducers.rs` may mutate `State` fields directly.

### Replay Safety
```
replay(events) == replay(events)   // always, for any event sequence
```
Verified by `EventPipeline::verify_replay()` and the integration tests
in `crates/omrp-core/tests/determinism.rs`.

### Ledger Append-Only
`LedgerStore` has no `remove`, `update`, or `truncate` methods.
The only mutating operation is `append`, which extends the chain.

### Reducer Never Fails
`dispatch` returns `()`. If an event references an unknown model, the
reducer silently no-ops. This keeps the pipeline infallible after
validation has already passed.

---

## File Map

```
bkg-flr/
├── Cargo.toml                     workspace root (virtual manifest)
├── Cargo.lock
├── .gitignore
├── README.md
├── TASKS.md                       Phase 2+ task list
│
├── crates/
│   ├── omrp-types/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── model.rs           Model, ModelId, ModelCapabilities
│   │       ├── task.rs            TaskType, RouteRequest
│   │       ├── routing.rs         RoutingDecision, ScoreFactor, RoutingCache
│   │       └── time.rs            SequencedInstant, Clock
│   │
│   ├── omrp-events/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── event.rs           Event (14 variants), ModelSource
│   │       ├── error.rs           ErrorKind, ProviderError
│   │       └── validate.rs        validate(), ValidationError
│   │
│   ├── omrp-core/
│   │   ├── tests/
│   │   │   └── determinism.rs     integration: replay identity, fuzz
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── state.rs           State, HealthStatus, Diagnostics
│   │       ├── reducers.rs        dispatch(), EMA helpers, garbage detection
│   │       ├── pipeline.rs        EventPipeline, ProjectionView
│   │       ├── ledger.rs          LedgerStore, LedgerEntry, hex_bytes serde
│   │       ├── scorer.rs          Scorer, ScoringWeights
│   │       └── router.rs          RouterEngine
│   │
│   └── omrp-runtime/
│       └── src/
│           └── main.rs            CLI: models | status | best <task>
│
└── docs/
    ├── ARCHITECTURE.md            ← this file
    ├── EVENTS.md                  event catalogue, state effects, validation
    └── ROUTING.md                 scoring algorithm, garbage detection
```
