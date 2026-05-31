//! OMRP CLI — deterministic LLM router
//!
//! Usage:
//!   omrp route [--task <type>] [--tier <tier>] [--caveman lite|full|ultra]
//!              [--rtk] [--max-tokens <n>] <prompt|stdin>
//!   omrp serve [--port N] [--host H] [--rtk] [--caveman lite|full|ultra]
//!   omrp models       List registered models
//!   omrp status       Health and routing scores
//!   omrp best <task>  Best model for a task (no API call)
//!   omrp dashboard    Live TUI dashboard
//!   omrp init         Write default config

mod config;
mod dashboard;
mod keys;
mod provider;
mod server;

use std::path::Path;

use omrp_core::caveman::{inject_caveman, CavemanLevel};
use omrp_core::classifier::{classify_prompt, detect_mode_override, PromptTier};
use omrp_core::pipeline::EventPipeline;
use omrp_core::router::RouterEngine;
use omrp_core::rtk::{compress_messages, format_rtk_log};
use omrp_core::state::State;
use omrp_events::event::Event;
use omrp_types::task::{RouteRequest, TaskType};

use config::{parse_task_type, Config};
use omrp_events::error::ProviderError;
use provider::{format_provider_error, provider_error_to_kind, CompatClient, Message};

// ─── Pipeline bootstrap ───────────────────────────────────────────────────────

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

pub fn save_ledger(pipeline: &EventPipeline, path: &Path) {
    if let Err(e) = pipeline.save_to_ledger(path) {
        eprintln!("Warning: could not save ledger: {e}");
    }
}

fn estimate_tokens(text: &str) -> u32 {
    (text.len() as u32 / 4).max(1)
}

/// Parse a tier string from config → `PromptTier`.
pub fn tier_from_str(s: &str) -> PromptTier {
    match s.trim().to_lowercase().as_str() {
        "simple"    => PromptTier::Simple,
        "complex"   => PromptTier::Complex,
        "reasoning" => PromptTier::Reasoning,
        _           => PromptTier::Medium,
    }
}

/// Select the best model for `tier`, falling back to all models if none in tier.
pub fn select_for_tier(
    state: &State,
    request: &RouteRequest,
    _tier: PromptTier,
    tier_model_ids: &[String],
    router: &RouterEngine,
) -> omrp_types::routing::RoutingDecision {
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

// ─── omrp route ───────────────────────────────────────────────────────────────

struct RouteArgs {
    task: TaskType,
    tier_override: Option<PromptTier>,
    max_tokens: Option<u32>,
    caveman: Option<CavemanLevel>,
    rtk: bool,
    prompt: String,
}

fn parse_route_args(args: &[String]) -> Result<RouteArgs, String> {
    let mut task = TaskType::Chat;
    let mut tier_override: Option<PromptTier> = None;
    let mut max_tokens: Option<u32> = None;
    let mut caveman: Option<CavemanLevel> = None;
    let mut rtk = false;
    let mut prompt_parts: Vec<String> = Vec::new();
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--task" | "-t" => {
                i += 1;
                let raw = args.get(i).ok_or("Expected task type after --task")?;
                task = parse_task_type(raw)
                    .ok_or_else(|| format!("Unknown task: {raw:?}"))?;
            }
            "--tier" => {
                i += 1;
                let raw = args.get(i).ok_or("Expected tier after --tier")?;
                tier_override = Some(tier_from_str(raw));
            }
            "--max-tokens" | "-m" => {
                i += 1;
                let raw = args.get(i).ok_or("Expected number after --max-tokens")?;
                max_tokens = Some(raw.parse().map_err(|_| format!("{raw:?} not a number"))?);
            }
            "--caveman" | "-c" => {
                i += 1;
                let raw = args.get(i).ok_or("Expected level after --caveman (lite|full|ultra)")?;
                caveman = Some(CavemanLevel::from_str(raw)
                    .ok_or_else(|| format!("Unknown caveman level: {raw:?}"))?);
            }
            "--rtk" => rtk = true,
            "--" => {
                prompt_parts.extend_from_slice(&args[i + 1..]);
                break;
            }
            arg if arg.starts_with('-') => return Err(format!("Unknown option: {arg}")),
            arg => prompt_parts.push(arg.to_string()),
        }
        i += 1;
    }

    let prompt = if prompt_parts.is_empty() {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf).map_err(|e| e.to_string())?;
        buf.trim().to_string()
    } else {
        prompt_parts.join(" ")
    };

    if prompt.is_empty() {
        return Err(
            "No prompt. Pass as argument or pipe via stdin.\n\
             Example: omrp route --task code \"write a fibonacci in Rust\""
                .into(),
        );
    }

    Ok(RouteArgs { task, tier_override, max_tokens, caveman, rtk, prompt })
}

