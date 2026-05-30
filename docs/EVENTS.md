# OMRP Events

## Event Enum

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Event {
    // Lifecycle
    DaemonStarted { version: String },
    DaemonStopped { reason: String },

    // Model Discovery
    ModelAdded { model: Model, source: ModelSource },
    ModelRemoved { model_id: ModelId, reason: String },
    ConfigReloaded { source: String },

    // Routing
    ModelSelected {
        model_id: ModelId,
        request: RouteRequest,
        score: f64,
        reason: RoutingReason,
    },
    FallbackTriggered { from: ModelId, to: ModelId, cause: String },
    DegradeModeEnabled { model_id: ModelId, reason: String },

    // Completion
    CompletionRequested {
        model_id: ModelId,
        task_type: TaskType,
        prompt_tokens: u32,
    },
    CompletionFinished {
        model_id: ModelId,
        latency_ms: u64,
        tokens_used: u64,
        success: bool,
    },
    ModelFailed { model_id: ModelId, error: ErrorKind },

    // Telemetry
    ProbeUpdated { model_id: ModelId, health: f32, latency_ms: u64 },
    ProbeFailed { model_id: ModelId, error: String },
    ReportReceived {
        model_id: ModelId,
        success: bool,
        latency_ms: u64,
        tokens: u64,
    },
}
```

## ModelSource

```rust
pub enum ModelSource {
    Bundled,
    LocalConfig,
    UserContributed,
    AutoDiscovered,
}
```

## ErrorKind

```rust
pub enum ErrorKind {
    RateLimited { retry_after: Option<u64> },
    Timeout { timeout_ms: u64 },
    AuthError,
    ModelNotAvailable,
    NetworkError(String),
    InternalError(String),
}
```

## Event Validation

Events are validated before processing:

| Event | Validation Rule |
|-------|---------------|
| ModelAdded | model.id not empty, model.provider not empty |
| ModelRemoved | model_id not empty |
| CompletionRequested | prompt_tokens > 0 |
| ProbeUpdated | health in range 0.0..=1.0 |