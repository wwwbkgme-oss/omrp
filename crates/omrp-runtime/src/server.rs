//! OMRP HTTP proxy + browser playground — `omrp serve`
//!
//! Drop-in OpenAI-compatible proxy with:
//! - Streaming SSE forwarded directly from provider to browser
//! - Browser playground at `GET /` (model selector, tier badge, caveman)
//! - Tier classification, RTK, Caveman injection on every request
//!
//! ## Endpoints
//! ```
//! GET  /                      Browser playground (full SPA)
//! GET  /playground            Same as /
//! POST /v1/chat/completions   stream|non-stream → classify → route → proxy
//! GET  /v1/models             registered models (OpenAI format)
//! GET  /health                uptime + stats
//! GET  /stats                 per-tier / per-model counters
//! POST /reload                hot-reload hint
//! ```
//!
//! ## Streaming
//! When the client sends `"stream": true`, the server pipes the raw SSE
//! body from the upstream provider directly to the browser.
//! `ureq::Response::into_reader()` returns a `Box<dyn Read + Send + 'static>`;
//! `tiny_http::Response::from_reader` wraps it as chunked transfer.
//! Zero extra buffering — first token reaches the browser immediately.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

use omrp_core::caveman::{inject_caveman, CavemanLevel};
use omrp_core::classifier::{classify_prompt, detect_mode_override, PromptTier};
use omrp_core::router::RouterEngine;
use omrp_core::rtk::{compress_messages, format_rtk_log};
use omrp_types::task::RouteRequest;

use crate::bootstrap_pipeline;
use crate::config::Config;
use crate::keys::KeyStore;
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

pub fn run(cfg: &Config, host: &str, port: u16, rtk: bool, caveman: Option<CavemanLevel>) {
    let addr = format!("{host}:{port}");
    let server = match Server::http(&addr) {
        Ok(s) => s,
        Err(e) => { eprintln!("Cannot bind {addr}: {e}"); std::process::exit(1); }
    };
    let stats   = Arc::new(Mutex::new(Stats::new()));
    let keys    = Arc::new(Mutex::new(KeyStore::load(KeyStore::default_path())));
    let started = Instant::now();

    {
        let ks = keys.lock().unwrap();
        println!("OMRP proxy  \x1b[36mhttp://{addr}\x1b[0m");
        println!("  Playground  http://{addr}/");
        println!("  API keys:   {} key(s) registered  (manage at http://{addr}/#keys)",
            ks.keys.len());
        if ks.auth_required() {
            println!("  Auth:       \x1b[32mEnabled\x1b[0m — Bearer token required for API endpoints");
        } else {
            println!("  Auth:       off — generate keys at http://{addr}/#keys");
        }
        println!("  RTK:        {}", if rtk { "on (compresses tool outputs)" } else { "off (--rtk to enable)" });
        println!("  Caveman:    {}", caveman.map(|l| l.as_str()).unwrap_or("off (--caveman lite|full|ultra)"));
        println!();
    }

    for request in server.incoming_requests() {
        handle_request(request, cfg, &stats, &keys, started.elapsed().as_secs(), rtk, caveman);
    }
}

// ─── Dispatch ─────────────────────────────────────────────────────────────────

fn handle_request(
    req: Request, cfg: &Config, stats: &Arc<Mutex<Stats>>,
    keys: &Arc<Mutex<KeyStore>>, uptime: u64, rtk: bool, caveman: Option<CavemanLevel>,
) {
    let method = req.method().clone();
    let url    = req.url().split('?').next().unwrap_or("/").to_string();

    if method == Method::Options {
        let _ = req.respond(
            Response::empty(204)
                .with_header(h("Access-Control-Allow-Origin", "*"))
                .with_header(h("Access-Control-Allow-Methods", "GET, POST, DELETE, OPTIONS"))
                .with_header(h("Access-Control-Allow-Headers", "Content-Type, Authorization")),
        );
        return;
    }

    // ── Bearer auth guard (only for API endpoints, not playground/health/keys) ──
    let needs_auth = matches!(
        (method.as_str(), url.as_str()),
        ("POST", "/v1/chat/completions") | ("POST", "/chat/completions")
        | ("GET",  "/v1/models") | ("GET",  "/models")
    );
    if needs_auth {
        let bearer = extract_bearer(req.headers());
        let ok = keys.lock().unwrap().validate(bearer.as_deref().unwrap_or(""));
        if !ok {
            let _ = json_err(req, 401, "Unauthorized: invalid or missing Bearer token. Generate a key at /v1/keys or in the playground.", "auth_error");
            return;
        }
    }

    let r = match (method.as_str(), url.as_str()) {
        ("GET",  "/") | ("GET",  "/playground") => serve_playground(req),
        ("POST", "/v1/chat/completions") | ("POST", "/chat/completions")
            => handle_completions(req, cfg, stats, rtk, caveman),
        ("GET",  "/v1/models") | ("GET",  "/models") => handle_models(req, cfg),
        ("GET",  "/health")       => handle_health(req, stats, uptime),
        ("GET",  "/stats")        => handle_stats(req, stats),
        ("POST", "/reload")       => handle_reload(req, cfg),
        // ── API key management ───────────────────────────────────────────────
        ("GET",  "/v1/keys")      => handle_keys_list(req, keys),
        ("POST", "/v1/keys")      => handle_keys_create(req, keys),
        _ if method.as_str() == "DELETE" && url.starts_with("/v1/keys/")
            => handle_keys_delete(req, keys, &url),
        _ => json_err(req, 404, &format!("Not found: {url}"), "not_found"),
    };
    if let Err(e) = r { eprintln!("[server] {e}"); }
}

