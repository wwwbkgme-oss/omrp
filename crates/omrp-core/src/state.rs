use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use omrp_types::model::{Model, ModelId};
use omrp_types::routing::RoutingCache;
use omrp_types::time::SequencedInstant;

/// Pure state projection. NO ledger reference.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct State {
    pub models: Vec<Model>,
    pub health: HashMap<ModelId, HealthStatus>,
    pub routing_cache: RoutingCache,
    pub inflight: HashMap<ModelId, u32>,
    pub diagnostics: Diagnostics,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HealthStatus {
    pub last_success: SequencedInstant,
    pub last_failure: SequencedInstant,
    /// EMA-based success ratio (kept for backward compatibility).
    pub success_ratio: f32,
    pub rolling_latency_avg_ms: f64,
    /// Wilson Score / Bayesian garbage flag.
    pub garbage: bool,
    /// Bayesian Beta distribution: α = number of successful completions.
    /// Combined with `failure_count` (β) this models the agent's competence.
    #[serde(default)]
    pub success_count: u64,
    /// Bayesian Beta distribution: β = number of failed completions.
    #[serde(default)]
    pub failure_count: u64,
}

impl HealthStatus {
    pub fn new() -> Self {
        Self {
            last_success: SequencedInstant::EPOCH,
            last_failure: SequencedInstant::EPOCH,
            success_ratio: 0.5,
            rolling_latency_avg_ms: 0.0,
            garbage: false,
            success_count: 0,
            failure_count: 0,
        }
    }

    /// α for the Beta distribution (Laplace-smoothed: +1 prior).
    #[inline]
    pub fn alpha(&self) -> f64 { self.success_count as f64 + 1.0 }

    /// β for the Beta distribution (Laplace-smoothed: +1 prior).
    #[inline]
    pub fn beta_param(&self) -> f64 { self.failure_count as f64 + 1.0 }

    /// Total observations (α + β before smoothing).
    #[inline]
    pub fn total_obs(&self) -> u64 { self.success_count + self.failure_count }

    /// Bayesian posterior mean competence = α/(α+β).
    #[inline]
    pub fn bayesian_competence(&self) -> f64 {
        let a = self.alpha();
        let b = self.beta_param();
        a / (a + b)
    }

    /// Wilson Score lower bound at 95% confidence (z = 1.96).
    /// Used for garbage detection: principled lower bound on true success rate.
    pub fn wilson_lower(&self) -> f64 {
        let n = self.total_obs();
        if n == 0 { return 0.5; }  // no data → neutral
        let p = self.success_count as f64 / n as f64;
        let z  = 1.96_f64;
        let z2 = z * z;
        let nf = n as f64;
        let num = p + z2 / (2.0 * nf)
            - z * f64::sqrt(p * (1.0 - p) / nf + z2 / (4.0 * nf * nf));
        let den = 1.0 + z2 / nf;
        (num / den).clamp(0.0, 1.0)
    }

    /// Variance of the Beta distribution = αβ / ((α+β)²(α+β+1)).
    /// Lower variance → more stable (more observations).
    pub fn beta_variance(&self) -> f64 {
        let a = self.alpha();
        let b = self.beta_param();
        let n = a + b;
        (a * b) / (n * n * (n + 1.0))
    }

    /// Stability score: 1 - normalised standard deviation of Beta.
    /// High = many observations + consistent behaviour.
    pub fn stability_score(&self) -> f64 {
        let std_dev = self.beta_variance().sqrt();
        // Max std-dev of a Beta is 0.5 (uniform); normalise to [0,1].
        1.0 - (std_dev / 0.5).clamp(0.0, 1.0)
    }
}

impl Default for HealthStatus {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Diagnostics {
    pub total_completions: u64,
    pub total_failures: u64,
    pub total_fallbacks: u64,
    pub total_degradations: u64,
}

impl State {
    pub fn new() -> Self {
        Self {
            models: Vec::new(),
            health: HashMap::new(),
            routing_cache: RoutingCache::default(),
            inflight: HashMap::new(),
            diagnostics: Diagnostics::default(),
        }
    }

    pub fn current_time(&self) -> SequencedInstant {
        // Derived from diagnostics count (deterministic proxy for seq)
        SequencedInstant {
            seq: self.diagnostics.total_completions + self.diagnostics.total_failures + 1,
            logical_time: self.diagnostics.total_completions + self.diagnostics.total_failures,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_new_is_empty() {
        let state = State::new();
        assert!(state.models.is_empty());
        assert!(state.health.is_empty());
        assert!(state.inflight.is_empty());
        assert_eq!(state.diagnostics.total_completions, 0);
    }
}
