pub(super) fn normalize_optional_tag(value: String) -> Option<String> {
    let normalized = normalize_tag(&value);
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

pub(super) fn push_tag(tags: &mut Vec<String>, tag: &str) {
    let tag = normalize_tag(tag);
    if !tag.is_empty() && !tags.iter().any(|existing| existing == &tag) {
        tags.push(tag);
    }
}

fn normalize_tag(value: &str) -> String {
    value
        .trim()
        .trim_matches(|ch: char| !ch.is_ascii_alphanumeric())
        .to_ascii_lowercase()
        .replace('_', "-")
}
