use std::process::Command;

use tempfile::TempDir;

#[test]
fn status_json_emits_valid_json_for_empty_db() {
    let tmp = TempDir::new().unwrap();
    let binary = env!("CARGO_BIN_EXE_yaaml");
    let output = Command::new(binary)
        .arg("status")
        .arg("--json")
        .env("HOME", tmp.path())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["memory_count"], 0);
    assert_eq!(value["backlog"]["discovered_files"], 0);
}
