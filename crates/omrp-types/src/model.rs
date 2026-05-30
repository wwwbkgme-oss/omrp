use serde::{Deserialize, Serialize};
use crate::task::TaskType;

pub type ModelId = String;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Model {
    pub id: ModelId,
    pub provider: String,
    pub capabilities: ModelCapabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelCapabilities {
    pub task_suitability: Vec<TaskType>,
    pub supports_vision: bool,
    pub supports_tool_use: bool,
    pub context_window: u32,
}

impl Model {
    pub fn new(id: impl Into<String>, provider: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            provider: provider.into(),
            capabilities: ModelCapabilities::default(),
        }
    }
}

impl Default for ModelCapabilities {
    fn default() -> Self {
        Self {
            task_suitability: vec![TaskType::Chat],
            supports_vision: false,
            supports_tool_use: false,
            context_window: 4096,
        }
    }
}
