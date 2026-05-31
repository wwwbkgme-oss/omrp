use omrp_types::model::{Model, ModelId};
use omrp_types::routing::{ModelScore, RoutingDecision, ScoreFactor};
use omrp_types::task::RouteRequest;
use crate::state::State;
use crate::scorer::{Scorer, ScoringWeights};

/// BKG-FMR routing engine.
///
/// ## Selection modes
///
/// - `select()` — **deterministic**: same state + same request → same decision.
///   Used by CLI (`omrp best`, `omrp status`) and anywhere reproducibility matters.
///
/// - `select_thompson()` — **probabilistic Thompson Sampling**: balances exploration
///   (trying uncertain models) vs exploitation (trusting proven ones) using
///   samples from each model's Beta(α, β) distribution.  Seeded from the current
///   ledger sequence so it varies naturally as the system learns.
pub struct RouterEngine {
    scorer: Scorer,
}

impl RouterEngine {
    pub fn new(scorer: Scorer) -> Self {
        Self { scorer }
    }

    /// Select the best model for a task — **deterministic**.
    /// Pure function. No IO. No side effects.
    pub fn select(&self, state: &State, request: &RouteRequest) -> RoutingDecision {
        let mut scored: Vec<(f64, Vec<ScoreFactor>, &Model)> = state
            .models
            .iter()
            .filter(|m| {
                let health = state.health.get(&m.id);
                health.map_or(true, |h| !h.garbage)
            })
            .map(|m| {
                let health = state.health.get(&m.id).cloned().unwrap_or_default();
                let inflight = state.inflight.get(&m.id).copied().unwrap_or(0);
                let (score, factors) = self.scorer.score(m, &health, inflight, request);
                (score, factors, m)
            })
            .collect();

        // Sort by score desc, then deterministic tiebreaker (model_id lexicographic)
        scored.sort_by(|(sa, _, a), (sb, _, b)| {
            sb.partial_cmp(sa)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.id.cmp(&b.id))
        });

        Self::build_decision(scored, state, request)
    }

    /// Select a model using **Thompson Sampling** (probabilistic exploration).
    ///
    /// Each non-garbage model is sampled from its Beta(α, β) distribution —
    /// models with fewer observations get wider samples, encouraging exploration.
    /// The capability match bonus is added to each sample before ranking.
    ///
    /// Seeded from `state.diagnostics.total_completions + total_failures` so the
    /// selected model varies as observations accumulate, but is reproducible for a
    /// given ledger state.
    pub fn select_thompson(&self, state: &State, request: &RouteRequest) -> RoutingDecision {
        let seed = state.diagnostics.total_completions
            .wrapping_add(state.diagnostics.total_failures.wrapping_mul(2654435761));
        let mut rng = Xorshift64::new(seed.max(1));

        let mut scored: Vec<(f64, Vec<ScoreFactor>, &Model)> = state
            .models
            .iter()
            .filter(|m| state.health.get(&m.id).map_or(true, |h| !h.garbage))
            .map(|m| {
                let health  = state.health.get(&m.id).cloned().unwrap_or_default();
                let inflight = state.inflight.get(&m.id).copied().unwrap_or(0);

                // Thompson sample from Beta(α, β)
                let ts_sample = sample_beta(&mut rng, health.alpha(), health.beta_param());

                // Build factors for display (still using the deterministic scorer)
                let (_, factors) = self.scorer.score(m, &health, inflight, request);

                // Load pressure (continuous, not binary)
                let max_inf = request.max_inflight_per_model.unwrap_or(3) as f64;
                let load_factor = if max_inf > 0.0 {
                    (1.0 - inflight as f64 / max_inf).max(0.0)
                } else {
                    0.0
                };

                // Capability bonus (same as deterministic scorer)
                let cap_bonus = if m.capabilities.task_suitability.contains(&request.task_type) {
                    0.15
                } else {
                    0.0
                };

                // Combined Thompson score
                let ts_total = ts_sample * 0.70 + load_factor * 0.20 + cap_bonus * 0.10;
                (ts_total, factors, m)
            })
            .collect();

        // Sort by Thompson sample desc, lexicographic tiebreaker for reproducibility
        scored.sort_by(|(sa, _, a), (sb, _, b)| {
            sb.partial_cmp(sa)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.id.cmp(&b.id))
        });

        Self::build_decision(scored, state, request)
    }

    fn build_decision(
        scored: Vec<(f64, Vec<ScoreFactor>, &Model)>,
        state: &State,
        request: &RouteRequest,
    ) -> RoutingDecision {
        let fallback_chain: Vec<ModelId> = scored.iter().map(|(_, _, m)| m.id.clone()).collect();
        let all_scores: Vec<ModelScore> = scored
            .iter()
            .map(|(total, factors, m)| ModelScore {
                model_id: m.id.clone(),
                total:    *total,
                factors:  factors.clone(),
            })
            .collect();

        let (selected_score, selected_factors, selected_model) = match scored.first() {
            Some((s, f, m)) => (*s, f.clone(), m.id.clone()),
            None            => (0.0, Vec::new(), String::new()),
        };

        RoutingDecision {
            selected_model,
            score:          selected_score,
            scores:         all_scores,
            reasoning:      selected_factors,
            fallback_chain,
            timestamp:      state.current_time().seq as u64,
            request:        request.clone(),
        }
    }

    /// Build fallback chain excluding a specific model.
    pub fn fallback_chain(&self, state: &State, request: &RouteRequest, after: &ModelId) -> Vec<ModelId> {
        let decision = self.select(state, request);
        decision
            .fallback_chain
            .into_iter()
            .filter(|id| id != after)
            .collect()
    }
}

