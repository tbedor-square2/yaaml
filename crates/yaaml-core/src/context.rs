use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::MemoryRecord;

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
            .filter_map(|tag| normalize_optional_tag(tag))
            .filter(|tag| seen.insert(tag.clone()))
            .collect();
        self
    }
}

pub fn infer_context_from_memory(memory: &MemoryRecord) -> ContextMetadata {
    let mut context = memory
        .project_id
        .as_deref()
        .map(|project_id| infer_context_from_path(Path::new(project_id)))
        .unwrap_or_default();
    let text_context = infer_context_from_text(&format!(
        "{}\n{}\n{}",
        memory.title,
        memory.body,
        memory.project_descriptor.as_deref().unwrap_or("")
    ));
    merge_contexts(&mut context, text_context);
    if context.classification_source.is_empty() {
        context.classification_source = "memory".to_string();
    }
    context.normalized()
}

pub fn infer_context_from_path(path: &Path) -> ContextMetadata {
    let normalized = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if is_broad_home_path(&normalized) {
        return ContextMetadata {
            classification_source: "broad_path".to_string(),
            classification_evidence: normalized.display().to_string(),
            ..ContextMetadata::default()
        };
    }

    let repo_root = git_root(&normalized).unwrap_or_else(|| normalized.clone());
    let repo_name = repo_root
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string);
    let repo_id = git_remote_slug(&repo_root).or(repo_name.clone());
    let work_area = normalized
        .strip_prefix(&repo_root)
        .ok()
        .and_then(first_meaningful_component);
    let mut subject_tags = Vec::new();
    if let Some(repo_name) = repo_name {
        push_tag(&mut subject_tags, &repo_name);
    }
    if let Some(work_area) = &work_area {
        push_tag(&mut subject_tags, work_area);
    }

    ContextMetadata {
        repo_id,
        repo_root: Some(repo_root.display().to_string()),
        work_area,
        activity_domain: Some("code".to_string()),
        subject_tags,
        classification_source: "path".to_string(),
        classification_evidence: normalized.display().to_string(),
    }
    .normalized()
}

pub fn infer_context_from_text(text: &str) -> ContextMetadata {
    let lower = text.to_ascii_lowercase();
    let mut subject_tags = Vec::new();
    let mut repo_id = first_github_repo(&lower);

    for (needle, tags) in KNOWN_PHRASES {
        if lower.contains(needle) {
            for tag in *tags {
                push_tag(&mut subject_tags, tag);
            }
        }
    }

    for repo in development_path_repos(&lower) {
        push_tag(&mut subject_tags, &repo);
        if repo_id.is_none() {
            repo_id = Some(repo);
        }
    }

    if let Some(repo) = &repo_id {
        if let Some(repo_name) = repo.rsplit('/').next() {
            push_tag(&mut subject_tags, repo_name);
        }
    }

    repo_id = repo_id.or_else(|| inferred_repo_from_tags(&subject_tags));
    let work_area = inferred_work_area(&subject_tags);
    let activity_domain = infer_activity_domain(&lower, &subject_tags);
    let evidence = subject_tags
        .iter()
        .take(8)
        .cloned()
        .collect::<Vec<_>>()
        .join(",");

    ContextMetadata {
        repo_id,
        repo_root: None,
        work_area,
        activity_domain,
        subject_tags,
        classification_source: "text".to_string(),
        classification_evidence: evidence,
    }
    .normalized()
}

