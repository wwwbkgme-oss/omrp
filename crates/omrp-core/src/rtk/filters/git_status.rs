//! Git status compact filter.
//! Limits changed and untracked files to keep output manageable.

const MAX_CHANGED: usize = 10;
const MAX_UNTRACKED: usize = 10;

pub fn compress(text: &str) -> String {
    let mut result: Vec<String> = Vec::new();
    let mut changed_count = 0usize;
    let mut untracked_count = 0usize;
    let mut in_untracked = false;
    let mut changed_skipped = 0usize;
    let mut untracked_skipped = 0usize;

    for line in text.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("Untracked files:") {
            in_untracked = true;
            result.push(line.to_string());
            continue;
        }

        // Status file lines start with a two-char XY code
        let b = line.as_bytes();
        let is_status_line = b.len() >= 3 && {
            let valid = b" MADRCU?!";
            valid.contains(&b[0]) && valid.contains(&b[1]) && b[2] == b' '
        };

        if is_status_line {
            if in_untracked {
                if untracked_count < MAX_UNTRACKED {
                    result.push(line.to_string());
                    untracked_count += 1;
                } else {
                    untracked_skipped += 1;
                }
            } else {
                if changed_count < MAX_CHANGED {
                    result.push(line.to_string());
                    changed_count += 1;
                } else {
                    changed_skipped += 1;
                }
            }
            continue;
        }

        // Indented file in `git status` (e.g., "  modified:   foo.rs")
        if line.starts_with('\t') || (line.starts_with("  ") && !trimmed.is_empty()) {
            if in_untracked {
                if untracked_count < MAX_UNTRACKED {
                    result.push(line.to_string());
                    untracked_count += 1;
                } else {
                    untracked_skipped += 1;
                }
            } else {
                if changed_count < MAX_CHANGED {
                    result.push(line.to_string());
                    changed_count += 1;
                } else {
                    changed_skipped += 1;
                }
            }
            continue;
        }

        result.push(line.to_string());
    }

    if changed_skipped > 0 {
        result.push(format!("  ... ({changed_skipped} more changed files)"));
    }
    if untracked_skipped > 0 {
        result.push(format!("  ... ({untracked_skipped} more untracked files)"));
    }

    result.join("\n")
}
