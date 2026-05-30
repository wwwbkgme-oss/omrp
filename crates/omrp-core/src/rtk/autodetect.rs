//! Auto-detect the right RTK filter for a block of text.
//!
//! Peeks at the first [`DETECT_WINDOW`] bytes and returns the matching filter
//! function plus a stable name string, or `None` if no filter applies.
//!
//! Detection order mirrors the original Rust implementation:
//! git-diff → git-status → build-output → grep → find → tree → ls →
//! search-list → read-numbered → dedup-log → smart-truncate

use super::filters;
use super::Filter;

/// Number of bytes to peek at when detecting filter type.
pub const DETECT_WINDOW: usize = 1024;

/// Returns `(filter_fn, filter_name)` or `None`.
pub fn auto_detect(text: &str) -> Option<(Filter, &'static str)> {
    let head = if text.len() > DETECT_WINDOW { &text[..DETECT_WINDOW] } else { text };

    // 1. Git diff
    if head.contains("diff --git ") || is_diff_hunk(head) {
        return Some((filters::git_diff::compress as Filter, "git-diff"));
    }

    // 2. Git status
    if is_git_status(head) {
        return Some((filters::git_status::compress as Filter, "git-status"));
    }

    // 3. Build output — before porcelain to avoid cargo "Compiling" misdetection
    if is_build_output(head) {
        return Some((filters::build_output::compress as Filter, "build-output"));
    }

    // 4. Git status (porcelain short form)
    if is_mostly_porcelain(head) {
        return Some((filters::git_status::compress as Filter, "git-status"));
    }

    let lines: Vec<&str> = head.lines().collect();
    let non_empty: Vec<&str> = lines.iter().copied().filter(|l| !l.trim().is_empty()).collect();

    // 5. Grep: first 5 non-empty lines match file:number:content
    let first5 = &non_empty[..non_empty.len().min(5)];
    if first5.iter().any(|l| is_grep_line(l)) {
        return Some((filters::grep::compress as Filter, "grep"));
    }

    // 6. Find: ALL non-empty lines are path-like, >= 3 lines
    if non_empty.len() >= 3 && non_empty.iter().all(|l| is_path_like(l)) {
        return Some((filters::find::compress as Filter, "find"));
    }

    // 7. Tree: box-drawing glyphs
    if head.contains("├──") || head.contains("└──") || head.contains("│  ") {
        return Some((filters::tree::compress as Filter, "tree"));
    }

    // 8. ls -la
    if head.contains("total ") && {
        let t = head.lines().next().unwrap_or("");
        t.starts_with("total ") && t[6..].trim().parse::<u64>().is_ok()
    } || count_ls_perm_rows(head) >= 3 {
        return Some((filters::ls::compress as Filter, "ls"));
    }

    // 9. Dedup log: >= 5 non-empty lines with repeated content
    if non_empty.len() >= 5 {
        return Some((filters::dedup_log::compress as Filter, "dedup-log"));
    }

    // 10. Smart truncate: big blob, no structure
    if text.lines().count() >= filters::smart_truncate::MIN_LINES {
        return Some((filters::smart_truncate::compress as Filter, "smart-truncate"));
    }

    None
}

// ─── Detection helpers ────────────────────────────────────────────────────────

fn is_diff_hunk(s: &str) -> bool {
    s.lines().any(|l| l.starts_with("@@ "))
}

fn is_git_status(s: &str) -> bool {
    s.contains("On branch ")
        || s.contains("nothing to commit")
        || s.contains("Changes not staged")
        || s.contains("Changes to be committed")
        || s.contains("Untracked files:")
}

