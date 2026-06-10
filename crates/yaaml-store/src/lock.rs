use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LockError {
    #[error("failed to create lock directory {path}: {source}")]
    CreateDir {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to read lock file {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to write lock file {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse lock file {path}: {source}")]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("daemon is already running with pid {pid}")]
    AlreadyRunning { pid: u32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockMetadata {
    pub pid: u32,
    pub started_at_unix_seconds: u64,
}

#[derive(Debug)]
pub struct DaemonLock {
    path: PathBuf,
    metadata: LockMetadata,
}

impl DaemonLock {
    pub fn acquire(data_dir: impl AsRef<Path>) -> Result<Self, LockError> {
        let data_dir = data_dir.as_ref();
        fs::create_dir_all(data_dir).map_err(|source| LockError::CreateDir {
            path: data_dir.to_path_buf(),
            source,
        })?;
        let path = data_dir.join("daemon.lock");

        if path.exists() {
            let existing = read_metadata(&path)?;
            if pid_is_running(existing.pid) {
                return Err(LockError::AlreadyRunning { pid: existing.pid });
            }
        }

        let metadata = LockMetadata {
            pid: process::id(),
            started_at_unix_seconds: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        };
        let text = serde_json::to_string_pretty(&metadata).expect("lock metadata serializes");
        fs::write(&path, text).map_err(|source| LockError::Write {
            path: path.clone(),
            source,
        })?;

        Ok(Self { path, metadata })
    }

    pub fn metadata(&self) -> &LockMetadata {
        &self.metadata
    }
}

impl Drop for DaemonLock {
    fn drop(&mut self) {
        let Ok(existing) = read_metadata(&self.path) else {
            return;
        };
        if existing.pid == self.metadata.pid {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn read_metadata(path: &Path) -> Result<LockMetadata, LockError> {
    let text = fs::read_to_string(path).map_err(|source| LockError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_str(&text).map_err(|source| LockError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

fn pid_is_running(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: kill with signal 0 does not send a signal; it only checks process existence/permission.
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    result == 0
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn rejects_second_active_owner() {
        let tmp = TempDir::new().unwrap();
        let lock = DaemonLock::acquire(tmp.path()).unwrap();

        let error = DaemonLock::acquire(tmp.path()).unwrap_err();

        assert!(matches!(
            error,
            LockError::AlreadyRunning { pid } if pid == lock.metadata().pid
        ));
    }

    #[test]
    fn recovers_stale_lock_when_pid_is_gone() {
        let tmp = TempDir::new().unwrap();
        let stale = LockMetadata {
            pid: u32::MAX - 1,
            started_at_unix_seconds: 1,
        };
        fs::write(
            tmp.path().join("daemon.lock"),
            serde_json::to_string(&stale).unwrap(),
        )
        .unwrap();

        let lock = DaemonLock::acquire(tmp.path()).unwrap();

        assert_eq!(lock.metadata().pid, process::id());
    }
}
