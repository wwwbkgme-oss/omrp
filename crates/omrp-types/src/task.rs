use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TaskType {
    Code,
    Reasoning,
    Chat,
    Vision,
    Analysis,
}

impl TaskType {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskType::Code => "code",
            TaskType::Reasoning => "reasoning",
            TaskType::Chat => "chat",
            TaskType::Vision => "vision",
            TaskType::Analysis => "analysis",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RouteRequest {
    pub task_type: TaskType,
    pub max_latency_ms: Option<u64>,
    pub require_vision: bool,
    pub require_tool_use: bool,
    pub min_context_window: Option<u32>,
    pub max_inflight_per_model: Option<u32>,
}

impl Default for RouteRequest {
    fn default() -> Self {
        Self {
            task_type: TaskType::Chat,
            max_latency_ms: None,
            require_vision: false,
            require_tool_use: false,
            min_context_window: None,
            max_inflight_per_model: Some(3),
        }
    }
}
