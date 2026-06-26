use std::process::Command;

use tempfile::TempDir;

#[test]
fn init_installs_codex_and_claude_skills() {
    let tmp = TempDir::new().unwrap();
    let binary = env!("CARGO_BIN_EXE_yaaml");

    let output = Command::new(binary)
        .arg("init")
        .env("HOME", tmp.path())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(tmp.path().join(".codex/skills/yaaml/SKILL.md").exists());
    assert!(tmp
        .path()
        .join(".codex/skills/yaaml-remember/SKILL.md")
        .exists());
    assert!(tmp.path().join(".claude/skills/yaaml/SKILL.md").exists());
    assert!(tmp
        .path()
        .join(".claude/skills/yaaml-remember/SKILL.md")
        .exists());
    assert!(!tmp
        .path()
        .join(".codex/hooks/yaaml-pre-tool-use.py")
        .exists());
    let codex_config = tmp.path().join(".codex/config.toml");
    assert!(!codex_config.exists());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("installed Codex recall skill"));
    assert!(stdout.contains("installed Claude remember skill"));
    assert!(stdout.contains("removed legacy Codex PreToolUse hook if present"));
}

#[test]
fn service_install_and_uninstall_manage_service_file() {
    let tmp = TempDir::new().unwrap();
    let binary = env!("CARGO_BIN_EXE_yaaml");

    let install = Command::new(binary)
        .arg("service")
        .arg("install")
        .env("HOME", tmp.path())
        .output()
        .unwrap();

    assert!(
        install.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&install.stderr)
    );
    let service_file = if cfg!(target_os = "macos") {
        tmp.path()
            .join("Library/LaunchAgents/com.yaaml.daemon.plist")
    } else {
        tmp.path().join(".config/systemd/user/yaaml.service")
    };
    assert!(service_file.exists());
    assert!(tmp.path().join(".codex/skills/yaaml/SKILL.md").exists());

    let uninstall = Command::new(binary)
        .arg("service")
        .arg("uninstall")
        .env("HOME", tmp.path())
        .output()
        .unwrap();

    assert!(
        uninstall.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&uninstall.stderr)
    );
    assert!(!service_file.exists());
}
