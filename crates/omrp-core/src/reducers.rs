use omrp_events::event::Event;
use crate::state::{State, HealthStatus};

/// Window-based success ratio helper
fn compute_success_ratio(state: &State, model_id: &str) -> f32 {
    let health = match state.health.get(model_id) {
        Some(h) => h,
        None => return 0.0,
    };
    if health.last_success == omrp_types::time::SequencedInstant::EPOCH && health.last_failure == omrp_types::time::SequencedInstant::EPOCH {
        return 0.5;
    }
    if health.last_failure == omrp_types::time::SequencedInstant::EPOCH {
        return 1.0;
    }
    if health.last_success == omrp_types::time::SequencedInstant::EPOCH {
        return 0.0;
    }
    if health.last_success > health.last_failure { 0.8 } else { 0.3 }
}

/// Windowed rolling average (deterministic)
fn windowed_avg(current: f64, new_value: f64, window_size: u64) -> f64 {
    current * (1.0 - 1.0 / window_size as f64) + new_value * (1.0 / window_size as f64)
}

/// Garbage detection
fn is_garbage(health: &HealthStatus) -> bool {
    health.success_ratio < 0.2 && health.last_failure > health.last_success
}

/// ─── ALL reducers in one dispatch table ───
pub fn dispatch(state: &mut State, event: &Event) -> Result<(), String> {
    // Helper to verify model existence
    fn ensure_model(state: &State, model_id: &str) -> Result<(), String> {
        if state.models.iter().any(|m| m.id == model_id) {
            Ok(())
        } else {
            Err(format!("Model not found: {}", model_id))
        }
    }

    match event {
        Event::DaemonStarted { .. } => {
            state.diagnostics = Default::default();
            Ok(())
        }
        Event::ModelAdded { model, .. } => {
            if state.models.iter().any(|m| m.id == model.id) {
                return Ok(()); // idempotent
            }
            state.models.push(model.clone());
            state.health.insert(model.id.clone(), HealthStatus::new());
            state.inflight.insert(model.id.clone(), 0);
            Ok(())
        }
        Event::ModelRemoved { model_id, .. } => {
            state.models.retain(|m| m.id != *model_id);
            state.health.remove(model_id);
            state.inflight.remove(model_id);
            Ok(())
        }
        Event::ModelSelected { model_id, .. } => {
            ensure_model(state, model_id)?;
            *state.inflight.entry(model_id.clone()).or_insert(0) += 1;
            Ok(())
        }
        Event::CompletionFinished { model_id, latency_ms, tokens_used: _, success } => {
            ensure_model(state, model_id)?;
            let now = state.current_time();
            let success_ratio = compute_success_ratio(state, model_id);
            if let Some(health) = state.health.get_mut(model_id) {
                if *success {
                    health.last_success = now;
                    health.rolling_latency_avg_ms = windowed_avg(health.rolling_latency_avg_ms, *latency_ms as f64, 20);
                } else {
                    health.last_failure = now;
                }
                health.success_ratio = success_ratio;
                health.garbage = is_garbage(health);
            }
            if let Some(count) = state.inflight.get_mut(model_id) {
                *count = count.saturating_sub(1);
            }
            state.diagnostics.total_completions += 1;
            if !success {
                state.diagnostics.total_failures += 1;
            }
            Ok(())
        }
        Event::ModelFailed { model_id, .. } => {
            ensure_model(state, model_id)?;
            let now = state.current_time();
            let success_ratio = compute_success_ratio(state, model_id);
            if let Some(health) = state.health.get_mut(model_id) {
                health.last_failure = now;
                health.success_ratio = success_ratio;
                health.garbage = is_garbage(health);
            }
            state.diagnostics.total_failures += 1;
            Ok(())
        }
        Event::ProbeUpdated { model_id, health: _, latency_ms } => {
            ensure_model(state, model_id)?;
            let now = state.current_time();
            if let Some(health) = state.health.get_mut(model_id) {
                health.last_success = now;
                health.rolling_latency_avg_ms = windowed_avg(health.rolling_latency_avg_ms, *latency_ms as f64, 10);
            }
            Ok(())
        }
        Event::ProbeFailed { model_id, .. } => {
            ensure_model(state, model_id)?;
            let now = state.current_time();
            if let Some(health) = state.health.get_mut(model_id) {
                health.last_failure = now;
                health.garbage = is_garbage(health);
            }
            Ok(())
        }
        Event::FallbackTriggered { from, to, .. } => {
            state.routing_cache.last_fallback = Some(omrp_types::routing::FallbackEntry { from: from.clone(), to: to.clone(), at: state.current_time() });
            state.diagnostics.total_fallbacks += 1;
            Ok(())
        }
        Event::DegradeModeEnabled { .. } => {
            state.diagnostics.total_degradations += 1;
            Ok(())
        }
        Event::CompletionRequested { model_id, .. } => {
            ensure_model(state, model_id)?;
            *state.inflight.entry(model_id.clone()).or_insert(0) += 1;
            Ok(())
        }
        Event::ReportReceived { model_id, success, latency_ms, .. } => {
            ensure_model(state, model_id)?;
            let now = state.current_time();
            let success_ratio = compute_success_ratio(state, model_id);
            if let Some(health) = state.health.get_mut(model_id) {
                if *success {
                    health.last_success = now;
                    health.rolling_latency_avg_ms = windowed_avg(health.rolling_latency_avg_ms, *latency_ms as f64, 20);
                } else {
                    health.last_failure = now;
                }
                health.success_ratio = success_ratio;
                health.garbage = is_garbage(health);
            }
            Ok(())
        }
        Event::ConfigReloaded { .. } | Event::DaemonStopped { .. } => {
            // no state mutation needed
            Ok(())
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
            let _ = dispatch(&mut state, event);
        }
        state
    }

    #[test]
    fn test_model_added_creates_entries() {
        let state = make_state(&[Event::ModelAdded { model: Model::new("test/model", "test"), source: ModelSource::Bundled }]);
        assert_eq!(state.models.len(), 1);
        assert!(state.health.contains_key("test/model"));
        assert_eq!(*state.inflight.get("test/model").unwrap(), 0);
    }

    #[test]
    fn test_model_removed_cleans_up() {
        let events = vec![
            Event::ModelAdded { model: Model::new("test/model", "test"), source: ModelSource::Bundled },
            Event::ModelRemoved { model_id: "test/model".into(), reason: "test".into() },
        ];
        let state = make_state(&events);
        assert!(state.models.is_empty());
        assert!(!state.health.contains_key("test/model"));
    }

    #[test]
    fn test_completion_success_updates_health() {
        let events = vec![
            Event::ModelAdded { model: Model::new("test/model", "test"), source: ModelSource::Bundled },
            Event::CompletionFinished { model_id: "test/model".into(), latency_ms: 500, tokens_used: 100, success: true },
        ];
        let state = make_state(&events);
        let health = state.health.get("test/model").unwrap();
        assert!(health.last_success != omrp_types::time::SequencedInstant::EPOCH);
        assert!(health.rolling_latency_avg_ms > 0.0);
    }

    #[test]
    fn test_completion_failure_tracks_failure() {
        let events = vec![
            Event::ModelAdded { model: Model::new("test/model", "test"), source: ModelSource::Bundled },
            Event::CompletionFinished { model_id: "test/model".into(), latency_ms: 500, tokens_used: 100, success: false },
        ];
        let state = make_state(&events);
        assert_eq!(state.diagnostics.total_failures, 1);
    }

    #[test]
    fn test_model_failed_sets_garbage_after_threshold() {
        let mut events = vec![
            Event::ModelAdded { model: Model::new("test/model", "test"), source: ModelSource::Bundled },
        ];
        // 10 failures should set garbage
        for _ in 0..10 {
            events.push(Event::ModelFailed { model_id: "test/model".into(), error: omrp_events::error::ErrorKind::Timeout { timeout_ms: 5000 } });
        }
        let state = make_state(&events);
        let health = state.health.get("test/model").unwrap();
        assert!(health.garbage, "model should be garbage after 10 failures");
        }
    }
}
