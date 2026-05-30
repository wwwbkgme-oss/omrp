//! Compact unified diffs — port of `rtk/src/cmds/git/git.rs compact_diff`.
//!
//! Keeps file headers and hunk markers.  Each hunk is capped at
//! [`HUNK_MAX_LINES`] changed lines; excess is replaced with a count summary.

/// Per-hunk changed-line cap.
pub const HUNK_MAX_LINES: usize = 100;
/// Overall output line cap.
pub const MAX_LINES: usize = 500;

/// Compact a unified diff string.
pub fn compress(diff: &str) -> String {
    let mut result: Vec<String> = Vec::new();
    let mut current_file = String::new();
    let mut added: usize = 0;
    let mut removed: usize = 0;
    let mut in_hunk = false;
    let mut hunk_shown: usize = 0;
    let mut hunk_skipped: usize = 0;
    let mut was_truncated = false;

    'outer: for line in diff.lines() {
        if line.starts_with("diff --git ") {
            if hunk_skipped > 0 {
                result.push(format!("  ... ({hunk_skipped} lines truncated)"));
                was_truncated = true;
                hunk_skipped = 0;
            }
            if !current_file.is_empty() && (added > 0 || removed > 0) {
                result.push(format!("  +{added} -{removed}"));
            }
            // Extract "b/path" from "diff --git a/path b/path"
            current_file = line
                .split(" b/")
                .nth(1)
                .unwrap_or("unknown")
                .to_string();
            result.push(format!("\n{current_file}"));
            added = 0;
            removed = 0;
            in_hunk = false;
            hunk_shown = 0;
        } else if line.starts_with("@@") {
            if hunk_skipped > 0 {
                result.push(format!("  ... ({hunk_skipped} lines truncated)"));
                was_truncated = true;
                hunk_skipped = 0;
            }
            in_hunk = true;
            hunk_shown = 0;
            result.push(format!("  {line}"));
        } else if in_hunk {
            if line.starts_with('+') && !line.starts_with("+++") {
                added += 1;
                if hunk_shown < HUNK_MAX_LINES {
                    result.push(format!("  {line}"));
                    hunk_shown += 1;
                } else {
                    hunk_skipped += 1;
                }
            } else if line.starts_with('-') && !line.starts_with("---") {
                removed += 1;
                if hunk_shown < HUNK_MAX_LINES {
                    result.push(format!("  {line}"));
                    hunk_shown += 1;
                } else {
                    hunk_skipped += 1;
                }
            } else if !line.starts_with('\\') && hunk_shown < HUNK_MAX_LINES && hunk_shown > 0 {
                result.push(format!("  {line}"));
                hunk_shown += 1;
            }
        }
        // index/---/+++ lines: skip (redundant)

        if result.len() >= MAX_LINES {
            result.push("\n... (more changes truncated)".to_string());
            was_truncated = true;
            break 'outer;
        }
    }

    if hunk_skipped > 0 {
        result.push(format!("  ... ({hunk_skipped} lines truncated)"));
        was_truncated = true;
    }
    if !current_file.is_empty() && (added > 0 || removed > 0) {
        result.push(format!("  +{added} -{removed}"));
    }
    if was_truncated {
        result.push("[full diff: use git diff]".to_string());
    }

    result.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_diff(files: usize, added_per_hunk: usize) -> String {
        let mut s = String::new();
        for f in 0..files {
            s.push_str(&format!("diff --git a/file{f}.rs b/file{f}.rs\n"));
            s.push_str("index abc..def 100644\n");
            s.push_str(&format!("--- a/file{f}.rs\n+++ b/file{f}.rs\n"));
            s.push_str("@@ -1,5 +1,7 @@\n");
            for i in 0..added_per_hunk {
                s.push_str(&format!("-old line {i}\n"));
                s.push_str(&format!("+new line {i}\n"));
            }
        }
        s
    }

    #[test]
    fn compresses_simple_diff() {
        let diff = make_diff(1, 5);
        let out = compress(&diff);
        assert!(out.contains("file0.rs"));
        assert!(out.contains("+5 -5"));
    }

    #[test]
    fn caps_large_hunk() {
        let diff = make_diff(1, 200); // 200 added lines
        let out = compress(&diff);
        assert!(out.contains("truncated"));
        assert!(!out.is_empty());
    }

    #[test]
    fn output_shorter_than_input_for_large_diff() {
        let diff = make_diff(3, 150);
        let out = compress(&diff);
        assert!(out.len() < diff.len());
    }
}
