//! OMRP CLI — deterministic LLM router
//!
//! Usage:
//!   omrp route [--task <type>] [--max-tokens <n>] <prompt>
//!   omrp route [--task <type>]                              # reads from stdin
//!   omrp models          List registered models
//!   omrp status          Health and routing scores
//!   omrp best <task>     Best model for a task (no API call)
//!   omrp init            Write default config to ~/.config/omrp/config.toml

mod config;
mod dashboard;
mod provider;

use std::path::Path;
use omrp_core::pipeline::EventPipeline;
use omrp_core::router::RouterEngine;
use omrp_events::event::Event;
use omrp_types::task::{RouteRequest, TaskType};

use config::{parse_task_type, Config};
use provider::{
    format_provider_error, provider_error_to_kind, CompatClient, Message,
};
use omrp_events::error::ProviderError;

// ─── Pipeline bootstrap ───────────────────────────────────────────────────────

/// Load the pipeline from the persistent ledger (if it exists) and register
/// any models from `cfg` that are not already in the state.
fn bootstrap_pipeline(cfg: &Config) -> EventPipeline {
    let ledger_path = cfg.ledger_path();

    // Try to restore history from disk; fall back to a fresh pipeline.
    let mut pipeline = match EventPipeline::load_from_ledger(&ledger_path) {
        Ok(p) => p,
        Err(_) => EventPipeline::new(),
    };

    // Collect model IDs already in state so we don't double-register.
    let existing: Vec<String> = pipeline.state().read(|s| {
        s.models.iter().map(|m| m.id.clone()).collect()
    });

    // Emit ModelAdded for any model in the config not yet seen.
    for event in cfg.to_model_events() {
        if let Event::ModelAdded { ref model, .. } = event {
            if !existing.contains(&model.id) {
                let _ = pipeline.process(event);
            }
        }
    }

    pipeline
}

/// Persist the event log to disk, printing a warning on failure.
fn save_ledger(pipeline: &EventPipeline, path: &Path) {
    if let Err(e) = pipeline.save_to_ledger(path) {
        eprintln!("Warning: could not save ledger to {}: {e}", path.display());
    }
}

/// Rough token estimate (4 chars ≈ 1 token) — used for CompletionRequested.
fn estimate_tokens(text: &str) -> u32 {
    (text.len() as u32 / 4).max(1)
}

// ─── omrp route ───────────────────────────────────────────────────────────────

struct RouteArgs {
    task: TaskType,
    max_tokens: Option<u32>,
    prompt: String,
}

fn parse_route_args(args: &[String]) -> Result<RouteArgs, String> {
    let mut task = TaskType::Chat;
    let mut max_tokens: Option<u32> = None;
    let mut prompt_parts: Vec<String> = Vec::new();
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--task" | "-t" => {
                i += 1;
                let raw = args.get(i).ok_or("Expected task type after --task")?;
                task = parse_task_type(raw)
                    .ok_or_else(|| format!("Unknown task type: {raw:?}. Use: code, reasoning, chat, vision, analysis"))?;
            }
            "--max-tokens" | "-m" => {
                i += 1;
                let raw = args.get(i).ok_or("Expected number after --max-tokens")?;
                max_tokens = Some(raw.parse::<u32>().map_err(|_| format!("{raw:?} is not a valid token count"))?);
            }
            "--" => {
                // Everything after -- is the prompt.
                prompt_parts.extend_from_slice(&args[i + 1..]);
                break;
            }
            arg if arg.starts_with('-') => {
                return Err(format!("Unknown option: {arg}"));
            }
            arg => {
                prompt_parts.push(arg.to_string());
            }
        }
        i += 1;
    }

    let prompt = if prompt_parts.is_empty() {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| format!("stdin read error: {e}"))?;
        buf.trim().to_string()
    } else {
        prompt_parts.join(" ")
    };

    if prompt.is_empty() {
        return Err(
            "No prompt provided. Pass a prompt as an argument or pipe via stdin.\n\
             Example: omrp route --task code \"write a hello world in Rust\""
                .into(),
        );
    }

    Ok(RouteArgs { task, max_tokens, prompt })
}

