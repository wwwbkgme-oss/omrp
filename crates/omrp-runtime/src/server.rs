//! OMRP HTTP proxy — `omrp serve`
//!
//! An OpenAI-compatible proxy that sits in front of any AI tool (Claude Code,
//! Cursor, Cline, Kilo Code, etc.) and routes every request to the best
//! available **free** model using the BKG-FMR scoring engine.
//!
//! Inspired by 9router (MIT) and FreeRouter (MIT), rewritten from scratch in
//! Rust with OMRP's event-sourced health-scoring engine.
//!
//! ## Endpoints
//! ```
//! POST /v1/chat/completions   classify → RTK → Caveman → tier route → proxy
//! GET  /v1/models             list registered models (OpenAI format)
//! GET  /health                uptime + stats
//! GET  /stats                 per-tier / per-model counters
//! POST /reload                hot-reload hint (restart required for now)
//! ```
//!
//! ## Model field
//! - `"auto"` | `"omrp/auto"` | `"omrp/auto-free"` → classify + route
//! - any other value → pass through to that model directly
//!
//! ## Response headers
//! - `X-OMRP-Model`      — actual model used
//! - `X-OMRP-Tier`       — routing tier (SIMPLE/MEDIUM/COMPLEX/REASONING)
//! - `X-OMRP-Reasoning`  — classifier signal string
//! - `X-OMRP-RTK`        — token savings if RTK was applied

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

use omrp_core::caveman::{inject_caveman, CavemanLevel};
use omrp_core::classifier::{classify_prompt, detect_mode_override, PromptTier};
use omrp_core::pipeline::EventPipeline;
use omrp_core::router::RouterEngine;
use omrp_core::rtk::{compress_messages, format_rtk_log};
use omrp_types::task::RouteRequest;

use crate::bootstrap_pipeline;
use crate::config::Config;
use crate::provider::{format_provider_error, CompatClient, Message};
use crate::tier_from_str;
use crate::select_for_tier;

// ─── Stats ────────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct Stats {
    pub requests: u64,
    pub errors: u64,
    pub by_tier: HashMap<String, u64>,
    pub by_model: HashMap<String, u64>,
    pub started_unix: u64,
}

impl Stats {
    fn new() -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};
        Self {
            started_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            ..Default::default()
        }
    }
}

// ─── Entry point ─────────────────────────────────────────────────────────────

/// Run the proxy server.  Blocks until the process is killed.
pub fn run(cfg: &Config, host: &str, port: u16, rtk: bool, caveman: Option<CavemanLevel>) {
    let addr = format!("{host}:{port}");
    let server = match Server::http(&addr) {
        Ok(s) => s,
        Err(e) => { eprintln!("Cannot bind {addr}: {e}"); std::process::exit(1); }
    };

    let stats = Arc::new(Mutex::new(Stats::new()));
    let started = Instant::now();

    println!("OMRP proxy  http://{addr}");
    println!("  RTK:     {}", if rtk { "enabled (compresses tool outputs)" } else { "disabled (--rtk to enable)" });
    println!("  Caveman: {}", caveman.map(|l| l.as_str()).unwrap_or("disabled (--caveman lite|full|ultra)"));
    println!();
    println!("  POST /v1/chat/completions  use model=\"auto\" or \"omrp/auto\" to let OMRP route");
    println!("  GET  /v1/models            registered models");
    println!("  GET  /health               uptime + stats");
    println!("  GET  /stats                per-tier counters");
    println!();

    for request in server.incoming_requests() {
        let elapsed = started.elapsed().as_secs();
        handle_request(request, cfg, &stats, elapsed, rtk, caveman);
    }
}

// ─── Request dispatch ─────────────────────────────────────────────────────────

fn handle_request(
    req: Request,
    cfg: &Config,
    stats: &Arc<Mutex<Stats>>,
    uptime_secs: u64,
    rtk: bool,
    caveman: Option<CavemanLevel>,
) {
    let method = req.method().clone();
    let url = req.url().to_string();

    if method == Method::Options {
        let _ = req.respond(
            Response::empty(204)
                .with_header(h("Access-Control-Allow-Origin", "*"))
                .with_header(h("Access-Control-Allow-Methods", "GET, POST, OPTIONS"))
                .with_header(h("Access-Control-Allow-Headers", "Content-Type, Authorization")),
        );
        return;
    }

    let result = match (method.as_str(), url.as_str()) {
        ("POST", "/v1/chat/completions") | ("POST", "/chat/completions") =>
            handle_completions(req, cfg, stats, rtk, caveman),
        ("GET",  "/v1/models") | ("GET", "/models") =>
            handle_models(req, cfg),
        ("GET",  "/health") => handle_health(req, stats, uptime_secs),
        ("GET",  "/stats")  => handle_stats(req, stats),
        ("POST", "/reload") => handle_reload(req, cfg),
        _ => json_error(req, 404, &format!("Not found: {url}"), "not_found"),
    };

    if let Err(e) = result {
        eprintln!("[server] {e}");
    }
}

