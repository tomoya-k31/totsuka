//! Log-file retention (§5.2): keep at most N daily-rotated files.
//!
//! `tracing-appender` handles daily rotation but does not delete old files, so
//! we prune on startup. Daily files are named `{prefix}.YYYY-MM-DD`, whose
//! lexical order matches chronological order — so sorting by name and dropping
//! the oldest is correct.

use std::fs;
use std::path::{Path, PathBuf};

/// Delete the oldest `{prefix}.*` files in `dir` so at most `max_files` remain.
///
/// Returns the paths that were removed. `max_files == 0` disables retention
/// (keep everything). Missing directory is treated as "nothing to prune".
pub fn enforce_retention(
    dir: &Path,
    prefix: &str,
    max_files: usize,
) -> std::io::Result<Vec<PathBuf>> {
    if max_files == 0 || !dir.exists() {
        return Ok(Vec::new());
    }

    let mut files: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(prefix))
        })
        .collect();

    files.sort(); // lexical == chronological for date-suffixed names.

    let mut removed = Vec::new();
    if files.len() > max_files {
        let excess = files.len() - max_files;
        for path in files.into_iter().take(excess) {
            fs::remove_file(&path)?;
            removed.push(path);
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("totsuka-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn keeps_only_the_newest_files() {
        let dir = scratch("rotation_keep");
        for day in [
            "2026-07-01",
            "2026-07-02",
            "2026-07-03",
            "2026-07-04",
            "2026-07-05",
        ] {
            fs::write(dir.join(format!("totsuka.log.{day}")), b"x").unwrap();
        }
        // An unrelated file must be untouched.
        fs::write(dir.join("README"), b"x").unwrap();

        let removed = enforce_retention(&dir, "totsuka.log", 3).unwrap();
        assert_eq!(removed.len(), 2);

        let mut remaining: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        remaining.sort();
        assert_eq!(
            remaining,
            vec![
                "README".to_string(),
                "totsuka.log.2026-07-03".to_string(),
                "totsuka.log.2026-07-04".to_string(),
                "totsuka.log.2026-07-05".to_string(),
            ]
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn zero_max_keeps_everything() {
        let dir = scratch("rotation_zero");
        fs::write(dir.join("totsuka.log.2026-07-01"), b"x").unwrap();
        assert!(
            enforce_retention(&dir, "totsuka.log", 0)
                .unwrap()
                .is_empty()
        );
        assert!(dir.join("totsuka.log.2026-07-01").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_directory_is_ok() {
        let dir = std::env::temp_dir().join("totsuka-nonexistent-rotation-xyz");
        let _ = fs::remove_dir_all(&dir);
        assert!(
            enforce_retention(&dir, "totsuka.log", 3)
                .unwrap()
                .is_empty()
        );
    }
}
