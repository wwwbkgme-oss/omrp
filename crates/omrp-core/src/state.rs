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
    pub success_ratio: f32,
    pub rolling_latency_avg_ms: f64,
    pub garbage: bool,
}

impl HealthStatus {
    pub fn new() -> Self {
        Self {
            last_success: SequencedInstant::EPOCH,
            last_failure: SequencedInstant::EPOCH,
            success_ratio: 0.5,
            rolling_latency_avg_ms: 0.0,
            garbage: false,
        }
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
