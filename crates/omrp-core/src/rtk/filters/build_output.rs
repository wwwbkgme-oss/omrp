//! Compact build/compiler output (npm, cargo, maven, gradle, pip …).

const MAX_ERRORS: usize = 20;
const MAX_WARNINGS: usize = 10;
const MAX_LINES: usize = 100;

pub fn compress(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= MAX_LINES {
        return text.to_string();
    }

    let mut errors: Vec<&str> = Vec::new();
    let mut warnings: Vec<&str> = Vec::new();
    let mut summary: Vec<&str> = Vec::new();
    let mut other: Vec<&str> = Vec::new();

    for line in &lines {
        let lower = line.to_lowercase();
        if is_error_line(&lower) {
            if errors.len() < MAX_ERRORS { errors.push(line); }
        } else if is_warning_line(&lower) {
            if warnings.len() < MAX_WARNINGS { warnings.push(line); }
        } else if is_summary_line(&lower) {
            summary.push(line);
        } else if is_progress_line(&lower) {
            // Skip verbose progress (Compiling foo v1.0, Downloading bar …)
        } else if other.len() < 20 {
            other.push(line);
        }
    }

    let mut out: Vec<&str> = Vec::new();
    if !errors.is_empty() {
        out.push("=== ERRORS ===");
        out.extend(&errors);
    }
    if !warnings.is_empty() {
        out.push("=== WARNINGS ===");
        out.extend(&warnings);
    }
    if !summary.is_empty() {
        out.push("=== SUMMARY ===");
        out.extend(&summary);
    }
    if !other.is_empty() && out.is_empty() {
        out.extend(&other);
    }

    if out.is_empty() {
        // Fall back to smart truncate
        return super::smart_truncate::compress(text);
    }

    out.join("\n")
}

fn is_error_line(lower: &str) -> bool {
    lower.contains("error") || lower.contains("[err") || lower.contains("fail")
        || lower.contains("exception") || lower.contains("panic")
        || lower.contains("fatal") || lower.starts_with("error[")
}

fn is_warning_line(lower: &str) -> bool {
    lower.contains("warn") || lower.starts_with("warning[")
}

fn is_summary_line(lower: &str) -> bool {
    lower.contains("build success") || lower.contains("build fail")
        || lower.contains("finished") || lower.contains("tests passed")
        || lower.contains("tests failed") || lower.contains("installed")
}

fn is_progress_line(lower: &str) -> bool {
    lower.trim_start().starts_with("compiling ")
        || lower.trim_start().starts_with("downloading ")
        || lower.trim_start().starts_with("checking ")
        || lower.trim_start().starts_with("running ")
        || lower.contains("added ") && lower.contains(" packages")
}
