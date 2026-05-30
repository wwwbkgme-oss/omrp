use serde::{Deserialize, Serialize};
use crate::event::Event;
use crate::error::ErrorKind;

/// Validation errors for `Event` variants.
#[derive(Debug, Serialize, Deserialize)]
pub enum ValidationError {
    // General
    EmptyStringField(&'static str),
    NegativeNumber(&'static str),
    // Specific variant errors
    DaemonStartedEmptyVersion,
    DaemonStoppedEmptyReason,
    ModelAddedInvalidModel,
    ModelRemovedEmptyReason,
    ConfigReloadedEmptySource,
    ModelSelectedNegativeScore,
    FallbackTriggeredEmptyCause,
    DegradeModeEnabledEmptyReason,
    CompletionRequestedEmptyPrompt,
    CompletionFinishedNegativeLatency,
    CompletionFinishedNegativeTokens,
    ModelFailedInvalidError,
    ProbeUpdatedInvalidHealth,
    ProbeUpdatedNegativeLatency,
    ProbeFailedEmptyError,
    ReportReceivedNegativeLatency,
    ReportReceivedNegativeTokens,
}

/// Validate an `Event` instance.
///
/// Returns `Ok(())` if the event satisfies all basic validation rules, otherwise
/// returns a `ValidationError` describing the first failure encountered.
pub fn validate(event: &Event) -> Result<(), ValidationError> {
    match event {
        // ─── Lifecycle ───
        Event::DaemonStarted { version } => {
            if version.trim().is_empty() {
                return Err(ValidationError::DaemonStartedEmptyVersion);
            }
            Ok(())
        }
        Event::DaemonStopped { reason } => {
            if reason.trim().is_empty() {
                return Err(ValidationError::DaemonStoppedEmptyReason);
            }
            Ok(())
        }
        // ─── Model Discovery ───
        Event::ModelAdded { model: _, source: _ } => {
            // Assuming `Model` validation is handled elsewhere.
            Ok(())
        }
        Event::ModelRemoved { model_id: _, reason } => {
            if reason.trim().is_empty() {
                return Err(ValidationError::ModelRemovedEmptyReason);
            }
            Ok(())
        }
        Event::ConfigReloaded { source } => {
            if source.trim().is_empty() {
                return Err(ValidationError::ConfigReloadedEmptySource);
            }
            Ok(())
        }
        // ─── Routing ───
        Event::ModelSelected {
            model_id: _,
            request: _,
            score,
            reason: _,
        } => {
            if *score < 0.0 {
                return Err(ValidationError::ModelSelectedNegativeScore);
            }
            Ok(())
        }
        Event::FallbackTriggered { from: _, to: _, cause } => {
            if cause.trim().is_empty() {
                return Err(ValidationError::FallbackTriggeredEmptyCause);
            }
            Ok(())
        }
        Event::DegradeModeEnabled { model_id: _, reason } => {
            if reason.trim().is_empty() {
                return Err(ValidationError::DegradeModeEnabledEmptyReason);
            }
            Ok(())
        }
        // ─── Completion ───
        Event::CompletionRequested {
            model_id: _,
            task_type: _,
            prompt_tokens,
        } => {
            // `prompt_tokens` should be non‑zero.
            if *prompt_tokens == 0 {
                return Err(ValidationError::CompletionRequestedEmptyPrompt);
            }
            Ok(())
        }
        Event::CompletionFinished {
            model_id: _,
            latency_ms,
            tokens_used,
            success: _,
        } => {
            if *latency_ms == 0 {
                return Err(ValidationError::CompletionFinishedNegativeLatency);
            }
            if *tokens_used == 0 {
                return Err(ValidationError::CompletionFinishedNegativeTokens);
            }
            Ok(())
        }
        Event::ModelFailed { model_id: _, error } => {
            // `ErrorKind` is assumed to be valid; we only check for a placeholder variant.
            let _ = error; // validation passes for all error kinds
            Ok(())
        }
        // ─── Telemetry ───
        Event::ProbeUpdated {
            model_id: _,
            health,
            latency_ms,
        } => {
            if *health < 0.0 || *health > 1.0 {
                return Err(ValidationError::ProbeUpdatedInvalidHealth);
            }
            if *latency_ms == 0 {
                return Err(ValidationError::ProbeUpdatedNegativeLatency);
            }
            Ok(())
        }
        Event::ProbeFailed { model_id: _, error } => {
            if error.trim().is_empty() {
                return Err(ValidationError::ProbeFailedEmptyError);
            }
            Ok(())
        }
        Event::ReportReceived {
            model_id: _,
            success: _,
            latency_ms,
            tokens,
        } => {
            if *latency_ms == 0 {
                return Err(ValidationError::ReportReceivedNegativeLatency);
            }
            if *tokens == 0 {
                return Err(ValidationError::ReportReceivedNegativeTokens);
            }
            Ok(())
        }
    }
}