/// Extract the Bearer token from `Authorization: Bearer <token>` header.
///
/// Uses `HeaderField::equiv` for case-insensitive field matching (tiny_http 0.12).
fn extract_bearer(headers: &[tiny_http::Header]) -> Option<String> {
    headers.iter()
        .find(|h| h.field.equiv("authorization"))
        .and_then(|h| {
            // AsciiString implements Display; to_string() gives us a plain &str-compatible String
            let v = h.value.to_string();
            v.strip_prefix("Bearer ").map(|t| t.trim().to_string())
        })
}

// ─── Playground SPA ──────────────────────────────────────────────────────────

fn serve_playground(req: Request) -> Result<(), Box<dyn std::error::Error>> {
    req.respond(
        Response::from_string(PLAYGROUND_HTML)
            .with_header(h("Content-Type", "text/html; charset=utf-8"))
            .with_header(h("Cache-Control", "no-cache")),
    )?;
    Ok(())
}

// ─── Chat completions (streaming + non-streaming) ─────────────────────────────

fn handle_completions(
    mut req: Request, cfg: &Config, stats: &Arc<Mutex<Stats>>,
    rtk_on: bool, caveman: Option<CavemanLevel>,
) -> Result<(), Box<dyn std::error::Error>> {
    let raw = read_body(&mut req)?;
    let mut body: Value = serde_json::from_str(&raw)
        .map_err(|e| format!("JSON: {e}"))?;

    let model_id   = body["model"].as_str().unwrap_or("auto").to_string();
    let max_tokens = body["max_tokens"].as_u64().map(|n| n as u32);
    let streaming  = body["stream"].as_bool().unwrap_or(false);

    let msgs_val = match body["messages"].as_array() {
        Some(m) if !m.is_empty() => m.clone(),
        _ => return json_err(req, 400, "messages array required", "invalid_request"),
    };

    let (user_text, sys_text) = extract_texts(&msgs_val);
    if user_text.is_empty() {
        return json_err(req, 400, "No user message found", "invalid_request");
    }

    // Classify + tier-route
    let is_auto = matches!(model_id.as_str(), "auto" | "omrp/auto" | "omrp/auto-free");

    let (routed_model, tier_str, reasoning) = if is_auto {
        let (effective, forced) = match detect_mode_override(&user_text) {
            Some(ov) => (ov.cleaned_prompt, Some(ov.tier)),
            None     => (user_text.clone(), None),
        };
        let cls  = classify_prompt(&effective, if sys_text.is_empty() { None } else { Some(&sys_text) });
        let tier = forced.unwrap_or_else(|| cls.tier.unwrap_or(PromptTier::Medium));

        let pl       = bootstrap_pipeline(cfg);
        let router   = RouterEngine::default();
        let req2     = RouteRequest { max_inflight_per_model: Some(3), ..Default::default() };
        let tier_ids: Vec<String> = cfg.model.iter()
            .filter(|m| tier_from_str(&m.tier) == tier).map(|m| m.id.clone()).collect();
        let dec = pl.state().read(|s| select_for_tier(s, &req2, tier, &tier_ids, &router));

        let rsn = format!("tier={} score={:.3} signals={}",
            tier.as_str(), dec.score,
            cls.signals.iter().take(3).cloned().collect::<Vec<_>>().join(","));
        (dec.selected_model, tier.as_str().to_string(), rsn)
    } else {
        (model_id.clone(), "EXPLICIT".into(), format!("explicit:{model_id}"))
    };

    if routed_model.is_empty() {
        stats.lock().unwrap().errors += 1;
        return json_err(req, 503, "No available model", "unavailable");
    }

    { let mut s = stats.lock().unwrap();
      s.requests += 1;
      *s.by_tier.entry(tier_str.clone()).or_insert(0) += 1;
      *s.by_model.entry(routed_model.clone()).or_insert(0) += 1; }

    // RTK
    let rtk_info = if rtk_on {
        let st = compress_messages(&mut body, true);
        format_rtk_log(st.as_ref()).unwrap_or_default()
    } else { String::new() };

    // Caveman
    if let Some(lvl) = caveman { inject_caveman(&mut body, lvl); }

    // Rebuild messages
    let final_msgs: Vec<Message> = body["messages"].as_array().unwrap_or(&vec![])
        .iter().map(|m| Message {
            role:    m["role"].as_str().unwrap_or("user").to_string(),
            content: m["content"].as_str().unwrap_or("").to_string(),
        }).collect();

    // Resolve provider
    let prov_name = cfg.model.iter()
        .find(|m| m.id == routed_model)
        .map(|m| m.provider.as_str()).unwrap_or("openrouter")
        .to_string();

    let client = match CompatClient::for_provider(&prov_name) {
        Ok(c) => c,
        Err(e) => {
            stats.lock().unwrap().errors += 1;
            return json_err(req, 503, &e, "unavailable");
        }
    };

    eprintln!("[omrp] {} → [{prov_name}] {routed_model} (tier:{tier_str} stream:{streaming})",
        user_text.chars().take(50).collect::<String>());

    // ── Build upstream body ──────────────────────────────────────────────────
    let upstream_body = json!({
        "model": routed_model,
        "messages": final_msgs.iter().map(|m| json!({"role":m.role,"content":m.content})).collect::<Vec<_>>(),
        "max_tokens": max_tokens.unwrap_or(1024),
        "stream": streaming,
    });

    if streaming {
        // ── STREAMING PATH: pipe SSE directly from provider to browser ──────
        // Find first successful streaming provider, then respond (consuming req once).
        let mut stream_result: Option<(Box<dyn std::io::Read + Send + 'static>, String)> = None;

        let fallback: Vec<String> = cfg.model.iter()
            .filter(|m| tier_from_str(&m.tier) == tier_from_str(&tier_str) && m.id != routed_model)
            .map(|m| m.id.clone()).take(3).collect();
        let mut chain = vec![routed_model.clone()];
        chain.extend(fallback);

        for model_id in &chain {
            let p = cfg.model.iter().find(|m| &m.id == model_id)
                .map(|m| m.provider.clone())
                .unwrap_or_else(|| prov_name.clone());
            let mc = match CompatClient::for_provider(&p) {
                Ok(c)  => c,
                Err(e) => { eprintln!("  ✗ {model_id} ({p}): {e}"); continue; }
            };

            let mut sb = upstream_body.clone();
            sb["model"] = json!(model_id);

            match mc.stream_request(&sb) {
                Ok(reader) => { stream_result = Some((reader, model_id.clone())); break; }
                Err(e) => eprintln!("  ✗ {model_id} stream failed: {}", format_provider_error(&e)),
            }
        }

        match stream_result {
            Some((reader, used_model)) => {
                let mut resp = Response::empty(StatusCode(200))
                    .with_data(reader, None)
                    .with_header(h("Content-Type", "text/event-stream"))
                    .with_header(h("Cache-Control", "no-cache"))
                    .with_header(h("Connection", "keep-alive"))
                    .with_header(h("Access-Control-Allow-Origin", "*"))
                    .with_header(h("X-OMRP-Model", &used_model))
                    .with_header(h("X-OMRP-Tier",  &tier_str))
                    .with_header(h("X-OMRP-Reasoning", &reasoning[..reasoning.len().min(200)]));
                if !rtk_info.is_empty() { resp = resp.with_header(h("X-OMRP-RTK", &rtk_info)); }
                req.respond(resp)?;
            }
            None => {
                stats.lock().unwrap().errors += 1;
                json_err(req, 502, "All streaming attempts failed", "upstream_error")?;
            }
        }
    } else {
        // ── NON-STREAMING PATH ───────────────────────────────────────────────
        match client.complete_with_retry(&routed_model, &final_msgs, max_tokens) {
            Ok(cr) => {
                let resp_body = json!({
                    "id": format!("omrp-{}", now_millis()),
                    "object": "chat.completion",
                    "model": cr.model_used,
                    "choices": [{"index":0,"message":{"role":"assistant","content":cr.text},"finish_reason":"stop"}],
                    "usage": {"prompt_tokens":0,"completion_tokens":cr.tokens_used,"total_tokens":cr.tokens_used}
                });
                let mut resp = Response::from_string(resp_body.to_string())
                    .with_status_code(StatusCode(200))
                    .with_header(h("Content-Type", "application/json"))
                    .with_header(h("Access-Control-Allow-Origin", "*"))
                    .with_header(h("X-OMRP-Model",     &cr.model_used))
                    .with_header(h("X-OMRP-Tier",      &tier_str))
                    .with_header(h("X-OMRP-Reasoning", &reasoning[..reasoning.len().min(200)]));
                if !rtk_info.is_empty() { resp = resp.with_header(h("X-OMRP-RTK", &rtk_info)); }
                req.respond(resp)?;
            }
            Err(e) => {
                stats.lock().unwrap().errors += 1;
                json_err(req, 502, &format_provider_error(&e), "upstream_error")?;
            }
        }
    }

    Ok(())
}

// ─── Other handlers ───────────────────────────────────────────────────────────

fn handle_models(req: Request, cfg: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let now = now_secs();
    let mut models = vec![
        json!({"id":"auto",          "object":"model","created":now,"owned_by":"omrp"}),
        json!({"id":"omrp/auto",     "object":"model","created":now,"owned_by":"omrp"}),
        json!({"id":"omrp/auto-free","object":"model","created":now,"owned_by":"omrp"}),
    ];
    for m in &cfg.model {
        models.push(json!({"id":m.id,"object":"model","created":now,"owned_by":m.provider}));
    }
    req.respond(
        Response::from_string(json!({"object":"list","data":models}).to_string())
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
        "status":"ok","version":env!("CARGO_PKG_VERSION"),
        "uptime_secs":uptime,"requests":s.requests,"errors":s.errors
    }).to_string()).with_header(h("Content-Type","application/json"))
                  .with_header(h("Access-Control-Allow-Origin","*")))?;
    Ok(())
}

