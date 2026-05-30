use omrp_types::model::{Model, ModelId};
use omrp_types::routing::{ModelScore, RoutingDecision, ScoreFactor};
use omrp_types::task::RouteRequest;
use crate::state::State;
use crate::scorer::{Scorer, ScoringWeights};

/// BKG-FMR routing engine.
/// Deterministic: same state + same request → same decision.
pub struct RouterEngine {
    scorer: Scorer,
}

impl RouterEngine {
    pub fn new(scorer: Scorer) -> Self {
        Self { scorer }
    }

    /// Select the best model for a task.
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

        let fallback_chain: Vec<ModelId> = scored.iter().map(|(_, _, m)| m.id.clone()).collect();
        let all_scores: Vec<ModelScore> = scored
            .iter()
            .map(|(total, factors, m)| ModelScore {
                model_id: m.id.clone(),
                total: *total,
                factors: factors.clone(),
            })
            .collect();

        let (selected_score, selected_factors, selected_model) = match scored.first() {
            Some((s, f, m)) => (*s, f.clone(), m.id.clone()),
            None => (0.0, Vec::new(), String::new()),
        };

        RoutingDecision {
            selected_model,
            score: selected_score,
            scores: all_scores,
            reasoning: selected_factors,
            fallback_chain,
            timestamp: state.current_time().seq as u64,
            request: request.clone(),
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
}