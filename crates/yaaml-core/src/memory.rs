use std::fs;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

use crate::{MemoryRecord, MemoryScope, SourceTurnRef};

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("failed to parse memory formulation response: {0}")]
    Parse(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryDraft {
    pub title: String,
    pub body: String,
    pub scope: MemoryScope,
    pub project_descriptor: String,
}

impl MemoryDraft {
    pub fn into_record(
        self,
        source_turn_refs: Vec<SourceTurnRef>,
        created_at: String,
        session_id: Option<String>,
        project_id: Option<String>,
    ) -> MemoryRecord {
        MemoryRecord {
            id: None,
            title: self.title,
            body: self.body,
            scope: self.scope,
            source_turn_refs,
            created_at: created_at.clone(),
            updated_at: created_at,
            is_active: true,
            session_id,
            project_id,
            project_descriptor: Some(self.project_descriptor),
            lineage_refs: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum FormulationResponse {
    Envelope { memories: Vec<FormulatedMemory> },
    Many(Vec<FormulatedMemory>),
    One(FormulatedMemory),
}

#[derive(Debug, Deserialize)]
struct FormulatedMemory {
    title: String,
    body: String,
    scope: Option<String>,
    project_descriptor: Option<String>,
}

pub fn parse_formulation_response(
    response: &Value,
    default_project_descriptor: &str,
    max_body_chars: usize,
) -> Result<Vec<MemoryDraft>, MemoryError> {
    let parsed: FormulationResponse = serde_json::from_value(response.clone())
        .map_err(|error| MemoryError::Parse(error.to_string()))?;
    let memories = match parsed {
        FormulationResponse::Envelope { memories } => memories,
        FormulationResponse::Many(memories) => memories,
        FormulationResponse::One(memory) => vec![memory],
    };

    memories
        .into_iter()
        .map(|memory| {
            let title = memory.title.trim().to_string();
            let body = truncate_chars(memory.body.trim(), max_body_chars);
            let scope = match memory.scope.as_deref().map(str::trim) {
                Some("global") => MemoryScope::Global,
                _ => MemoryScope::Project,
            };
            let project_descriptor = memory
                .project_descriptor
                .map(|descriptor| descriptor.trim().to_string())
                .filter(|descriptor| !descriptor.is_empty())
                .unwrap_or_else(|| default_project_descriptor.to_string());

            Ok(MemoryDraft {
                title,
                body,
                scope,
                project_descriptor,
            })
        })
        .collect()
}

pub fn embedding_text(memory: &MemoryRecord) -> String {
    let project_descriptor = memory.project_descriptor.as_deref().unwrap_or("unknown");
    format!(
        "title: {}\nscope: {}\nproject: {}\nbody:\n{}",
        memory.title,
        memory.scope.as_str(),
        project_descriptor,
        memory.body
    )
}

pub fn embedded_text_hash(text: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

pub fn derive_project_descriptor(project_dir: &Path, formulation_phrase: Option<&str>) -> String {
    let basename = project_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown-project");
    let mut parts = Vec::new();

    if let Some(package_name) = cargo_package_name(project_dir) {
        push_unique(&mut parts, package_name);
    }
    push_unique(&mut parts, basename.to_string());

    let ecosystem = detect_ecosystem(project_dir);
    if let Some(ecosystem) = ecosystem {
        push_unique(&mut parts, ecosystem.to_string());
    }
    if let Some(phrase) = formulation_phrase {
        let phrase = phrase.trim();
        if !phrase.is_empty() {
            push_unique(&mut parts, phrase.to_string());
        }
    }

    parts.join(", ")
}

fn cargo_package_name(project_dir: &Path) -> Option<String> {
    let manifest = fs::read_to_string(project_dir.join("Cargo.toml")).ok()?;
    let value = manifest.parse::<toml::Value>().ok()?;
    value
        .get("package")
        .and_then(|package| package.get("name"))
        .and_then(|name| name.as_str())
        .map(str::to_string)
}

fn detect_ecosystem(project_dir: &Path) -> Option<&'static str> {
    if project_dir.join("Cargo.toml").exists() {
        Some("Rust")
    } else if project_dir.join("package.json").exists() {
        Some("Node")
    } else if project_dir.join("pyproject.toml").exists() {
        Some("Python")
    } else {
        None
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

fn push_unique(parts: &mut Vec<String>, part: String) {
    if !parts.iter().any(|existing| existing == &part) {
        parts.push(part);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn project_descriptor_uses_local_metadata_only() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("Cargo.toml"),
            r#"[package]
name = "yaaml-test"
version = "0.1.0"
"#,
        )
        .unwrap();
        fs::create_dir(tmp.path().join(".git")).unwrap();
        fs::write(
            tmp.path().join(".git").join("config"),
            "[remote \"origin\"]\nurl = git@github.com:block/yaaml-test.git\n",
        )
        .unwrap();

        let descriptor = derive_project_descriptor(tmp.path(), Some("memory daemon"));

        assert!(descriptor.contains("yaaml-test"));
        assert!(descriptor.contains("Rust"));
        assert!(!descriptor.contains("github"));
        assert!(!descriptor.contains(".com"));
        assert!(!descriptor.contains("git@"));
    }

    #[test]
    fn invalid_or_missing_scope_defaults_to_project() {
        let memories = parse_formulation_response(
            &json!({
                "memories": [
                    {"title":"One","body":"A","scope":"project"},
                    {"title":"Two","body":"B","scope":"team"},
                    {"title":"Three","body":"C","scope":"global"}
                ]
            }),
            "yaaml, Rust",
            100,
        )
        .unwrap();

        assert_eq!(memories[0].scope, MemoryScope::Project);
        assert_eq!(memories[1].scope, MemoryScope::Project);
        assert_eq!(memories[2].scope, MemoryScope::Global);
    }

    #[test]
    fn memory_body_length_cap_is_enforced() {
        let memories = parse_formulation_response(
            &json!({"title":"One","body":"abcdef","project_descriptor":"yaaml"}),
            "default",
            3,
        )
        .unwrap();

        assert_eq!(memories[0].body, "abc");
    }

    #[test]
    fn parses_multiple_memories_from_one_response() {
        let memories = parse_formulation_response(
            &json!({
                "memories": [
                    {"title":"One","body":"A","project_descriptor":"yaaml"},
                    {"title":"Two","body":"B","scope":"global","project_descriptor":"workflow"}
                ]
            }),
            "default",
            100,
        )
        .unwrap();

        assert_eq!(memories.len(), 2);
        assert_eq!(memories[0].project_descriptor, "yaaml");
        assert_eq!(memories[1].scope, MemoryScope::Global);
    }

    #[test]
    fn embedding_text_includes_key_memory_fields() {
        let memory = MemoryRecord {
            id: Some(1),
            title: "Recall files".to_string(),
            body: "Agents read daemon-owned recall files.".to_string(),
            scope: MemoryScope::Project,
            source_turn_refs: Vec::new(),
            created_at: "2026-06-08T00:00:00Z".to_string(),
            updated_at: "2026-06-08T00:00:00Z".to_string(),
            is_active: true,
            session_id: Some("session-1".to_string()),
            project_id: Some("/tmp/yaaml".to_string()),
            project_descriptor: Some("yaaml, Rust CLI memory daemon".to_string()),
            lineage_refs: Vec::new(),
        };

        let text = embedding_text(&memory);

        assert!(text.contains("Recall files"));
        assert!(text.contains("Agents read daemon-owned recall files."));
        assert!(text.contains("project"));
        assert!(text.contains("yaaml, Rust CLI memory daemon"));
    }
}
