use std::collections::HashSet;

use serde::{Deserialize, Serialize};

mod inference;
mod scoring;
mod tags;

pub use inference::{infer_context_from_memory, infer_context_from_path, infer_context_from_text};
pub use scoring::context_score;

use tags::{normalize_optional_tag, push_tag};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextMetadata {
    pub repo_id: Option<String>,
    pub repo_root: Option<String>,
    pub work_area: Option<String>,
    pub activity_domain: Option<String>,
    pub subject_tags: Vec<String>,
    pub classification_source: String,
    pub classification_evidence: String,
}

impl ContextMetadata {
    pub fn normalized(mut self) -> Self {
        self.repo_id = self.repo_id.and_then(normalize_optional_tag);
        self.work_area = self.work_area.and_then(normalize_optional_tag);
        self.activity_domain = self.activity_domain.and_then(normalize_optional_tag);
        let mut seen = HashSet::new();
        self.subject_tags = self
            .subject_tags
            .into_iter()
            .filter_map(normalize_optional_tag)
            .filter(|tag| seen.insert(tag.clone()))
            .collect();
        self
    }
}

pub fn merge_contexts(base: &mut ContextMetadata, incoming: ContextMetadata) {
    if base.repo_id.is_none() {
        base.repo_id = incoming.repo_id;
    }
    if base.repo_root.is_none() {
        base.repo_root = incoming.repo_root;
    }
    if base.work_area.is_none() {
        base.work_area = incoming.work_area;
    }
    if base.activity_domain.is_none() || base.activity_domain.as_deref() == Some("code") {
        base.activity_domain = incoming.activity_domain.or(base.activity_domain.take());
    }
    for tag in incoming.subject_tags {
        push_tag(&mut base.subject_tags, &tag);
    }
    if base.classification_source.is_empty() {
        base.classification_source = incoming.classification_source;
    } else if !incoming.classification_source.is_empty()
        && base.classification_source != incoming.classification_source
    {
        base.classification_source = format!(
            "{},{}",
            base.classification_source, incoming.classification_source
        );
    }
    if base.classification_evidence.is_empty() {
        base.classification_evidence = incoming.classification_evidence;
    }
    *base = std::mem::take(base).normalized();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MemoryKind, MemoryRecord, MemoryScope};

    #[test]
    fn text_context_extracts_github_repo_and_subject_tags() {
        let context = infer_context_from_text(
            "ensure remote ci passes https://github.com/squareup/forge-signalsmith/pull/456",
        );

        assert_eq!(
            context.repo_id.as_deref(),
            Some("squareup/forge-signalsmith")
        );
        assert!(context
            .subject_tags
            .contains(&"forge-signalsmith".to_string()));
        assert!(context.subject_tags.contains(&"ci".to_string()));
    }

    #[test]
    fn sss_context_scores_above_unrelated_aida_docs() {
        let query = infer_context_from_text("moving dumbo to aws for Sad Sack Signals");
        let sss = infer_context_from_memory(&memory(
            "Sad Sack Signals",
            "forge-signalsmith and forge-cash-risk-ml Slack jobs for SSS",
            Some("/Users/tbedor"),
            Some("tbedor, Node"),
        ));
        let aida = infer_context_from_memory(&memory(
            "aida-docs rule cleanup",
            "Risk Arbiter automated cleanup plan with Linear milestones",
            Some("/Users/tbedor/Development/aida-docs"),
            Some("aida-docs"),
        ));

        assert!(context_score(&query, &sss) > context_score(&query, &aida));
    }

    fn memory(
        title: &str,
        body: &str,
        project_id: Option<&str>,
        project_descriptor: Option<&str>,
    ) -> MemoryRecord {
        MemoryRecord {
            id: Some(1),
            title: title.to_string(),
            body: body.to_string(),
            scope: MemoryScope::Project,
            kind: MemoryKind::Lesson,
            task_keys: Vec::new(),
            source_turn_refs: Vec::new(),
            created_at: "now".to_string(),
            updated_at: "now".to_string(),
            is_active: true,
            session_id: None,
            project_id: project_id.map(str::to_string),
            project_descriptor: project_descriptor.map(str::to_string),
            lineage_refs: Vec::new(),
        }
    }
}
