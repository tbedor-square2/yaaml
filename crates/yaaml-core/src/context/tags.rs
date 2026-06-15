pub(super) const KNOWN_PHRASES: &[(&str, &[&str])] = &[
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

pub(super) fn is_high_signal_tag(tag: &str) -> bool {
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

fn normalize_tag(value: &str) -> String {
    value
        .trim()
        .trim_matches(|ch: char| !ch.is_ascii_alphanumeric())
        .to_ascii_lowercase()
        .replace('_', "-")
}
