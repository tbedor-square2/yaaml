use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("failed to read directory {path}: {source}")]
    ReadDir {
        path: PathBuf,
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptFile {
    pub path: PathBuf,
    pub modified_at: Option<SystemTime>,
}

pub fn discover_codex_backlog(
    sessions_root: impl AsRef<Path>,
    already_cursored: &HashSet<PathBuf>,
) -> Result<Vec<TranscriptFile>, DiscoveryError> {
    let mut files = Vec::new();
    visit_jsonl_files(sessions_root.as_ref(), already_cursored, &mut files)?;
    files.sort_by(|a, b| {
        b.modified_at
            .cmp(&a.modified_at)
            .then_with(|| b.path.cmp(&a.path))
    });
    Ok(files)
}

fn visit_jsonl_files(
    dir: &Path,
    already_cursored: &HashSet<PathBuf>,
    files: &mut Vec<TranscriptFile>,
) -> Result<(), DiscoveryError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(DiscoveryError::ReadDir {
                path: dir.to_path_buf(),
                source,
            })
        }
    };

    for entry in entries {
        let entry = entry.map_err(|source| DiscoveryError::ReadDir {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let metadata = entry.metadata().ok();
        if metadata.as_ref().is_some_and(|metadata| metadata.is_dir()) {
            visit_jsonl_files(&path, already_cursored, files)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "jsonl")
            && !already_cursored.contains(&path)
        {
            files.push(TranscriptFile {
                path,
                modified_at: metadata.and_then(|metadata| metadata.modified().ok()),
            });
        }
    }

    Ok(())
}

pub fn should_flush_idle_session(
    unformulated_turns: usize,
    threshold_turns: usize,
    idle_seconds: u64,
    idle_threshold_seconds: u64,
) -> bool {
    unformulated_turns > 0
        && (unformulated_turns >= threshold_turns || idle_seconds >= idle_threshold_seconds)
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn discovers_codex_jsonl_newest_first_and_skips_cursored() {
        let tmp = TempDir::new().unwrap();
        let old = tmp.path().join("2026/06/01/old.jsonl");
        let new = tmp.path().join("2026/06/08/new.jsonl");
        fs::create_dir_all(old.parent().unwrap()).unwrap();
        fs::create_dir_all(new.parent().unwrap()).unwrap();
        fs::write(&old, "{}\n").unwrap();
        thread::sleep(Duration::from_millis(10));
        fs::write(&new, "{}\n").unwrap();

        let mut cursored = HashSet::new();
        cursored.insert(old.clone());

        let files = discover_codex_backlog(tmp.path(), &cursored).unwrap();

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, new);
    }

    #[test]
    fn idle_session_flushes_below_threshold() {
        assert!(should_flush_idle_session(2, 10, 600, 600));
        assert!(!should_flush_idle_session(2, 10, 599, 600));
        assert!(should_flush_idle_session(10, 10, 0, 600));
        assert!(!should_flush_idle_session(0, 10, 999, 600));
    }
}
