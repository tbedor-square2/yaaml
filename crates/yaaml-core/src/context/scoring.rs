use std::collections::HashSet;

use crate::context::tags::is_high_signal_tag;
use crate::ContextMetadata;

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

fn has_any_tag(tags: &HashSet<&String>, needles: &[&str]) -> bool {
    needles
        .iter()
        .any(|needle| tags.iter().any(|tag| tag == needle))
}

fn high_signal_overlap(left: &HashSet<&String>, right: &HashSet<&String>) -> bool {
    left.iter()
        .any(|tag| is_high_signal_tag(tag) && right.contains(tag))
}