fn cmd_route(route_args: &[String], cfg: &Config) {
    let RouteArgs { task, max_tokens, prompt } = match parse_route_args(route_args) {
        Ok(a) => a,
        Err(e) => { eprintln!("Error: {e}"); std::process::exit(1); }
    };

    // Bootstrap.
    let mut pipeline = bootstrap_pipeline(cfg);

    // Select best model.
    let router = RouterEngine::default();
    let request = RouteRequest { task_type: task, max_inflight_per_model: Some(3), ..Default::default() };
    let decision = pipeline.state().read(|s| router.select(s, &request));

    if decision.selected_model.is_empty() {
        eprintln!("No models available for task {:?}.", task.as_str());
        eprintln!("Run `omrp models` to see what is registered.");
        std::process::exit(1);
    }

    // Print routing summary.
    println!("Routing for task: {}", task.as_str());
    for (i, ms) in decision.scores.iter().enumerate() {
        let marker = if i == 0 { "  ← selected" } else { "" };
        println!("  {}. {:<45} score={:.3}{}", i + 1, ms.model_id, ms.total, marker);
    }
    println!();

    // Emit CompletionRequested.
    let _ = pipeline.process(Event::CompletionRequested {
        model_id: decision.selected_model.clone(),
        task_type: task,
        prompt_tokens: estimate_tokens(&prompt),
    });

    let messages = vec![Message::user(&prompt)];
    let mut outcome: Result<provider::CompletionResult, ProviderError> =
        Err(ProviderError::Internal("no attempt".into()));

    // Try primary model, then each fallback in order.
    // Each model may belong to a different provider (openrouter / kilo).
    // We resolve the right CompatClient per model and skip gracefully if the
    // API key for that provider is not set.
    for (i, model_id) in decision.fallback_chain.iter().enumerate() {
        // Resolve provider name from state.
        let provider_name = pipeline.state().read(|s| {
            s.models
                .iter()
                .find(|m| &m.id == model_id)
                .map(|m| m.provider.clone())
                .unwrap_or_else(|| "openrouter".into())
        });

        // Build the client for this provider.
        let client = match CompatClient::for_provider(&provider_name) {
            Ok(c) => c,
            Err(e) => {
                if i == 0 {
                    eprintln!("  ✗ {model_id} — skipped: {e}");
                } else {
                    eprintln!("  fallback → {model_id} — skipped: {e}");
                }
                continue;
            }
        };

        if i == 0 {
            eprint!("  [{provider_name}] calling {model_id}… ");
        } else {
            eprint!("  fallback → [{provider_name}] {model_id}… ");
        }

        outcome = client.complete_with_retry(model_id, &messages, max_tokens);

        match &outcome {
            Ok(_) => {
                eprint!("\r{}", " ".repeat(70));
                eprint!("\r");
                break;
            }
            Err(ProviderError::Auth(_)) => {
                eprintln!("auth error — check API key for {provider_name}");
                break;
            }
            Err(e) => {
                eprintln!("failed ({})", format_provider_error(e));
                let _ = pipeline.process(Event::ModelFailed {
                    model_id: model_id.clone(),
                    error: provider_error_to_kind(e),
                });
                let _ = pipeline.process(Event::CompletionFinished {
                    model_id: model_id.clone(),
                    latency_ms: 1,
                    tokens_used: 1,
                    success: false,
                });
            }
        }
    }

    // Handle final outcome.
    match outcome {
        Ok(cr) => {
            let _ = pipeline.process(Event::CompletionFinished {
                model_id: cr.model_used.clone(),
                latency_ms: cr.latency_ms,
                tokens_used: cr.tokens_used,
                success: true,
            });

            let sep = "─".repeat(72);
            println!("{sep}");
            println!("{}", cr.text);
            println!("{sep}");
            println!(
                "  model: {}  |  tokens: {}  |  {:.1}s",
                cr.model_used,
                cr.tokens_used,
                cr.latency_ms as f64 / 1000.0
            );
        }
        Err(e) => {
            eprintln!("\nError: all models failed. Last error:\n  {}", format_provider_error(&e));
            save_ledger(&pipeline, &cfg.ledger_path());
            std::process::exit(1);
        }
    }

    save_ledger(&pipeline, &cfg.ledger_path());
}

// ─── omrp models ─────────────────────────────────────────────────────────────

