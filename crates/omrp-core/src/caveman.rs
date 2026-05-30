//! Caveman Mode — inject terse-reply prompts to save output tokens.
//!
//! Based on 9router's caveman module (`open-sse/rtk/caveman.js`) and the
//! upstream [caveman skill](https://github.com/JuliusBrussee/caveman).
//!
//! ## Intensity levels
//!
//! | Level | Token savings (typical) | Style |
//! |-------|------------------------|-------|
//! | `Lite`  | ~20%  | Terse but grammatical; drops filler words |
//! | `Full`  | ~40%  | Caveman fragments; drops articles, hedging |
//! | `Ultra` | ~65%  | Telegraphic; abbreviations, arrow causality |
//!
//! ## Safe boundaries (all levels)
//! Code blocks, file paths, commands, errors, and URLs are **never** affected.
//! Security warnings, irreversible action confirmations, and multi-step ordered
//! sequences use normal style.
//!
//! ## Usage
//! ```ignore
//! use omrp_core::caveman::{inject_caveman, CavemanLevel};
//! inject_caveman(&mut body, CavemanLevel::Full);
//! ```

use serde_json::Value;

// ─── Public types ─────────────────────────────────────────────────────────────

/// Caveman intensity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CavemanLevel {
    /// Terse but grammatical.  Drops filler and pleasantries.
    Lite,
    /// Caveman fragments.  Drops articles, hedging, and most politeness.
    Full,
    /// Telegraphic abbreviations and causality arrows.
    Ultra,
}

impl CavemanLevel {
    /// Parse from a string (case-insensitive).
    pub fn from_str(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "lite" | "light" => Some(Self::Lite),
            "full" => Some(Self::Full),
            "ultra" | "max" => Some(Self::Ultra),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Lite => "lite",
            Self::Full => "full",
            Self::Ultra => "ultra",
        }
    }

    /// Returns the system-prompt injection text.
    pub fn prompt(&self) -> &'static str {
        match self {
            Self::Lite => LITE_PROMPT,
            Self::Full => FULL_PROMPT,
            Self::Ultra => ULTRA_PROMPT,
        }
    }
}

// ─── Caveman prompts (adapted from 9router, MIT) ──────────────────────────────

const SHARED: &str =
    "Code blocks, file paths, commands, errors, URLs: keep exact. \
     Security warnings, irreversible action confirmations, multi-step ordered sequences: \
     write normal. Resume terse style after. \
     Active every response until user asks for normal mode.";

static LITE_PROMPT: &str = concat!(
    "Respond tersely. Keep grammar and full sentences but drop filler, hedging and \
     pleasantries (just/really/basically/sure/of course/I'd be happy to). \
     Pattern: state the thing, the action, the reason. Then next step. ",
    "Code blocks, file paths, commands, errors, URLs: keep exact. \
     Security warnings, irreversible action confirmations, multi-step ordered sequences: \
     write normal. Resume terse style after. \
     Active every response until user asks for normal mode."
);

static FULL_PROMPT: &str = concat!(
    "Respond like terse caveman. All technical substance stay exact, only fluff die. \
     Drop: articles (a/an/the), filler (just/really/basically/actually/simply), \
     pleasantries, hedging. Fragments OK. \
     Short synonyms (big not extensive, fix not implement a solution for). \
     Pattern: [thing] [action] [reason]. [next step]. ",
    "Code blocks, file paths, commands, errors, URLs: keep exact. \
     Security warnings, irreversible action confirmations, multi-step ordered sequences: \
     write normal. Resume terse style after. \
     Active every response until user asks for normal mode."
);

static ULTRA_PROMPT: &str = concat!(
    "Respond ultra-terse. Maximum compression. Telegraphic. \
     Abbreviate (DB/auth/config/req/res/fn/impl), strip conjunctions, \
     use arrows for causality (X → Y). One word when one word enough. \
     Pattern: [thing] → [result]. [fix]. ",
    "Code blocks, file paths, commands, errors, URLs: keep exact. \
     Security warnings, irreversible action confirmations, multi-step ordered sequences: \
     write normal. Resume terse style after. \
     Active every response until user asks for normal mode."
);

// ─── Injection ────────────────────────────────────────────────────────────────

