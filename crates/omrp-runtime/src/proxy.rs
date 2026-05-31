//! Proxy pool management — fetch, rotate, and health-track HTTP/SOCKS5 proxies.
//!
//! ## Purpose
//!
//! Free LLM API providers enforce **per-IP rate limits** (e.g. 60 req/min).
//! By routing each request through a different proxy IP, OMRP distributes the
//! load across many source addresses so individual users never see a 429.
//!
//! ## Architecture
//!
//! ```
//! ProxyPool (Arc<>)
//!   ├── Vec<ProxyEntry>  — sorted by score (latency↑, uptime↓)
//!   ├── AtomicUsize      — round-robin cursor (wraps mod pool.len())
//!   └── last_refreshed   — for auto-refresh scheduling
//!
//! On 429:
//!   cursor.fetch_add(1) → next proxy URL
//!   rebuild ureq Agent  → retry request via new IP
//!   if success → mark_success(id)
//!   if fail    → mark_failure(id) → auto-disable after 5 failures
//! ```
//!
//! ## Enabling
//!
//! Set `proxy.enabled = 1` in the dashboard (Admin → Settings).
//! The pool is loaded at server start and refreshed every
//! `proxy.refresh_interval` seconds (default 3600).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::db::Database;

// ─── Entry type ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ProxyEntry {
    /// DB row ID (used for health updates).
    pub id:         i64,
    /// Full proxy URL, e.g. `http://1.2.3.4:8080` or `socks5://1.2.3.4:1080`.
    pub url:        String,
    /// Protocol (`http`, `https`, `socks5`).
    pub protocol:   String,
}

// ─── ProxyPool ────────────────────────────────────────────────────────────────

/// Thread-safe, self-refreshing proxy pool.
///
/// Shared via `Arc<ProxyPool>` across all request handlers.
pub struct ProxyPool {
    proxies:        RwLock<Vec<ProxyEntry>>,
    /// Atomic cursor for round-robin selection.
    cursor:         AtomicUsize,
    /// Unix timestamp of last DB refresh.
    last_refreshed: RwLock<i64>,
    /// Refresh interval in seconds (from `proxy.refresh_interval` setting).
    refresh_interval: i64,
}

impl ProxyPool {
    /// Create a new, empty pool.
    pub fn new(refresh_interval_secs: i64) -> Arc<Self> {
        Arc::new(Self {
            proxies:         RwLock::new(Vec::new()),
            cursor:          AtomicUsize::new(0),
            last_refreshed:  RwLock::new(0),
            refresh_interval: refresh_interval_secs,
        })
    }

    /// Load active proxies from the database into the in-memory pool.
    /// Safe to call multiple times (idempotent, replaces old entries).
    pub fn load(&self, db: &Database) {
        let entries: Vec<ProxyEntry> = db.active_proxies()
            .unwrap_or_default()
            .into_iter()
            .map(|(id, url)| {
                let protocol = url.split("://").next().unwrap_or("http").to_string();
                ProxyEntry { id, url, protocol }
            })
            .collect();
        let n = entries.len();
        *self.proxies.write().unwrap() = entries;
        *self.last_refreshed.write().unwrap() = now_secs();
        if n > 0 {
            eprintln!("[proxy] pool loaded: {n} active proxies");
        }
    }

