# BKG-FMR Routing Engine

**BKG-FMR** = Best Known Garbage-Free Models Router.

The routing engine selects the optimal LLM for each request using a
5-factor weighted score, a capability match bonus, deterministic
tiebreaking, and a pre-computed fallback chain.

---

## Quick Reference

```
omrp best code        → routes a code request, prints score breakdown
omrp best reasoning   → routes a reasoning request
omrp best vision      → routes a vision request
omrp status           → shows all models scored for the default (chat) request
```

---

## Routing Algorithm

`RouterEngine::select(state, request)` runs these steps in order:

```
1. filter      remove all models with health.garbage == true
2. score       compute score(model, health, inflight, request) for each
3. sort        descending by score; ascending by model_id as tiebreaker
4. return      first entry = selected_model
               full sorted list = fallback_chain
```

The function is pure: no IO, no side effects, same inputs → same output.

---

## Scoring Formula

```
score(model, request) =
    health_score(model)    × 0.35
  + latency_score(model)   × 0.20
  + success_rate(model)    × 0.25
  + stability_score(model) × 0.10
  + load_score(model)      × 0.10
  + capability_bonus(model, request)    ← not weighted; flat additive bonus
```

### Default Weights

| Factor | Weight | Rationale |
|--------|--------|-----------|
| `health` | 0.35 | Heaviest factor — a failing model must be avoided above all else |
| `success_rate` | 0.25 | Second-heaviest — EMA over outcomes is the most reliable long-term signal |
| `latency` | 0.20 | User-facing quality signal — high latency degrades UX |
| `stability` | 0.10 | Rewards models whose last event was a success |
| `load` | 0.10 | Prevents overloading a single model |

Weights can be overridden by passing a custom `ScoringWeights` to
`Scorer::new(weights)` and wrapping with `RouterEngine::new(scorer)`.

---

## Factor Definitions

### `health_score`

```rust
fn health_score(health: &HealthStatus) -> f64 {
    if health.garbage          { return 0.0; }  // excluded pre-filter, but guarded
    if health.last_success == EPOCH { return 0.5; }  // no data → neutral prior
    1.0
}
```

| Condition | Score |
|-----------|-------|
| `garbage == true` | 0.0 (model pre-filtered anyway) |
| `last_success == EPOCH` (never seen) | 0.5 (neutral prior) |
| `last_success` set at least once | 1.0 |

---

### `latency_score`

```rust
fn latency_score(health: &HealthStatus) -> f64 {
    if health.rolling_latency_avg_ms <= 0.0 { return 0.5; }  // no data → neutral
    f64::max(0.0, 1.0 - (avg_ms - 500.0) / 9500.0)
}
```

Linear scale: **500 ms → 1.0**, **10 000 ms → 0.0**, clamped at 0.

| Avg latency | Score |
|-------------|-------|
| no data | 0.5 |
| 500 ms | 1.00 |
| 1 000 ms | 0.947 |
| 2 000 ms | 0.842 |
| 5 000 ms | 0.526 |
| 10 000 ms | 0.00 |
| > 10 000 ms | 0.00 (clamped) |

`rolling_latency_avg_ms` is maintained as an EMA:
- `ProbeUpdated`: window = 10 samples
- `CompletionFinished` / `ReportReceived`: window = 20 samples

---

### `success_rate_score`

```rust
fn success_rate_score(health: &HealthStatus) -> f64 {
    if last_success == EPOCH && last_failure == EPOCH { return 0.5; }
    health.success_ratio as f64
}
```

Uses `health.success_ratio` which is maintained by the reducer as an
EMA with **α = 0.1**:

```
new_ratio = old_ratio × 0.9 + outcome × 0.1
    outcome = 1.0  (success)
            = 0.0  (failure)
```

Initial value: `0.5` (neutral prior).

| Scenario | Resulting ratio |
|----------|-----------------|
| No events | 0.5 |
| 1 success from 0.5 | 0.550 |
| 10 consecutive successes from 0.5 | ≈ 0.803 |
| 10 consecutive failures from 0.5 | ≈ 0.174 |
| 1 success → 10 failures from 0.5 | ≈ 0.192 (< 0.2 → garbage) |

---

### `stability_score`

```rust
fn stability_score(health: &HealthStatus) -> f64 {
    if health.last_success > health.last_failure { 1.0 } else { 0.0 }
}
```

Binary: `1.0` if the last significant event was a success, `0.0` if it
was a failure. Uses `SequencedInstant` ordering (monotonic, deterministic).

| Condition | Score |
|-----------|-------|
| `last_success > last_failure` | 1.0 |
| `last_failure >= last_success` | 0.0 |
| Both at EPOCH | 0.0 (equal → not greater) |

---

### `load_score`

```rust
fn load_score(inflight: u32, max_inflight: u32) -> f64 {
    if max_inflight == 0 { return 0.0; }
    f64::max(0.0, 1.0 - inflight as f64 / max_inflight as f64)
}
```

Linear: `0 inflight → 1.0`, `max_inflight → 0.0`. Default
`max_inflight_per_model` from `RouteRequest` is 3.

| Inflight / Max | Score |
|----------------|-------|
| 0 / 3 | 1.000 |
| 1 / 3 | 0.667 |
| 2 / 3 | 0.333 |
| 3 / 3 | 0.000 (clamped) |
| > max | 0.000 (clamped) |

---

### `capability_bonus`

