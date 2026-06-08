use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;

const CODEX_SKILL: &str = r#"---
name: yaaml
description: Use when project or user memory may help the current coding task; resolves and reads YAAML's daemon-owned recall file for the current project.
---

# YAAML Recall

Use this skill when project memory could help with the current task.

Run `yaaml path` from the current working directory to resolve the daemon-owned recall file for this project. If the file exists, read it and use the memories as contextual hints. Do not assume a project-local `.yaaml/recall.md` path.
"#;

const CLAUDE_SKILL: &str = CODEX_SKILL;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitPaths {
    pub codex_home: PathBuf,
    pub claude_home: PathBuf,
    pub daemon_socket: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitReport {
    pub codex_skill: PathBuf,
    pub claude_skill: PathBuf,
    pub hook_snippet: String,
}

impl InitPaths {
    pub fn for_home(home: &Path) -> Self {
        Self {
            codex_home: home.join(".codex"),
            claude_home: home.join(".claude"),
            daemon_socket: home.join(".yaaml").join("daemon.sock"),
        }
    }
}

pub fn init(paths: &InitPaths) -> anyhow::Result<InitReport> {
    let codex_skill = install_skill(&paths.codex_home, "yaaml", CODEX_SKILL)
        .context("failed to install Codex skill")?;
    let claude_skill = install_skill(&paths.claude_home, "yaaml", CLAUDE_SKILL)
        .context("failed to install Claude skill")?;
    Ok(InitReport {
        codex_skill,
        claude_skill,
        hook_snippet: hook_snippet(&paths.daemon_socket),
    })
}

pub fn install_skill(agent_home: &Path, name: &str, contents: &str) -> anyhow::Result<PathBuf> {
    let skill_dir = agent_home.join("skills").join(name);
    fs::create_dir_all(&skill_dir)
        .with_context(|| format!("failed to create {}", skill_dir.display()))?;
    let skill_path = skill_dir.join("SKILL.md");
    fs::write(&skill_path, contents)
        .with_context(|| format!("failed to write {}", skill_path.display()))?;
    Ok(skill_path)
}

pub fn hook_snippet(socket_path: &Path) -> String {
    format!(
        "# YAAML daemon socket\nYAAML_DAEMON_SOCKET=\"{}\"\n",
        socket_path.display()
    )
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn skill_files_are_copied_not_symlinked() {
        let tmp = TempDir::new().unwrap();
        let skill = install_skill(tmp.path(), "yaaml", CODEX_SKILL).unwrap();
        let metadata = fs::symlink_metadata(skill).unwrap();

        assert!(metadata.file_type().is_file());
        assert!(!metadata.file_type().is_symlink());
    }

    #[test]
    fn skill_template_has_yaml_frontmatter() {
        assert!(CODEX_SKILL.starts_with("---\n"));
        assert!(CODEX_SKILL.contains("name: yaaml"));
        assert!(CODEX_SKILL.contains("description:"));
    }

    #[test]
    fn init_is_idempotent_and_preserves_unrelated_skill_dirs() {
        let tmp = TempDir::new().unwrap();
        let paths = InitPaths::for_home(tmp.path());
        let unrelated = paths.codex_home.join("skills").join("other");
        fs::create_dir_all(&unrelated).unwrap();
        fs::write(unrelated.join("SKILL.md"), "other").unwrap();

        init(&paths).unwrap();
        init(&paths).unwrap();

        assert!(paths.codex_home.join("skills").join("yaaml").exists());
        assert_eq!(
            fs::read_to_string(unrelated.join("SKILL.md")).unwrap(),
            "other"
        );
    }

    #[test]
    fn hook_snippet_includes_daemon_socket_path() {
        let snippet = hook_snippet(Path::new("/tmp/yaaml.sock"));

        assert!(snippet.contains("YAAML_DAEMON_SOCKET"));
        assert!(snippet.contains("/tmp/yaaml.sock"));
    }
}
