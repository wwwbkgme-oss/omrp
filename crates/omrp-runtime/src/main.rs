//! OMRP CLI — deterministic LLM routing engine (Phase 1)
//!
//! Usage:
//!   omrp models               List all registered models
//!   omrp status               Show health/scoring summary for every model
//!   omrp best <task>          Print the best model for a task type
//!                             task: code | reasoning | chat | vision | analysis

use omrp_core::pipeline::EventPipeline;
use omrp_core::router::RouterEngine;
use omrp_events::event::{Event, ModelSource};
use omrp_types::model::{Model, ModelCapabilities};
use omrp_types::task::{RouteRequest, TaskType};

// ─── Demo bootstrap ──────────────────────────────────────────────────────────

/// Build a demo `EventPipeline` seeded with three models and representative events.
///
/// This function is the ONLY source of state in Phase 1 (no persistent ledger
/// is used at runtime yet — that is Phase 2 work).
fn demo_pipeline() -> EventPipeline {
    let mut p = EventPipeline::new();

    // ── Register demo models ──────────────────────────────────────────────

    let models: Vec<(Model, Vec<Event>)> = vec![
        (
            Model {
                id: "openrouter/claude-3-5-sonnet".into(),
                provider: "openrouter".into(),
                capabilities: ModelCapabilities {
                    task_suitability: vec![TaskType::Code, TaskType::Reasoning, TaskType::Chat, TaskType::Analysis],
                    supports_vision: false,
                    supports_tool_use: true,
                    context_window: 200_000,
                },
            },
            vec![
                Event::ProbeUpdated { model_id: "openrouter/claude-3-5-sonnet".into(), health: 0.99, latency_ms: 820 },
                Event::CompletionFinished { model_id: "openrouter/claude-3-5-sonnet".into(), latency_ms: 900, tokens_used: 350, success: true },
                Event::CompletionFinished { model_id: "openrouter/claude-3-5-sonnet".into(), latency_ms: 750, tokens_used: 280, success: true },
                Event::CompletionFinished { model_id: "openrouter/claude-3-5-sonnet".into(), latency_ms: 1100, tokens_used: 420, success: true },
            ],
        ),
        (
            Model {
                id: "openrouter/gpt-4o".into(),
                provider: "openrouter".into(),
                capabilities: ModelCapabilities {
                    task_suitability: vec![TaskType::Chat, TaskType::Vision, TaskType::Analysis],
                    supports_vision: true,
                    supports_tool_use: true,
                    context_window: 128_000,
                },
            },
            vec![
                Event::ProbeUpdated { model_id: "openrouter/gpt-4o".into(), health: 0.95, latency_ms: 1200 },
                Event::CompletionFinished { model_id: "openrouter/gpt-4o".into(), latency_ms: 1300, tokens_used: 310, success: true },
                Event::CompletionFinished { model_id: "openrouter/gpt-4o".into(), latency_ms: 2100, tokens_used: 500, success: false },
                Event::CompletionFinished { model_id: "openrouter/gpt-4o".into(), latency_ms: 1150, tokens_used: 290, success: true },
            ],
        ),
        (
            Model {
                id: "qwen/qwen-2-5-72b".into(),
                provider: "qwen".into(),
                capabilities: ModelCapabilities {
                    task_suitability: vec![TaskType::Code, TaskType::Chat, TaskType::Reasoning],
                    supports_vision: false,
                    supports_tool_use: false,
                    context_window: 32_768,
                },
            },
            vec![
                Event::ProbeUpdated { model_id: "qwen/qwen-2-5-72b".into(), health: 0.92, latency_ms: 480 },
                Event::CompletionFinished { model_id: "qwen/qwen-2-5-72b".into(), latency_ms: 510, tokens_used: 200, success: true },
                Event::CompletionFinished { model_id: "qwen/qwen-2-5-72b".into(), latency_ms: 495, tokens_used: 180, success: true },
                Event::CompletionFinished { model_id: "qwen/qwen-2-5-72b".into(), latency_ms: 530, tokens_used: 210, success: true },
            ],
        ),
    ];

    for (model, events) in models {
        let model_id = model.id.clone();
        let _ = p.process(Event::ModelAdded {
            model,
            source: ModelSource::Bundled,
        });
        for event in events {
            // Probe and completion events — ignore validation errors (demo data is valid).
            let _ = p.process(event);
        }
        // Simulate 1 inflight request resolved to bring inflight back to 0.
        let _ = p.process(Event::CompletionFinished {
            model_id,
            latency_ms: 600,
            tokens_used: 150,
            success: true,
        });
    }

    p
}

// ─── Command handlers ─────────────────────────────────────────────────────────