fn handle_stats(req: Request, stats: &Arc<Mutex<Stats>>)
    -> Result<(), Box<dyn std::error::Error>>
{
    let s = stats.lock().unwrap();
    req.respond(Response::from_string(json!({
        "requests":s.requests,"errors":s.errors,
        "by_tier":s.by_tier,"by_model":s.by_model,"started_unix":s.started_unix
    }).to_string()).with_header(h("Content-Type","application/json"))
                  .with_header(h("Access-Control-Allow-Origin","*")))?;
    Ok(())
}

fn handle_reload(req: Request, cfg: &Config)
    -> Result<(), Box<dyn std::error::Error>>
{
    req.respond(Response::from_string(json!({
        "status":"ok",
        "note":"restart omrp serve to pick up config changes",
        "models":cfg.model.len()
    }).to_string()).with_header(h("Content-Type","application/json")))?;
    Ok(())
}

// ─── API key management ───────────────────────────────────────────────────────

/// `GET /v1/keys` — list all registered keys (id, label, created; full key hidden).
fn handle_keys_list(req: Request, keys: &Arc<Mutex<KeyStore>>)
    -> Result<(), Box<dyn std::error::Error>>
{
    let ks = keys.lock().unwrap();
    let list: Vec<Value> = ks.keys.iter().map(|k| json!({
        "id":         k.id,
        "label":      k.label,
        "created":    k.created,
        "key_prefix": &k.key[..k.key.len().min(16)],
    })).collect();
    let body = json!({
        "object":        "list",
        "data":          list,
        "auth_required": ks.auth_required(),
    });
    req.respond(
        Response::from_string(body.to_string())
            .with_header(h("Content-Type", "application/json"))
            .with_header(h("Access-Control-Allow-Origin", "*")),
    )?;
    Ok(())
}