impl Default for RouterEngine {
    fn default() -> Self {
        Self::new(Scorer::new(ScoringWeights::default()))
    }
}

// ─── Thompson Sampling: Beta distribution sampler ────────────────────────────
//
// Beta(a, b) is sampled via the Gamma ratio method:
//   X ~ Gamma(a, 1),  Y ~ Gamma(b, 1)  →  X/(X+Y) ~ Beta(a, b)
//
// Gamma samples use the Marsaglia-Tsang "squeeze" algorithm (no external deps).

/// Xorshift64 pseudo-random number generator — deterministic for a given seed.
struct Xorshift64(u64);

impl Xorshift64 {
    fn new(seed: u64) -> Self { Self(seed.max(1)) }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    /// Sample from U(0, 1).
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }
    /// Sample from N(0, 1) via Box-Muller transform.
    fn next_normal(&mut self) -> f64 {
        let u1 = self.next_f64().max(1e-10);
        let u2 = self.next_f64();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }
}

/// Sample from Gamma(shape, 1) using the Marsaglia-Tsang squeeze method.
fn sample_gamma(rng: &mut Xorshift64, shape: f64) -> f64 {
    if shape < 1.0 {
        return sample_gamma(rng, shape + 1.0) * rng.next_f64().powf(1.0 / shape);
    }
    let d = shape - 1.0 / 3.0;
    let c = 1.0 / f64::sqrt(9.0 * d);
    loop {
        let x = rng.next_normal();
        let v = 1.0 + c * x;
        if v > 0.0 {
            let v3 = v * v * v;
            let u  = rng.next_f64();
            if u < 1.0 - 0.0331 * (x * x) * (x * x)
               || u.ln() < 0.5 * x * x + d * (1.0 - v3 + v3.ln())
            {
                return d * v3;
            }
        }
    }
}

