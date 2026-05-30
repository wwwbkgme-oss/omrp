use crate::event::Event;

#[derive(Debug, Clone, PartialEq)]
pub enum ValidationError {
    InvalidEvent(&'static str),
}

pub fn validate(event: &Event) -> Result<(), ValidationError> {
    match event {
        Event::ModelAdded { model, .. } => {
            if model.id.is_empty() {
                return Err(ValidationError::InvalidEvent("model id must not be empty"));
            }
            if model.provider.is_empty() {
                return Err(ValidationError::InvalidEvent("model provider must not be empty"));
            }
        }
        Event::ModelRemoved { model_id, .. } => {
            if model_id.is_empty() {
                return Err(ValidationError::InvalidEvent("model_id must not be empty"));
            }
        }
        Event::CompletionRequested { prompt_tokens, .. } => {
            if *prompt_tokens == 0 {
                return Err(ValidationError::InvalidEvent("prompt_tokens must be > 0"));
            }
        }
        Event::ProbeUpdated { health, .. } => {
            if !(0.0..=1.0).contains(health) {
                return Err(ValidationError::InvalidEvent("health must be 0.0..=1.0"));
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::*;
    use omrp_types::model::*;

    #[test]
    fn test_valid_model_added() {
        let event = Event::ModelAdded {
            model: Model::new("openrouter/o4-mini", "openrouter"),
            source: ModelSource::Bundled,
        };
        assert_eq!(validate(&event), Ok(()));
    }

    #[test]
    fn test_invalid_empty_model_id() {
        let event = Event::ModelAdded {
            model: Model::new("", "openrouter"),
            source: ModelSource::Bundled,
        };
        assert_eq!(validate(&event), Err(ValidationError::InvalidEvent("model id must not be empty")));
    }

    #[test]
    fn test_invalid_health_range() {
        let event = Event::ProbeUpdated {
            model_id: "test".into(),
            health: 1.5,
            latency_ms: 100,
        };
        assert_eq!(validate(&event), Err(ValidationError::InvalidEvent("health must be 0.0..=1.0")));
    }
}