/// `POST /v1/keys` — create a new key.
///
/// Body (JSON, optional): `{ "label": "Cursor" }`
/// Response: includes the full `key` field — **shown once, not retrievable later**.
fn handle_keys_create(mut req: Request, keys: &Arc<Mutex<KeyStore>>)
    -> Result<(), Box<dyn std::error::Error>>
{
    let raw = read_body(&mut req).unwrap_or_default();
    let label = serde_json::from_str::<Value>(&raw)
        .ok()
        .and_then(|v| v["label"].as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "default".to_string());
    let label = label.chars().take(64).collect::<String>();

    let mut ks = keys.lock().unwrap();
    match ks.create(&label) {
        Ok(entry) => {
            let body = json!({
                "id":      entry.id,
                "key":     entry.key,
                "label":   entry.label,
                "created": entry.created,
                "note":    "Copy this key — it will not be shown again.",
            });
            req.respond(
                Response::from_string(body.to_string())
                    .with_status_code(StatusCode(201))
                    .with_header(h("Content-Type", "application/json"))
                    .with_header(h("Access-Control-Allow-Origin", "*")),
            )?;
        }
        Err(e) => {
            json_err(req, 500, &format!("Could not create key: {e}"), "server_error")?;
        }
    }
    Ok(())
}

/// `DELETE /v1/keys/{id}` — revoke a key by its short ID.
fn handle_keys_delete(req: Request, keys: &Arc<Mutex<KeyStore>>, url: &str)
    -> Result<(), Box<dyn std::error::Error>>
{
    let id = url.trim_start_matches("/v1/keys/");
    if id.is_empty() {
        return json_err(req, 400, "Key ID required", "invalid_request");
    }
    let mut ks = keys.lock().unwrap();
    match ks.delete(id) {
        Ok(true)  => {
            req.respond(
                Response::from_string(json!({"status":"ok","deleted":id}).to_string())
                    .with_header(h("Content-Type", "application/json"))
                    .with_header(h("Access-Control-Allow-Origin", "*")),
            )?;
        }
        Ok(false) => {
            json_err(req, 404, &format!("Key not found: {id}"), "not_found")?;
        }
        Err(e) => {
            json_err(req, 500, &format!("Could not delete key: {e}"), "server_error")?;
        }
    }
    Ok(())
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn extract_texts(msgs: &[Value]) -> (String, String) {
    let mut sys = String::new();
    let mut user = String::new();
    for m in msgs {
        let role    = m["role"].as_str().unwrap_or("");
        let content = m["content"].as_str().unwrap_or("");
        match role {
            "system"|"developer" => { if !sys.is_empty() { sys.push('\n'); } sys.push_str(content); }
            "user" => user = content.to_string(),
            _ => {}
        }
    }
    (user, sys)
}

fn read_body(req: &mut Request) -> Result<String, std::io::Error> {
    use std::io::Read;
    let mut buf = String::new();
    req.as_reader().read_to_string(&mut buf)?;
    Ok(buf)
}

fn h(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header")
}

fn json_err(req: Request, status: u16, msg: &str, t: &str)
    -> Result<(), Box<dyn std::error::Error>>
{
    req.respond(
        Response::from_string(json!({"error":{"message":msg,"type":t,"code":status}}).to_string())
            .with_status_code(StatusCode(status))
            .with_header(h("Content-Type", "application/json"))
            .with_header(h("Access-Control-Allow-Origin", "*")),
    )?;
    Ok(())
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()
}
fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis()
}

// ─── Playground HTML ─────────────────────────────────────────────────────────

