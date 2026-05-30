use serde::{Deserialize, Serialize};
use omrp_types::model::{Model, ModelId};
use omrp_types::routing::RoutingReason;
use omrp_types::task::{RouteRequest, TaskType};
use crate::error::ErrorKind;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ModelSource {
    Bundled,
    LocalConfig,
    UserContributed,
    AutoDiscovered,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Event {
    // ─── Lifecycle ───
    DaemonStarted { version: String },
    DaemonStopped { reason: String },

    // ─── Model Discovery ───
    ModelAdded { model: Model, source: ModelSource },
    ModelRemoved { model_id: ModelId, reason: String },
    ConfigReloaded { source: String },

    // ─── Routing ───
    ModelSelected {
        model_id: ModelId,
        request: RouteRequest,
        score: f64,
        reason: RoutingReason,
    },
    FallbackTriggered {
        from: ModelId,
        to: ModelId,
        cause: String,
    },
    DegradeModeEnabled { model_id: ModelId, reason: String },

    // ─── Completion ───
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
    ModelFailed {
        model_id: ModelId,
        error: ErrorKind,
    },

    // ─── Telemetry ───
    ProbeUpdated {
        model_id: ModelId,
        health: f32,
        latency_ms: u64,
    },
    ProbeFailed {
        model_id: ModelId,
        error: String,
    },
    ReportReceived {
        model_id: ModelId,
        success: bool,
        latency_ms: u64,
        tokens: u64,
    },
}
