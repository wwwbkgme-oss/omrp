//! Compact `tree` output — truncate at MAX_LINES.

pub const MAX_LINES: usize = 200;

pub fn compress(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= MAX_LINES {
        return text.to_string();
    }
    let mut out: Vec<&str> = lines[..MAX_LINES].to_vec();
    out.push("... (tree output truncated)");
    out.join("\n")
}