/// `omrp models` — print the model registry.
fn cmd_models(pipeline: &EventPipeline) {
    pipeline.state().read(|state| {
        println!("{:<40} {:<15} {:<8} {:>10}  {}", "Model ID", "Provider", "Vision", "CtxWindow", "Tasks");
        println!("{}", "-".repeat(90));
        for m in &state.models {
            let tasks: Vec<&str> = m.capabilities.task_suitability.iter()
                .map(|t| t.as_str())
                .collect();
            println!(
                "{:<40} {:<15} {:<8} {:>10}  {}",
                m.id,
                m.provider,
                if m.capabilities.supports_vision { "yes" } else { "no" },
                m.capabilities.context_window,
                tasks.join(", "),
            );
        }
        println!("\n{} model(s) registered.", state.models.len());
    });
}

/// `omrp status` — print health/scoring summary.
fn cmd_status(pipeline: &EventPipeline) {
    let router = RouterEngine::default();
    pipeline.state().read(|state| {
        let request = RouteRequest::default();
        let decision = router.select(state, &request);

        println!("{:<40} {:>8}  {:>8}  {:>8}  {}", "Model ID", "Score", "Latency", "Ratio", "Garbage?");
        println!("{}", "-".repeat(80));

        // Print all scores from the routing decision (already sorted best-first).
        for ms in &decision.scores {
            let health = state.health.get(&ms.model_id);
            let latency = health.map(|h| h.rolling_latency_avg_ms as u64).unwrap_or(0);
            let ratio = health.map(|h| h.success_ratio).unwrap_or(0.5);
            let garbage = health.map(|h| h.garbage).unwrap_or(false);
            println!(
                "{:<40} {:>8.3}  {:>7}ms  {:>7.1}%  {}",
                ms.model_id,
                ms.total,
                latency,
                ratio * 100.0,
                if garbage { "GARBAGE" } else { "ok" },
            );
        }
        println!("\nSelected for default (chat) request: {}", decision.selected_model);
    });
}

/// `omrp best <task>` — print routing decision for a task type.
fn cmd_best(pipeline: &EventPipeline, task_str: &str) {
    let task_type = match task_str.to_lowercase().as_str() {
        "code"      => TaskType::Code,
        "reasoning" => TaskType::Reasoning,
        "chat"      => TaskType::Chat,
        "vision"    => TaskType::Vision,
        "analysis"  => TaskType::Analysis,
        other => {
            eprintln!("Unknown task type: {other:?}");
            eprintln!("Valid task types: code, reasoning, chat, vision, analysis");
            std::process::exit(1);
        }
    };

    let router = RouterEngine::default();
    pipeline.state().read(|state| {
        let request = RouteRequest {
            task_type,
            max_inflight_per_model: Some(3),
            ..Default::default()
        };
        let decision = router.select(state, &request);

        if decision.selected_model.is_empty() {
            println!("No suitable model found for task: {}", task_str);
            return;
        }

        println!("Best model for {task_str:?}: {}", decision.selected_model);
        println!("Score: {:.3}", decision.score);
        println!("\nScore breakdown:");
        for factor in &decision.reasoning {
            println!(
                "  {:15} value={:.3}  weight={:.2}  contribution={:.3}",
                factor.name, factor.value, factor.weight, factor.contribution()
            );
        }
        if decision.fallback_chain.len() > 1 {
            let fallbacks: Vec<&str> = decision.fallback_chain[1..]
                .iter().map(|s| s.as_str()).collect();
            println!("\nFallback chain: {}", fallbacks.join(" → "));
        }
    });
}

// ─── Entry point ─────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        print_usage();
        std::process::exit(1);
    }

    let pipeline = demo_pipeline();

    match args[1].as_str() {
        "models" => cmd_models(&pipeline),
        "status" => cmd_status(&pipeline),
        "best" => {
            if args.len() < 3 {
                eprintln!("Usage: omrp best <task>");
                eprintln!("  task: code | reasoning | chat | vision | analysis");
                std::process::exit(1);
            }
            cmd_best(&pipeline, &args[2]);
        }
        other => {
            eprintln!("Unknown command: {other:?}");
            print_usage();
            std::process::exit(1);
        }
    }
}

fn print_usage() {
    eprintln!("OMRP — Open Model Routing Protocol (Phase 1)");
    eprintln!();
    eprintln!("Usage:");
    eprintln!("  omrp models           List all registered models");
    eprintln!("  omrp status           Show health and routing scores");
    eprintln!("  omrp best <task>      Best model for a task type");
    eprintln!();
    eprintln!("Task types: code, reasoning, chat, vision, analysis");
}