static PLAYGROUND_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>OMRP Playground</title>
<style>
:root{
  --bg:#0d1117;--surface:#161b22;--border:#30363d;
  --text:#e6edf3;--muted:#8b949e;--accent:#58a6ff;
  --green:#3fb950;--orange:#e3b341;--red:#f85149;--purple:#a371f7;
}
*{box-sizing:border-box;margin:0;padding:0}
body{background:var(--bg);color:var(--text);font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',system-ui,sans-serif;font-size:14px;height:100dvh;display:flex;flex-direction:column;overflow:hidden}

/* ── Header ── */
header{background:var(--surface);border-bottom:1px solid var(--border);padding:8px 14px;display:flex;align-items:center;gap:10px;flex-shrink:0;flex-wrap:wrap}
.logo{font-weight:700;font-size:15px;color:var(--accent);white-space:nowrap;margin-right:4px}
.controls{display:flex;gap:7px;flex-wrap:wrap;align-items:center;flex:1}
select{background:var(--bg);border:1px solid var(--border);color:var(--text);border-radius:6px;padding:4px 8px;font-size:12px;cursor:pointer;max-width:260px}
select:focus{outline:1px solid var(--accent)}
.rtk-lbl{display:flex;align-items:center;gap:5px;cursor:pointer;user-select:none;color:var(--muted);font-size:12px;padding:4px 8px;border:1px solid var(--border);border-radius:6px;background:var(--bg)}
.rtk-lbl.on{color:var(--green);border-color:var(--green)}
.rtk-lbl input{width:13px;height:13px;cursor:pointer;accent-color:var(--green)}
.hdr-right{margin-left:auto;display:flex;gap:6px}
.btn-sm{background:transparent;border:1px solid var(--border);color:var(--muted);border-radius:6px;padding:4px 10px;font-size:12px;cursor:pointer}
.btn-sm:hover{border-color:var(--red);color:var(--red)}
.btn-keys{background:transparent;border:1px solid var(--border);color:var(--muted);border-radius:6px;padding:4px 10px;font-size:12px;cursor:pointer}
.btn-keys:hover{border-color:var(--accent);color:var(--accent)}
.btn-keys.active{border-color:var(--orange);color:var(--orange)}

/* ── Keys panel ── */
.keys-overlay{display:none;position:fixed;inset:0;background:rgba(0,0,0,.65);z-index:100;align-items:flex-start;justify-content:center;padding-top:56px}
.keys-overlay.open{display:flex}
.keys-panel{background:var(--surface);border:1px solid var(--border);border-radius:10px;padding:20px;width:min(580px,96vw);max-height:78vh;overflow-y:auto;display:flex;flex-direction:column;gap:14px}
.keys-panel h3{font-size:14px;font-weight:700}
.keys-info{font-size:12px;color:var(--muted);line-height:1.6}
.keys-info strong{color:var(--text)}
.keys-info code{background:var(--bg);padding:1px 5px;border-radius:4px;font-family:monospace;font-size:11px}
.keys-row{display:flex;gap:8px;align-items:center}
.keys-row input{flex:1;background:var(--bg);border:1px solid var(--border);color:var(--text);border-radius:6px;padding:6px 10px;font-size:13px;outline:none}
.keys-row input:focus{border-color:var(--accent)}
.btn-gen{background:var(--accent);border:none;color:#000;border-radius:6px;padding:7px 14px;font-size:13px;font-weight:600;cursor:pointer;white-space:nowrap}
.key-item{background:var(--bg);border:1px solid var(--border);border-radius:6px;padding:10px 12px;display:flex;align-items:center;gap:10px}
.key-info{flex:1;min-width:0}
.key-label{font-weight:600;font-size:13px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.key-meta{font-size:11px;color:var(--muted);font-family:monospace;margin-top:3px}
.btn-revoke{background:transparent;border:1px solid var(--border);color:var(--muted);border-radius:5px;padding:3px 9px;font-size:11px;cursor:pointer;white-space:nowrap}
.btn-revoke:hover{border-color:var(--red);color:var(--red)}
.key-new-box{background:var(--bg);border:1px solid var(--green);border-radius:6px;padding:10px 12px;display:flex;align-items:center;gap:8px}
.key-new-val{flex:1;font-family:monospace;font-size:11px;color:var(--green);word-break:break-all;line-height:1.5}
.btn-copy{background:var(--green);border:none;color:#000;border-radius:5px;padding:5px 10px;font-size:12px;font-weight:600;cursor:pointer;white-space:nowrap}
.auth-badge{padding:2px 8px;border-radius:8px;font-size:11px;font-weight:700;margin-left:8px}
.auth-on{background:#0d2e18;color:var(--green)}
.auth-off{background:#1a1f2b;color:var(--muted)}

/* ── Messages ── */
#msgs{flex:1;overflow-y:auto;padding:16px;display:flex;flex-direction:column;gap:14px;scroll-behavior:smooth}
#msgs:empty::after{content:"Type a message to start.\a Mode prefixes are stripped before sending:\a /simple  /max  /reasoning  [complex]  deep mode:";white-space:pre;color:var(--muted);font-size:13px;text-align:center;display:block;margin:40px auto;line-height:2}

.msg{max-width:860px;width:100%;margin:0 auto}
.msg-hdr{display:flex;align-items:center;gap:7px;margin-bottom:5px;font-size:12px}
.role-you{color:var(--accent);font-weight:600}
.role-ai{color:var(--green);font-weight:600}
.tier-badge{padding:2px 7px;border-radius:10px;font-size:10px;font-weight:700;letter-spacing:.05em}
.t-SIMPLE   {background:#0d2e18;color:#3fb950}
.t-MEDIUM   {background:#0d1f3d;color:#58a6ff}
.t-COMPLEX  {background:#3a2900;color:#e3b341}
.t-REASONING{background:#2a1550;color:#a371f7}
.t-EXPLICIT {background:#1a1f2b;color:#8b949e}
.model-lbl{font-family:monospace;font-size:11px;color:var(--muted)}
.time-lbl{font-size:11px;color:var(--muted)}

.msg-body{line-height:1.65;word-break:break-word}
.msg.user .msg-body{background:var(--surface);border:1px solid var(--border);border-radius:8px;padding:9px 13px;white-space:pre-wrap}
.msg.ai .msg-body{padding:2px 0}
.code-block{background:var(--surface);border:1px solid var(--border);border-radius:6px;padding:11px 14px;margin:7px 0;overflow-x:auto;font-family:'SFMono-Regular',Consolas,monospace;font-size:12.5px;line-height:1.5;white-space:pre}
.icode{background:var(--surface);padding:1px 5px;border-radius:4px;font-family:monospace;font-size:12px}
.cursor{display:inline-block;width:2px;height:.9em;background:var(--accent);animation:blink .7s step-end infinite;vertical-align:text-bottom;margin-left:1px}
@keyframes blink{50%{opacity:0}}

/* ── Footer ── */
footer{background:var(--surface);border-top:1px solid var(--border);padding:8px 14px;flex-shrink:0}
.hint{font-size:11px;color:var(--muted);margin-bottom:7px}
.inp-row{display:flex;gap:8px;align-items:flex-end}
textarea{flex:1;background:var(--bg);border:1px solid var(--border);color:var(--text);border-radius:8px;padding:9px 12px;font-size:14px;font-family:inherit;resize:none;min-height:42px;max-height:180px;line-height:1.5;outline:none}
textarea:focus{border-color:var(--accent)}
textarea::placeholder{color:var(--muted)}
#send{background:var(--accent);border:none;color:#000;border-radius:8px;padding:9px 16px;font-size:14px;font-weight:600;cursor:pointer;height:42px;white-space:nowrap}
#send:disabled{opacity:.4;cursor:not-allowed}
</style>
</head>
<body>
<header>
  <span class="logo">⚡ OMRP</span>
  <div class="controls">
    <select id="mdl" title="Model"><option value="omrp/auto">auto (OMRP picks)</option></select>
    <select id="tier" title="Tier override">
      <option value="">auto tier</option>
      <option value="simple">🟢 simple</option>
      <option value="medium">🔵 medium</option>
      <option value="complex">🟡 complex</option>
      <option value="reasoning">🟣 reasoning</option>
    </select>
    <select id="cave" title="Caveman mode">
      <option value="">caveman: off</option>
      <option value="lite">🪨 lite  (~20% less)</option>
      <option value="full">🪨🪨 full  (~40% less)</option>
      <option value="ultra">🪨🪨🪨 ultra (~65% less)</option>
    </select>
    <label class="rtk-lbl" id="rtkLbl" title="Compress tool outputs (RTK)">
      <input type="checkbox" id="rtk"> RTK
    </label>
  </div>
  <div class="hdr-right">
    <button class="btn-keys" id="keysBtn" onclick="toggleKeys()" title="Manage API keys">🔑 Keys</button>
    <button class="btn-sm" onclick="clearChat()">Clear</button>
  </div>
</header>

<!-- ── API Keys overlay ── -->
<div class="keys-overlay" id="keysOverlay" onclick="if(event.target===this)toggleKeys()">
  <div class="keys-panel">
    <div style="display:flex;align-items:center;justify-content:space-between">
      <h3>API Keys <span id="authBadge" class="auth-badge auth-off">auth: off</span></h3>
      <button class="btn-sm" onclick="toggleKeys()" style="padding:2px 8px">✕</button>
    </div>
    <div class="keys-info" id="keysInfo">Loading…</div>
    <div class="keys-row">
      <input id="keyLabel" placeholder='Label (e.g. "Cursor", "Claude Desktop", "Continue")' maxlength="64">
      <button class="btn-gen" onclick="genKey()">Generate key</button>
    </div>
    <div id="newKeyBox" style="display:none" class="key-new-box">
      <span class="key-new-val" id="newKeyVal"></span>
      <button class="btn-copy" id="copyBtn" onclick="copyKey()">Copy</button>
    </div>
    <div id="keysList"></div>
  </div>
</div>

<div id="msgs"></div>

<footer>
  <div class="hint">Mode prefixes work too: /simple &nbsp; /max &nbsp; /reasoning &nbsp; [complex] &nbsp; deep mode: &hellip;</div>
  <div class="inp-row">
    <textarea id="inp" placeholder="Message OMRP… (Enter = send, Shift+Enter = newline)" rows="1"></textarea>
    <button id="send" onclick="send()">Send ↵</button>
  </div>
</footer>

<script>
// ── State ─────────────────────────────────────────────────────────────────────
const hist = [];   // [{role, content}]
let busy = false;

// ── Boot: load model list ─────────────────────────────────────────────────────
(async () => {
  try {
    const {data=[]} = await (await fetch('/v1/models')).json();
    const sel = document.getElementById('mdl');
    data
      .filter(m => m.id && m.id !== 'auto' && !m.id.startsWith('omrp/'))
      .sort((a,b) => a.id.localeCompare(b.id))
      .forEach(m => {
        const o = document.createElement('option');
        o.value = m.id;
        o.textContent = m.owned_by ? `${m.id}  (${m.owned_by})` : m.id;
        sel.appendChild(o);
      });
  } catch {}
})();

// ── Helpers ───────────────────────────────────────────────────────────────────
const esc = s => s.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;');

function render(raw) {
  // fenced code blocks
  raw = raw.replace(/```[\w.-]*\n?([\s\S]*?)```/g,
    (_,c)=>`<div class="code-block">${esc(c.trimEnd())}</div>`);
  // inline code
  raw = raw.replace(/`([^`\n]+)`/g, (_,c)=>`<span class="icode">${esc(c)}</span>`);
  // bold
  raw = raw.replace(/\*\*(.+?)\*\*/g,'<strong>$1</strong>');
  return raw;
}

function tierCls(t) {
  return 't-' + (t||'EXPLICIT').toUpperCase().split(' ')[0];
}

function addMsg(role, text='', meta={}) {
  const el = document.createElement('div');
  el.className = 'msg ' + role;

  const hdr = document.createElement('div');
  hdr.className = 'msg-hdr';
  const rl = document.createElement('span');
  rl.className = role==='user' ? 'role-you' : 'role-ai';
  rl.textContent = role==='user' ? 'You' : 'OMRP';
  hdr.appendChild(rl);
  if (meta.tier) {
    const b = document.createElement('span');
    b.className = 'tier-badge '+tierCls(meta.tier);
    b.textContent = meta.tier;
    hdr.appendChild(b);
  }
  if (meta.model) {
    const m = document.createElement('span');
    m.className = 'model-lbl';
    m.textContent = meta.model;
    hdr.appendChild(m);
  }

  const body = document.createElement('div');
  body.className = 'msg-body';
  if (role==='user') body.textContent = text;
  else body.innerHTML = render(text);

  el.appendChild(hdr);
  el.appendChild(body);
  document.getElementById('msgs').appendChild(el);
  scrollDown();
  return {hdr, body};
}

function scrollDown() {
  const m = document.getElementById('msgs');
  m.scrollTop = m.scrollHeight;
}

function clearChat() { hist.length=0; document.getElementById('msgs').innerHTML=''; }

// ── Send ──────────────────────────────────────────────────────────────────────
async function send() {
  if (busy) return;
  const inp  = document.getElementById('inp');
  const text = inp.value.trim();
  if (!text) return;
  inp.value = ''; inp.style.height = 'auto';

  busy = true;
  document.getElementById('send').disabled = true;

  const model   = document.getElementById('mdl').value  || 'omrp/auto';
  const tierOvr = document.getElementById('tier').value;
  const cave    = document.getElementById('cave').value;
  const rtk     = document.getElementById('rtk').checked;

  // Inject system hint for tier / caveman overrides (server also handles these)
  const sysHints = [
    tierOvr && `tier:${tierOvr}`,
    cave    && `caveman:${cave}`,
    rtk     && 'rtk:on',
  ].filter(Boolean).join(' ');

  addMsg('user', text);
  hist.push({role:'user', content:text});

  const msgs = [
    ...(sysHints ? [{role:'system', content:sysHints}] : []),
    ...hist,
  ];

  // Create assistant placeholder + blinking cursor
  const {hdr, body:aiBody} = addMsg('ai', '', {});
  const cur = document.createElement('span');
  cur.className = 'cursor';
  aiBody.appendChild(cur);

  const t0 = Date.now();
  let fullText = '';

  try {
    const res = await fetch('/v1/chat/completions', {
      method:'POST',
      headers:{'Content-Type':'application/json'},
      body: JSON.stringify({model, messages:msgs, stream:true, max_tokens:2048}),
    });

    // Read OMRP routing metadata from headers (available before body)
    const omrpTier  = res.headers.get('X-OMRP-Tier')  || '';
    const omrpModel = res.headers.get('X-OMRP-Model') || '';

    if (omrpTier) {
      const b = document.createElement('span');
      b.className = 'tier-badge '+tierCls(omrpTier);
      b.textContent = omrpTier;
      hdr.appendChild(b);
    }
    if (omrpModel) {
      const m = document.createElement('span');
      m.className = 'model-lbl';
      m.textContent = omrpModel;
      hdr.appendChild(m);
    }

    if (!res.ok) {
      let msg = 'HTTP ' + res.status;
      try { msg = (await res.json()).error?.message || msg; } catch {}
      cur.remove();
      aiBody.innerHTML = `<span style="color:var(--red)">${esc(msg)}</span>`;
      hist.pop();
      return;
    }

    // Stream body chunks
    const reader  = res.body.getReader();
    const decoder = new TextDecoder();
    let   lineBuf = '';

    while (true) {
      const {done, value} = await reader.read();
      if (done) break;

      lineBuf += decoder.decode(value, {stream:true});
      const lines = lineBuf.split('\n');
      lineBuf = lines.pop() || '';

      for (const line of lines) {
        if (!line.startsWith('data: ')) continue;
        const data = line.slice(6).trim();
        if (data === '[DONE]') { reader.cancel(); break; }
        try {
          const chunk = JSON.parse(data);
          const delta = chunk.choices?.[0]?.delta?.content || '';
          if (delta) {
            fullText += delta;
            cur.remove();
            aiBody.innerHTML = render(fullText);
            aiBody.appendChild(cur);
            scrollDown();
          }
        } catch {}
      }
    }

    // Finalise
    cur.remove();
    aiBody.innerHTML = render(fullText || '(empty response)');

    const t = document.createElement('span');
    t.className = 'time-lbl';
    t.textContent = ((Date.now()-t0)/1000).toFixed(1)+'s';
    hdr.appendChild(t);

    if (fullText) hist.push({role:'assistant', content:fullText});

  } catch(err) {
    cur.remove();
    aiBody.innerHTML = `<span style="color:var(--red)">${esc(String(err))}</span>`;
    hist.pop();
  } finally {
    busy = false;
    document.getElementById('send').disabled = false;
    scrollDown();
  }
}

// ── Keyboard ──────────────────────────────────────────────────────────────────
document.getElementById('inp').addEventListener('keydown', e => {
  if (e.key==='Enter' && !e.shiftKey) { e.preventDefault(); send(); return; }
  setTimeout(()=>{
    const t = e.target;
    t.style.height='auto';
    t.style.height=Math.min(t.scrollHeight,180)+'px';
  },0);
});

document.getElementById('rtk').addEventListener('change', e =>
  document.getElementById('rtkLbl').classList.toggle('on', e.target.checked));

// ── API Keys ──────────────────────────────────────────────────────────────────
let keysVisible = false;
let latestKey = '';

function toggleKeys() {
  keysVisible = !keysVisible;
  document.getElementById('keysOverlay').classList.toggle('open', keysVisible);
  document.getElementById('keysBtn').classList.toggle('active', keysVisible);
  if (keysVisible) loadKeys();
}

async function loadKeys() {
  try {
    const res = await fetch('/v1/keys');
    const {data=[], auth_required=false} = await res.json();

    const badge = document.getElementById('authBadge');
    badge.className = 'auth-badge ' + (auth_required ? 'auth-on' : 'auth-off');
    badge.textContent = auth_required ? 'auth: ON' : 'auth: off';

    document.getElementById('keysInfo').innerHTML = auth_required
      ? 'Bearer auth is <strong>enabled</strong>. Every API client must send:<br><code>Authorization: Bearer omrp-sk-…</code>'
      : 'No keys registered yet — all requests are accepted. Generate a key to enable authentication.';

    const list = document.getElementById('keysList');
    list.innerHTML = '';
    if (data.length === 0) {
      list.innerHTML = '<div style="color:var(--muted);font-size:12px;text-align:center;padding:10px 0">No keys yet. Generate one above.</div>';
    } else {
      data.forEach(k => {
        const ts = new Date(k.created * 1000).toLocaleDateString();
        const item = document.createElement('div');
        item.className = 'key-item';
        item.id = 'keyrow-' + k.id;
        item.innerHTML =
          '<div class="key-info">' +
            '<div class="key-label">' + esc(k.label) + '</div>' +
            '<div class="key-meta">' + esc(k.key_prefix) + '…&nbsp;&nbsp;·&nbsp;&nbsp;created ' + ts + '&nbsp;&nbsp;·&nbsp;&nbsp;id: ' + esc(k.id) + '</div>' +
          '</div>' +
          '<button class="btn-revoke" onclick="revokeKey(\'' + esc(k.id) + '\')">Revoke</button>';
        list.appendChild(item);
      });
    }
  } catch(e) {
    document.getElementById('keysInfo').textContent = 'Error loading keys: ' + e;
  }
}

async function genKey() {
  const label = (document.getElementById('keyLabel').value.trim() || 'default');
  try {
    const res = await fetch('/v1/keys', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify({label}),
    });
    if (!res.ok) { alert('Error generating key: ' + (await res.text())); return; }
    const {key} = await res.json();
    latestKey = key;
    document.getElementById('newKeyVal').textContent = key;
    document.getElementById('newKeyBox').style.display = 'flex';
    document.getElementById('copyBtn').textContent = 'Copy';
    document.getElementById('keyLabel').value = '';
    loadKeys();
  } catch(e) { alert('Error: ' + e); }
}

async function revokeKey(id) {
  if (!confirm('Revoke key ' + id + '?\nThis cannot be undone.')) return;
  try {
    await fetch('/v1/keys/' + id, {method: 'DELETE'});
    const row = document.getElementById('keyrow-' + id);
    if (row) row.remove();
    loadKeys();
    document.getElementById('newKeyBox').style.display = 'none';
    latestKey = '';
  } catch(e) { alert('Error: ' + e); }
}

function copyKey() {
  if (!latestKey) return;
  navigator.clipboard.writeText(latestKey).then(() => {
    const btn = document.getElementById('copyBtn');
    btn.textContent = 'Copied!';
    setTimeout(() => { btn.textContent = 'Copy'; }, 2000);
  }).catch(() => {
    // Fallback for browsers without clipboard API
    prompt('Copy this key (Ctrl+C):', latestKey);
  });
}

// Close keys overlay on Escape
document.addEventListener('keydown', e => {
  if (e.key === 'Escape' && keysVisible) toggleKeys();
});
</script>
</body>
</html>"#;