/// Inject a caveman-style instruction into the system message of `body`.
///
/// Supports OpenAI chat format (`messages` array with `role:"system"`) and
/// OpenAI Responses API (`instructions` string).  If no system message
/// exists, one is prepended.
pub fn inject_caveman(body: &mut Value, level: CavemanLevel) {
    let prompt = level.prompt();

    // OpenAI Responses API: body["instructions"] string
    if let Some(Value::String(instr)) = body.get_mut("instructions") {
        if instr.is_empty() {
            *instr = prompt.to_string();
        } else {
            *instr = format!("{instr}\n\n{prompt}");
        }
        return;
    }

    // Standard OpenAI / Claude-as-OpenAI: body["messages"] array
    let msgs = body.get_mut("messages").and_then(|m| m.as_array_mut());
    let Some(msgs) = msgs else { return };

    // Find existing system message
    let sys_idx = msgs.iter().position(|m| {
        m.get("role").and_then(|r| r.as_str()) == Some("system")
    });

    match sys_idx {
        Some(i) => {
            // Append to existing system content
            let msg = &mut msgs[i];
            match msg.get_mut("content") {
                Some(Value::String(s)) => {
                    *s = format!("{s}\n\n{prompt}");
                }
                Some(Value::Array(parts)) => {
                    parts.push(serde_json::json!({ "type": "text", "text": prompt }));
                }
                _ => {
                    msg["content"] = Value::String(prompt.to_string());
                }
            }
        }
        None => {
            // Prepend a new system message
            msgs.insert(0, serde_json::json!({
                "role": "system",
                "content": prompt
            }));
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_body_with_system(sys: &str) -> Value {
        json!({
            "messages": [
                { "role": "system", "content": sys },
                { "role": "user", "content": "hello" }
            ]
        })
    }

    fn make_body_without_system() -> Value {
        json!({
            "messages": [
                { "role": "user", "content": "hello" }
            ]
        })
    }

    // ── Level parsing ────────────────────────────────────────────────────────

    #[test]
    fn from_str_parses_all_levels() {
        assert_eq!(CavemanLevel::from_str("lite"), Some(CavemanLevel::Lite));
        assert_eq!(CavemanLevel::from_str("FULL"), Some(CavemanLevel::Full));
        assert_eq!(CavemanLevel::from_str("ultra"), Some(CavemanLevel::Ultra));
        assert_eq!(CavemanLevel::from_str("max"),   Some(CavemanLevel::Ultra));
        assert_eq!(CavemanLevel::from_str("nope"),  None);
    }

    #[test]
    fn prompts_are_nonempty() {
        assert!(!CavemanLevel::Lite.prompt().is_empty());
        assert!(!CavemanLevel::Full.prompt().is_empty());
        assert!(!CavemanLevel::Ultra.prompt().is_empty());
    }

    // ── Injection — system message exists ────────────────────────────────────

    #[test]
    fn appends_to_existing_system_string() {
        let mut body = make_body_with_system("You are a helpful assistant.");
        inject_caveman(&mut body, CavemanLevel::Lite);
        let content = body["messages"][0]["content"].as_str().unwrap();
        assert!(content.starts_with("You are a helpful assistant."));
        assert!(content.contains("Respond tersely"));
    }

    #[test]
    fn lite_full_ultra_produce_different_prompts() {
        let mut b1 = make_body_without_system();
        let mut b2 = make_body_without_system();
        let mut b3 = make_body_without_system();
        inject_caveman(&mut b1, CavemanLevel::Lite);
        inject_caveman(&mut b2, CavemanLevel::Full);
        inject_caveman(&mut b3, CavemanLevel::Ultra);
        let s1 = b1["messages"][0]["content"].as_str().unwrap();
        let s2 = b2["messages"][0]["content"].as_str().unwrap();
        let s3 = b3["messages"][0]["content"].as_str().unwrap();
        assert_ne!(s1, s2);
        assert_ne!(s2, s3);
        assert_ne!(s1, s3);
    }

    // ── Injection — no system message ────────────────────────────────────────

    #[test]
    fn prepends_system_message_when_none() {
        let mut body = make_body_without_system();
        inject_caveman(&mut body, CavemanLevel::Full);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2, "should have added a system message");
        assert_eq!(msgs[0]["role"].as_str().unwrap(), "system");
        assert!(msgs[0]["content"].as_str().unwrap().contains("caveman"));
        // User message still there
        assert_eq!(msgs[1]["role"].as_str().unwrap(), "user");
    }

    // ── Injection — Responses API ────────────────────────────────────────────

    #[test]
    fn injects_into_instructions_string() {
        let mut body = json!({
            "instructions": "You are helpful.",
            "input": [{ "role": "user", "content": "hi" }]
        });
        inject_caveman(&mut body, CavemanLevel::Ultra);
        let instr = body["instructions"].as_str().unwrap();
        assert!(instr.starts_with("You are helpful."));
        assert!(instr.contains("Telegraphic"));
    }

    #[test]
    fn empty_instructions_replaced() {
        let mut body = json!({ "instructions": "", "input": [] });
        inject_caveman(&mut body, CavemanLevel::Lite);
        let instr = body["instructions"].as_str().unwrap();
        assert!(!instr.is_empty());
        assert!(instr.contains("tersely"));
    }

    // ── Safety ───────────────────────────────────────────────────────────────

    #[test]
    fn all_prompts_include_safety_boundary() {
        for level in [CavemanLevel::Lite, CavemanLevel::Full, CavemanLevel::Ultra] {
            let p = level.prompt();
            assert!(p.contains("Code blocks"), "{:?} missing safety boundary", level);
            assert!(p.contains("Security warnings"), "{:?} missing security note", level);
        }
    }

    #[test]
    fn no_system_message_not_panics_on_empty_messages() {
        let mut body = json!({ "messages": [] });
        inject_caveman(&mut body, CavemanLevel::Full); // must not panic
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
    }
}