fn cmd_route(route_args: &[String], cfg: &Config) {
    let RouteArgs { task, tier_override, max_tokens, caveman, rtk, prompt } =
        match parse_route_args(route_args) {
            Ok(a) => a,
            Err(e) => { eprintln!("Error: {e}"); std::process::exit(1); }
        };

    // ── Mode override + classifier ────────────────────────────────────────────
    let (effective_prompt, forced_tier) = match detect_mode_override(&prompt) {
        Some(ov) => (ov.cleaned_prompt, Some(ov.tier)),
        None     => (prompt.clone(), None),
    };

    let cls = classify_prompt(&effective_prompt, None);
    let tier = tier_override
        .or(forced_tier)
        .unwrap_or_else(|| cls.tier.unwrap_or(PromptTier::Medium));

    // ── Bootstrap pipeline ────────────────────────────────────────────────────
    let mut pipeline = bootstrap_pipeline(cfg);

    // ── Tier-aware routing ────────────────────────────────────────────────────
    let tier_ids: Vec<String> = cfg.model.iter()
        .filter(|m| tier_from_str(&m.tier) == tier)
        .map(|m| m.id.clone())
        .collect();

    let router = RouterEngine::default();
    let request = RouteRequest { task_type: task, max_inflight_per_model: Some(3), ..Default::default() };
    let decision = pipeline.state().read(|s| select_for_tier(s, &request, tier, &tier_ids, &router));

    if decision.selected_model.is_empty() {
        eprintln!("No models available. Run `omrp models` to see registered models.");
        std::process::exit(1);
    }

    // ── Print routing header ──────────────────────────────────────────────────
    let tier_src = if tier_override.is_some() { "flag" }
                   else if forced_tier.is_some() { "override" }
                   else { "classifier" };
    println!(
        "Tier: {} ({}) | task: {} | conf: {:.0}% | signals: {}",
        tier.as_str(), tier_src, task.as_str(),
        cls.confidence * 100.0,
        if cls.signals.is_empty() { "none".into() }
        else { cls.signals.iter().take(3).cloned().collect::<Vec<_>>().join(", ") }
    );
    for (i, ms) in decision.scores.iter().enumerate() {
        let tag = if i == 0 { "  ← selected" } else { "" };
        println!("  {}. {:<48} {:.3}{}", i + 1, ms.model_id, ms.total, tag);
    }
    if let Some(lvl) = caveman { println!("  [caveman: {}]", lvl.as_str()); }
    if rtk { println!("  [rtk: enabled]"); }
    println!();

    // ── Build request body (RTK + Caveman pre-processing) ────────────────────
    let mut body = serde_json::json!({
        "model": decision.selected_model,
        "messages": [{ "role": "user", "content": &effective_prompt }],
        "max_tokens": max_tokens.unwrap_or(1024)
    });

    if let Some(lvl) = caveman {
        inject_caveman(&mut body, lvl);
    }
    if rtk {
        if let Some(stats) = compress_messages(&mut body, true) {
            if let Some(line) = format_rtk_log(Some(&stats)) {
                eprintln!("{line}");
            }
        }
    }

    // Rebuild messages from body for the actual call
    let final_messages: Vec<Message> = body["messages"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .map(|m| Message {
            role: m["role"].as_str().unwrap_or("user").to_string(),
            content: m["content"].as_str().unwrap_or("").to_string(),
        })
        .collect();

    // ── Emit CompletionRequested ──────────────────────────────────────────────
    let _ = pipeline.process(Event::CompletionRequested {
        model_id: decision.selected_model.clone(),
        task_type: task,
        prompt_tokens: estimate_tokens(&effective_prompt),
    });

    // ── Try primary model + fallback chain ────────────────────────────────────
    let mut outcome: Result<provider::CompletionResult, ProviderError> =
        Err(ProviderError::Internal("no attempt".into()));

    for (i, model_id) in decision.fallback_chain.iter().enumerate() {
        let provider_name = pipeline.state().read(|s| {
            s.models.iter()
                .find(|m| &m.id == model_id)
                .map(|m| m.provider.clone())
                .unwrap_or_else(|| "openrouter".into())
        });

        let client = match CompatClient::for_provider(&provider_name) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("  ✗ {model_id} — skipped ({provider_name}): {e}");
                continue;
            }
        };

        if i == 0 {
            eprint!("  [{provider_name}] {model_id}… ");
        } else {
            eprint!("  fallback [{provider_name}] {model_id}… ");
        }

        outcome = client.complete_with_retry(model_id, &final_messages, max_tokens);

        match &outcome {
            Ok(_) => { eprint!("\r{}\r", " ".repeat(72)); break; }
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
                    latency_ms: 1, tokens_used: 1, success: false,
                });
            }
        }
    }

    // ── Handle result ─────────────────────────────────────────────────────────
    match outcome {
        Ok(cr) => {
            let _ = pipeline.process(Event::CompletionFinished {
                model_id: cr.model_used.clone(),
                latency_ms: cr.latency_ms, tokens_used: cr.tokens_used, success: true,
            });
            let sep = "─".repeat(72);
            println!("{sep}");
            println!("{}", cr.text);
            println!("{sep}");
            println!(
                "  model: {}  |  tokens: {}  |  {:.1}s",
                cr.model_used, cr.tokens_used, cr.latency_ms as f64 / 1000.0
            );
        }
        Err(e) => {
            eprintln!("\nError: all models failed.\n  {}", format_provider_error(&e));
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
            println!("No models registered. Run `omrp init` to create a config.");
            return;
        }
        println!("{:<48} {:<11} {:<10} {:>9}  {}", "Model ID", "Provider", "Tier", "Ctx", "Tasks");
        println!("{}", "─".repeat(105));
        for m in &state.models {
            let tasks: Vec<&str> = m.capabilities.task_suitability.iter().map(|t| t.as_str()).collect();
            println!(
                "{:<48} {:<11} {:<10} {:>9}  {}",
                m.id, m.provider, "—", m.capabilities.context_window, tasks.join(", ")
            );
        }
        println!("\n{} model(s)", state.models.len());
    });
}

