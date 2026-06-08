use std::fs;
use std::process::Command;

use tempfile::TempDir;
use yaaml_core::recall_file_path;

#[test]
fn path_prints_current_project_recall_file() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    let recall_dir = home.join(".yaaml").join("recall");
    fs::create_dir_all(home.join(".yaaml")).unwrap();
    fs::create_dir_all(&project).unwrap();
    fs::write(
        home.join(".yaaml").join("config.toml"),
        format!(r#"recall_dir = "{}""#, recall_dir.display()),
    )
    .unwrap();

    let binary = env!("CARGO_BIN_EXE_yaaml");
    let output = Command::new(binary)
        .arg("path")
        .current_dir(&project)
        .env("HOME", &home)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = recall_file_path(&recall_dir, &project.canonicalize().unwrap());

    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        expected.display().to_string()
    );
}
