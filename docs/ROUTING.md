# BKG-FMR Routing Engine

## Scoring Formula

```
score(model, task) =
    w_health × health_score(model)
  + w_latency × latency_score(model)
  + w_success × success_rate_score(model)
  + w_stability × stability_score(model)
  + w_load × load_score(model, task)
  + capability_bonus(model, task)
```

### Weights (Default)

| Factor | Weight | Description |
|--------|--------|-------------|
| health | 0.35 | Model health status |
| latency | 0.20 | Average response time |
| success_rate | 0.25 | Trailing success ratio |
| stability | 0.10 | Stability index |
| load | 0.10 | Inflight count penalty |

## Scoring Functions

### health_score
- Returns 0.0 if model is garbage (consecutive failures > threshold)
- Returns 0.5 if no data (unknown)
- Returns 1.0 if healthy

### latency_score
- 500ms = 1.0, 10s = 0.0
- Lower is better

### success_rate_score
- Returns 0.5 if no data
- Otherwise returns success_ratio

### stability_score
- 1.0 if last_success > last_failure
- 0.0 otherwise

### load_score
- 1.0 if inflight = 0
- Decreases linearly as inflight approaches max_inflight_per_model

## Tiebreaking

When two models have equal scores:
1. Compare model_id lexicographically
2. Lower model_id wins

This ensures deterministic selection across all platforms.

## Fallback Chain

The fallback chain is computed deterministically:
1. All non-garbage models are scored
2. Sorted by score descending
3. Then by model_id ascending (tiebreaker)
4. Chain = ordered list of model_ids

## Garbage Detection

A model is marked as garbage when:
- `success_ratio < 0.2` AND
- `last_failure > last_success`

Garbage models are excluded from routing decisions.