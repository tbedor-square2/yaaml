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
    ("recall eval", &["recall-eval", "recall", "eval"]),
    ("recall evaluation", &["recall-eval", "recall", "eval"]),
    ("recall quality", &["recall-quality", "recall"]),
    ("tool recall", &["tool-recall", "recall"]),
    ("background recall", &["background-recall", "recall"]),
    ("task state", &["task-state"]),
    ("task-state", &["task-state"]),
    ("conversation segment", &["conversation-segment", "segment"]),
    ("project categorization", &["project-classification"]),
    ("project classification", &["project-classification"]),
    ("memory consolidation", &["memory-consolidation"]),
    ("memory formation", &["memory-formation"]),
    ("llm filter", &["llm-filter"]),
    ("vector search", &["vector-search"]),
    ("embedding", &["embedding"]),
    ("transcript", &["transcript"]),
    ("ingestion", &["ingestion"]),
    ("backlog", &["ingestion", "backlog"]),
    ("daemon", &["daemon"]),
    ("pretooluse", &["tool-hook"]),
    ("pre-tool", &["tool-hook"]),
    ("hook", &["tool-hook"]),
    ("eval", &["eval"]),
    ("segment", &["segment"]),
    ("consolidation", &["memory-consolidation"]),
    ("elroy", &["elroy"]),
    ("model-debugger", &["model-debugger"]),
    ("linear.app", &["linear", "work-tracking"]),
    ("linear", &["linear", "work-tracking"]),
    ("work tracking", &["work-tracking"]),
    ("slack", &["slack"]),
    ("github", &["github"]),
    ("pull request", &["github", "pr"]),
    ("pull request body", &["github", "pr", "pr-management"]),
    ("pr body", &["github", "pr", "pr-management"]),
    (" pr ", &["github", "pr"]),
    ("gh pr edit", &["github", "pr", "pr-management"]),
    ("gh pr view", &["github", "pr", "pr-management"]),
    ("base branch", &["github", "pr", "branch-management"]),
    ("target branch", &["github", "pr", "branch-management"]),
    ("retarget", &["github", "pr", "branch-management"]),
    ("retargeting", &["github", "pr", "branch-management"]),
    ("rebase", &["github", "pr", "branch-management"]),
    ("rebasing", &["github", "pr", "branch-management"]),
    ("commit list", &["github", "pr", "branch-management"]),
    ("draft pr", &["github", "pr", "pr-management"]),
    ("failed test", &["test-fix"]),
    ("test failure", &["test-fix"]),
    ("failing test", &["test-fix"]),
    ("stale test", &["code-cleanup", "test-fix"]),
    ("remove stale", &["code-cleanup"]),
    (" ci ", &["ci"]),
    ("kitt", &["kitt"]),
    ("prefect", &["prefect"]),
    ("bigquery", &["bigquery"]),
    ("aws", &["aws"]),
    ("datadog", &["datadog", "observability"]),
    ("cloudflare access", &["cloudflare-access", "auth"]),
    ("cloudflare", &["cloudflare-access", "auth"]),
    ("warp", &["warp", "auth"]),
    ("vpn", &["vpn", "auth"]),
    ("authentication", &["auth"]),
    ("auth redirect", &["auth"]),
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
