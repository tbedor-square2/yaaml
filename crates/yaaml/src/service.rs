use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{bail, Context};

use crate::skills::{self, InitPaths, InitReport};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServicePaths {
    pub home: PathBuf,
    pub binary_path: PathBuf,
    pub config_path: PathBuf,
    pub stdout_log_path: PathBuf,
    pub stderr_log_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceInstallReport {
    pub service_file: PathBuf,
    pub init: InitReport,
}

impl ServicePaths {
    pub fn for_home(home: &Path, binary_path: PathBuf) -> Self {
        let data_dir = home.join(".yaaml");
        Self {
            home: home.to_path_buf(),
            binary_path,
            config_path: data_dir.join("config.toml"),
            stdout_log_path: data_dir.join("daemon.log"),
            stderr_log_path: data_dir.join("daemon.err.log"),
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
    let init = skills::init(&InitPaths::for_home_with_binary(
        &paths.home,
        paths.binary_path.clone(),
    ))?;
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
        propagate_provider_env_to_launchd()?;
        let service_target = launchd_service_target()?;
        if !launchd_service_loaded(&service_target)? {
            let domain_target = launchd_domain_target()?;
            checked_command(
                Command::new("launchctl")
                    .arg("bootstrap")
                    .arg(domain_target)
                    .arg(paths.launch_agent_path()),
                "launchctl bootstrap",
            )?;
        }
        checked_command(
            Command::new("launchctl")
                .arg("kickstart")
                .arg("-k")
                .arg(service_target),
            "launchctl kickstart",
        )?;
    } else {
        import_provider_env_to_systemd()?;
        checked_command(
            Command::new("systemctl")
                .arg("--user")
                .arg("start")
                .arg("yaaml.service"),
            "systemctl start",
        )?;
    }
    Ok(())
}

fn propagate_provider_env_to_launchd() -> anyhow::Result<()> {
    for key in provider_env_keys() {
        if let Ok(value) = std::env::var(key) {
            checked_command(
                Command::new("launchctl").arg("setenv").arg(key).arg(value),
                &format!("launchctl setenv {key}"),
            )?;
        }
    }
    Ok(())
}

fn import_provider_env_to_systemd() -> anyhow::Result<()> {
    let keys = provider_env_keys()
        .into_iter()
        .filter(|key| std::env::var(key).is_ok())
        .collect::<Vec<_>>();
    if keys.is_empty() {
        return Ok(());
    }
    checked_command(
        Command::new("systemctl")
            .arg("--user")
            .arg("import-environment")
            .args(keys),
        "systemctl import-environment",
    )?;
    Ok(())
}

fn provider_env_keys() -> Vec<&'static str> {
    vec!["ANTHROPIC_API_KEY", "OPENAI_API_KEY"]
}

fn redact_provider_env(text: &str) -> String {
    let mut redacted = text
        .lines()
        .map(|line| {
            if provider_env_keys().iter().any(|key| line.contains(key)) {
                match line.split_once("=>") {
                    Some((prefix, _)) => format!("{prefix}=> [redacted]"),
                    None => line.to_string(),
                }
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    if text.ends_with('\n') {
        redacted.push('\n');
    }
    redacted
}

pub fn stop() -> anyhow::Result<()> {
    if cfg!(target_os = "macos") {
        checked_command(
            Command::new("launchctl")
                .arg("bootout")
                .arg(launchd_service_target()?),
            "launchctl bootout",
        )?;
    } else {
        checked_command(
            Command::new("systemctl")
                .arg("--user")
                .arg("stop")
                .arg("yaaml.service"),
            "systemctl stop",
        )?;
    }
    Ok(())
}

pub fn status() -> anyhow::Result<()> {
    if cfg!(target_os = "macos") {
        let output = Command::new("launchctl")
            .arg("print")
            .arg(launchd_service_target()?)
            .output()
            .context("failed to run launchctl print")?;
        print!(
            "{}",
            redact_provider_env(&String::from_utf8_lossy(&output.stdout))
        );
        eprint!(
            "{}",
            redact_provider_env(&String::from_utf8_lossy(&output.stderr))
        );
        ensure_success(output, "launchctl print")?;
    } else {
        let output = Command::new("systemctl")
            .arg("--user")
            .arg("status")
            .arg("yaaml.service")
            .output()
            .context("failed to run systemctl status")?;
        print!(
            "{}",
            redact_provider_env(&String::from_utf8_lossy(&output.stdout))
        );
        eprint!(
            "{}",
            redact_provider_env(&String::from_utf8_lossy(&output.stderr))
        );
        ensure_success(output, "systemctl status")?;
    }
    Ok(())
}

fn launchd_service_loaded(service_target: &str) -> anyhow::Result<bool> {
    let output = Command::new("launchctl")
        .arg("print")
        .arg(service_target)
        .output()
        .context("failed to run launchctl print")?;
    Ok(output.status.success())
}

fn checked_command(command: &mut Command, description: &str) -> anyhow::Result<()> {
    let output = command
        .output()
        .with_context(|| format!("failed to run {description}"))?;
    ensure_success(output, description)
}

fn ensure_success(output: Output, description: &str) -> anyhow::Result<()> {
    if output.status.success() {
        return Ok(());
    }
    let stdout = redact_provider_env(&String::from_utf8_lossy(&output.stdout));
    let stderr = redact_provider_env(&String::from_utf8_lossy(&output.stderr));
    bail!(
        "{description} exited with status {}{}{}{}{}",
        output.status,
        if stdout.trim().is_empty() {
            ""
        } else {
            "\nstdout:\n"
        },
        stdout.trim_end(),
        if stderr.trim().is_empty() {
            ""
        } else {
            "\nstderr:\n"
        },
        stderr.trim_end()
    );
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
  <key>StandardOutPath</key>
  <string>{}</string>
  <key>StandardErrorPath</key>
  <string>{}</string>
</dict>
</plist>
"#,
        paths.binary_path.display(),
        paths.config_path.display(),
        paths.stdout_log_path.display(),
        paths.stderr_log_path.display()
    )
}

pub fn render_systemd_user_unit(paths: &ServicePaths) -> String {
    format!(
        r#"[Unit]
Description=YAAML daemon

[Service]
ExecStart={} daemon --config {}
Restart=on-failure
StandardOutput=append:{}
StandardError=append:{}

[Install]
WantedBy=default.target
"#,
        paths.binary_path.display(),
        paths.config_path.display(),
        paths.stdout_log_path.display(),
        paths.stderr_log_path.display()
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
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;
    #[cfg(unix)]
    use std::process::ExitStatus;

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
        assert!(plist.contains("<key>StandardOutPath</key>"));
        assert!(plist.contains("/Users/test/.yaaml/daemon.log"));
        assert!(plist.contains("<key>StandardErrorPath</key>"));
        assert!(plist.contains("/Users/test/.yaaml/daemon.err.log"));
    }

    #[test]
    fn systemd_user_unit_renders_expected_paths() {
        let paths = ServicePaths::for_home(Path::new("/home/test"), PathBuf::from("/bin/yaaml"));
        let unit = render_systemd_user_unit(&paths);

        assert!(unit.contains("ExecStart=/bin/yaaml daemon --config /home/test/.yaaml/config.toml"));
        assert!(unit.contains("StandardOutput=append:/home/test/.yaaml/daemon.log"));
        assert!(unit.contains("StandardError=append:/home/test/.yaaml/daemon.err.log"));
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
    fn provider_env_keys_cover_remote_defaults() {
        assert_eq!(
            provider_env_keys(),
            vec!["ANTHROPIC_API_KEY", "OPENAI_API_KEY"]
        );
    }

    #[test]
    fn status_redacts_provider_environment_values() {
        let output = "  OPENAI_API_KEY => sk-test\n  OTHER => value\n";

        let redacted = redact_provider_env(output);

        assert!(redacted.contains("OPENAI_API_KEY => [redacted]"));
        assert!(redacted.contains("OTHER => value"));
        assert!(!redacted.contains("sk-test"));
    }

    #[cfg(unix)]
    #[test]
    fn ensure_success_reports_failed_commands_with_redacted_output() {
        let output = Output {
            status: ExitStatus::from_raw(1 << 8),
            stdout: b"OPENAI_API_KEY => sk-test\n".to_vec(),
            stderr: b"not loaded\n".to_vec(),
        };

        let error = ensure_success(output, "launchctl print").unwrap_err();
        let rendered = error.to_string();

        assert!(rendered.contains("launchctl print exited with status"));
        assert!(rendered.contains("OPENAI_API_KEY => [redacted]"));
        assert!(rendered.contains("not loaded"));
        assert!(!rendered.contains("sk-test"));
    }

    #[test]
    fn service_install_invokes_idempotent_init_when_skills_are_missing() {
        let tmp = TempDir::new().unwrap();
        let paths = ServicePaths::for_home(tmp.path(), PathBuf::from("/bin/yaaml"));

        let report = install(&paths).unwrap();

        assert!(report.service_file.exists());
        assert!(report.init.codex_skill.exists());
        assert!(report.init.codex_remember_skill.exists());
        assert!(report.init.claude_skill.exists());
        assert!(report.init.claude_remember_skill.exists());
    }
}