// ─── Completions ─────────────────────────────────────────────────────────────

fn handle_completions(
    mut req: Request,
    cfg: &Config,
    stats: &Arc<Mutex<Stats>>,
    rtk_enabled: bool,
    caveman: Option<CavemanLevel>,
) -> Result<(), Box<dyn std::error::Error>> {
    let raw = read_body(&mut req)?;
    let mut body: Value = serde_json::from_str(&raw)
        .map_err(|e| format!("JSON parse error: {e}"))?;

    let model_id = body["model"].as_str().unwrap_or("auto").to_string();
    let max_tokens = body["max_tokens"].as_u64().map(|n| n as u32);

    // Extract messages for classification
    let msgs = match body["messages"].as_array() {
        Some(m) if !m.is_empty() => m.clone(),
        _ => return json_error(req, 400, "messages array required", "invalid_request"),
    };

    let (user_text, system_text) = extract_texts(&msgs);
    if user_text.is_empty() {
        return json_error(req, 400, "No user message found", "invalid_request");
    }

    // Decide routing mode
    let is_auto = matches!(model_id.as_str(), "auto" | "omrp/auto" | "omrp/auto-free");

    let (routed_model, tier_str, reasoning) = if is_auto {
        // Classify + tier-route
        let (effective, forced_tier) = match detect_mode_override(&user_text) {
            Some(ov) => (ov.cleaned_prompt, Some(ov.tier)),
            None => (user_text.clone(), None),
        };
        let cls = classify_prompt(&effective, if system_text.is_empty() { None } else { Some(&system_text) });
        let tier = forced_tier.unwrap_or_else(|| cls.tier.unwrap_or(PromptTier::Medium));

        let pipeline = build_readonly_pipeline(cfg);
        let router = RouterEngine::default();
        let req2 = RouteRequest { max_inflight_per_model: Some(3), ..Default::default() };
        let tier_ids: Vec<String> = cfg.model.iter()
            .filter(|m| tier_from_str(&m.tier) == tier)
            .map(|m| m.id.clone())
            .collect();
        let decision = pipeline.state().read(|s| select_for_tier(s, &req2, tier, &tier_ids, &router));

        let rsn = format!(
            "tier={} score={:.3} signals={}",
            tier.as_str(), decision.score,
            cls.signals.iter().take(3).cloned().collect::<Vec<_>>().join(",")
        );
        (decision.selected_model, tier.as_str().to_string(), rsn)
    } else {
        (model_id.clone(), "EXPLICIT".to_string(), format!("explicit:{model_id}"))
    };

    if routed_model.is_empty() {
        stats.lock().unwrap().errors += 1;
        return json_error(req, 503, "No available model", "unavailable");
    }

    // Update stats
    {
        let mut s = stats.lock().unwrap();
        s.requests += 1;
        *s.by_tier.entry(tier_str.clone()).or_insert(0) += 1;
        *s.by_model.entry(routed_model.clone()).or_insert(0) += 1;
    }

    // Apply RTK (compress tool results before forwarding)
    let rtk_savings = if rtk_enabled {
        let stats = compress_messages(&mut body, true);
        format_rtk_log(stats.as_ref()).unwrap_or_default()
    } else {
        String::new()
    };

    // Apply Caveman (inject terse-reply system prompt)
    if let Some(lvl) = caveman {
        inject_caveman(&mut body, lvl);
    }

    // Rebuild messages after mutations
    let final_messages: Vec<Message> = body["messages"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .map(|m| Message {
            role: m["role"].as_str().unwrap_or("user").to_string(),
            content: m["content"].as_str().unwrap_or("").to_string(),
        })
        .collect();

    // Resolve provider
    let provider_name = cfg.model.iter()
        .find(|m| m.id == routed_model)
        .map(|m| m.provider.as_str())
        .unwrap_or("openrouter")
        .to_string();

    let client = match CompatClient::for_provider(&provider_name) {
        Ok(c) => c,
        Err(e) => {
            stats.lock().unwrap().errors += 1;
            return json_error(req, 503, &e, "unavailable");
        }
    };

    eprintln!("[omrp] {} → [{provider_name}] {routed_model} (tier:{tier_str})",
        &user_text.chars().take(60).collect::<String>());

    // Forward to provider
    match client.complete_with_retry(&routed_model, &final_messages, max_tokens) {
        Ok(cr) => {
            let resp_body = json!({
                "id": format!("omrp-{}", std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default().as_millis()),
                "object": "chat.completion",
                "model": cr.model_used,
                "choices": [{
                    "index": 0,
                    "message": { "role": "assistant", "content": cr.text },
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 0,
                    "completion_tokens": cr.tokens_used,
                    "total_tokens": cr.tokens_used
                }
            });

            let mut resp = Response::from_string(resp_body.to_string())
                .with_status_code(StatusCode(200))
                .with_header(h("Content-Type", "application/json"))
                .with_header(h("Access-Control-Allow-Origin", "*"))
                .with_header(h("X-OMRP-Model", &cr.model_used))
                .with_header(h("X-OMRP-Tier", &tier_str))
                .with_header(h("X-OMRP-Reasoning", &reasoning[..reasoning.len().min(200)]));
            if !rtk_savings.is_empty() {
                resp = resp.with_header(h("X-OMRP-RTK", &rtk_savings));
            }
            req.respond(resp)?;
        }
        Err(e) => {
            stats.lock().unwrap().errors += 1;
            let msg = format_provider_error(&e);
            return json_error(req, 502, &msg, "upstream_error");
        }
    }
    Ok(())
}

// ─── Other handlers ───────────────────────────────────────────────────────────

fn handle_models(req: Request, cfg: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut models = vec![
        json!({ "id": "auto",          "object": "model", "created": now, "owned_by": "omrp" }),
        json!({ "id": "omrp/auto",     "object": "model", "created": now, "owned_by": "omrp" }),
        json!({ "id": "omrp/auto-free","object": "model", "created": now, "owned_by": "omrp" }),
    ];
    for m in &cfg.model {
        models.push(json!({ "id": m.id, "object": "model", "created": now, "owned_by": m.provider }));
    }
    req.respond(
        Response::from_string(json!({ "object": "list", "data": models }).to_string())
            .with_header(h("Content-Type", "application/json"))
            .with_header(h("Access-Control-Allow-Origin", "*")),
    )?;
    Ok(())
}

fn handle_health(req: Request, stats: &Arc<Mutex<Stats>>, uptime: u64)
    -> Result<(), Box<dyn std::error::Error>>
{
    let s = stats.lock().unwrap();
    req.respond(Response::from_string(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_secs": uptime,
        "requests": s.requests,
        "errors": s.errors,
    }).to_string())
    .with_header(h("Content-Type", "application/json"))
    .with_header(h("Access-Control-Allow-Origin", "*")))?;
    Ok(())
}

