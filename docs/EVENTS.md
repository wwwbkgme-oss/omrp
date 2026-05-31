# OMRP Event Catalogue

Events are the only mechanism that mutates state in OMRP.
Every event is validated before it enters the pipeline, chained into
the `LedgerStore`, and then applied to `State` via `dispatch()`.

---

## Event Enum

```rust
pub enum Event {
    // Lifecycle
    DaemonStarted { version: String },
    DaemonStopped { reason: String },

    // Model Discovery
    ModelAdded { model: Model, source: ModelSource },
    ModelRemoved { model_id: ModelId, reason: String },
    ConfigReloaded { source: String },

    // Routing
    ModelSelected { model_id, request, score, reason },
    FallbackTriggered { from, to, cause },
    DegradeModeEnabled { model_id, reason },

    // Completion
    CompletionRequested { model_id, task_type, prompt_tokens },
    CompletionFinished { model_id, latency_ms, tokens_used, success },
    ModelFailed { model_id, error: ErrorKind },

    // Telemetry
    ProbeUpdated { model_id, health: f32, latency_ms },
    ProbeFailed { model_id, error: String },
    ReportReceived { model_id, success, latency_ms, tokens },
}
```

---

## Event Reference

### Lifecycle

#### `DaemonStarted { version: String }`

Emitted when the OMRP daemon boots.

| Field | Type | Constraint |
|-------|------|-----------|
| `version` | `String` | non-empty after trim |

**State effect:** resets `state.diagnostics` to `Diagnostics::default()`.
All counters go to zero; existing model data is preserved.

---

#### `DaemonStopped { reason: String }`

Emitted on graceful shutdown.

| Field | Type | Constraint |
|-------|------|-----------|
| `reason` | `String` | non-empty after trim |

**State effect:** none. Recorded in the ledger for audit purposes.

---

### Model Discovery

#### `ModelAdded { model: Model, source: ModelSource }`

Registers a new model. **Idempotent**: a second `ModelAdded` for the same
`model.id` is silently ignored.

| Field | Type | Notes |
|-------|------|-------|
| `model` | `Model` | See `Model` schema below |
| `source` | `ModelSource` | `Bundled`, `LocalConfig`, `UserContributed`, `AutoDiscovered` |

**Validation:** always passes (Model-level validation is left to the caller).

**State effect:**
- Appends `model` to `state.models`
- Inserts `HealthStatus::new()` into `state.health` (success_ratio = 0.5, all timestamps = EPOCH)
- Inserts `0` into `state.inflight`

---

#### `ModelRemoved { model_id: ModelId, reason: String }`

Removes a model from the routing pool.

| Field | Type | Constraint |
|-------|------|-----------|
| `model_id` | `String` | (no empty check in current validation) |
| `reason` | `String` | non-empty after trim |

**State effect:** removes the model from `state.models`, `state.health`,
and `state.inflight`. If the model does not exist, this is a no-op.

---

#### `ConfigReloaded { source: String }`

Signals that a configuration file was reloaded.

| Field | Type | Constraint |
|-------|------|-----------|
| `source` | `String` | non-empty after trim (file path, URL, etc.) |

**State effect:** none. Recorded for traceability.

---

### Routing

#### `ModelSelected { model_id, request, score, reason }`

Emitted each time the `RouterEngine` selects a model for a request.

| Field | Type | Constraint |
|-------|------|-----------|
| `model_id` | `ModelId` | — |
| `request` | `RouteRequest` | full original request |
| `score` | `f64` | must be `>= 0.0` |
| `reason` | `RoutingReason` | `TopScore`, `Fallback`, `UserPreference`, etc. |

**State effect:**
- Sets `state.routing_cache.last_selected` to a new `RoutingCacheEntry`
  recording `model_id`, `score`, and `selected_at = state.current_time()`
- Increments `state.inflight[model_id]` by 1

---

#### `FallbackTriggered { from: ModelId, to: ModelId, cause: String }`

Emitted when a completion is retried on a different model.

| Field | Type | Constraint |
|-------|------|-----------|
| `from` | `ModelId` | original model that failed |
| `to` | `ModelId` | replacement model |
| `cause` | `String` | non-empty after trim |

**State effect:**
- Sets `state.routing_cache.last_fallback` to a new `FallbackEntry`
- Increments `state.diagnostics.total_fallbacks`

---

#### `DegradeModeEnabled { model_id: ModelId, reason: String }`

