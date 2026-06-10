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
    let normalized = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    codex_worktree_project_root(&normalized)
        .or_else(|| git_project_root(&normalized))
        .unwrap_or(normalized)
}

fn codex_worktree_project_root(path: &Path) -> Option<PathBuf> {
    let components = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let Some(project_name) = component
            .strip_prefix('.')
            .and_then(|value| value.strip_suffix("-codex-worktrees"))
        else {
            continue;
        };
        let Some(worktree_name) = components.get(index + 1) else {
            continue;
        };
        if !worktree_name.starts_with(&format!("{project_name}-")) {
            continue;
        }
        let mut root = PathBuf::new();
        for component in &components[..index] {
            root.push(component);
        }
        root.push(project_name);
        if root.exists() {
            return Some(root.canonicalize().unwrap_or(root));
        }
    }
    None
}

fn git_project_root(path: &Path) -> Option<PathBuf> {
    let mut current = Some(path);
    while let Some(candidate) = current {
        if candidate.join(".git").exists() {
            return Some(
                candidate
                    .canonicalize()
                    .unwrap_or_else(|_| candidate.to_path_buf()),
            );
        }
        current = candidate.parent();
    }
    None
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
    use std::fs;

    use super::*;

    #[test]
    fn project_hash_is_stable() {
        assert_eq!(
            project_hash(Path::new("/Users/tbedor/Development/yaaml")),
            project_hash(Path::new("/Users/tbedor/Development/yaaml"))
        );
    }

    #[test]
    fn normalize_project_id_prefers_git_root() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let nested = repo.join("nested");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(&nested).unwrap();

        assert_eq!(normalize_project_id(&nested), repo.canonicalize().unwrap());
    }

    #[test]
    fn normalize_project_id_maps_codex_worktree_to_sibling_project() {
        let tmp = tempfile::tempdir().unwrap();
        let development = tmp.path().join("Development");
        let project = development.join("elroy");
        let worktree = development
            .join(".elroy-codex-worktrees")
            .join("elroy-a278dae492")
            .join("agent");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&worktree).unwrap();

        assert_eq!(
            normalize_project_id(&worktree),
            project.canonicalize().unwrap()
        );
    }
}