fn handle_stats(req: Request, stats: &Arc<Mutex<Stats>>)
    -> Result<(), Box<dyn std::error::Error>>
{
    let s = stats.lock().unwrap();
    req.respond(Response::from_string(json!({
        "requests": s.requests,
        "errors": s.errors,
        "by_tier": s.by_tier,
        "by_model": s.by_model,
        "started_unix": s.started_unix,
    }).to_string())
    .with_header(h("Content-Type", "application/json"))
    .with_header(h("Access-Control-Allow-Origin", "*")))?;
    Ok(())
}

fn handle_reload(req: Request, cfg: &Config)
    -> Result<(), Box<dyn std::error::Error>>
{
    req.respond(Response::from_string(json!({
        "status": "ok",
        "note": "restart omrp serve to reload config changes",
        "models": cfg.model.len(),
    }).to_string())
    .with_header(h("Content-Type", "application/json")))?;
    Ok(())
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn build_readonly_pipeline(cfg: &Config) -> EventPipeline {
    bootstrap_pipeline(cfg)
}

fn extract_texts(msgs: &[Value]) -> (String, String) {
    let mut sys = String::new();
    let mut user = String::new();
    for m in msgs {
        let role = m["role"].as_str().unwrap_or("");
        let content = m["content"].as_str().unwrap_or("");
        match role {
            "system" | "developer" => { if !sys.is_empty() { sys.push('\n'); } sys.push_str(content); }
            "user" => user = content.to_string(),
            _ => {}
        }
    }
    (user, sys)
}

fn read_body(req: &mut Request) -> Result<String, std::io::Error> {
    let mut buf = String::new();
    req.as_reader().read_to_string(&mut buf)?;
    Ok(buf)
}

fn h(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header")
}

fn json_error(req: Request, status: u16, msg: &str, t: &str)
    -> Result<(), Box<dyn std::error::Error>>
{
    req.respond(
        Response::from_string(json!({ "error": { "message": msg, "type": t, "code": status } }).to_string())
            .with_status_code(StatusCode(status))
            .with_header(h("Content-Type", "application/json"))
            .with_header(h("Access-Control-Allow-Origin", "*")),
    )?;
    Ok(())
}
