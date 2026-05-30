//! Compact `ls -la` output — extension summary.

const NOISE_DIRS: &[&str] = &[
    "node_modules", ".git", "target", "__pycache__",
    ".next", "dist", "build", ".venv", "venv",
    ".cache", ".idea", ".vscode",
];
const EXT_TOP: usize = 5;
const MAX_LINES: usize = 40;

pub fn compress(text: &str) -> String {
    let mut result: Vec<String> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    let mut dirs: Vec<String> = Vec::new();
    let mut total_line = String::new();
    let mut ext_counts: std::collections::HashMap<String, usize> = Default::default();

    for line in text.lines() {
        let t = line.trim();
        // Keep "total N" line
        if t.starts_with("total ") {
            total_line = line.to_string();
            continue;
        }
        // Permission string lines
        let bytes = t.as_bytes();
        if bytes.len() < 10 { continue; }
        if !matches!(bytes[0], b'-' | b'd' | b'l' | b'b' | b'c' | b'p' | b's') { continue; }
        if !bytes[1..10].iter().all(|&c| matches!(c, b'r' | b'w' | b'x' | b'-')) { continue; }

        let is_dir = bytes[0] == b'd';
        // Extract filename (last field after splitting on whitespace)
        let parts: Vec<&str> = t.splitn(9, ' ').filter(|s| !s.is_empty()).collect();
        let name = parts.last().copied().unwrap_or("?");

        if is_dir {
            if !NOISE_DIRS.contains(&name) {
                dirs.push(name.to_string());
            }
        } else {
            // Track extension counts
            let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
            if !ext.is_empty() && ext.len() <= 8 && ext != name.to_lowercase() {
                *ext_counts.entry(ext).or_insert(0) += 1;
            }
            files.push(name.to_string());
        }
    }

    if !total_line.is_empty() { result.push(total_line); }

    // Dirs
    if !dirs.is_empty() {
        result.push(format!("dirs: {}", dirs.join("  ")));
    }

    // Top extensions
    let mut ext_vec: Vec<(String, usize)> = ext_counts.into_iter().collect();
    ext_vec.sort_by(|a, b| b.1.cmp(&a.1));
    if !ext_vec.is_empty() {
        let summary: Vec<String> = ext_vec.iter().take(EXT_TOP)
            .map(|(ext, n)| format!(".{ext}×{n}"))
            .collect();
        result.push(format!("files({}): {}", files.len(), summary.join("  ")));
    } else if !files.is_empty() {
        result.push(format!("files: {}", files.len()));
    }

    // Show up to MAX_LINES individual entries if still small
    if files.len() + dirs.len() <= MAX_LINES {
        result.push("---".to_string());
        for name in &dirs { result.push(format!("d  {name}")); }
        for name in files.iter().take(MAX_LINES) { result.push(format!("   {name}")); }
    }

    result.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ls(n_files: usize) -> String {
        let mut s = format!("total {}\n", n_files * 4096);
        for i in 0..n_files {
            s.push_str(&format!("-rw-r--r-- 1 user group 1234 May 1 12:00 file{i}.rs\n"));
        }
        s
    }

    #[test]
    fn shows_extension_summary() {
        let text = make_ls(10);
        let out = compress(&text);
        assert!(out.contains(".rs") || out.contains("files"));
    }

    #[test]
    fn filters_noise_dirs() {
        let text = "drwxr-xr-x 2 u g 0 May 1 12:00 node_modules\ndrwxr-xr-x 2 u g 0 May 1 12:00 src\n";
        let out = compress(&text);
        assert!(!out.contains("node_modules"));
        assert!(out.contains("src"));
    }
}
