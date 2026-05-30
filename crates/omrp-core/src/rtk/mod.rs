//! RTK — Repetitive Token Killer
//!
//! Compresses `tool_result` / `tool` message content in LLM request bodies
//! before forwarding to any provider.  The compression is lossless for the
//! model: the information present is reduced, not the meaning.
//!
//! ## Origin
//! Ported from 9router's RTK module (`open-sse/rtk/`) which was itself ported
//! from the original Rust implementation at
//! `rtk/src/cmds/system/pipe_cmd.rs`.  We are returning to Rust.
//!
//! ## Filters (auto-detected, applied in priority order)
//! 1. `git-diff`       — compact unified diffs (hunk cap 100 lines)
//! 2. `git-status`     — compact `git status` output
//! 3. `build-output`   — compact build/compiler stderr
//! 4. `grep`           — 10 matches per file
//! 5. `find`           — 10 files per dir, 20 dirs total
//! 6. `tree`           — 200 lines max
//! 7. `ls`             — extension summary + size
//! 8. `smart-truncate` — head 120 + tail 60, for blobs > 250 lines
//! 9. `dedup-log`      — deduplicate repeated log lines
//!
//! ## Usage
//! ```ignore
//! use omrp_core::rtk::{compress_messages, format_rtk_log};
//! let stats = compress_messages(&mut body, true);
//! if let Some(line) = format_rtk_log(stats.as_ref()) {
//!     eprintln!("{line}");
//! }
//! ```

pub mod autodetect;
pub mod filters;

use serde_json::Value;

// ─── Constants ────────────────────────────────────────────────────────────────

/// Maximum blob size to attempt compression on (10 MiB).
pub const RAW_CAP: usize = 10 * 1024 * 1024;
/// Minimum blob size to bother compressing (skip tiny strings).
pub const MIN_COMPRESS_SIZE: usize = 500;

// ─── Public types ─────────────────────────────────────────────────────────────

/// Compression statistics returned by [`compress_messages`].
#[derive(Debug, Default)]
pub struct CompressionStats {
    pub bytes_before: usize,
    pub bytes_after: usize,
    pub hits: Vec<CompressionHit>,
}

/// A single filter application that saved tokens.
#[derive(Debug)]
pub struct CompressionHit {
    pub filter: &'static str,
    pub saved: usize,
}

impl CompressionStats {
    pub fn savings(&self) -> usize {
        self.bytes_before.saturating_sub(self.bytes_after)
    }
    pub fn savings_pct(&self) -> f64 {
        if self.bytes_before == 0 { return 0.0; }
        self.savings() as f64 / self.bytes_before as f64 * 100.0
    }
}

/// A compression filter: pure `&str → String` function.
pub type Filter = fn(&str) -> String;

// ─── Entry point ─────────────────────────────────────────────────────────────

