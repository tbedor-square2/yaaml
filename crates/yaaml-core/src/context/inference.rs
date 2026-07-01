use std::fs;
use std::path::{Path, PathBuf};

use crate::context::tags::push_tag;
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
    let mut subject_tags = generic_subject_tags(text);
    let mut repo_id = first_github_repo(&lower);

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

    let activity_domain = repo_id.as_ref().map(|_| "code".to_string());
    let evidence = subject_tags
        .iter()
        .take(8)
        .cloned()
        .collect::<Vec<_>>()
        .join(",");

    ContextMetadata {
        repo_id,
        repo_root: None,
        work_area: None,
        activity_domain,
        subject_tags,
        classification_source: "text".to_string(),
        classification_evidence: evidence,
    }
    .normalized()
}

fn generic_subject_tags(text: &str) -> Vec<String> {
    let mut tags = Vec::new();
    let normalized_text = text
        .lines()
        .map(strip_transcript_role_prefix)
        .collect::<Vec<_>>()
        .join("\n");
    for token in normalized_text.split_whitespace().map(trim_topic_token) {
        if token.is_empty() {
            continue;
        }
        if acronym_topic(token)
            || (token.len() >= 3 && (identifier_like_topic(token) || long_word_topic(token)))
        {
            push_tag(&mut tags, token);
        }
    }
    for phrase in capitalized_topic_phrases(&normalized_text) {
        push_tag(&mut tags, &phrase);
    }
    tags
}

fn strip_transcript_role_prefix(line: &str) -> &str {
    let trimmed = line.trim_start();
    for prefix in ["user:", "assistant:", "system:", "tool:"] {
        if trimmed
            .get(..prefix.len())
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
        {
            return trimmed[prefix.len()..].trim_start();
        }
    }
    line
}

fn trim_topic_token(token: &str) -> &str {
    token.trim_matches(|ch: char| {
        !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' || ch == '/')
    })
}

fn identifier_like_topic(token: &str) -> bool {
    let has_separator = token.contains('-') || token.contains('_');
    let has_letter = token.chars().any(|ch| ch.is_ascii_alphabetic());
    let is_not_path = !token.contains('/');
    has_separator && has_letter && is_not_path
}

fn acronym_topic(token: &str) -> bool {
    let letters = token
        .chars()
        .filter(|ch| ch.is_ascii_alphabetic())
        .collect::<Vec<_>>();
    (2..=10).contains(&letters.len())
        && letters.iter().all(|ch| ch.is_ascii_uppercase())
        && token
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

fn long_word_topic(token: &str) -> bool {
    token.len() >= 9 && token.chars().all(|ch| ch.is_ascii_lowercase())
}

fn capitalized_topic_phrases(text: &str) -> Vec<String> {
    let mut phrases = Vec::new();
    let mut current = Vec::new();
    for token in text.split_whitespace().map(trim_topic_token) {
        if title_word(token) {
            current.push(token.to_ascii_lowercase());
            if current.len() == 4 {
                phrases.push(current.join("-"));
                current.clear();
            }
        } else {
            push_capitalized_phrase(&mut phrases, &mut current);
        }
    }
    push_capitalized_phrase(&mut phrases, &mut current);
    phrases
}

fn title_word(token: &str) -> bool {
    if token.len() < 3 || !token.chars().all(|ch| ch.is_ascii_alphabetic()) {
        return false;
    }
    let mut chars = token.chars();
    chars.next().is_some_and(|ch| ch.is_ascii_uppercase())
        && chars.all(|ch| ch.is_ascii_lowercase())
}

fn push_capitalized_phrase(phrases: &mut Vec<String>, current: &mut Vec<String>) {
    if current.len() >= 2 {
        phrases.push(current.join("-"));
    }
    current.clear();
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