Emitted when a model enters degraded operation mode.

| Field | Type | Constraint |
|-------|------|-----------|
| `model_id` | `ModelId` | — |
| `reason` | `String` | non-empty after trim |

**State effect:** increments `state.diagnostics.total_degradations`.

---

### Completion

#### `CompletionRequested { model_id, task_type, prompt_tokens }`

Emitted when a completion is dispatched to a provider.

| Field | Type | Constraint |
|-------|------|-----------|
| `model_id` | `ModelId` | — |
| `task_type` | `TaskType` | `Code`, `Reasoning`, `Chat`, `Vision`, `Analysis` |
| `prompt_tokens` | `u32` | must be `> 0` |

**State effect:** increments `state.inflight[model_id]` by 1.

---

#### `CompletionFinished { model_id, latency_ms, tokens_used, success }`

The primary feedback signal. Updates health metrics and tracks outcomes.

| Field | Type | Constraint |
|-------|------|-----------|
| `model_id` | `ModelId` | — |
| `latency_ms` | `u64` | must be `> 0` |
| `tokens_used` | `u64` | must be `> 0` |
| `success` | `bool` | whether the completion succeeded |

**State effect (success = true):**
- `health.last_success = state.current_time()`
- `health.rolling_latency_avg_ms` updated via EMA (window = 20)
- `health.success_ratio = EMA_update(ratio, 1.0, α=0.1)`
- `health.garbage` rechecked
- `inflight[model_id]` decremented (saturating)
- `diagnostics.total_completions += 1`

**State effect (success = false):**
- `health.last_failure = state.current_time()`
- `health.success_ratio = EMA_update(ratio, 0.0, α=0.1)`
- `health.garbage` rechecked
- `inflight[model_id]` decremented (saturating)
- `diagnostics.total_completions += 1`
- `diagnostics.total_failures += 1`

---

#### `ModelFailed { model_id: ModelId, error: ErrorKind }`

Emitted when a model produces a non-completion failure (e.g., auth error,
rate limit, network failure — as opposed to a failed completion).

| Field | Type | Notes |
|-------|------|-------|
| `model_id` | `ModelId` | — |
| `error` | `ErrorKind` | see `ErrorKind` reference below |

**Validation:** always passes for all `ErrorKind` variants.

**State effect:**
- `health.last_failure = state.current_time()`
- `health.success_ratio = EMA_update(ratio, 0.0, α=0.1)`
- `health.garbage` rechecked
- `diagnostics.total_failures += 1`

---

### Telemetry

#### `ProbeUpdated { model_id, health: f32, latency_ms }`

Emitted by the health probe scheduler when a probe succeeds.

| Field | Type | Constraint |
|-------|------|-----------|
| `model_id` | `ModelId` | — |
| `health` | `f32` | must be in `0.0..=1.0` |
| `latency_ms` | `u64` | must be `> 0` |

**State effect:**
- `health.last_success = state.current_time()`
- `health.rolling_latency_avg_ms` updated via EMA (window = 10)

> Note: `health` (the probe score) is not stored directly; only
> `last_success` and `rolling_latency_avg_ms` are updated.

---

#### `ProbeFailed { model_id: ModelId, error: String }`

Emitted when a health probe fails.

| Field | Type | Constraint |
|-------|------|-----------|
| `model_id` | `ModelId` | — |
| `error` | `String` | non-empty after trim |

**State effect:**
- `health.last_failure = state.current_time()`
- `health.garbage` rechecked

---

#### `ReportReceived { model_id, success, latency_ms, tokens }`

Aggregated report from a monitoring agent or external telemetry source.
Same health update logic as `CompletionFinished` but without inflight
tracking (inflight was not incremented by this path).

| Field | Type | Constraint |
|-------|------|-----------|
| `model_id` | `ModelId` | — |
| `success` | `bool` | — |
| `latency_ms` | `u64` | must be `> 0` |
| `tokens` | `u64` | must be `> 0` |

**State effect:** same as `CompletionFinished` minus the inflight decrement
and diagnostics increment. Updates `last_success`/`last_failure`,
`rolling_latency_avg_ms`, `success_ratio`, and `garbage` flag.

---

## Supporting Types

### `ModelSource`

```rust
pub enum ModelSource {
    Bundled,          // shipped with the binary
    LocalConfig,      // loaded from a local config file
    UserContributed,  // added manually at runtime
    AutoDiscovered,   // found via provider API
}
```

