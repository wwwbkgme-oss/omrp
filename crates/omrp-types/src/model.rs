//! Model definitions for omrp-types crate.

/// Alias for a model identifier.
pub type ModelId = String;

use serde::{Deserialize, Serialize};
use crate::task::TaskType;

/// Capabilities of a model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    /// Task types this model is suitable for.
    pub task_suitability: Vec<TaskType>,
    /// Whether the model can process vision inputs.
    pub supports_vision: bool,
    /// Whether the model can use tool calls.
    pub supports_tool_use: bool,
    /// Size of the context window in tokens.
    pub context_window: u32,
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

/// Representation of a model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Model {
    /// Unique identifier for the model.
    pub id: ModelId,
    /// Provider name (e.g., "openai", "anthropic").
    pub provider: String,
    /// Capabilities of the model.
    pub capabilities: ModelCapabilities,
}

impl Model {
    /// Create a new `Model` with the given id and provider.
    ///
    /// Capabilities are set to their default values.
    pub fn new(id: impl Into<ModelId>, provider: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            provider: provider.into(),
            capabilities: ModelCapabilities::default(),
        }
    }
}
