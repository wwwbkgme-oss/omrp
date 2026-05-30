//! Deduplicate repeated log lines (e.g., server access logs, repeated warnings).

const MAX_LINES: usize = 2000;

pub fn compress(text: &str) -> String {
    let mut seen: std::collections::HashSet<&str> = Default::default();
    let mut out: Vec<&str> = Vec::new();
    let mut duplicates: usize = 0;

    for line in text.lines().take(MAX_LINES) {
        if seen.insert(line) {
            out.push(line);
        } else {
            duplicates += 1;
        }
    }

    if duplicates > 0 {
        out.push("[dedup-log: {duplicates} duplicate lines removed]");
    }

    // Use a format! for the dedup summary line instead of the literal
    let mut result = out.join("\n");
    if duplicates > 0 {
        let summary = format!("[dedup-log: {duplicates} duplicate lines removed]");
        // Replace the placeholder we pushed above
        result = result.replace("[dedup-log: {duplicates} duplicate lines removed]", &summary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deduplicates_lines() {
        let text = "INFO: started\nINFO: started\nINFO: started\nINFO: done\n";
        let out = compress(text);
        let count = out.matches("INFO: started").count();
        assert_eq!(count, 1);
        assert!(out.contains("duplicate"));
    }

    #[test]
    fn unique_lines_unchanged() {
        let text = "a\nb\nc\n";
        let out = compress(text);
        assert!(out.contains('a') && out.contains('b') && out.contains('c'));
        assert!(!out.contains("duplicate"));
    }
}