A flat bonus (not weighted) added to the weighted sum:

| Condition | Bonus |
|-----------|-------|
| `request.task_type ∈ model.capabilities.task_suitability` | +0.15 |
| `request.require_vision == true` AND `model.supports_vision == true` | +0.10 |
| `request.require_tool_use == true` AND `model.supports_tool_use == true` | +0.10 |
| `request.min_context_window` set AND `model.context_window >= min` | +0.05 |

Maximum possible bonus: **+0.40** (task + vision + tool + ctx).

---

## Theoretical Score Range

With default weights and no capability bonus, the raw weighted sum
ranges from `0.0` to `1.0`. A capability bonus can push the total
above `1.0`.

For a fully healthy, idle model with perfect stats and all capabilities
matching: score ≈ `1.0 × 0.35 + 1.0 × 0.20 + 1.0 × 0.25 + 1.0 × 0.10 + 1.0 × 0.10 + 0.40 = 1.40`.

---

## Garbage Detection

A model is marked `health.garbage = true` when:

```rust
fn is_garbage(health: &HealthStatus) -> bool {
    health.success_ratio < 0.2
        && health.last_failure > health.last_success
}
```

Both conditions must hold simultaneously:
1. **EMA ratio below threshold**: `success_ratio < 0.2`
2. **Last significant event was a failure**: `last_failure > last_success`

Starting from the neutral prior `0.5`, a model's ratio crosses `0.2`
after approximately 11 consecutive failures (with no successes in between).
Once marked garbage, the model is excluded from `RouterEngine::select`
before scoring begins.

The flag is re-evaluated (and may be cleared) on every
`CompletionFinished`, `ModelFailed`, and `ReportReceived` event:

```rust
health.garbage = is_garbage(health);
```

A garbage model can recover: a single successful `CompletionFinished`
resets `last_success` and bumps `success_ratio` upward via EMA. If the
ratio climbs back above `0.2`, the flag is cleared on the next event.

---

## Deterministic Tiebreaking

When two or more models have equal scores (e.g., all-new models with
no historical data), selection order is resolved by lexicographic
comparison of `model_id`:

```rust
scored.sort_by(|(sa, _, a), (sb, _, b)| {
    sb.partial_cmp(sa)
        .unwrap_or(std::cmp::Ordering::Equal)   // NaN-safe
        .then_with(|| a.id.cmp(&b.id))           // ascending model_id
});
```

Lower `model_id` string sorts first (alphabetically earlier wins).
This guarantee means the same pool of models always produces the same
routing order on any platform, regardless of HashMap iteration order
or floating-point representation.

---

## Fallback Chain

`RoutingDecision.fallback_chain` is the full scored-and-sorted list of
non-garbage models in priority order. The first entry is the selected
model; subsequent entries are tried in order if the primary fails.

```
decision.fallback_chain = ["best-model", "second-best", "third-best", ...]
```

`RouterEngine::fallback_chain(state, request, after)` returns the list
without the specified model — useful after a failure to get the next
candidate without re-running the full selection.

---

## `RouteRequest` Fields

```rust
pub struct RouteRequest {
    pub task_type: TaskType,                  // Code | Reasoning | Chat | Vision | Analysis
    pub max_latency_ms: Option<u64>,          // (Phase 2: used to pre-filter)
    pub require_vision: bool,
    pub require_tool_use: bool,
    pub min_context_window: Option<u32>,
    pub max_inflight_per_model: Option<u32>,  // default: Some(3)
}
```

`task_type` affects the capability bonus. `max_latency_ms`,
`require_vision`, `require_tool_use`, and `min_context_window` are
used for the capability bonus check. `max_inflight_per_model` caps
the load score.

> **Phase 2**: `max_latency_ms` will be used as a hard pre-filter,
> removing models whose `rolling_latency_avg_ms` exceeds the limit
> before scoring.

---

## `RoutingDecision` Output

```rust
pub struct RoutingDecision {
    pub selected_model: ModelId,         // empty string if no model available
    pub score: f64,                      // score of selected_model
    pub scores: Vec<ModelScore>,         // all scored models, best-first
    pub reasoning: Vec<ScoreFactor>,     // factors for selected_model
    pub fallback_chain: Vec<ModelId>,    // ordered fallback list
    pub timestamp: u64,                  // state.current_time().seq
    pub request: RouteRequest,           // original request (echo)
}

pub struct ModelScore {
    pub model_id: ModelId,
    pub total: f64,
    pub factors: Vec<ScoreFactor>,
}

pub struct ScoreFactor {
    pub name: String,           // "health", "latency", "success_rate", "stability", "load"
    pub value: f64,             // raw sub-score (0.0..=1.0+)
    pub weight: f64,            // factor weight
    // contribution() = value × weight
}
```

---

## CLI Output Example

```
$ omrp best reasoning

Best model for "reasoning": openrouter/claude-3-5-sonnet
Score: 1.223

Score breakdown:
  health          value=1.000  weight=0.35  contribution=0.350
  latency         value=0.968  weight=0.20  contribution=0.194
  success_rate    value=0.776  weight=0.25  contribution=0.194
  stability       value=1.000  weight=0.10  contribution=0.100
  load            value=1.000  weight=0.10  contribution=0.100
  [capability bonus: +0.15 task match, +0.10 tool use = +0.25]

Fallback chain: qwen/qwen-2-5-72b → openrouter/gpt-4o
```