// ─── omrp status ─────────────────────────────────────────────────────────────

fn cmd_status(pipeline: &EventPipeline) {
    let router = RouterEngine::default();
    pipeline.state().read(|state| {
        let decision = router.select(state, &RouteRequest::default());
        println!("{:<48} {:>7}  {:>9}  {:>7}  {}", "Model ID", "Score", "Latency", "Ratio", "Status");
        println!("{}", "─".repeat(86));
        for ms in &decision.scores {
            let h = state.health.get(&ms.model_id);
            let lat = h.map(|h| h.rolling_latency_avg_ms as u64).unwrap_or(0);
            let rat = h.map(|h| h.success_ratio).unwrap_or(0.5);
            let bad = h.map(|h| h.garbage).unwrap_or(false);
            println!(
                "{:<48} {:>7.3}  {:>8}ms  {:>6.1}%  {}",
                ms.model_id, ms.total, lat, rat * 100.0,
                if bad { "GARBAGE" } else { "ok" }
            );
        }
        let d = &state.diagnostics;
        println!("\nCompletions: {}  Failures: {}  Fallbacks: {}  Ledger events: {}",
            d.total_completions, d.total_failures, d.total_fallbacks, pipeline.event_log().len());
        if !decision.selected_model.is_empty() {
            println!("Best (chat): {}", decision.selected_model);
        }
    });
}

// ─── omrp best ───────────────────────────────────────────────────────────────

