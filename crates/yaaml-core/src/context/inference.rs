use std::fs;
use std::path::{Path, PathBuf};

use crate::context::tags::{push_tag, KNOWN_PHRASES};
use crate::{merge_contexts, ContextMetadata, MemoryRecord};

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
    } else {
        trimmed.strip_prefix("ssh://git@github.com/")?
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
