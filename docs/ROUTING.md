# BKG-FMR Routing Engine — v0.2

**BKG-FMR** = Best Known Garbage-Free Models Router.

v0.2 upgrades the scoring system from EMA-based heuristics to a
**Bayesian Beta-distribution model** with **Thompson Sampling** for
probabilistic exploration, and replaces the simple garbage threshold
with a **Wilson Score lower bound** for statistically principled exclusion.

---

## Quick Reference

```bash
omrp best code           # deterministic BKG-FMR routing
omrp best reasoning      # score breakdown printed
omrp status              # all models scored
omrp serve               # axum server uses select_thompson() for routing
```

---

## Routing Algorithm

### Deterministic path (`RouterEngine::select`)

Used by CLI commands and anywhere reproducibility is required.

```
1. filter      remove all models where health.garbage == true
2. score       Bayesian 5-factor score for each remaining model
3. sort        descending by score; ascending by model_id as tiebreaker
4. return      first = selected_model; full list = fallback_chain
```

### Probabilistic path (`RouterEngine::select_thompson`)

Used by `omrp serve` for production request routing.

```
1. filter      remove garbage models
2. sample      draw θᵢ ~ Beta(αᵢ, βᵢ) for each candidate model i
3. combine     ts_score = θᵢ×0.70 + load×0.20 + cap_bonus×0.10
4. sort        descending by ts_score; lexicographic tiebreaker
5. return      highest-sample model as selected_model
```

Thompson Sampling balances **exploitation** (models with high α/(α+β)) with
**exploration** (models with few observations have wide Beta distributions
and occasionally score high, giving them a chance to prove themselves).

---

## Bayesian Agent Scoring

### Beta Distribution Model

Each model `i` maintains a Beta distribution `Beta(αᵢ, βᵢ)` where:

- **αᵢ = success_count + 1** (Laplace smoothing: +1 prior prevents division by zero)
- **βᵢ = failure_count + 1**

The counts are updated by `reducers::dispatch` on every `CompletionFinished`,
`ModelFailed`, `ProbeUpdated`, and `ProbeFailed` event.

### Posterior Mean (Competence Score)

```
competence(i) = αᵢ / (αᵢ + βᵢ)
```

With Laplace smoothing, a model with no observations starts at 0.5 (neutral).
As observations accumulate, the mean converges toward the true success rate.

| Observations | Competence |
|--------------|-----------|
| 0 success, 0 failure | 0.500 (prior) |
| 1 success, 0 failure | 0.667 |
| 10 success, 0 failure | 0.917 |
| 9 success, 1 failure | 0.833 |
| 1 success, 9 failure | 0.167 |
| 0 success, 10 failure | 0.083 |

### Beta Stability Score

The **variance** of `Beta(α, β)` measures how uncertain we are about a
model's true success rate:

```
var(Beta(α,β)) = αβ / ((α+β)² × (α+β+1))
```

