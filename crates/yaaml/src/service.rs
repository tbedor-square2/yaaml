use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;

use crate::skills::{self, InitPaths, InitReport};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServicePaths {
    pub home: PathBuf,
    pub binary_path: PathBuf,
    pub config_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceInstallReport {
    pub service_file: PathBuf,
    pub init: InitReport,
}

impl ServicePaths {
    pub fn for_home(home: &Path, binary_path: PathBuf) -> Self {
        Self {
            home: home.to_path_buf(),
            binary_path,
            config_path: home.join(".yaaml").join("config.toml"),
        }
    }

    pub fn launch_agent_path(&self) -> PathBuf {
        self.home
            .join("Library")
            .join("LaunchAgents")
            .join("com.yaaml.daemon.plist")
    }

    pub fn systemd_user_unit_path(&self) -> PathBuf {
        self.home
            .join(".config")
            .join("systemd")
            .join("user")
            .join("yaaml.service")
    }
}

pub fn install(paths: &ServicePaths) -> anyhow::Result<ServiceInstallReport> {
    let init = skills::init(&InitPaths::for_home(&paths.home))?;
    let service_file = if cfg!(target_os = "macos") {
        let path = paths.launch_agent_path();
        write_file(&path, &render_launch_agent(paths))?;
        path
    } else {
        let path = paths.systemd_user_unit_path();
        write_file(&path, &render_systemd_user_unit(paths))?;
        path
    };
    Ok(ServiceInstallReport { service_file, init })
}

pub fn uninstall(paths: &ServicePaths) -> anyhow::Result<()> {
    let service_file = if cfg!(target_os = "macos") {
        paths.launch_agent_path()
    } else {
        paths.systemd_user_unit_path()
    };
    if service_file.exists() {
        fs::remove_file(&service_file)
            .with_context(|| format!("failed to remove {}", service_file.display()))?;
    }
    Ok(())
}

pub fn start(paths: &ServicePaths) -> anyhow::Result<()> {
    if cfg!(target_os = "macos") {
        Command::new("launchctl")
            .arg("bootstrap")
            .arg(launchd_domain_target()?)
            .arg(paths.launch_agent_path())
            .status()
            .context("failed to run launchctl bootstrap")?;
        Command::new("launchctl")
            .arg("kickstart")
            .arg("-k")
            .arg(launchd_service_target()?)
            .status()
            .context("failed to run launchctl kickstart")?;
    } else {
        Command::new("systemctl")
            .arg("--user")
            .arg("start")
            .arg("yaaml.service")
            .status()
            .context("failed to run systemctl start")?;
    }
    Ok(())
}

pub fn stop() -> anyhow::Result<()> {
    if cfg!(target_os = "macos") {
        Command::new("launchctl")
            .arg("bootout")
            .arg(launchd_service_target()?)
            .status()
            .context("failed to run launchctl bootout")?;
    } else {
        Command::new("systemctl")
            .arg("--user")
            .arg("stop")
            .arg("yaaml.service")
            .status()
            .context("failed to run systemctl stop")?;
    }
    Ok(())
}

pub fn status() -> anyhow::Result<()> {
    if cfg!(target_os = "macos") {
        Command::new("launchctl")
            .arg("print")
            .arg(launchd_service_target()?)
            .status()
            .context("failed to run launchctl print")?;
    } else {
        Command::new("systemctl")
            .arg("--user")
            .arg("status")
            .arg("yaaml.service")
            .status()
            .context("failed to run systemctl status")?;
    }
    Ok(())
}

fn launchd_service_target() -> anyhow::Result<String> {
    let output = Command::new("id")
        .arg("-u")
        .output()
        .context("failed to resolve current uid")?;
    let uid = String::from_utf8(output.stdout).context("id -u output was not UTF-8")?;
    Ok(launchd_service_target_for_uid(uid.trim()))
}

fn launchd_domain_target() -> anyhow::Result<String> {
    let output = Command::new("id")
        .arg("-u")
        .output()
        .context("failed to resolve current uid")?;
    let uid = String::from_utf8(output.stdout).context("id -u output was not UTF-8")?;
    Ok(launchd_domain_target_for_uid(uid.trim()))
}

fn launchd_domain_target_for_uid(uid: &str) -> String {
    format!("gui/{uid}")
}

fn launchd_service_target_for_uid(uid: &str) -> String {
    format!("gui/{uid}/com.yaaml.daemon")
}

pub fn render_launch_agent(paths: &ServicePaths) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.yaaml.daemon</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}</string>
    <string>daemon</string>
    <string>--config</string>
    <string>{}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
</dict>
</plist>
"#,
        paths.binary_path.display(),
        paths.config_path.display()
    )
}

pub fn render_systemd_user_unit(paths: &ServicePaths) -> String {
    format!(
        r#"[Unit]
Description=YAAML daemon

[Service]
ExecStart={} daemon --config {}
Restart=on-failure

[Install]
WantedBy=default.target
"#,
        paths.binary_path.display(),
        paths.config_path.display()
    )
}

fn write_file(path: &Path, contents: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(path, contents).with_context(|| format!("failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn launch_agent_plist_renders_expected_paths() {
        let paths = ServicePaths::for_home(
            Path::new("/Users/test"),
            PathBuf::from("/usr/local/bin/yaaml"),
        );
        let plist = render_launch_agent(&paths);

        assert!(plist.contains("com.yaaml.daemon"));
        assert!(plist.contains("/usr/local/bin/yaaml"));
        assert!(plist.contains("/Users/test/.yaaml/config.toml"));
    }

    #[test]
    fn systemd_user_unit_renders_expected_paths() {
        let paths = ServicePaths::for_home(Path::new("/home/test"), PathBuf::from("/bin/yaaml"));
        let unit = render_systemd_user_unit(&paths);

        assert!(unit.contains("ExecStart=/bin/yaaml daemon --config /home/test/.yaaml/config.toml"));
    }

    #[test]
    fn launchd_service_target_uses_concrete_uid() {
        assert_eq!(
            launchd_service_target_for_uid("501"),
            "gui/501/com.yaaml.daemon"
        );
        assert_eq!(launchd_domain_target_for_uid("501"), "gui/501");
    }

    #[test]
    fn service_install_invokes_idempotent_init_when_skills_are_missing() {
        let tmp = TempDir::new().unwrap();
        let paths = ServicePaths::for_home(tmp.path(), PathBuf::from("/bin/yaaml"));

        let report = install(&paths).unwrap();

        assert!(report.service_file.exists());
        assert!(report.init.codex_skill.exists());
        assert!(report.init.claude_skill.exists());
    }
}