fn cmd_best(pipeline: &EventPipeline, task_str: &str) {
    let task = match parse_task_type(task_str) {
        Some(t) => t,
        None => { eprintln!("Unknown task: {task_str:?}"); std::process::exit(1); }
    };
    let router = RouterEngine::default();
    pipeline.state().read(|state| {
        let request = RouteRequest { task_type: task, max_inflight_per_model: Some(3), ..Default::default() };
        let decision = router.select(state, &request);
        if decision.selected_model.is_empty() {
            println!("No model found for {task_str:?}"); return;
        }
        println!("Best for {task_str:?}: {}", decision.selected_model);
        println!("Score: {:.3}", decision.score);
        println!("\nFactors:");
        for f in &decision.reasoning {
            println!("  {:<16} value={:.3}  weight={:.2}  contribution={:.3}",
                f.name, f.value, f.weight, f.contribution());
        }
        if decision.fallback_chain.len() > 1 {
            let rest: Vec<&str> = decision.fallback_chain[1..].iter().map(|s| s.as_str()).collect();
            println!("\nFallback: {}", rest.join(" → "));
        }
    });
}

// ─── omrp init ───────────────────────────────────────────────────────────────

fn cmd_init() {
    let path = config::default_config_path();
    if path.exists() {
        println!("Config exists: {}  (edit to change models)", path.display());
        return;
    }
    match Config::write_default(&path) {
        Ok(()) => {
            println!("Config written: {}", path.display());
            println!();
            println!("Set API keys for the providers you want to use:");
            println!("  export CEREBRAS_API_KEY=...    # fastest (14k req/day)");
            println!("  export GROQ_API_KEY=...         # ultra-low latency");
            println!("  export KILO_API_KEY=...         # kilo/auto-free smart router");
            println!("  export OPENROUTER_API_KEY=...   # 50-1000 req/day");
            println!("  export BUW_API_KEY=...          # BUW virtual model gateway");
            println!();
            println!("Then route:");
            println!("  omrp route --task code \"write fibonacci in Rust\"");
            println!("  omrp serve   # OpenAI-compat proxy on :18800");
        }
        Err(e) => { eprintln!("Error: {e}"); std::process::exit(1); }
    }
}

// ─── Entry point ─────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 { print_usage(); std::process::exit(1); }

    let (cfg, was_missing) = Config::load_or_default();

    match args[1].as_str() {
        "route" => {
            if was_missing {
                eprintln!("Note: no config. Run `omrp init` to create one.\n");
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
                std::process::exit(1);
            }
            let pipeline = bootstrap_pipeline(&cfg);
            cmd_best(&pipeline, &args[2]);
        }
        "dashboard" => {
            if let Err(e) = dashboard::run(&cfg) {
                eprintln!("Dashboard error: {e}"); std::process::exit(1);
            }
        }
        "serve" => {
            let mut port: u16 = 18800;
            let mut host = "127.0.0.1".to_string();
            let mut srv_rtk = false;
            let mut srv_caveman: Option<CavemanLevel> = None;
            let mut i = 2usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--port" | "-p" => {
                        i += 1;
                        port = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(port);
                    }
                    "--host" | "-H" => {
                        i += 1;
                        if let Some(h) = args.get(i) { host = h.clone(); }
                    }
                    "--rtk" => srv_rtk = true,
                    "--caveman" | "-c" => {
                        i += 1;
                        if let Some(l) = args.get(i) {
                            srv_caveman = CavemanLevel::from_str(l);
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            server::run(&cfg, &host, port, srv_rtk, srv_caveman);
        }
        "init" => cmd_init(),
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
    eprintln!("  omrp route [--task T] [--tier T] [--caveman lite|full|ultra] [--rtk] [--max-tokens N] <prompt>");
    eprintln!("  omrp serve [--port N] [--host H] [--rtk] [--caveman lite|full|ultra]");
    eprintln!("  omrp models    List registered models");
    eprintln!("  omrp status    Health and routing scores");
    eprintln!("  omrp best <t>  Best model for a task (no API call)");
    eprintln!("  omrp dashboard Live TUI (q to quit)");
    eprintln!("  omrp init      Create default config");
    eprintln!();
    eprintln!("Task types: code, reasoning, chat, vision, analysis");
    eprintln!("Tier types: simple, medium, complex, reasoning");
    eprintln!();
    eprintln!("API keys (set for the providers you use):");
    eprintln!("  CEREBRAS_API_KEY      GROQ_API_KEY");
    eprintln!("  KILO_API_KEY          OPENROUTER_API_KEY");
    eprintln!("  BUW_API_KEY");
}
