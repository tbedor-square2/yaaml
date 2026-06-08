use std::env;
use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PathError {
    #[error("HOME is not set")]
    MissingHome,
}

pub fn home_dir() -> Result<PathBuf, PathError> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or(PathError::MissingHome)
}

pub fn expand_tilde(path: &str) -> Result<PathBuf, PathError> {
    if path == "~" {
        return home_dir();
    }

    if let Some(rest) = path.strip_prefix("~/") {
        return Ok(home_dir()?.join(rest));
    }

    Ok(PathBuf::from(path))
}

pub fn normalize_project_id(cwd: &Path) -> PathBuf {
    cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf())
}

pub fn project_hash(project_id: &Path) -> String {
    // Stable FNV-1a hash. Not cryptographic; just compact path addressing.
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in project_id.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_hash_is_stable() {
        assert_eq!(
            project_hash(Path::new("/Users/tbedor/Development/yaaml")),
            project_hash(Path::new("/Users/tbedor/Development/yaaml"))
        );
    }
}
