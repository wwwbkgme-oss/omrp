//! Compact `find` output — limit files per directory.

pub const PER_DIR_MAX: usize = 10;
pub const TOTAL_DIR_MAX: usize = 20;

pub fn compress(text: &str) -> String {
    // Group paths by their parent directory.
    let mut dirs: Vec<(String, Vec<String>)> = Vec::new();
    let mut dir_count = 0usize;

    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() { continue; }

        let parent = parent_of(t);

        match dirs.iter_mut().find(|(d, _)| d == &parent) {
            Some((_, files)) => files.push(t.to_string()),
            None => {
                if dir_count < TOTAL_DIR_MAX {
                    dirs.push((parent, vec![t.to_string()]));
                    dir_count += 1;
                }
                // Paths in dirs beyond the cap are silently dropped (count is noted at end)
            }
        }
    }

    let total_paths: usize = dirs.iter().map(|(_, f)| f.len()).sum();
    let mut out = Vec::new();
    let mut total_shown = 0usize;

    for (dir, files) in &dirs {
        if !dir.is_empty() && files.len() > 1 {
            out.push(format!("{dir}/"));
        }
        let shown = files.len().min(PER_DIR_MAX);
        for f in &files[..shown] {
            out.push(format!("  {f}"));
            total_shown += 1;
        }
        if files.len() > PER_DIR_MAX {
            out.push(format!("  ... ({} more)", files.len() - PER_DIR_MAX));
        }
    }

    if total_shown < total_paths {
        out.push(format!("... ({} paths shown of {total_paths})", total_shown));
    }

    out.join("\n")
}

fn parent_of(path: &str) -> String {
    match path.rfind('/') {
        Some(i) if i > 0 => path[..i].to_string(),
        _ => ".".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_per_dir() {
        let paths: Vec<String> = (0..20)
            .map(|i| format!("./src/file{i}.rs"))
            .collect();
        let out = compress(&paths.join("\n"));
        let shown = out.lines().filter(|l| l.contains("file")).count();
        assert!(shown <= PER_DIR_MAX, "shown {shown} > {PER_DIR_MAX}");
        assert!(out.contains("more"));
    }
}