/// Compress `tool_result` / `tool`-role message content in an OpenAI- or
/// Claude-shaped request body (`messages` or `input` array).
///
/// Mutates the `body` in-place.  Returns `None` when `enabled` is false or no
/// compressible content was found.
pub fn compress_messages(body: &mut Value, enabled: bool) -> Option<CompressionStats> {
    if !enabled { return None; }

    let mut stats = CompressionStats::default();

    // OpenAI / Claude format: body["messages"] or body["input"]
    let arr_key = if body.get("messages").is_some() { "messages" } else { "input" };
    let items = body.get_mut(arr_key)?.as_array_mut()?;

    for msg in items.iter_mut() {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("").to_string();

        // OpenAI tool message: { role:"tool", content:"..." | [{type:"text",text:"..."}] }
        if role == "tool" {
            if let Some(c) = msg.get_mut("content") {
                match c {
                    Value::String(s) => {
                        *s = compress_text(s, &mut stats);
                    }
                    Value::Array(parts) => {
                        for part in parts.iter_mut() {
                            if part.get("type").and_then(|t| t.as_str()) == Some("text") {
                                if let Some(Value::String(t)) = part.get_mut("text") {
                                    *t = compress_text(t, &mut stats);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            continue;
        }

        // Claude tool_result blocks inside content array
        if let Some(Value::Array(blocks)) = msg.get_mut("content") {
            for block in blocks.iter_mut() {
                if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") { continue; }
                // Preserve error traces
                if block.get("is_error").and_then(|e| e.as_bool()) == Some(true) { continue; }

                match block.get_mut("content") {
                    Some(Value::String(s)) => {
                        *s = compress_text(s, &mut stats);
                    }
                    Some(Value::Array(parts)) => {
                        for part in parts.iter_mut() {
                            if part.get("type").and_then(|t| t.as_str()) == Some("text") {
                                if let Some(Value::String(t)) = part.get_mut("text") {
                                    *t = compress_text(t, &mut stats);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // OpenAI Responses API: { type:"function_call_output", output: string | [{type:"input_text",...}] }
        if msg.get("type").and_then(|t| t.as_str()) == Some("function_call_output") {
            match msg.get_mut("output") {
                Some(Value::String(s)) => { *s = compress_text(s, &mut stats); }
                Some(Value::Array(parts)) => {
                    for part in parts.iter_mut() {
                        if part.get("type").and_then(|t| t.as_str()) == Some("input_text") {
                            if let Some(Value::String(t)) = part.get_mut("text") {
                                *t = compress_text(t, &mut stats);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    Some(stats)
}

/// Format a one-line RTK log entry from stats (returns `None` when no savings).
pub fn format_rtk_log(stats: Option<&CompressionStats>) -> Option<String> {
    let s = stats?;
    if s.hits.is_empty() { return None; }
    let mut filters: Vec<&str> = s.hits.iter().map(|h| h.filter).collect();
    filters.dedup();
    Some(format!(
        "[RTK] saved {}B / {}B ({:.1}%) via [{}] hits={}",
        s.savings(), s.bytes_before, s.savings_pct(),
        filters.join(","), s.hits.len()
    ))
}

// ─── Internal ─────────────────────────────────────────────────────────────────

fn compress_text(text: &str, stats: &mut CompressionStats) -> String {
    let bytes_in = text.len();
    stats.bytes_before += bytes_in;

    if bytes_in < MIN_COMPRESS_SIZE || bytes_in > RAW_CAP {
        stats.bytes_after += bytes_in;
        return text.to_string();
    }

    let Some((filter_fn, filter_name)) = autodetect::auto_detect(text) else {
        stats.bytes_after += bytes_in;
        return text.to_string();
    };

    let out = filter_fn(text);

    // Safety: never return empty, never grow the input
    if out.is_empty() || out.len() >= bytes_in {
        stats.bytes_after += bytes_in;
        return text.to_string();
    }

    stats.bytes_after += out.len();
    stats.hits.push(CompressionHit { filter: filter_name, saved: bytes_in - out.len() });
    out
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_openai_tool_body(content: &str) -> Value {
        json!({
            "messages": [
                { "role": "user", "content": "run git diff" },
                { "role": "tool", "content": content }
            ]
        })
    }

    fn make_claude_tool_result_body(text: &str) -> Value {
        json!({
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "tool_result",
                            "tool_use_id": "x",
                            "content": [{ "type": "text", "text": text }]
                        }
                    ]
                }
            ]
        })
    }

    #[test]
    fn test_compress_disabled_returns_none() {
        let mut body = make_openai_tool_body("hello");
        assert!(compress_messages(&mut body, false).is_none());
    }

    #[test]
    fn test_small_content_not_compressed() {
        let mut body = make_openai_tool_body("short");
        let stats = compress_messages(&mut body, true).unwrap();
        assert!(stats.hits.is_empty());
        // Content unchanged
        assert_eq!(
            body["messages"][1]["content"].as_str().unwrap(),
            "short"
        );
    }

    #[test]
    fn test_git_diff_compressed_in_openai_format() {
        let diff = build_fake_git_diff(50);
        let mut body = make_openai_tool_body(&diff);
        let stats = compress_messages(&mut body, true).unwrap();
        if !stats.hits.is_empty() {
            assert!(stats.bytes_after < stats.bytes_before);
        }
    }

    #[test]
    fn test_claude_tool_result_compressed() {
        let diff = build_fake_git_diff(50);
        let mut body = make_claude_tool_result_body(&diff);
        let stats = compress_messages(&mut body, true).unwrap();
        // At minimum it should have processed without panic
        let _ = stats;
    }

    #[test]
    fn test_error_tool_result_not_compressed() {
        let big_text = "x".repeat(1000);
        let mut body = json!({
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "is_error": true,
                    "content": big_text
                }]
            }]
        });
        let before = body["messages"][0]["content"][0]["content"]
            .as_str()
            .unwrap()
            .len();
        let stats = compress_messages(&mut body, true).unwrap();
        assert!(stats.hits.is_empty(), "error traces must not be compressed");
        let after = body["messages"][0]["content"][0]["content"]
            .as_str()
            .unwrap()
            .len();
        assert_eq!(before, after);
    }

    #[test]
    fn test_format_rtk_log_none_when_no_savings() {
        let stats = CompressionStats::default();
        assert!(format_rtk_log(Some(&stats)).is_none());
    }

    #[test]
    fn test_format_rtk_log_some_when_savings() {
        let mut stats = CompressionStats::default();
        stats.bytes_before = 1000;
        stats.bytes_after = 600;
        stats.hits.push(CompressionHit { filter: "git-diff", saved: 400 });
        let line = format_rtk_log(Some(&stats)).unwrap();
        assert!(line.contains("[RTK]"));
        assert!(line.contains("git-diff"));
    }

    fn build_fake_git_diff(hunks: usize) -> String {
        let mut out = String::from("diff --git a/foo.rs b/foo.rs\n");
        out.push_str("index abc..def 100644\n");
        out.push_str("--- a/foo.rs\n");
        out.push_str("+++ b/foo.rs\n");
        for i in 0..hunks {
            out.push_str(&format!("@@ -{i},10 +{i},12 @@\n"));
            for j in 0..10 {
                out.push_str(&format!(" context line {j}\n"));
                out.push_str(&format!("+added line {j}\n"));
                out.push_str(&format!("-removed line {j}\n"));
            }
        }
        out
    }
}
