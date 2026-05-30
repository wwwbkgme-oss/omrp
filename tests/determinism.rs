use omrp_core::state::State;
use omrp_core::reducers::dispatch;
use omrp_events::event::{Event, ModelSource};
use omrp_types::model::Model;

/// THE determinism contract.
/// Same events → identical state. Always.
fn replay(events: &[Event]) -> State {
    let mut state = State::new();
    for event in events {
        dispatch(&mut state, event);
    }
    state
}

#[test]
fn test_replay_identity() {
    let events = vec![
        Event::ModelAdded {
            model: Model::new("openrouter/o4-mini", "openrouter"),
            source: ModelSource::Bundled,
        },
        Event::ProbeUpdated {
            model_id: "openrouter/o4-mini".into(),
            health: 0.9,
            latency_ms: 1200,
        },
        Event::CompletionFinished {
            model_id: "openrouter/o4-mini".into(),
            latency_ms: 1500,
            tokens_used: 250,
            success: true,
        },
    ];

    let state_a = replay(&events);
    let state_b = replay(&events);

    assert_eq!(serde_json::to_value(&state_a).unwrap(), serde_json::to_value(&state_b).unwrap(), "Replay must produce identical state");
}

#[test]
fn test_empty_replay() {
    let state = replay(&[]);
    assert!(state.models.is_empty());
    assert!(state.health.is_empty());
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
    let state = replay(&events);
    assert_eq!(state.models.len(), 1, "model should only be added once");
}

#[test]
fn test_determinism_fuzz() {
    use omrp_types::model::ModelCapabilities;
    use omrp_types::task::TaskType;
    use omrp_events::event::{Event, ModelSource};

    let models = vec![
        ("openrouter/o4-mini", "openrouter"),
        ("kilo/kilo-7b", "kilo"),
        ("qwen/qwen-7b", "qwen"),
    ];

    let mut events = Vec::new();
    for (id, provider) in &models {
        events.push(Event::ModelAdded {
            model: Model::new(*id, *provider),
            source: ModelSource::Bundled,
        });
    }

    for (id, _) in &models {
        events.push(Event::ProbeUpdated {
            model_id: (*id).into(),
            health: 0.8,
            latency_ms: 1000,
        });
        events.push(Event::CompletionFinished {
            model_id: (*id).into(),
            latency_ms: 1200,
            tokens_used: 100,
            success: true,
        });
    }

    // Run twice → must be identical
    let a = replay(&events);
    let b = replay(&events);

    assert_eq!(serde_json::to_value(&a).unwrap(), serde_json::to_value(&b).unwrap(), "FUZZ: replay must be identical");
}