### `ErrorKind`

```rust
pub enum ErrorKind {
    RateLimited { retry_after: Option<u64> }, // retry_after = seconds
    Timeout { timeout_ms: u64 },
    AuthError,
    ModelNotAvailable,
    NetworkError(String),
    InternalError(String),
}
```

### `ProviderError` (internal)

`ProviderError` is a richer internal type used by provider adapters.
It maps to `ErrorKind` via `.kind()`:

| ProviderError | → ErrorKind |
|---------------|------------|
| `Network(s)` | `NetworkError(s)` |
| `Auth(s)` | `AuthError` |
| `RateLimited { retry_after }` | `RateLimited { retry_after }` |
| `ModelNotFound(s)` | `ModelNotAvailable` |
| `Timeout(ms)` | `Timeout { timeout_ms: ms }` |
| `Internal(s)` | `InternalError(s)` |
| `CircuitBreakerOpen` | `InternalError("circuit breaker open")` |

---

## Validation Summary

| Event | Validated field | Rule |
|-------|-----------------|------|
| `DaemonStarted` | `version` | non-empty after trim |
| `DaemonStopped` | `reason` | non-empty after trim |
| `ModelAdded` | — | always passes |
| `ModelRemoved` | `reason` | non-empty after trim |
| `ConfigReloaded` | `source` | non-empty after trim |
| `ModelSelected` | `score` | `>= 0.0` |
| `FallbackTriggered` | `cause` | non-empty after trim |
| `DegradeModeEnabled` | `reason` | non-empty after trim |
| `CompletionRequested` | `prompt_tokens` | `> 0` |
| `CompletionFinished` | `latency_ms`, `tokens_used` | both `> 0` |
| `ModelFailed` | — | always passes |
| `ProbeUpdated` | `health`, `latency_ms` | `0.0..=1.0`, `> 0` |
| `ProbeFailed` | `error` | non-empty after trim |
| `ReportReceived` | `latency_ms`, `tokens` | both `> 0` |

Validation is enforced in `EventPipeline::process` before any event
reaches the ledger or the reducer. An invalid event returns
`Err(PipelineError::Validation(...))` and is not recorded.

---

## Model Schema

```rust
pub struct Model {
    pub id: ModelId,                    // e.g. "openrouter/claude-3-5-sonnet"
    pub provider: String,               // e.g. "openrouter"
    pub capabilities: ModelCapabilities,
}

pub struct ModelCapabilities {
    pub task_suitability: Vec<TaskType>, // which tasks this model is good at
    pub supports_vision: bool,
    pub supports_tool_use: bool,
    pub context_window: u32,             // max tokens
}
```

---

## `HealthStatus` Fields (v0.2)

`HealthStatus` is what the reducer maintains per model and what the
scorer reads. It is never set directly — only `dispatch` writes to it.

```rust
pub struct HealthStatus {
    pub last_success:           SequencedInstant, // EPOCH = never succeeded
    pub last_failure:           SequencedInstant, // EPOCH = never failed
    pub success_ratio:          f32,              // EMA α=0.1, init 0.5 (backward compat)
    pub rolling_latency_avg_ms: f64,              // EMA, init 0.0
    pub garbage:                bool,             // excluded from routing when true

    // v0.2: Bayesian Beta distribution parameters
    pub success_count:          u64,              // α — incremented on success events
    pub failure_count:          u64,              // β — incremented on failure events
}
```

**Bayesian helpers available on `HealthStatus`:**
- `alpha()` — `success_count + 1` (Laplace-smoothed α)
- `beta_param()` — `failure_count + 1` (Laplace-smoothed β)
- `bayesian_competence()` — `α/(α+β)` posterior mean
- `wilson_lower()` — Wilson Score 95% lower bound (used for garbage detection)
- `beta_variance()` — `αβ/((α+β)²(α+β+1))` variance of the Beta distribution
- `stability_score()` — `1 − sqrt(variance)/0.5` normalised to `[0,1]`

**`success_count` and `failure_count`** are updated on:
`CompletionFinished` (success or failure), `ModelFailed`, `ProbeUpdated` (success),
`ProbeFailed` (failure), `ReportReceived`.

**Garbage detection** uses the Wilson Score lower bound (see `docs/ROUTING.md`
for full details). The flag is re-evaluated on every state-mutating event.