fn cmd_models(pipeline: &EventPipeline) {
    pipeline.state().read(|state| {
        if state.models.is_empty() {
            println!("No models registered. Check ~/.config/omrp/config.toml");
            return;
        }
        println!(
            "{:<45} {:<12} {:<7} {:>9}  {}",
            "Model ID", "Provider", "Vision", "CtxWindow", "Tasks"
        );
        println!("{}", "─".repeat(100));
        for m in &state.models {
            let tasks: Vec<&str> = m.capabilities.task_suitability.iter().map(|t| t.as_str()).collect();
            println!(
                "{:<45} {:<12} {:<7} {:>9}  {}",
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

// ─── omrp status ─────────────────────────────────────────────────────────────

fn cmd_status(pipeline: &EventPipeline) {
    let router = RouterEngine::default();
    pipeline.state().read(|state| {
        let decision = router.select(state, &RouteRequest::default());

        println!(
            "{:<45} {:>7}  {:>9}  {:>7}  {}",
            "Model ID", "Score", "Latency", "Ratio", "Status"
        );
        println!("{}", "─".repeat(82));

        for ms in &decision.scores {
            let health = state.health.get(&ms.model_id);
            let latency = health.map(|h| h.rolling_latency_avg_ms as u64).unwrap_or(0);
            let ratio   = health.map(|h| h.success_ratio).unwrap_or(0.5);
            let garbage = health.map(|h| h.garbage).unwrap_or(false);
            println!(
                "{:<45} {:>7.3}  {:>8}ms  {:>6.1}%  {}",
                ms.model_id,
                ms.total,
                latency,
                ratio * 100.0,
                if garbage { "GARBAGE" } else { "ok" },
            );
        }

        let n = state.diagnostics.total_completions;
        let f = state.diagnostics.total_failures;
        println!(
            "\nEvents in ledger: {} completions, {} failures",
            n, f
        );
        if !decision.selected_model.is_empty() {
            println!("Best for chat:    {}", decision.selected_model);
        }
    });
}

// ─── omrp best ───────────────────────────────────────────────────────────────

fn cmd_best(pipeline: &EventPipeline, task_str: &str) {
    let task_type = match parse_task_type(task_str) {
        Some(t) => t,
        None => {
            eprintln!("Unknown task type: {task_str:?}");
            eprintln!("Valid: code, reasoning, chat, vision, analysis");
            std::process::exit(1);
        }
    };

    let router = RouterEngine::default();
    pipeline.state().read(|state| {
        let request = RouteRequest { task_type, max_inflight_per_model: Some(3), ..Default::default() };
        let decision = router.select(state, &request);

        if decision.selected_model.is_empty() {
            println!("No suitable model found for task: {task_str}");
            return;
        }

        println!("Best for {task_str:?}: {}", decision.selected_model);
        println!("Score:  {:.3}", decision.score);
        println!("\nFactor breakdown:");
        for f in &decision.reasoning {
            println!(
                "  {:<16} value={:.3}  weight={:.2}  contribution={:.3}",
                f.name, f.value, f.weight, f.contribution()
            );
        }
        if decision.fallback_chain.len() > 1 {
            let rest: Vec<&str> = decision.fallback_chain[1..].iter().map(|s| s.as_str()).collect();
            println!("\nFallback chain: {}", rest.join(" → "));
        }
    });
}

// ─── omrp init ───────────────────────────────────────────────────────────────

fn cmd_init() {
    let path = config::default_config_path();
    if path.exists() {
        println!("Config already exists: {}", path.display());
        println!("Edit it to add or remove models.");
        return;
    }
    match Config::write_default(&path) {
        Ok(()) => {
            println!("Config written to: {}", path.display());
            println!();
            println!("Set API keys for the providers you want to use:");
            println!("  export OPENROUTER_API_KEY=sk-or-v1-...   # https://openrouter.ai/keys");
            println!("  export KILO_API_KEY=...                   # https://kilo.ai");
            println!();
            println!("Then route a request:");
            println!("  omrp route --task code \"write a hello world in Rust\"");
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

// ─── Entry point ─────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        print_usage();
        std::process::exit(1);
    }

    // Load config once — commands that need it will use it.
    // `route` and informational commands get a properly bootstrapped pipeline.
    let (cfg, was_missing) = Config::load_or_default();

    match args[1].as_str() {
        "route" => {
            if was_missing {
                eprintln!(
                    "Note: no config found at {}",
                    config::default_config_path().display()
                );
                eprintln!("      Using built-in defaults. Run `omrp init` to create one.\n");
            }
            cmd_route(&args[2..], &cfg);
        }

        "models" => {
            let pipeline = bootstrap_pipeline(&cfg);
            cmd_models(&pipeline);
        }

        "status" => {
            let pipeline = bootstrap_pipeline(&cfg);
            cmd_status(&pipeline);
        }

        "best" => {
            if args.len() < 3 {
                eprintln!("Usage: omrp best <task>");
                eprintln!("  task: code, reasoning, chat, vision, analysis");
                std::process::exit(1);
            }
            let pipeline = bootstrap_pipeline(&cfg);
            cmd_best(&pipeline, &args[2]);
        }

        "init" => cmd_init(),

        "dashboard" => {
            if let Err(e) = dashboard::run(&cfg) {
                eprintln!("Dashboard error: {e}");
                std::process::exit(1);
            }
        }

        other => {
            eprintln!("Unknown command: {other:?}");
            print_usage();
            std::process::exit(1);
        }
    }
}

fn print_usage() {
    eprintln!("OMRP — Open Model Routing Protocol");
    eprintln!();
    eprintln!("Usage:");
    eprintln!("  omrp route [--task <type>] [--max-tokens <n>] <prompt>");
    eprintln!("  omrp route [--task <type>]                     # reads from stdin");
    eprintln!("  omrp models       List registered models");
    eprintln!("  omrp status       Health, latency, and routing scores");
    eprintln!("  omrp best <task>  Best model for a task (dry run, no API call)");
    eprintln!("  omrp dashboard    Live TUI dashboard (q to quit)");
    eprintln!("  omrp init         Create default config at ~/.config/omrp/config.toml");
    eprintln!();
    eprintln!("Task types: code, reasoning, chat, vision, analysis");
    eprintln!();
    eprintln!("Environment (set the key for each provider you use):");
    eprintln!("  OPENROUTER_API_KEY=sk-or-v1-...   https://openrouter.ai/keys");
    eprintln!("  KILO_API_KEY=...                   https://kilo.ai");
}