/// Sample from Beta(a, b) in (0, 1) via the Gamma ratio method.
fn sample_beta(rng: &mut Xorshift64, a: f64, b: f64) -> f64 {
    let x = sample_gamma(rng, a);
    let y = sample_gamma(rng, b);
    let s = x + y;
    if s <= 0.0 { 0.5 } else { (x / s).clamp(0.0, 1.0) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omrp_types::model::{Model, ModelCapabilities};
    use omrp_types::task::TaskType;

    fn test_model(id: &str, provider: &str) -> Model {
        Model {
            id: id.into(),
            provider: provider.into(),
            capabilities: ModelCapabilities {
                task_suitability: vec![TaskType::Chat, TaskType::Code],
                supports_vision: false,
                supports_tool_use: true,
                context_window: 8192,
            },
        }
    }

    fn test_state_with_models() -> State {
        let mut state = State::new();
        state.models = vec![
            test_model("model/a", "provider1"),
            test_model("model/b", "provider2"),
            test_model("model/c", "provider3"),
        ];
        state
    }

    #[test]
    fn test_router_engine_select_returns_decision() {
        let engine = RouterEngine::default();
        let state = test_state_with_models();
        let request = RouteRequest::default();
        let decision = engine.select(&state, &request);
        assert!(!decision.selected_model.is_empty());
        assert!(!decision.scores.is_empty());
    }

    #[test]
    fn test_router_engine_deterministic() {
        let engine = RouterEngine::default();
        let state = test_state_with_models();
        let request = RouteRequest::default();
        let decision_a = engine.select(&state, &request);
        let decision_b = engine.select(&state, &request);
        assert_eq!(decision_a.selected_model, decision_b.selected_model);
        assert_eq!(decision_a.fallback_chain, decision_b.fallback_chain);
    }

    #[test]
    fn test_router_engine_fallback_chain() {
        let engine = RouterEngine::default();
        let state = test_state_with_models();
        let request = RouteRequest::default();
        let chain = engine.fallback_chain(&state, &request, &"model/a".to_string());
        assert!(!chain.contains(&"model/a".to_string()));
    }

    #[test]
    fn test_thompson_sampling_returns_decision() {
        let engine = RouterEngine::default();
        let state = test_state_with_models();
        let request = RouteRequest::default();
        let decision = engine.select_thompson(&state, &request);
        assert!(!decision.selected_model.is_empty());
        assert_eq!(decision.scores.len(), 3);
    }

    #[test]
    fn test_thompson_sampling_deterministic_for_same_state() {
        let engine = RouterEngine::default();
        let state = test_state_with_models();
        let request = RouteRequest::default();
        let d1 = engine.select_thompson(&state, &request);
        let d2 = engine.select_thompson(&state, &request);
        // Same state → same seed → same sample → same result
        assert_eq!(d1.selected_model, d2.selected_model);
    }

    #[test]
    fn test_thompson_prefers_model_with_more_successes() {
        let engine = RouterEngine::default();
        let mut state = test_state_with_models();
        // Give model/b a very high success count (α >> β)
        let h = state.health.entry("model/b".into()).or_default();
        h.success_count = 1000;
        h.failure_count = 1;
        // Run many Thompson samples; model/b should dominate
        let mut wins = 0usize;
        for i in 0..20u64 {
            let mut s2 = state.clone();
            s2.diagnostics.total_completions = i;
            let d = engine.select_thompson(&s2, &RouteRequest::default());
            if d.selected_model == "model/b" { wins += 1; }
        }
        assert!(wins >= 15, "model/b should win most Thompson samples, won {wins}/20");
    }

    #[test]
    fn test_xorshift_generates_different_values() {
        let mut rng = Xorshift64::new(42);
        let a = rng.next_f64();
        let b = rng.next_f64();
        assert_ne!(a, b);
        assert!((0.0..=1.0).contains(&a));
        assert!((0.0..=1.0).contains(&b));
    }

    #[test]
    fn test_beta_sampler_in_range() {
        let mut rng = Xorshift64::new(1234);
        for _ in 0..100 {
            let v = sample_beta(&mut rng, 2.0, 5.0);
            assert!((0.0..=1.0).contains(&v), "Beta sample out of range: {v}");
        }
    }

    #[test]
    fn test_beta_mean_approx_correct() {
        let mut rng = Xorshift64::new(999);
        let a = 3.0_f64;
        let b = 7.0_f64;
        let expected_mean = a / (a + b);  // 0.3
        let n = 2000;
        let mean = (0..n).map(|_| sample_beta(&mut rng, a, b)).sum::<f64>() / n as f64;
        assert!(
            (mean - expected_mean).abs() < 0.05,
            "Beta({a},{b}) sample mean {mean:.3} far from expected {expected_mean:.3}"
        );
    }
}
