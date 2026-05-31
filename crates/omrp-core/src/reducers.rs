use omrp_events::event::Event;
use crate::state::{State, HealthStatus};

/// Exponential moving average update for the success ratio.
///
/// alpha = 0.1 means each observation shifts the ratio by 10% toward 1.0 (success)
/// or 0.0 (failure).  After ~10 consecutive failures starting from 0.5 + one prior
/// success the ratio drops below 0.2, triggering garbage detection.
/// Deterministic: same sequence of calls always produces the same value.
fn update_success_ratio(current: f32, success: bool) -> f32 {
    const ALPHA: f32 = 0.1;
    if success {
        current * (1.0 - ALPHA) + ALPHA
    } else {
        current * (1.0 - ALPHA)
    }
}

/// Windowed exponential moving average (deterministic).
fn windowed_avg(current: f64, new_value: f64, window_size: u64) -> f64 {
    current * (1.0 - 1.0 / window_size as f64) + new_value * (1.0 / window_size as f64)
}

/// Wilson Score garbage detection.
///
/// A model is garbage when its Wilson Score lower bound (95% CI) falls below
/// `GARBAGE_THRESHOLD` AND we have enough observations to make a confident
/// assertion (at least `MIN_OBS`).
///
/// The Wilson Score lower bound is more principled than a plain ratio check:
/// it accounts for statistical uncertainty so a model with 1 success and 3
/// failures is treated differently from one with 100 successes and 300 failures.
const GARBAGE_THRESHOLD: f64 = 0.15;
const MIN_OBS:           u64 = 5;

fn is_garbage(health: &HealthStatus) -> bool {
    // Require a minimum number of observations before labelling as garbage.
    if health.total_obs() < MIN_OBS {
        return false;
    }
    // Wilson Score lower bound < threshold AND last event was a failure.
    health.wilson_lower() < GARBAGE_THRESHOLD && health.last_failure > health.last_success
}

