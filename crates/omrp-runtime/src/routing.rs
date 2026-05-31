//! Shared LLM-routing helpers used by both the CLI (`main.rs`) and the
//! axum web server (`web_server.rs`).
//!
//! These functions were originally inlined in `main.rs`; they are extracted
//! here so the web server can reuse the same tier-aware routing logic.

use std::path::Path;

use omrp_core::classifier::PromptTier;
use omrp_core::pipeline::EventPipeline;
use omrp_core::router::RouterEngine;
use omrp_core::state::State;
use omrp_types::routing::RoutingDecision;
use omrp_events::event::Event;
use omrp_types::task::RouteRequest;

use crate::config::Config;

// ─── Pipeline bootstrap ───────────────────────────────────────────────────────

/// Build (or restore from ledger) an `EventPipeline` seeded with the models
/// defined in `cfg`.  Models already present in the ledger are not duplicated.
pub fn bootstrap_pipeline(cfg: &Config) -> EventPipeline {
    let ledger_path = cfg.ledger_path();
    let mut pipeline = EventPipeline::load_from_ledger(&ledger_path)
        .unwrap_or_else(|_| EventPipeline::new());
    let existing: Vec<String> =
        pipeline.state().read(|s| s.models.iter().map(|m| m.id.clone()).collect());
    for event in cfg.to_model_events() {
        if let Event::ModelAdded { ref model, .. } = event {
            if !existing.contains(&model.id) {
                let _ = pipeline.process(event);
            }
        }
    }
    pipeline
}

/// Persist the pipeline ledger, printing a warning on failure.
#[allow(dead_code)]
pub fn save_ledger(pipeline: &EventPipeline, path: &Path) {
    if let Err(e) = pipeline.save_to_ledger(path) {
        eprintln!("Warning: could not save ledger: {e}");
    }
}

// ─── Prompt utilities ─────────────────────────────────────────────────────────

/// Rough token estimate: 1 token ≈ 4 bytes of UTF-8.
#[allow(dead_code)]
pub fn estimate_tokens(text: &str) -> u32 {
    (text.len() as u32 / 4).max(1)
}

// ─── Tier helpers ─────────────────────────────────────────────────────────────

/// Parse a tier name (case-insensitive) into a `PromptTier`.
pub fn tier_from_str(s: &str) -> PromptTier {
    match s.trim().to_lowercase().as_str() {
        "simple"    => PromptTier::Simple,
        "complex"   => PromptTier::Complex,
        "reasoning" => PromptTier::Reasoning,
        _           => PromptTier::Medium,
    }
}

/// Select the best model for `tier`, falling back to the full pool if no
/// tier-specific model is available.
pub fn select_for_tier(
    state: &State,
    request: &RouteRequest,
    _tier: PromptTier,
    tier_model_ids: &[String],
    router: &RouterEngine,
) -> RoutingDecision {
    if !tier_model_ids.is_empty() {
        let mut ts = state.clone();
        ts.models.retain(|m| tier_model_ids.contains(&m.id));
        if !ts.models.is_empty() {
            let d = router.select(&ts, request);
            if !d.selected_model.is_empty() {
                return d;
            }
        }
    }
    router.select(state, request)
}

/// IDs of all config models whose tier matches `tier`.
pub fn tier_model_ids(cfg: &Config, tier: PromptTier) -> Vec<String> {
    cfg.model
        .iter()
        .filter(|m| tier_from_str(&m.tier) == tier)
        .map(|m| m.id.clone())
        .collect()
}
