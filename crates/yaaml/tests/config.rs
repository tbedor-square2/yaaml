use std::process::Command;

use tempfile::TempDir;

#[test]
fn config_effective_prints_merged_toml() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(home.join(".yaaml")).unwrap();
    std::fs::create_dir_all(project.join(".yaaml")).unwrap();
    std::fs::write(
        home.join(".yaaml").join("config.toml"),
        r#"
turns_between_memory = 20
summary_model = "user-summary-model"
"#,
    )
    .unwrap();
    std::fs::write(
        project.join(".yaaml").join("config.toml"),
        r#"
turns_between_memory = 12
"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_yaaml"))
        .arg("config")
        .arg("--effective")
        .current_dir(&project)
        .env("HOME", &home)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("turns_between_memory = 12"));
    assert!(stdout.contains(r#"summary_model = "user-summary-model""#));
}

#[test]
fn config_effective_json_emits_merged_config() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(home.join(".yaaml")).unwrap();
    std::fs::create_dir_all(project.join(".yaaml")).unwrap();
    std::fs::write(
        home.join(".yaaml").join("config.toml"),
        r#"
recall_result_limit = 7
"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_yaaml"))
        .arg("config")
        .arg("--effective")
        .arg("--json")
        .current_dir(&project)
        .env("HOME", &home)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["recall_result_limit"], 7);
    assert_eq!(value["vector_index_backend"], "sqlite-exact");
}
