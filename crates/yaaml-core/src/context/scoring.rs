use std::collections::HashSet;

use crate::ContextMetadata;

pub fn context_score(query: &ContextMetadata, memory: &ContextMetadata) -> f32 {
    let query_tags = query.subject_tags.iter().collect::<HashSet<_>>();
    let memory_tags = memory.subject_tags.iter().collect::<HashSet<_>>();
    let overlap = query_tags.intersection(&memory_tags).count();
    let mut score = (overlap as f32 * 0.21).min(0.63);

    match (query.repo_id.as_deref(), memory.repo_id.as_deref()) {
        (Some(left), Some(right)) if left == right => score += 0.30,
        (Some(_), Some(_)) if tag_overlap(&query_tags, &memory_tags) => score -= 0.08,
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

    if !query.subject_tags.is_empty() && !tag_overlap(&query_tags, &memory_tags) {
        score -= 0.18;
    }

    score
}

fn tag_overlap(left: &HashSet<&String>, right: &HashSet<&String>) -> bool {
    left.iter().any(|tag| right.contains(tag))
}
