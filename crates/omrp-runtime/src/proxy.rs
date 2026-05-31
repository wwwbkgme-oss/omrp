//! Proxy pool management — fetch, store, and rotate HTTP/SOCKS5 proxies.

use crate::db::Database;

/// Fetch proxies from the ProxyScrape API and upsert them into the pool.
/// Called on startup (if `proxy.enabled=1`) and by the admin refresh endpoint.
pub fn refresh_proxy_pool(db: &Database, source_url: &str) {
    let ts = now_secs();
    let resp = match ureq::get(source_url).call() {
        Ok(r)  => r,
        Err(e) => { eprintln!("[proxy] fetch error: {e}"); return; }
    };
    let body = match resp.into_string() {
        Ok(b)  => b,
        Err(e) => { eprintln!("[proxy] read error: {e}"); return; }
    };
    // Try JSON array of objects or plain text (one proxy per line)
    let count = if body.trim_start().starts_with('[') {
        ingest_json(db, &body, ts)
    } else {
        ingest_text(db, &body, ts)
    };
    eprintln!("[proxy] upserted {count} proxies");
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
        // Formats: http://ip:port  OR  ip:port  OR  protocol://ip:port
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
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
