use std::collections::{BTreeMap, HashSet};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use yaaml_store::database::EvalRunRecord;
use yaaml_store::Database;

use crate::recall_filter::RecallFilterTelemetry;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct StatsAnchor {
    session_id: String,
    turn_ordinal: u64,
}

#[derive(Debug, Deserialize)]
struct StatsRecallTaskPayload {
    session_id: String,
    turn_ordinal: u64,
}

#[derive(Debug, Deserialize)]
struct StatsRecallEvalTaskPayload {
    session_id: String,
    turn_ordinal: Option<u64>,
    recall_text: String,
    memory_ids: Vec<i64>,
    filter_telemetry: Option<RecallFilterTelemetry>,
    recall_origin: String,
    tool_name: Option<String>,
}

#[derive(Debug, Clone)]
struct StatsRecallVolumeRun {
    memory_count: usize,
    recall_chars: usize,
    filter_telemetry: Option<RecallFilterTelemetry>,
    recall_origin: String,
    tool_name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct StatsOutput {
    eligible_turns: usize,
    recall_runs: usize,
    recall_rate: f64,
    non_empty_recall_runs: usize,
    non_empty_recall_rate: f64,
    non_empty_per_recall_rate: f64,
    abstention: StatsAbstention,
    volume: StatsVolume,
    useful: StatsUseful,
    llm_filter: StatsLlmFilter,
    by_origin: Vec<StatsSegment>,
    by_tool: Vec<StatsSegment>,
}

#[derive(Debug, Default, Clone, Serialize)]
struct StatsSegment {
    name: String,
    recall_runs: usize,
    non_empty_recall_runs: usize,
    evaluated_recall_runs: usize,
    useful_recall_runs: usize,
    judged_memory_results: usize,
    good_memory_results: usize,
    low_memory_results: usize,
    average_score: Option<f64>,
    average_recall_chars_per_non_empty_run: f64,
}

#[derive(Debug, Serialize)]
struct StatsVolume {
    average_memories_per_non_empty_run: f64,
    p50_memories_per_non_empty_run: usize,
    p90_memories_per_non_empty_run: usize,
    average_recall_chars_per_non_empty_run: f64,
    p50_recall_chars_per_non_empty_run: usize,
    p90_recall_chars_per_non_empty_run: usize,
    memory_count_buckets: StatsMemoryCountBuckets,
}

#[derive(Debug, Default, Serialize)]
struct StatsMemoryCountBuckets {
    zero: usize,
    one_to_two: usize,
    three_to_five: usize,
    more_than_five: usize,
}

#[derive(Debug, Serialize)]
struct StatsAbstention {
    empty_recall_runs: usize,
    evaluated_empty_recall_runs: usize,
    clean_abstention_runs: usize,
    missed_useful_abstention_runs: usize,
    unjudged_empty_recall_runs: usize,
    clean_abstention_rate_per_empty_recall: f64,
    missed_useful_abstention_rate_per_empty_recall: f64,
    missed_useful_abstention_rate_per_evaluated_empty_recall: f64,
}

#[derive(Debug, Serialize)]
struct StatsUseful {
    evaluated_recall_runs: usize,
    useful_recall_runs: usize,
    useful_run_rate_per_eligible_turn: f64,
    useful_run_rate_per_evaluated_recall: f64,
    judged_memory_results: usize,
    good_memory_results: usize,
    low_memory_results: usize,
    good_memory_rate: f64,
    low_memory_rate: f64,
    insufficient_context_results: usize,
}

#[derive(Debug, Serialize)]
struct StatsLlmFilter {
    runs_with_filter_telemetry: usize,
    llm_attempted_runs: usize,
    llm_applied_runs: usize,
    llm_error_runs: usize,
    average_candidates_per_filtered_run: f64,
    average_dropped_memories_per_applied_run: f64,
}

pub fn build_stats(db: &Database, eval_limit: usize) -> Result<StatsOutput> {
    let eligible = db
        .completed_turn_anchors()
        .context("failed to load completed turn anchors")?
        .into_iter()
        .map(|turn| StatsAnchor {
            session_id: turn.session_id,
            turn_ordinal: turn.ordinal,
        })
        .collect::<HashSet<_>>();

    let mut recall_runs = db
        .list_tasks_by_kind(crate::daemon::TASK_KIND_RECALL)
        .context("failed to list recall tasks")?
        .into_iter()
        .filter(|task| task.status == "completed")
        .filter_map(|task| recall_task_anchor(&task.payload_json))
        .filter(|anchor| eligible.contains(anchor))
        .collect::<HashSet<_>>();

    let mut recall_eval_by_anchor = BTreeMap::<StatsAnchor, StatsRecallVolumeRun>::new();
    let mut volume_runs = Vec::<StatsRecallVolumeRun>::new();
    for task in db
        .list_tasks_by_kind(crate::daemon::TASK_KIND_RECALL_EVAL)
        .context("failed to list recall eval tasks")?
    {
        let Some((anchor, volume)) = recall_eval_task_volume(&task.payload_json) else {
            continue;
        };
        volume_runs.push(volume.clone());
        if let Some(anchor) = anchor {
            if !eligible.contains(&anchor) {
                continue;
            }
            recall_runs.insert(anchor.clone());
            recall_eval_by_anchor.entry(anchor).or_insert(volume);
        }
    }

    let eval_runs = db
        .list_eval_runs(eval_limit)
        .context("failed to list eval runs")?;
    let mut latest_eval_run_by_anchor = BTreeMap::<StatsAnchor, EvalRunRecord>::new();
    for run in &eval_runs {
        let Some(anchor) = eval_run_anchor(run) else {
            continue;
        };
        if eligible.contains(&anchor) {
            latest_eval_run_by_anchor
                .entry(anchor)
                .or_insert_with(|| run.clone());
        }
    }

    let mut evaluated_recall_runs = 0_usize;
    let mut useful_recall_runs = 0_usize;
    let mut judged_memory_results = 0_usize;
    let mut good_memory_results = 0_usize;
    let mut low_memory_results = 0_usize;
    let mut insufficient_context_results = 0_usize;
    let mut eval_outcome_by_anchor = BTreeMap::<StatsAnchor, StatsEvalOutcome>::new();
    for run in latest_eval_run_by_anchor.values() {
        let results = db
            .eval_results_for_run(run.id)
            .with_context(|| format!("failed to load eval results for run {}", run.id))?;
        let mut has_numeric_score = false;
        let mut has_useful_score = false;
        let mut has_clean_abstention = false;
        let mut has_missed_useful_abstention = false;
        let Some(anchor) = eval_run_anchor(run) else {
            continue;
        };
        for result in results {
            match result.judge_score.as_deref() {
                Some("clean_abstention") => {
                    has_clean_abstention = true;
                }
                Some("missed_useful_abstention") => {
                    has_missed_useful_abstention = true;
                }
                Some(score) => {
                    let Some(score) = numeric_eval_score(score) else {
                        if score == "insufficient_context" {
                            insufficient_context_results += 1;
                        }
                        continue;
                    };
                    has_numeric_score = true;
                    judged_memory_results += 1;
                    if score >= 4 {
                        good_memory_results += 1;
                        has_useful_score = true;
                    } else if score <= 2 {
                        low_memory_results += 1;
                    }
                }
                None => {}
            }
        }
        if has_numeric_score {
            evaluated_recall_runs += 1;
        }
        if has_useful_score {
            useful_recall_runs += 1;
        }
        eval_outcome_by_anchor.insert(
            anchor,
            StatsEvalOutcome {
                has_numeric_score,
                has_useful_score,
                has_clean_abstention,
                has_missed_useful_abstention,
            },
        );
    }

    let non_empty_by_anchor = recall_eval_by_anchor
        .iter()
        .filter(|(_, volume)| volume.memory_count > 0)
        .map(|(anchor, volume)| (anchor.clone(), volume.clone()))
        .collect::<BTreeMap<_, _>>();
    let empty_recall_anchors = recall_eval_by_anchor
        .iter()
        .filter(|(_, volume)| volume.memory_count == 0)
        .map(|(anchor, _)| anchor.clone())
        .collect::<Vec<_>>();
    let abstention = build_abstention_stats(&empty_recall_anchors, &eval_outcome_by_anchor);
    let volumes = non_empty_by_anchor.values().cloned().collect::<Vec<_>>();
    let volume = build_volume_stats(&volumes);
    let llm_filter = build_llm_filter_stats(&volumes);
    let (mut segment_accumulators, mut tool_accumulators) =
        build_stats_segment_accumulators(db, &eval_runs)?;
    for volume_run in &volume_runs {
        let segment = segment_accumulators
            .entry(volume_run.recall_origin.clone())
            .or_default();
        segment.recall_runs += 1;
        if volume_run.memory_count > 0 {
            segment.non_empty_runs += 1;
            segment.recall_chars.push(volume_run.recall_chars);
        }
        if let Some(tool_name) = &volume_run.tool_name {
            let tool_segment = tool_accumulators.entry(tool_name.clone()).or_default();
            tool_segment.recall_runs += 1;
            if volume_run.memory_count > 0 {
                tool_segment.non_empty_runs += 1;
                tool_segment.recall_chars.push(volume_run.recall_chars);
            }
        }
    }
    let by_origin = stats_segments(segment_accumulators);
    let by_tool = stats_segments(tool_accumulators);
    let eligible_count = eligible.len();
    let recall_count = recall_runs.len();
    let non_empty_count = non_empty_by_anchor.len();
    let useful = StatsUseful {
        evaluated_recall_runs,
        useful_recall_runs,
        useful_run_rate_per_eligible_turn: rate(useful_recall_runs, eligible_count),
        useful_run_rate_per_evaluated_recall: rate(useful_recall_runs, evaluated_recall_runs),
        judged_memory_results,
        good_memory_results,
        low_memory_results,
        good_memory_rate: rate(good_memory_results, judged_memory_results),
        low_memory_rate: rate(low_memory_results, judged_memory_results),
        insufficient_context_results,
    };

    Ok(StatsOutput {
        eligible_turns: eligible_count,
        recall_runs: recall_count,
        recall_rate: rate(recall_count, eligible_count),
        non_empty_recall_runs: non_empty_count,
        non_empty_recall_rate: rate(non_empty_count, eligible_count),
        non_empty_per_recall_rate: rate(non_empty_count, recall_count),
        abstention,
        volume,
        useful,
        llm_filter,
        by_origin,
        by_tool,
    })
}

#[derive(Debug, Clone)]
struct StatsEvalOutcome {
    has_numeric_score: bool,
    has_useful_score: bool,
    has_clean_abstention: bool,
    has_missed_useful_abstention: bool,
}

#[derive(Debug, Default)]
struct StatsSegmentAccumulator {
    recall_runs: usize,
    non_empty_runs: usize,
    eval_runs: usize,
    evaluated_runs: usize,
    useful_runs: usize,
    numeric_scores: Vec<u8>,
    recall_chars: Vec<usize>,
}

fn stats_segments(accumulators: BTreeMap<String, StatsSegmentAccumulator>) -> Vec<StatsSegment> {
    let mut segments = accumulators
        .into_iter()
        .map(|(name, accumulator)| {
            let good_memory_results = accumulator
                .numeric_scores
                .iter()
                .filter(|score| **score >= 4)
                .count();
            let low_memory_results = accumulator
                .numeric_scores
                .iter()
                .filter(|score| **score <= 2)
                .count();
            let average_score = if accumulator.numeric_scores.is_empty() {
                None
            } else {
                Some(
                    accumulator
                        .numeric_scores
                        .iter()
                        .map(|score| f64::from(*score))
                        .sum::<f64>()
                        / accumulator.numeric_scores.len() as f64,
                )
            };
            StatsSegment {
                name,
                recall_runs: accumulator.recall_runs.max(accumulator.eval_runs),
                non_empty_recall_runs: accumulator.non_empty_runs,
                evaluated_recall_runs: accumulator.evaluated_runs,
                useful_recall_runs: accumulator.useful_runs,
                judged_memory_results: accumulator.numeric_scores.len(),
                good_memory_results,
                low_memory_results,
                average_score,
                average_recall_chars_per_non_empty_run: average(&accumulator.recall_chars),
            }
        })
        .collect::<Vec<_>>();
    segments.sort_by_key(|segment| std::cmp::Reverse(segment.recall_runs));
    segments
}

fn build_stats_segment_accumulators(
    db: &Database,
    runs: &[EvalRunRecord],
) -> Result<(
    BTreeMap<String, StatsSegmentAccumulator>,
    BTreeMap<String, StatsSegmentAccumulator>,
)> {
    let mut origin_accumulators = BTreeMap::<String, StatsSegmentAccumulator>::new();
    let mut tool_accumulators = BTreeMap::<String, StatsSegmentAccumulator>::new();
    for run in runs {
        let origin = run.recall_origin.clone();
        let origin_accumulator = origin_accumulators.entry(origin).or_default();
        origin_accumulator.eval_runs += 1;
        let mut tool_accumulator = run
            .tool_name
            .clone()
            .map(|tool_name| tool_accumulators.entry(tool_name).or_default());
        if let Some(tool_accumulator) = tool_accumulator.as_mut() {
            tool_accumulator.eval_runs += 1;
        }
        let results = db
            .eval_results_for_run(run.id)
            .with_context(|| format!("failed to load eval results for run {}", run.id))?;
        let mut has_numeric_score = false;
        let mut has_useful_score = false;
        for result in results {
            let Some(score) = result.judge_score.as_deref().and_then(numeric_eval_score) else {
                continue;
            };
            has_numeric_score = true;
            if score >= 4 {
                has_useful_score = true;
            }
            origin_accumulator.numeric_scores.push(score);
            if let Some(tool_accumulator) = tool_accumulator.as_mut() {
                tool_accumulator.numeric_scores.push(score);
            }
        }
        if has_numeric_score {
            origin_accumulator.evaluated_runs += 1;
            if let Some(tool_accumulator) = tool_accumulator.as_mut() {
                tool_accumulator.evaluated_runs += 1;
            }
        }
        if has_useful_score {
            origin_accumulator.useful_runs += 1;
            if let Some(tool_accumulator) = tool_accumulator.as_mut() {
                tool_accumulator.useful_runs += 1;
            }
        }
    }
    Ok((origin_accumulators, tool_accumulators))
}

fn recall_task_anchor(payload_json: &str) -> Option<StatsAnchor> {
    serde_json::from_str::<StatsRecallTaskPayload>(payload_json)
        .ok()
        .map(|payload| StatsAnchor {
            session_id: payload.session_id,
            turn_ordinal: payload.turn_ordinal,
        })
}

fn recall_eval_task_volume(
    payload_json: &str,
) -> Option<(Option<StatsAnchor>, StatsRecallVolumeRun)> {
    let payload = serde_json::from_str::<StatsRecallEvalTaskPayload>(payload_json).ok()?;
    let anchor = payload.turn_ordinal.map(|turn_ordinal| StatsAnchor {
        session_id: payload.session_id,
        turn_ordinal,
    });
    Some((
        anchor,
        StatsRecallVolumeRun {
            memory_count: payload.memory_ids.len(),
            recall_chars: payload.recall_text.chars().count(),
            filter_telemetry: payload.filter_telemetry,
            recall_origin: payload.recall_origin,
            tool_name: payload.tool_name,
        },
    ))
}

fn eval_run_anchor(run: &EvalRunRecord) -> Option<StatsAnchor> {
    Some(StatsAnchor {
        session_id: run.session_id.clone()?,
        turn_ordinal: run.turn_ordinal?,
    })
}

fn build_volume_stats(volumes: &[StatsRecallVolumeRun]) -> StatsVolume {
    let memory_counts = volumes
        .iter()
        .map(|volume| volume.memory_count)
        .collect::<Vec<_>>();
    let recall_chars = volumes
        .iter()
        .map(|volume| volume.recall_chars)
        .collect::<Vec<_>>();
    let mut buckets = StatsMemoryCountBuckets::default();
    for count in &memory_counts {
        match *count {
            0 => buckets.zero += 1,
            1..=2 => buckets.one_to_two += 1,
            3..=5 => buckets.three_to_five += 1,
            _ => buckets.more_than_five += 1,
        }
    }

    StatsVolume {
        average_memories_per_non_empty_run: average(&memory_counts),
        p50_memories_per_non_empty_run: percentile(&memory_counts, 0.50),
        p90_memories_per_non_empty_run: percentile(&memory_counts, 0.90),
        average_recall_chars_per_non_empty_run: average(&recall_chars),
        p50_recall_chars_per_non_empty_run: percentile(&recall_chars, 0.50),
        p90_recall_chars_per_non_empty_run: percentile(&recall_chars, 0.90),
        memory_count_buckets: buckets,
    }
}

fn build_llm_filter_stats(volumes: &[StatsRecallVolumeRun]) -> StatsLlmFilter {
    let telemetry = volumes
        .iter()
        .filter_map(|volume| volume.filter_telemetry.as_ref())
        .collect::<Vec<_>>();
    let applied = telemetry
        .iter()
        .filter(|telemetry| telemetry.llm_applied)
        .copied()
        .collect::<Vec<_>>();
    let candidate_counts = telemetry
        .iter()
        .map(|telemetry| telemetry.candidate_count)
        .collect::<Vec<_>>();
    let dropped_counts = applied
        .iter()
        .map(|telemetry| {
            telemetry
                .deterministic_selected_count
                .saturating_sub(telemetry.final_selected_count)
        })
        .collect::<Vec<_>>();
    StatsLlmFilter {
        runs_with_filter_telemetry: telemetry.len(),
        llm_attempted_runs: telemetry
            .iter()
            .filter(|telemetry| telemetry.llm_attempted)
            .count(),
        llm_applied_runs: applied.len(),
        llm_error_runs: telemetry
            .iter()
            .filter(|telemetry| telemetry.llm_error.is_some())
            .count(),
        average_candidates_per_filtered_run: average(&candidate_counts),
        average_dropped_memories_per_applied_run: average(&dropped_counts),
    }
}

fn build_abstention_stats(
    empty_recall_anchors: &[StatsAnchor],
    eval_outcome_by_anchor: &BTreeMap<StatsAnchor, StatsEvalOutcome>,
) -> StatsAbstention {
    let mut evaluated_empty_recall_runs = 0_usize;
    let mut clean_abstention_runs = 0_usize;
    let mut missed_useful_abstention_runs = 0_usize;
    let mut unjudged_empty_recall_runs = 0_usize;

    for anchor in empty_recall_anchors {
        match eval_outcome_by_anchor.get(anchor) {
            Some(outcome) if outcome.has_missed_useful_abstention => {
                evaluated_empty_recall_runs += 1;
                missed_useful_abstention_runs += 1;
            }
            Some(outcome) if outcome.has_clean_abstention => {
                evaluated_empty_recall_runs += 1;
                clean_abstention_runs += 1;
            }
            Some(outcome) if outcome.has_numeric_score => {
                evaluated_empty_recall_runs += 1;
                if outcome.has_useful_score {
                    missed_useful_abstention_runs += 1;
                } else {
                    clean_abstention_runs += 1;
                }
            }
            _ => unjudged_empty_recall_runs += 1,
        }
    }

    StatsAbstention {
        empty_recall_runs: empty_recall_anchors.len(),
        evaluated_empty_recall_runs,
        clean_abstention_runs,
        missed_useful_abstention_runs,
        unjudged_empty_recall_runs,
        clean_abstention_rate_per_empty_recall: rate(
            clean_abstention_runs,
            empty_recall_anchors.len(),
        ),
        missed_useful_abstention_rate_per_empty_recall: rate(
            missed_useful_abstention_runs,
            empty_recall_anchors.len(),
        ),
        missed_useful_abstention_rate_per_evaluated_empty_recall: rate(
            missed_useful_abstention_runs,
            evaluated_empty_recall_runs,
        ),
    }
}

fn rate(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn numeric_eval_score(score: &str) -> Option<u8> {
    match score.trim() {
        "1" => Some(1),
        "2" => Some(2),
        "3" => Some(3),
        "4" => Some(4),
        "5" => Some(5),
        _ => None,
    }
}

fn average(values: &[usize]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<usize>() as f64 / values.len() as f64
    }
}

fn percentile(values: &[usize], percentile: f64) -> usize {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let index = ((sorted.len() as f64 * percentile).ceil() as usize).saturating_sub(1);
    sorted[index.min(sorted.len() - 1)]
}

pub fn print_human_stats(stats: &StatsOutput) {
    println!("YAAML recall stats");
    println!("  eligible turns: {}", stats.eligible_turns);
    println!(
        "  recall runs: {} ({})",
        stats.recall_runs,
        percent(stats.recall_rate)
    );
    println!(
        "  non-empty recall: {} ({} of eligible, {} of recall runs)",
        stats.non_empty_recall_runs,
        percent(stats.non_empty_recall_rate),
        percent(stats.non_empty_per_recall_rate)
    );
    println!(
        "  recall volume: avg {:.2} memories, p50 {}, p90 {}; avg {:.0} chars, p50 {}, p90 {}",
        stats.volume.average_memories_per_non_empty_run,
        stats.volume.p50_memories_per_non_empty_run,
        stats.volume.p90_memories_per_non_empty_run,
        stats.volume.average_recall_chars_per_non_empty_run,
        stats.volume.p50_recall_chars_per_non_empty_run,
        stats.volume.p90_recall_chars_per_non_empty_run
    );
    println!(
        "  memory-count buckets: 0={}, 1-2={}, 3-5={}, >5={}",
        stats.volume.memory_count_buckets.zero,
        stats.volume.memory_count_buckets.one_to_two,
        stats.volume.memory_count_buckets.three_to_five,
        stats.volume.memory_count_buckets.more_than_five
    );
    println!(
        "  abstention: empty={} evaluated={} clean={} ({}) missed_useful={} ({} of empty, {} of evaluated empty) unjudged={}",
        stats.abstention.empty_recall_runs,
        stats.abstention.evaluated_empty_recall_runs,
        stats.abstention.clean_abstention_runs,
        percent(stats.abstention.clean_abstention_rate_per_empty_recall),
        stats.abstention.missed_useful_abstention_runs,
        percent(stats.abstention.missed_useful_abstention_rate_per_empty_recall),
        percent(stats.abstention.missed_useful_abstention_rate_per_evaluated_empty_recall),
        stats.abstention.unjudged_empty_recall_runs
    );
    println!(
        "  useful recall: {} / {} evaluated runs ({}); {} of eligible turns",
        stats.useful.useful_recall_runs,
        stats.useful.evaluated_recall_runs,
        percent(stats.useful.useful_run_rate_per_evaluated_recall),
        percent(stats.useful.useful_run_rate_per_eligible_turn)
    );
    println!(
        "  memory judgments: {} good={} ({}) low={} ({}) n/a={}",
        stats.useful.judged_memory_results,
        stats.useful.good_memory_results,
        percent(stats.useful.good_memory_rate),
        stats.useful.low_memory_results,
        percent(stats.useful.low_memory_rate),
        stats.useful.insufficient_context_results
    );
    println!(
        "  llm filter: telemetry_runs={} attempted={} applied={} errors={} avg_candidates={:.2} avg_dropped={:.2}",
        stats.llm_filter.runs_with_filter_telemetry,
        stats.llm_filter.llm_attempted_runs,
        stats.llm_filter.llm_applied_runs,
        stats.llm_filter.llm_error_runs,
        stats.llm_filter.average_candidates_per_filtered_run,
        stats.llm_filter.average_dropped_memories_per_applied_run
    );
    print_stats_segments("by origin", &stats.by_origin);
    print_stats_segments("by tool", &stats.by_tool);
}

fn percent(rate: f64) -> String {
    format!("{:.1}%", rate * 100.0)
}

fn print_stats_segments(label: &str, segments: &[StatsSegment]) {
    if segments.is_empty() {
        return;
    }
    println!("  {label}:");
    for segment in segments {
        let average_score = segment
            .average_score
            .map(|score| format!("{score:.2}"))
            .unwrap_or_else(|| "-".to_string());
        println!(
            "    {}: recall={} non_empty={} evaluated={} useful={} avg_score={} avg_chars={:.0}",
            segment.name,
            segment.recall_runs,
            segment.non_empty_recall_runs,
            segment.evaluated_recall_runs,
            segment.useful_recall_runs,
            average_score,
            segment.average_recall_chars_per_non_empty_run
        );
    }
}
