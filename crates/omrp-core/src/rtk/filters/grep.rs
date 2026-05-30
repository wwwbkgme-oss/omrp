//! Compact grep output — port of `rtk/src/cmds/system/grep.rs`.
//!
//! Groups matches by file and caps at [`PER_FILE_MAX`] per file.

pub const PER_FILE_MAX: usize = 10;

pub fn compress(text: &str) -> String {
    // Maintain insertion order: vec of (file, matches).
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();

    for line in text.lines() {
        if let Some((file, rest)) = split_grep_line(line) {
            match groups.iter_mut().rev().find(|(f, _)| f == file) {
                Some((_, matches)) => matches.push(rest.to_string()),
                None => groups.push((file.to_string(), vec![rest.to_string()])),
            }
        } else {
            // Non-grep line — emit as-is
            groups.push((String::new(), vec![line.to_string()]));
        }
    }

    let mut out = Vec::new();
    for (file, matches) in &groups {
        if file.is_empty() {
            out.extend(matches.iter().cloned());
            continue;
        }
        out.push(file.clone());
        let shown = matches.len().min(PER_FILE_MAX);
        for m in &matches[..shown] {
            out.push(format!("  {m}"));
        }
        if matches.len() > PER_FILE_MAX {
            out.push(format!("  ... ({} more matches)", matches.len() - PER_FILE_MAX));
        }
    }
    out.join("\n")
}

/// Split `file:linenum:content` → `(file, "linenum:content")` if line number is digits.
fn split_grep_line(line: &str) -> Option<(&str, &str)> {
    let first = line.find(':')?;
    let rest = &line[first + 1..];
    let second = rest.find(':')?;
    let lineno = &rest[..second];
    if !lineno.is_empty() && lineno.chars().all(|c| c.is_ascii_digit()) {
        Some((&line[..first], &line[first + 1..]))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_matches_per_file() {
        let lines: Vec<String> = (0..20)
            .map(|i| format!("src/main.rs:{i}:    let x = {i};"))
            .collect();
        let out = compress(&lines.join("\n"));
        assert!(out.contains("more matches"));
        let match_lines = out.lines().filter(|l| l.contains("let x =")).count();
        assert!(match_lines <= PER_FILE_MAX, "got {match_lines}");
    }

    #[test]
    fn multiple_files_all_shown() {
        let text = "a.rs:1:foo\nb.rs:2:bar\nc.rs:3:baz";
        let out = compress(text);
        assert!(out.contains("a.rs"));
        assert!(out.contains("b.rs"));
        assert!(out.contains("c.rs"));
    }

    #[test]
    fn non_grep_lines_preserved() {
        let text = "Binary file foo.bin matches\nsrc/a.rs:1:code";
        let out = compress(text);
        assert!(out.contains("Binary file"));
    }
}