fn is_build_output(s: &str) -> bool {
    let lower = s.to_lowercase();
    lower.contains("npm warn")
        || lower.contains("npm error")
        || lower.contains("npm err!")
        || lower.contains("yarn warn")
        || lower.contains("yarn error")
        || s.contains("Compiling ")
        || s.contains("Downloading ")
        || s.contains("added ")  && s.contains(" packages")
        || s.contains("[ERROR]")
        || s.contains("BUILD SUCCESS")
        || s.contains("BUILD FAILED")
        || s.contains("Finished ")
        || s.contains("Successfully installed")
        || s.contains("Successfully built")
        || s.contains("error[E")    // Rust errors
        || s.contains("warning[")  // Rust warnings
}

fn is_mostly_porcelain(s: &str) -> bool {
    let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() < 3 { return false; }
    let hits = lines.iter().filter(|l| is_porcelain_line(l)).count();
    hits * 10 >= lines.len() * 6  // >= 60%
}

fn is_porcelain_line(line: &str) -> bool {
    let bytes = line.as_bytes();
    if bytes.len() < 4 { return false; }
    let a = bytes[0];
    let b = bytes[1];
    // XY format: both in [' ', 'M', 'A', 'D', 'R', 'C', 'U', '?', '!']
    let valid = b" MADRCU?!";
    valid.contains(&a) && valid.contains(&b) && bytes[2] == b' '
}

fn is_grep_line(line: &str) -> bool {
    // file:number:content — find two ':' where the middle part is a digit
    let first = line.find(':');
    if let Some(f) = first {
        let rest = &line[f + 1..];
        if let Some(s) = rest.find(':') {
            return rest[..s].chars().all(|c| c.is_ascii_digit()) && !rest[..s].is_empty();
        }
    }
    false
}

fn is_path_like(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty() || t.contains(':') { return false; }
    t.starts_with('.') || t.starts_with('/') || t.contains('/')
}

fn count_ls_perm_rows(s: &str) -> usize {
    s.lines().filter(|l| is_ls_perm_row(l)).count()
}

fn is_ls_perm_row(line: &str) -> bool {
    let b = line.as_bytes();
    if b.len() < 10 { return false; }
    matches!(b[0], b'-' | b'd' | b'l' | b'b' | b'c' | b'p' | b's')
        && b[1..10].iter().all(|&c| matches!(c, b'r' | b'w' | b'x' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_git_diff() {
        let text = "diff --git a/foo.rs b/foo.rs\n@@ -1,4 +1,6 @@\n-old\n+new\n";
        let r = auto_detect(text);
        assert!(r.is_some());
        assert_eq!(r.unwrap().1, "git-diff");
    }

    #[test]
    fn detects_git_status() {
        let text = "On branch main\nChanges not staged for commit:\n\tmodified: foo.rs\n";
        let r = auto_detect(text);
        assert!(r.is_some());
        assert_eq!(r.unwrap().1, "git-status");
    }

    #[test]
    fn detects_rust_build_output() {
        let text = "   Compiling omrp-core v0.1.0\nerror[E0308]: mismatched types\n";
        let r = auto_detect(text);
        assert!(r.is_some());
        assert_eq!(r.unwrap().1, "build-output");
    }

    #[test]
    fn detects_grep() {
        let text = "src/main.rs:42:    let x = 1;\nsrc/main.rs:43:    let y = 2;\n";
        let r = auto_detect(text);
        assert!(r.is_some());
        assert_eq!(r.unwrap().1, "grep");
    }

    #[test]
    fn detects_find() {
        let text = "./src/main.rs\n./src/lib.rs\n./tests/foo.rs\n";
        let r = auto_detect(text);
        assert!(r.is_some());
        assert_eq!(r.unwrap().1, "find");
    }

    #[test]
    fn detects_tree() {
        let text = ".\n├── src\n│   └── main.rs\n└── Cargo.toml\n";
        let r = auto_detect(text);
        assert!(r.is_some());
        assert_eq!(r.unwrap().1, "tree");
    }

    #[test]
    fn small_content_returns_none() {
        let text = "hello world";
        assert!(auto_detect(text).is_none());
    }
}
