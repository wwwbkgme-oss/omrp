//! Smart head+tail truncation for unrecognised blobs.
//!
//! Port of `rtk/src/core/filter.rs smart_truncate`.

pub const HEAD: usize = 120;
pub const TAIL: usize = 60;
pub const MIN_LINES: usize = 250;

pub fn compress(input: &str) -> String {
    let lines: Vec<&str> = input.lines().collect();
    if lines.len() < MIN_LINES {
        return input.to_string();
    }
    let cut = lines.len() - HEAD - TAIL;
    let head = &lines[..HEAD];
    let summary = format!("... +{cut} lines truncated");
    let tail = &lines[lines.len() - TAIL..];
    let mut out: Vec<String> = head.iter().map(|s| s.to_string()).collect();
    out.push(summary);
    out.extend(tail.iter().map(|s| s.to_string()));
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_content_unchanged() {
        let s = "line\n".repeat(100);
        assert_eq!(compress(&s), s);
    }

    #[test]
    fn long_content_truncated() {
        let s = (0..300).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let out = compress(&s);
        assert!(out.contains("truncated"));
        assert!(out.len() < s.len());
        // Head and tail preserved
        assert!(out.contains("line 0"));
        assert!(out.contains("line 299"));
    }
}