/// ─── ALL reducers in one dispatch table ───
///
/// Pure state transition: no IO, no randomness, no ledger reference.
/// Same sequence of events always produces identical state (determinism guarantee).
pub fn dispatch(state: &mut State, event: &Event) {
    match event {
        Event::DaemonStarted { .. } => {
            state.diagnostics = Default::default();
        }

        Event::ModelAdded { model, .. } => {
            // Idempotent: adding the same model twice is a no-op.
            if state.models.iter().any(|m| m.id == model.id) {
                return;
            }
            state.models.push(model.clone());
            state.health.insert(model.id.clone(), HealthStatus::new());
            state.inflight.insert(model.id.clone(), 0);
        }

        Event::ModelRemoved { model_id, .. } => {
            state.models.retain(|m| m.id != *model_id);
            state.health.remove(model_id);
            state.inflight.remove(model_id);
        }

        Event::ModelSelected { model_id, score, reason: _, request: _ } => {
            // Record the routing decision in the cache and increment inflight.
            state.routing_cache.last_selected = Some(omrp_types::routing::RoutingCacheEntry {
                model_id: model_id.clone(),
                score: *score,
                selected_at: state.current_time(),
            });
            *state.inflight.entry(model_id.clone()).or_insert(0) += 1;
        }

        Event::CompletionRequested { model_id, .. } => {
            *state.inflight.entry(model_id.clone()).or_insert(0) += 1;
        }

        Event::CompletionFinished { model_id, latency_ms, tokens_used: _, success } => {
            let now = state.current_time();
            if let Some(health) = state.health.get_mut(model_id) {
                if *success {
                    health.last_success = now;
                    health.rolling_latency_avg_ms =
                        windowed_avg(health.rolling_latency_avg_ms, *latency_ms as f64, 20);
                    health.success_count = health.success_count.saturating_add(1);
                } else {
                    health.last_failure = now;
                    health.failure_count = health.failure_count.saturating_add(1);
                }
                health.success_ratio = update_success_ratio(health.success_ratio, *success);
                health.garbage = is_garbage(health);
            }
            if let Some(count) = state.inflight.get_mut(model_id) {
                *count = count.saturating_sub(1);
            }
            state.diagnostics.total_completions += 1;
            if !success {
                state.diagnostics.total_failures += 1;
            }
        }

        Event::ModelFailed { model_id, .. } => {
            let now = state.current_time();
            if let Some(health) = state.health.get_mut(model_id) {
                health.last_failure = now;
                health.failure_count = health.failure_count.saturating_add(1);
                health.success_ratio = update_success_ratio(health.success_ratio, false);
                health.garbage = is_garbage(health);
            }
            state.diagnostics.total_failures += 1;
        }

        Event::ProbeUpdated { model_id, latency_ms, .. } => {
            let now = state.current_time();
            if let Some(health) = state.health.get_mut(model_id) {
                health.last_success = now;
                health.rolling_latency_avg_ms =
                    windowed_avg(health.rolling_latency_avg_ms, *latency_ms as f64, 10);
                health.success_count = health.success_count.saturating_add(1);
                health.garbage = is_garbage(health);
            }
        }

        Event::ProbeFailed { model_id, .. } => {
            let now = state.current_time();
            if let Some(health) = state.health.get_mut(model_id) {
                health.last_failure = now;
                health.failure_count = health.failure_count.saturating_add(1);
                health.garbage = is_garbage(health);
            }
        }

        Event::FallbackTriggered { from, to, .. } => {
            state.routing_cache.last_fallback = Some(omrp_types::routing::FallbackEntry {
                from: from.clone(),
                to: to.clone(),
                at: state.current_time(),
            });
            state.diagnostics.total_fallbacks += 1;
        }

        Event::DegradeModeEnabled { .. } => {
            state.diagnostics.total_degradations += 1;
        }

        Event::ReportReceived { model_id, success, latency_ms, .. } => {
            let now = state.current_time();
            if let Some(health) = state.health.get_mut(model_id) {
                if *success {
                    health.last_success = now;
                    health.rolling_latency_avg_ms =
                        windowed_avg(health.rolling_latency_avg_ms, *latency_ms as f64, 20);
                    health.success_count = health.success_count.saturating_add(1);
                } else {
                    health.last_failure = now;
                    health.failure_count = health.failure_count.saturating_add(1);
                }
                health.success_ratio = update_success_ratio(health.success_ratio, *success);
                health.garbage = is_garbage(health);
            }
        }

        Event::ConfigReloaded { .. } | Event::DaemonStopped { .. } => {
            // No state mutation needed for these lifecycle events.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omrp_events::event::{Event, ModelSource};
    use omrp_types::model::Model;

    fn make_state(events: &[Event]) -> State {
        let mut state = State::new();
        for event in events {
            dispatch(&mut state, event);
        }
        state
    }

    #[test]
    fn test_model_added_creates_entries() {
        let state = make_state(&[Event::ModelAdded {
            model: Model::new("test/model", "test"),
            source: ModelSource::Bundled,
        }]);
        assert_eq!(state.models.len(), 1);
        assert!(state.health.contains_key("test/model"));
        assert_eq!(*state.inflight.get("test/model").unwrap(), 0);
    }

    #[test]
    fn test_model_removed_cleans_up() {
        let events = vec![
            Event::ModelAdded {
                model: Model::new("test/model", "test"),
                source: ModelSource::Bundled,
            },
            Event::ModelRemoved {
                model_id: "test/model".into(),
                reason: "test".into(),
            },
        ];
        let state = make_state(&events);
        assert!(state.models.is_empty());
        assert!(!state.health.contains_key("test/model"));
    }

    #[test]
    fn test_completion_success_updates_health() {
        let events = vec![
            Event::ModelAdded {
                model: Model::new("test/model", "test"),
                source: ModelSource::Bundled,
            },
            Event::CompletionFinished {
                model_id: "test/model".into(),
                latency_ms: 500,
                tokens_used: 100,
                success: true,
            },
        ];
        let state = make_state(&events);
        let health = state.health.get("test/model").unwrap();
        assert!(health.last_success != omrp_types::time::SequencedInstant::EPOCH);
        assert!(health.rolling_latency_avg_ms > 0.0);
    }

    #[test]
    fn test_completion_failure_tracks_failure() {
        let events = vec![
            Event::ModelAdded {
                model: Model::new("test/model", "test"),
                source: ModelSource::Bundled,
            },
            Event::CompletionFinished {
                model_id: "test/model".into(),
                latency_ms: 500,
                tokens_used: 100,
                success: false,
            },
        ];
        let state = make_state(&events);
        assert_eq!(state.diagnostics.total_failures, 1);
    }

    #[test]
    fn test_model_failed_sets_garbage_after_repeated_failures() {
        let mut events = vec![Event::ModelAdded {
            model: Model::new("test/model", "test"),
            source: ModelSource::Bundled,
        }];
        // Alternate failure/check: success_ratio drops below 0.2 threshold.
        // First add a success so last_success is set, then many failures to flip it.
        events.push(Event::CompletionFinished {
            model_id: "test/model".into(),
            latency_ms: 100,
            tokens_used: 10,
            success: true,
        });
        for _ in 0..10 {
            events.push(Event::ModelFailed {
                model_id: "test/model".into(),
                error: omrp_events::error::ErrorKind::Timeout { timeout_ms: 5000 },
            });
        }
        let state = make_state(&events);
        let health = state.health.get("test/model").unwrap();
        assert!(health.garbage, "model should be garbage after repeated failures");
    }

    #[test]
    fn test_idempotent_model_add() {
        let events = vec![
            Event::ModelAdded {
                model: Model::new("m1", "p1"),
                source: ModelSource::Bundled,
            },
            Event::ModelAdded {
                model: Model::new("m1", "p1"),
                source: ModelSource::LocalConfig,
            },
        ];
        let state = make_state(&events);
        assert_eq!(state.models.len(), 1, "duplicate ModelAdded must be a no-op");
    }
}
