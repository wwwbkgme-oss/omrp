use omrp_types::model::Model;
use omrp_types::task::RouteRequest;
use crate::state::HealthStatus;

pub struct ScoringWeights {
    pub health: f64,
    pub latency: f64,
    pub success_rate: f64,
    pub stability: f64,
    pub load: f64,
}

impl Default for ScoringWeights {
    fn default() -> Self {
        Self {
            health: 0.35,
            latency: 0.20,
            success_rate: 0.25,
            stability: 0.10,
            load: 0.10,
        }
    }
}

pub struct Scorer {
    pub weights: ScoringWeights,
}

impl Scorer {
    pub fn new(weights: ScoringWeights) -> Self {
        Self { weights }
    }

    /// Score a model for a given task and load state.
    /// Returns (total_score, vec_of_factors).
    /// Deterministic: same inputs → same result.
    pub fn score(
        &self,
        model: &Model,
        health: &HealthStatus,
        inflight: u32,
        request: &RouteRequest,
    ) -> (f64, Vec<omrp_types::routing::ScoreFactor>) {
        let mut factors = Vec::new();

        // Health score
        let h = self.health_score(health);
        factors.push(omrp_types::routing::ScoreFactor::new("health", h, self.weights.health));

        // Latency score
        let l = self.latency_score(health);
        factors.push(omrp_types::routing::ScoreFactor::new("latency", l, self.weights.latency));

        // Success rate score
        let sr = self.success_rate_score(health);
        factors.push(omrp_types::routing::ScoreFactor::new("success_rate", sr, self.weights.success_rate));

        // Stability score
        let st = self.stability_score(health);
        factors.push(omrp_types::routing::ScoreFactor::new("stability", st, self.weights.stability));

        // Load score
        let ld = self.load_score(inflight, request.max_inflight_per_model.unwrap_or(3));
        factors.push(omrp_types::routing::ScoreFactor::new("load", ld, self.weights.load));

        // Capability match bonus
        let bonus = self.capability_match(model, request);

        let total = factors.iter().map(|f| f.contribution()).sum::<f64>() + bonus;
        (total, factors)
    }

    fn health_score(&self, health: &HealthStatus) -> f64 {
        if health.garbage { return 0.0; }
        if health.last_success == omrp_types::time::SequencedInstant::EPOCH {
            return 0.5; // unknown → neutral
        }
        1.0
    }

    fn latency_score(&self, health: &HealthStatus) -> f64 {
        if health.rolling_latency_avg_ms <= 0.0 {
            return 0.5; // no data → neutral
        }
        // 500ms = 1.0, 10s = 0.0
        f64::max(0.0, 1.0 - (health.rolling_latency_avg_ms - 500.0) / 9500.0)
    }

    fn success_rate_score(&self, health: &HealthStatus) -> f64 {
        if health.last_success == omrp_types::time::SequencedInstant::EPOCH && health.last_failure == omrp_types::time::SequencedInstant::EPOCH {
            return 0.5; // no data → neutral
        }
        health.success_ratio as f64
    }

    fn stability_score(&self, health: &HealthStatus) -> f64 {
        if health.last_success > health.last_failure { 1.0 } else { 0.0 }
    }

    fn load_score(&self, inflight: u32, max_inflight: u32) -> f64 {
        if max_inflight == 0 { return 0.0; }
        f64::max(0.0, 1.0 - inflight as f64 / max_inflight as f64)
    }

    fn capability_match(&self, model: &Model, request: &RouteRequest) -> f64 {
        let mut score = 0.0;
        if model.capabilities.task_suitability.contains(&request.task_type) {
            score += 0.15;
        }
        if request.require_vision && model.capabilities.supports_vision {
            score += 0.10;
        }
        if request.require_tool_use && model.capabilities.supports_tool_use {
            score += 0.10;
        }
        if let Some(min_ctx) = request.min_context_window {
            if model.capabilities.context_window >= min_ctx {
                score += 0.05;
            }
        }
        score
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omrp_types::model::{Model, ModelCapabilities};
    use omrp_types::task::{TaskType, RouteRequest};

    fn default_scorer() -> Scorer {
        Scorer::new(ScoringWeights::default())
    }

    fn healthy_model() -> (Model, HealthStatus) {
        let model = Model {
            id: "test/model".into(),
            provider: "test".into(),
            capabilities: ModelCapabilities {
                task_suitability: vec![TaskType::Chat, TaskType::Code],
                supports_vision: false,
                supports_tool_use: true,
                context_window: 8192,
            },
        };
        let health = HealthStatus {
            last_success: omrp_types::time::SequencedInstant { seq: 10, logical_time: 10 },
            last_failure: omrp_types::time::SequencedInstant::EPOCH,
            success_ratio: 0.9,
            rolling_latency_avg_ms: 800.0,
            garbage: false,
        };
        (model, health)
    }

    #[test]
    fn test_scorer_returns_factors() {
        let scorer = default_scorer();
        let (model, health) = healthy_model();
        let request = RouteRequest::default();
        let (total, factors) = scorer.score(&model, &health, 0, &request);
        assert!(total > 0.0, "score should be positive");
        assert_eq!(factors.len(), 5, "should have 5 factors");
    }

    #[test]
    fn test_scorer_deterministic() {
        let scorer = default_scorer();
        let (model, health) = healthy_model();
        let request = RouteRequest::default();
        let (a, _) = scorer.score(&model, &health, 0, &request);
        let (b, _) = scorer.score(&model, &health, 0, &request);
        assert!((a - b).abs() < f64::EPSILON, "scorer must be deterministic");
    }

    #[test]
    fn test_garbage_model_scores_zero() {
        let scorer = default_scorer();
        let model = Model::new("garbage", "test");
        let health = HealthStatus {
            last_success: omrp_types::time::SequencedInstant::EPOCH,
            last_failure: omrp_types::time::SequencedInstant { seq: 5, logical_time: 5 },
            success_ratio: 0.1,
            rolling_latency_avg_ms: 5000.0,
            garbage: true,
        };
        let (total, _) = scorer.score(&model, &health, 0, &RouteRequest::default());
        assert!(total < 0.5, "garbage model should score low, got {total}");
    }

    #[test]
    fn test_load_penalty() {
        let scorer = default_scorer();
        let (model, health) = healthy_model();
        let request = RouteRequest { max_inflight_per_model: Some(2), ..Default::default() };

        let (idle, _) = scorer.score(&model, &health, 0, &request);
        let (busy, _) = scorer.score(&model, &health, 2, &request);

        assert!(idle > busy, "busy model should score lower than idle");
    }

    #[test]
    fn test_capability_bonus() {
        let scorer = default_scorer();
        let (model, health) = healthy_model();
        let request = RouteRequest {
            task_type: TaskType::Code,
            require_tool_use: true,
            ..Default::default()
        };
        let (score_with, _) = scorer.score(&model, &health, 0, &request);
        let request_no_match = RouteRequest {
            task_type: TaskType::Vision, // model doesn't support vision
            ..Default::default()
        };
        let (score_without, _) = scorer.score(&model, &health, 0, &request_no_match);
        assert!(score_with > score_without, "capability match should increase score");
    }
}
