use crate::SegmentLabelDraft;
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

const MAX_LABEL_LENGTH: usize = 80;
const MAX_EVIDENCE_ITEMS: usize = 3;
const MAX_EVIDENCE_LENGTH: usize = 160;

pub fn normalize_segment_label(label: &str) -> Option<String> {
    let mut normalized = String::new();
    let mut previous_separator = false;
    for character in label.trim().chars() {
        if character.is_ascii_alphanumeric() {
            normalized.push(character.to_ascii_lowercase());
            previous_separator = false;
        } else if !previous_separator && !normalized.is_empty() {
            normalized.push('-');
            previous_separator = true;
        }
    }
    while normalized.ends_with('-') {
        normalized.pop();
    }
    if normalized.len() < 3 {
        None
    } else {
        normalized.truncate(MAX_LABEL_LENGTH);
        while normalized.ends_with('-') {
            normalized.pop();
        }
        Some(normalized)
    }
}

pub fn segment_label_task_key(normalized_label: &str) -> String {
    format!("label:{normalized_label}")
}

pub fn sanitize_segment_label_drafts(
    drafts: impl IntoIterator<Item = SegmentLabelDraft>,
    max_labels: usize,
) -> Vec<SegmentLabelDraft> {
    let mut labels = Vec::new();
    for mut draft in drafts {
        let label = draft.label.trim();
        let Some(normalized_label) = normalize_segment_label(label) else {
            continue;
        };
        if labels
            .iter()
            .any(|existing: &SegmentLabelDraft| existing.normalized_label == normalized_label)
        {
            continue;
        }
        draft.label = label.chars().take(MAX_LABEL_LENGTH).collect();
        draft.normalized_label = normalized_label;
        draft.kind = normalize_label_kind(&draft.kind);
        draft.evidence = draft
            .evidence
            .into_iter()
            .map(|evidence| evidence.trim().chars().take(MAX_EVIDENCE_LENGTH).collect())
            .filter(|evidence: &String| !evidence.is_empty())
            .take(MAX_EVIDENCE_ITEMS)
            .collect();
        labels.push(draft);
        if labels.len() >= max_labels {
            break;
        }
    }
    labels
}

pub fn parse_segment_label_response(
    response: &Value,
    max_labels: usize,
) -> Result<Vec<SegmentLabelDraft>, SegmentLabelError> {
    let parsed: SegmentLabelResponse = serde_json::from_value(response.clone())
        .map_err(|source| SegmentLabelError::InvalidJson { source })?;
    Ok(sanitize_segment_label_drafts(parsed.labels, max_labels))
}

#[derive(Debug, Error)]
pub enum SegmentLabelError {
    #[error("failed to parse segment label response JSON: {source}")]
    InvalidJson { source: serde_json::Error },
}

#[derive(Debug, Deserialize)]
struct SegmentLabelResponse {
    #[serde(default)]
    labels: Vec<SegmentLabelDraft>,
}

fn normalize_label_kind(kind: &str) -> String {
    let mut normalized = String::new();
    let mut previous_separator = false;
    for character in kind.trim().chars() {
        if character.is_ascii_alphanumeric() {
            normalized.push(character.to_ascii_lowercase());
            previous_separator = false;
        } else if !previous_separator && !normalized.is_empty() {
            normalized.push('_');
            previous_separator = true;
        }
    }
    while normalized.ends_with('_') {
        normalized.pop();
    }
    if normalized.is_empty() {
        "topic".to_string()
    } else {
        normalized.chars().take(32).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_label_text_to_stable_key_fragment() {
        assert_eq!(
            normalize_segment_label(" Recall Quality / Eval Metrics ").as_deref(),
            Some("recall-quality-eval-metrics")
        );
        assert_eq!(normalize_segment_label("AI").as_deref(), None);
    }

    #[test]
    fn sanitizes_label_drafts_and_dedupes_by_normalized_label() {
        let labels = sanitize_segment_label_drafts(
            [
                SegmentLabelDraft {
                    label: "Recall Quality".to_string(),
                    normalized_label: String::new(),
                    kind: "Workstream".to_string(),
                    evidence: vec![" evaluating recall ".to_string()],
                },
                SegmentLabelDraft {
                    label: "recall-quality".to_string(),
                    normalized_label: String::new(),
                    kind: "topic".to_string(),
                    evidence: Vec::new(),
                },
            ],
            3,
        );

        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].normalized_label, "recall-quality");
        assert_eq!(labels[0].kind, "workstream");
        assert_eq!(labels[0].evidence, vec!["evaluating recall".to_string()]);
    }

    #[test]
    fn parses_segment_label_response() {
        let labels = parse_segment_label_response(
            &serde_json::json!({
                "labels": [
                    {
                        "label": "YAAML recall evaluation",
                        "normalized_label": "",
                        "kind": "workstream",
                        "evidence": ["score recall runs"]
                    }
                ]
            }),
            3,
        )
        .unwrap();

        assert_eq!(labels[0].normalized_label, "yaaml-recall-evaluation");
    }
}
