use serde::{Deserialize, Serialize};
use crate::model::ModelId;
use crate::task::RouteRequest;
use crate::time::SequencedInstant;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RoutingReason {
    TopScore,
    Fallback,
    UserPreference,
    DegradedMode,
    LatencyOptimization,
    CostOptimization,
    LoadBalancing,
    Exclusive,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoutingDecision {
    pub selected_model: ModelId,
    pub score: f64,
    pub scores: Vec<ModelScore>,
    pub reasoning: Vec<ScoreFactor>,
    pub fallback_chain: Vec<ModelId>,
    pub timestamp: u64,
    pub request: RouteRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelScore {
    pub model_id: ModelId,
    pub total: f64,
    pub factors: Vec<ScoreFactor>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScoreFactor {
    pub name: String,
    pub value: f64,
    pub weight: f64,
}

impl ScoreFactor {
    pub fn new(name: &str, value: f64, weight: f64) -> Self {
        Self {
            name: name.to_string(),
            value,
            weight,
        }
    }

    pub fn contribution(&self) -> f64 {
        self.value * self.weight
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RoutingCache {
    pub last_selected: Option<RoutingCacheEntry>,
    pub last_fallback: Option<FallbackEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoutingCacheEntry {
    pub model_id: ModelId,
    pub score: f64,
    pub selected_at: SequencedInstant,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FallbackEntry {
    pub from: ModelId,
    pub to: ModelId,
    pub at: SequencedInstant,
}