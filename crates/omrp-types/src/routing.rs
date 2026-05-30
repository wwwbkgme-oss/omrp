use serde::{Deserialize, Serialize};
use crate::model::ModelId;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoutingDecision {
    pub selected_model: ModelId,
    pub score: f64,
    pub scores: Vec<ModelScore>,
    pub reasoning: Vec<ScoreFactor>,
    pub fallback_chain: Vec<ModelId>,
    pub timestamp: u64,
    pub request: crate::task::RouteRequest,
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
    pub fn contribution(&self) -> f64 {
        self.value * self.weight
    }
}