pub fn context_score(query: &ContextMetadata, memory: &ContextMetadata) -> f32 {
    let query_tags = query.subject_tags.iter().collect::<HashSet<_>>();
    let memory_tags = memory.subject_tags.iter().collect::<HashSet<_>>();
    let overlap = query_tags.intersection(&memory_tags).count();
    let mut score = (overlap as f32 * 0.09).min(0.36);

    match (query.repo_id.as_deref(), memory.repo_id.as_deref()) {
        (Some(left), Some(right)) if left == right => score += 0.30,
        (Some(_), Some(_)) if high_signal_overlap(&query_tags, &memory_tags) => score -= 0.08,
        (Some(_), Some(_)) => score -= 0.25,
        _ => {}
    }

    match (query.work_area.as_deref(), memory.work_area.as_deref()) {
        (Some(left), Some(right)) if left == right => score += 0.18,
        (Some(_), Some(_)) if overlap == 0 => score -= 0.08,
        _ => {}
    }

    if query.activity_domain.is_some()
        && query.activity_domain == memory.activity_domain
        && query.activity_domain.as_deref() != Some("code")
    {
        score += 0.06;
    }

    let high_signal_query_tags = query
        .subject_tags
        .iter()
        .filter(|tag| is_high_signal_tag(tag))
        .count();
    if high_signal_query_tags > 0 && !high_signal_overlap(&query_tags, &memory_tags) {
        score -= 0.18;
    }

    if has_any_tag(&query_tags, &["sss", "sad-sack-signals", "dumbo"])
        && has_any_tag(
            &memory_tags,
            &["aida-docs", "riskarbiter", "elroy", "model-debugger"],
        )
    {
        score -= 0.22;
    }

    score
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

const KNOWN_PHRASES: &[(&str, &[&str])] = &[
    ("sad sack signals", &["sss", "sad-sack-signals"]),
    ("sad-sack-signals", &["sss", "sad-sack-signals"]),
    (
        "proj-sack-sad-signals",
        &["sss", "sad-sack-signals", "slack"],
    ),
    ("dumbo", &["dumbo", "sss"]),
    ("forge-signalsmith", &["forge-signalsmith", "sss"]),
    (
        "forge-cash-risk-ml",
        &["forge-cash-risk-ml", "sss", "risk-ml"],
    ),
    (
        "signals-deprecate-and-delete",
        &["forge-signalsmith", "sss"],
    ),
    ("scheduled_signal_metrics", &["forge-cash-risk-ml", "sss"]),
    ("aida-docs", &["aida-docs", "docs"]),
    ("risk arbiter", &["riskarbiter", "risk-arbiter"]),
    ("riskarbiter", &["riskarbiter", "risk-arbiter"]),
    ("yaaml", &["yaaml"]),
    ("elroy", &["elroy"]),
    ("model-debugger", &["model-debugger"]),
    ("linear.app", &["linear", "work-tracking"]),
    ("linear", &["linear", "work-tracking"]),
    ("work tracking", &["work-tracking"]),
    ("slack", &["slack"]),
    ("github", &["github"]),
    ("pull request", &["github", "pr"]),
    (" pr ", &["github", "pr"]),
    (" ci ", &["ci"]),
    ("kitt", &["kitt"]),
    ("prefect", &["prefect"]),
    ("bigquery", &["bigquery"]),
    ("aws", &["aws"]),
    ("java", &["java"]),
    ("codex", &["codex"]),
    ("claude code", &["claude-code"]),
    ("sq agent-tools", &["sq-agent-tools"]),
];

fn infer_activity_domain(lower: &str, tags: &[String]) -> Option<String> {
    let has_tag = |tag: &str| tags.iter().any(|candidate| candidate == tag);
    if has_tag("linear")
        || has_tag("work-tracking")
        || lower.contains("ticket")
        || lower.contains("milestone")
    {
        Some("work-tracking".to_string())
    } else if has_tag("slack") {
        Some("communications".to_string())
    } else if has_tag("docs") || lower.contains("decision doc") {
        Some("planning".to_string())
    } else if has_tag("github") || has_tag("ci") || lower.contains("repo") {
        Some("code".to_string())
    } else {
        None
    }
}

fn inferred_repo_from_tags(tags: &[String]) -> Option<String> {
    if tags.iter().any(|tag| tag == "forge-signalsmith") {
        Some("squareup/forge-signalsmith".to_string())
    } else if tags.iter().any(|tag| tag == "forge-cash-risk-ml") {
        Some("squareup/forge-cash-risk-ml".to_string())
    } else if tags.iter().any(|tag| tag == "aida-docs") {
        Some("aida-docs".to_string())
    } else if tags.iter().any(|tag| tag == "yaaml") {
        Some("yaaml".to_string())
    } else if tags.iter().any(|tag| tag == "elroy") {
        Some("elroy".to_string())
    } else if tags.iter().any(|tag| tag == "model-debugger") {
        Some("model-debugger".to_string())
    } else {
        None
    }
}

fn inferred_work_area(tags: &[String]) -> Option<String> {
    if tags.iter().any(|tag| tag == "riskarbiter") {
        Some("riskarbiter".to_string())
    } else if tags.iter().any(|tag| tag == "sss") {
        Some("sad-sack-signals".to_string())
    } else {
        None
    }
}

fn first_github_repo(lower: &str) -> Option<String> {
    let mut rest = lower;
    while let Some(index) = rest.find("github.com/") {
        let after = &rest[index + "github.com/".len()..];
        let mut parts = after
            .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.'));
        let owner = parts.next().unwrap_or_default();
        let repo = parts.next().unwrap_or_default();
        if !owner.is_empty() && !repo.is_empty() {
            return Some(format!("{owner}/{repo}"));
        }
        rest = &after[after.len().min(1)..];
    }
    None
}

fn development_path_repos(lower: &str) -> Vec<String> {
    let mut repos = Vec::new();
    for marker in ["/users/tbedor/development/", "~/development/"] {
        let mut rest = lower;
        while let Some(index) = rest.find(marker) {
            let after = &rest[index + marker.len()..];
            let repo = after
                .split(|ch: char| {
                    !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.')
                })
                .next()
                .unwrap_or_default();
            if !repo.is_empty() {
                push_tag(&mut repos, repo);
            }
            rest = &after[after.len().min(1)..];
        }
    }
    repos
}

fn git_root(path: &Path) -> Option<PathBuf> {
    let mut current = Some(path);
    while let Some(candidate) = current {
        if candidate.join(".git").exists() {
            return Some(candidate.to_path_buf());
        }
        current = candidate.parent();
    }
    None
}

fn git_remote_slug(repo_root: &Path) -> Option<String> {
    let config = fs::read_to_string(repo_root.join(".git").join("config")).ok()?;
    config
        .lines()
        .filter_map(|line| line.trim().strip_prefix("url = "))
        .find_map(remote_url_slug)
}

fn remote_url_slug(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches(".git");
    let path = if let Some(rest) = trimmed.strip_prefix("git@github.com:") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("https://github.com/") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("ssh://git@github.com/") {
        rest
    } else {
        return None;
    };
    let mut parts = path.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    if owner.is_empty() || repo.is_empty() {
        None
    } else {
        Some(format!("{owner}/{repo}").to_ascii_lowercase())
    }
}

fn first_meaningful_component(path: &Path) -> Option<String> {
    path.components()
        .filter_map(|component| component.as_os_str().to_str())
        .find(|component| {
            !component.is_empty()
                && *component != "."
                && *component != "src"
                && *component != "lib"
                && *component != "crates"
        })
        .map(str::to_string)
}

fn is_broad_home_path(path: &Path) -> bool {
    let text = path.to_string_lossy();
    text == "/Users/tbedor" || text == "/Users/tbedor/Development"
}

fn normalize_optional_tag(value: String) -> Option<String> {
    let normalized = normalize_tag(&value);
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn normalize_tag(value: &str) -> String {
    value
        .trim()
        .trim_matches(|ch: char| !ch.is_ascii_alphanumeric())
        .to_ascii_lowercase()
        .replace('_', "-")
}

fn push_tag(tags: &mut Vec<String>, tag: &str) {
    let tag = normalize_tag(tag);
    if !tag.is_empty() && !tags.iter().any(|existing| existing == &tag) {
        tags.push(tag);
    }
}

fn has_any_tag(tags: &HashSet<&String>, needles: &[&str]) -> bool {
    needles
        .iter()
        .any(|needle| tags.iter().any(|tag| tag == needle))
}

fn high_signal_overlap(left: &HashSet<&String>, right: &HashSet<&String>) -> bool {
    left.iter()
        .any(|tag| is_high_signal_tag(tag) && right.contains(tag))
}

fn is_high_signal_tag(tag: &str) -> bool {
    !matches!(
        tag,
        "linear"
            | "work-tracking"
            | "github"
            | "pr"
            | "ci"
            | "docs"
            | "slack"
            | "java"
            | "codex"
            | "claude-code"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MemoryRecord, MemoryScope};

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