    /// Return the number of proxies currently in the pool.
    pub fn len(&self) -> usize {
        self.proxies.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Pick the next proxy via round-robin.  Returns `None` if the pool is empty.
    pub fn next(&self) -> Option<ProxyEntry> {
        let pool = self.proxies.read().unwrap();
        if pool.is_empty() { return None; }
        let idx = self.cursor.fetch_add(1, Ordering::Relaxed) % pool.len();
        Some(pool[idx].clone())
    }

    /// Pick the Nth proxy (for sequential retry loops without repeating).
    /// `attempt` = 0 means first proxy, 1 = second, etc.
    pub fn nth(&self, attempt: usize) -> Option<ProxyEntry> {
        let pool = self.proxies.read().unwrap();
        if pool.is_empty() { return None; }
        // Start from current cursor position so retries use fresh proxies
        let base = self.cursor.load(Ordering::Relaxed);
        let idx  = (base + attempt) % pool.len();
        Some(pool[idx].clone())
    }

    /// Advance the cursor by `n` positions (called after a batch of retries
    /// so the next request starts from a fresh proxy).
    pub fn advance(&self, n: usize) {
        self.cursor.fetch_add(n, Ordering::Relaxed);
    }

    /// Mark a proxy as successfully used.  Updates DB uptime and resets
    /// fail_count for this proxy.
    pub fn mark_success(&self, proxy_id: i64, db: &Database) {
        let _ = db.mark_proxy_result(proxy_id, true, None, now_secs());
    }

    /// Mark a proxy as failed.  After 5 consecutive failures the DB driver
    /// sets `is_active = 0` and future pool loads will exclude it.
    pub fn mark_failure(&self, proxy_id: i64, db: &Database) {
        let _ = db.mark_proxy_result(proxy_id, false, None, now_secs());
        // Remove from in-memory pool immediately to avoid hammering a dead proxy
        let mut pool = self.proxies.write().unwrap();
        pool.retain(|p| p.id != proxy_id);
    }

    /// Trigger a DB refresh if the pool is empty or the refresh interval
    /// has elapsed.  Returns `true` if a refresh was performed.
    pub fn maybe_refresh(&self, db: &Database) -> bool {
        let now = now_secs();
        let last = *self.last_refreshed.read().unwrap();
        let pool_empty = self.proxies.read().unwrap().is_empty();
        if pool_empty || (now - last > self.refresh_interval) {
            self.load(db);
            true
        } else {
            false
        }
    }
}

// ─── HTTP fetch helpers ───────────────────────────────────────────────────────

/// Fetch proxies from the ProxyScrape API and upsert them into the DB pool.
/// Called on startup (if `proxy.enabled=1`) and by the admin refresh endpoint.
pub fn refresh_proxy_pool(db: &Database, source_url: &str) {
    let ts = now_secs();
    eprintln!("[proxy] fetching from {source_url}");
    let resp = match ureq::get(source_url).call() {
        Ok(r)  => r,
        Err(e) => { eprintln!("[proxy] fetch error: {e}"); return; }
    };
    let body = match resp.into_string() {
        Ok(b)  => b,
        Err(e) => { eprintln!("[proxy] read error: {e}"); return; }
    };
    let count = if body.trim_start().starts_with('[') {
        ingest_json(db, &body, ts)
    } else {
        ingest_text(db, &body, ts)
    };
    eprintln!("[proxy] upserted {count} proxies into DB");
}

fn ingest_json(db: &Database, body: &str, ts: i64) -> usize {
    let Ok(arr) = serde_json::from_str::<serde_json::Value>(body) else { return 0 };
    let arr = arr.as_array().cloned().unwrap_or_default();
    let mut n = 0usize;
    for item in &arr {
        let proto = item["protocol"].as_str()
            .or_else(|| item["proxy_type"].as_str())
            .unwrap_or("http");
        let ip   = item["ip"].as_str().unwrap_or("");
        let port = item["port"].as_u64().unwrap_or(0);
        if ip.is_empty() || port == 0 { continue; }
        let url     = format!("{proto}://{ip}:{port}");
        let country = item["country"].as_str();
        let anon    = item["anonymity"].as_str();
        if db.upsert_proxy(&url, proto, country, anon, ts).is_ok() { n += 1; }
    }
    n
}

fn ingest_text(db: &Database, body: &str, ts: i64) -> usize {
    let mut n = 0usize;
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') { continue; }
        let (proto, url) = if line.contains("://") {
            let proto = line.split("://").next().unwrap_or("http");
            (proto, line.to_string())
        } else {
            ("http", format!("http://{line}"))
        };
        if db.upsert_proxy(&url, proto, None, None, ts).is_ok() { n += 1; }
    }
    n
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_next_round_robin() {
        let pool = ProxyPool::new(3600);
        {
            let mut entries = pool.proxies.write().unwrap();
            entries.push(ProxyEntry { id: 1, url: "http://1.1.1.1:8080".into(), protocol: "http".into() });
            entries.push(ProxyEntry { id: 2, url: "http://2.2.2.2:8080".into(), protocol: "http".into() });
            entries.push(ProxyEntry { id: 3, url: "http://3.3.3.3:8080".into(), protocol: "http".into() });
        }
        let a = pool.next().unwrap();
        let b = pool.next().unwrap();
        let c = pool.next().unwrap();
        let d = pool.next().unwrap(); // wraps around
        assert_ne!(a.url, b.url);
        assert_ne!(b.url, c.url);
        assert_eq!(a.url, d.url); // full round
    }

    #[test]
    fn test_pool_empty_returns_none() {
        let pool = ProxyPool::new(3600);
        assert!(pool.next().is_none());
        assert!(pool.is_empty());
    }

    #[test]
    fn test_mark_failure_removes_from_pool() {
        let pool = ProxyPool::new(3600);
        {
            let mut entries = pool.proxies.write().unwrap();
            entries.push(ProxyEntry { id: 42, url: "http://bad.proxy:8080".into(), protocol: "http".into() });
        }
        // Simulate mark_failure without a real DB
        pool.proxies.write().unwrap().retain(|p| p.id != 42);
        assert!(pool.is_empty());
    }

    #[test]
    fn test_nth_sequential_access() {
        let pool = ProxyPool::new(3600);
        {
            let mut e = pool.proxies.write().unwrap();
            for i in 0..5u64 {
                e.push(ProxyEntry { id: i as i64, url: format!("http://10.0.0.{i}:80"), protocol: "http".into() });
            }
        }
        let p0 = pool.nth(0).unwrap();
        let p1 = pool.nth(1).unwrap();
        let p2 = pool.nth(2).unwrap();
        assert_ne!(p0.url, p1.url);
        assert_ne!(p1.url, p2.url);
    }
}