A model with few observations has high variance (we're uncertain about it).
A model with many consistent observations has low variance (we trust our estimate).

The **stability score** used in the scorer converts this to a [0, 1] range:

```
stability = 1 − min(1, sqrt(variance) / 0.5)
```

Max variance of a Beta is 0.25 (uniform prior), std-dev = 0.5, so
the normalisation is by 0.5.

---

## Scoring Formula

```
score(model, request) =
    health_score               × 0.35
  + latency_score              × 0.20
  + bayesian_competence        × 0.25   ← v0.2: replaces EMA ratio
  + beta_stability_score       × 0.10   ← v0.2: replaces binary last_event check
  + load_score                 × 0.10
  + capability_bonus                    ← flat additive, not weighted
```

### Factor Definitions

| Factor | Formula |
|--------|---------|
| `health_score` | `0.0` if garbage, `0.5` if no data (last_success=EPOCH), else `1.0` |
| `latency_score` | `max(0, 1 − (avg_ms − 500) / 9500)` · 500ms→1.0, 10s→0.0 |
| `bayesian_competence` | `α/(α+β)` · Laplace-smoothed posterior mean |
| `beta_stability` | `1 − sqrt(variance)/0.5` · high observations→high stability |
| `load_score` | `max(0, 1 − inflight/max_inflight)` |
| `capability_bonus` | `+0.15` task, `+0.10` vision, `+0.10` tool-use, `+0.05` ctx |

### Theoretical Score Range

Fully healthy, idle, task-matching model: `1.0+1.0+1.0+1.0+1.0 weighted + 0.40 bonus = 1.40`

---

## Wilson Score Garbage Detection

### Why Wilson Score?

The old check `success_ratio < 0.2` has two problems:

1. **Sample size blindness**: 1 success + 3 failures gives ratio 0.25 (not garbage),
   but 100 successes + 1900 failures also gives 0.05 (garbage). The first case
   may be noise; the second is a real pattern.
2. **No confidence interval**: it treats 5 observations the same as 5000.

The Wilson Score lower bound at 95% confidence addresses both:

```
WS_lower = (p̂ + z²/2n - z·√(p̂(1-p̂)/n + z²/4n²)) / (1 + z²/n)

where:
  p̂ = success_count / total_obs    (observed success rate)
  n  = success_count + failure_count  (total observations)
  z  = 1.96                           (95% confidence)
```

With few observations, `WS_lower` stays close to 0.5 even for a bad run.
With many observations, it converges toward the true success rate.

### Garbage Threshold

```rust
const GARBAGE_THRESHOLD: f64 = 0.15;
const MIN_OBS:           u64 = 5;

fn is_garbage(health: &HealthStatus) -> bool {
    health.total_obs() >= MIN_OBS
        && health.wilson_lower() < GARBAGE_THRESHOLD
        && health.last_failure > health.last_success
}
```

Both conditions must hold:
1. Enough data (≥ 5 observations)
2. Wilson lower bound below 0.15 **and** last event was a failure

### Recovery

A garbage model can recover: a successful `CompletionFinished` increments
`success_count`, which raises `wilson_lower()`. If it climbs above 0.15,
the garbage flag is cleared on the next event dispatch.

---

## Thompson Sampling Implementation

### Gamma Ratio Method

`Beta(a, b)` is sampled via:
1. Draw `X ~ Gamma(a, 1)` and `Y ~ Gamma(b, 1)`
2. Return `X / (X + Y)` — this is exactly `Beta(a, b)`

### Gamma Sampler (Marsaglia-Tsang Squeeze)

Pure Rust implementation, no external RNG dependency:

```rust
fn sample_gamma(rng: &mut Xorshift64, shape: f64) -> f64 {
    // shape < 1: use Gamma(shape+1) * U^(1/shape)
    // shape >= 1: Marsaglia-Tsang squeeze method
    let d = shape - 1.0/3.0;
    let c = 1.0 / sqrt(9.0 * d);
    loop {
        let x = rng.next_normal();     // Box-Muller N(0,1)
        let v = 1.0 + c * x;
        if v > 0.0 {
            let v3 = v³;
            let u  = rng.next_f64();
            if u < 1 - 0.0331·x⁴ { return d·v3; }
            if ln(u) < ½x² + d(1-v3+ln(v3)) { return d·v3; }
        }
    }
}
```

### RNG: Xorshift64

Seeded from the ledger sequence:
```rust
let seed = total_completions + total_failures × 2_654_435_761;
let mut rng = Xorshift64::new(seed.max(1));
```

This means:
- **Same ledger state → same sample** (reproducible for debugging)
- **Different state → different sample** (exploration varies as system learns)

---

## Deterministic Tiebreaking

Both `select()` and `select_thompson()` use the same tiebreaker:

```rust
scored.sort_by(|(sa, _, a), (sb, _, b)| {
    sb.partial_cmp(sa)
        .unwrap_or(Ordering::Equal)   // NaN-safe
        .then_with(|| a.id.cmp(&b.id)) // ascending model_id (lexicographic)
});
```

Lower `model_id` string wins ties. This guarantees consistent ordering
regardless of HashMap iteration order or floating-point representation.

---

## Fallback Chain

`RoutingDecision.fallback_chain` is the complete sorted list of non-garbage
models. The web proxy tries them in order when the primary fails.

```
decision.fallback_chain = ["selected", "second-best", "third-best", …]
```

`RouterEngine::fallback_chain(state, request, after)` returns the list
without `after` — useful for building the next candidate after a failure.

---

## RouteRequest Fields

```rust
pub struct RouteRequest {
    pub task_type:              TaskType,       // Code|Reasoning|Chat|Vision|Analysis
    pub max_latency_ms:         Option<u64>,    // future pre-filter
    pub require_vision:         bool,
    pub require_tool_use:       bool,
    pub min_context_window:     Option<u32>,
    pub max_inflight_per_model: Option<u32>,    // default: Some(3)
}
```

---

## RoutingDecision Output

```rust
pub struct RoutingDecision {
    pub selected_model:  ModelId,        // empty if no model available
    pub score:           f64,            // combined score of selected model
    pub scores:          Vec<ModelScore>, // all scored models, best-first
    pub reasoning:       Vec<ScoreFactor>, // factors for selected model
    pub fallback_chain:  Vec<ModelId>,   // ordered fallback list
    pub timestamp:       u64,            // state.current_time().seq
    pub request:         RouteRequest,   // original request (echo)
}
```

---

## CLI Score Breakdown Example

```
$ omrp best reasoning

Best for "reasoning": deepseek/deepseek-v4-flash:free
Score: 1.283

Factors:
  health          value=1.000  weight=0.35  contribution=0.350
  latency         value=1.038  weight=0.20  contribution=0.208
  success_rate    value=0.909  weight=0.25  contribution=0.227
  stability       value=0.982  weight=0.10  contribution=0.098
  load            value=1.000  weight=0.10  contribution=0.100
  [capability bonus: +0.15 task match = +0.15]
  [Wilson lower: 0.82 ≥ 0.15 — not garbage]
  [β-distribution: α=10, β=1, competence=0.917]

Fallback: qwen/qwen3-coder:free → moonshotai/kimi-k2.6:free
```
