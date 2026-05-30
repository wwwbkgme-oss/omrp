use std::sync::{Arc, RwLock};
use omrp_events::event::Event;
use omrp_events::validate::{validate, ValidationError};
use crate::state::State;
use crate::reducers::dispatch;

/// Read-only projection view for safe shared access.
#[derive(Debug, Clone)]
pub struct ProjectionView<T> {
    inner: Arc<RwLock<T>>,
}

impl<T> ProjectionView<T> {
    pub fn new(state: T) -> Self {
        Self { inner: Arc::new(RwLock::new(state)) }
    }

    pub fn read<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        f(&self.inner.read().expect("Poisoned lock"))
    }

    pub fn write(&self, f: impl FnOnce(&mut T)) {
        f(&mut self.inner.write().expect("Poisoned lock"));
    }
}

/// Event pipeline errors.
#[derive(Debug)]
pub enum PipelineError {
    Validation(ValidationError),
    Ledger(String),
}

impl std::fmt::Display for PipelineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PipelineError::Validation(e) => write!(f, "validation error: {:?}", e),
            PipelineError::Ledger(s) => write!(f, "ledger error: {}", s),
        }
    }
}

impl std::error::Error for PipelineError {}

/// The canonical execution pipeline.
/// Pipeline: Validate → Apply → Project
/// Phase 1: No ledger persistence yet. Just validate → apply → project.
pub struct EventPipeline {
    state: ProjectionView<State>,
    event_log: Vec<Event>,
}

impl EventPipeline {
    pub fn new() -> Self {
        Self {
            state: ProjectionView::new(State::new()),
            event_log: Vec::new(),
        }
    }

    /// Process a single event through the pipeline.
    pub fn process(&mut self, event: Event) -> Result<(), PipelineError> {
        // 1. Validate
        validate(&event).map_err(PipelineError::Validation)?;

        // 2. Persist
        self.event_log.push(event.clone());

        // 3. Apply
        self.state.write(|state| dispatch(state, &event));

        Ok(())
    }

    /// Get a read-only view of current state.
    pub fn state(&self) -> &ProjectionView<State> {
        &self.state
    }

    /// Get all events (for replay).
    pub fn event_log(&self) -> &[Event] {
        &self.event_log
    }

    /// Full replay: rebuild state from event log.
    /// Identical to processing events in order.
    pub fn replay(&self) -> State {
        let mut state = State::new();
        for event in &self.event_log {
            dispatch(&mut state, event);
        }
        state
    }

    /// Verify that replay produces the same state as live processing.
    pub fn verify_replay(&self) -> bool {
        let live = self.state.read(|s| s.clone());
        let replayed = self.replay();
        live == replayed
    }

    // ── Persistence ───────────────────────────────────────────────────────

    /// Load a pipeline from an existing ledger file, replaying all events to
    /// reconstruct `State`.  If the file does not exist an empty pipeline is
    /// returned (first-run case).
    ///
    /// Stored events bypass validation: they were already validated when first
    /// processed, and re-validating would reject semantically valid history
    /// (e.g. zero-latency seed events used in tests).
    pub fn load_from_ledger(path: &std::path::Path) -> Result<Self, crate::ledger::LedgerError> {
        let ledger = crate::ledger::LedgerStore::load(path.to_path_buf())?;
        let events = ledger.replay();
        let mut pipeline = Self::new();
        for event in events {
            // Bypass validation — apply directly.
            pipeline.state.write(|state| dispatch(state, &event));
            pipeline.event_log.push(event);
        }
        Ok(pipeline)
    }

    /// Persist the current event log to a `LedgerStore` (JSON Lines file).
    ///
    /// Creates parent directories automatically.  On every call the file is
    /// rewritten from scratch (Phase 2 design; segmented append comes later).
    pub fn save_to_ledger(&self, path: &std::path::Path) -> Result<(), crate::ledger::LedgerError> {
        let mut store = crate::ledger::LedgerStore::new(path.to_path_buf());
        for event in &self.event_log {
            store.append(event.clone());
        }
        store.persist()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omrp_events::event::{Event, ModelSource};
    use omrp_types::model::Model;

    #[test]
    fn test_pipeline_process_event() {
        let mut pipeline = EventPipeline::new();
        pipeline.process(Event::ModelAdded {
            model: Model::new("test/model", "test"),
            source: ModelSource::Bundled,
        }).unwrap();

        let model_count = pipeline.state().read(|s| s.models.len());
        assert_eq!(model_count, 1);
    }

    #[test]
    fn test_pipeline_replay_identity() {
        let mut pipeline = EventPipeline::new();

        pipeline.process(Event::ModelAdded {
            model: Model::new("m1", "p1"),
            source: ModelSource::Bundled,
        }).unwrap();

        pipeline.process(Event::ProbeUpdated {
            model_id: "m1".into(),
            health: 0.9,
            latency_ms: 1000,
        }).unwrap();

        pipeline.process(Event::CompletionFinished {
            model_id: "m1".into(),
            latency_ms: 500,
            tokens_used: 100,
            success: true,
        }).unwrap();

        assert!(pipeline.verify_replay(), "replay must match live state");
    }

    #[test]
    fn test_pipeline_invalid_event_rejected() {
        let mut pipeline = EventPipeline::new();
        let result = pipeline.process(Event::ProbeUpdated {
            model_id: "m1".into(),
            health: 1.5, // invalid: > 1.0
            latency_ms: 100,
        });
        assert!(result.is_err());
    }

    #[test]
    fn test_pipeline_empty_log() {
        let pipeline = EventPipeline::new();
        assert!(pipeline.event_log().is_empty());
        let state = pipeline.replay();
        assert!(state.models.is_empty());
    }
}
