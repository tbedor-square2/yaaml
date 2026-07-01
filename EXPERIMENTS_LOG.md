# YAAML Experiments Log

This log records durable lessons from YAAML recall and memory-quality
experiments. Keep generated run artifacts under `experiments/recall/<date>-...`
and summarize the decision-level result here so later experiments do not have
to rediscover the same tradeoffs.

## 2026-07-01: Remove Hardcoded Topic Classifiers

Sources:

1. `target/recall-backtests/no-hardcoded-topics/no-hardcoded-topics.summary.json`
2. `target/recall-backtests/no-hardcoded-topics-eval-library/no-hardcoded-topics-eval-library.details.jsonl`
3. `just quality`

Experiment:

1. Removed the static phrase-to-topic table from core context inference.
2. Removed hardcoded broad/high-signal tag denylists and named topic gates for
   branch-management, test-fix, and access-blocker contexts.
3. Stopped generating new `tool:*` task keys from command names. Legacy
   `tool:*` keys can still exist in old memories and historical eval fixtures,
   but command names no longer act as active recall identity keys.
4. Replaced phrase topics with mechanical generic labels from identifiers,
   acronyms, long lowercase terms, capitalized phrases, repo URLs, and local
   development paths.
5. Increased generic context-overlap weight so two independent generic labels
   can still represent strong context after removing hand-tuned labels.

Metrics:

1. `just quality` passed, including formatting, typecheck, strict clippy, full
   tests, and coverage. Final line coverage was 90.62%.
2. Fixed-anchor backtest over 16 anchors selected 7 memories, averaged 0.44
   selected memories per anchor, and returned 9 empty recalls. The selected
   memories were newer than the old oracle rows, so known-score coverage was 0.
3. Partial eval-library backtest processed 29 of 50 anchors before stopping on
   an anchor with no historical recall text. It selected 12 memories, averaged
   0.41 selected memories per anchor, and returned 18 empty recalls.
4. In the partial eval-library run, 2 selected memories had known oracle scores:
   both were useful, with average known score 4.5 and 0 low known selections.
5. The same partial run had 10 oracle-useful anchors, 2 useful-capture runs,
   and 6 missed-useful empty recalls.

Lessons:

1. Removing hardcoded topic classes improves precision on the small known-score
   subset but substantially increases abstention.
2. The current generic extractor is acceptable as a product-safe fallback, but
   it is not a full replacement for learned/oracle segment topics.
3. The next recall-quality step should focus on learned segment labels or
   oracle-generated segment summaries, not restoring fixed topic vocabularies.

Decision:

1. Keep core free of company/project-specific topic phrase tables and denylist
   classifiers.
2. Treat the increased missed-useful abstention rate as expected until YAAML has
   learned segment/topic labels that are not hardcoded into the binary.

## 2026-06-30: Current Runtime Recall Snapshot

Sources:

1. `yaaml stats --since 24h --json`
2. `yaaml eval summary --since 24h --exclude-origin replay --json`

Observed production shape after the recall-quality changes:

1. 252 eligible turns.
2. 124 recall runs, or 0.49 recall runs per eligible turn.
3. 87 turns with recall, for a 34.5% turn recall rate.
4. 107 non-empty recall runs, or 42.5% per eligible turn.
5. Non-empty recall runs returned 1.21 memories on average, with p50 1 and p90
   2 memories.
6. Non-empty recall runs averaged 1169 chars, with p50 1030 and p90 1860 chars.
7. 77 evaluated recall runs; 64 were useful, for an 83.1% useful run rate among
   evaluated recalls.
8. 95 judged memory results: 79 good and 16 low, for a 16.8% low-memory rate.
9. 17 empty recalls: 14 clean abstentions, 1 missed-useful abstention, and 2
   unjudged empty recalls.
10. `session_background` recall averaged 4.11 across 81 judged memory results.
11. `manual_query` recall averaged 4.14 across 14 judged memory results.

Decision:

1. Current context volume is acceptable: recent recall usually injects one
   memory, sometimes two, and not three or more.
2. Remaining failures are mostly context-sensitive placement problems, not raw
   vector-search failures or excessive memory length.
3. Empty recall should continue to be tracked as abstention, not as a low score.

## 2026-06-30: Segment/Task-Fit Recall 5x5

Source: `experiments/recall/2026-06-30-segment-task-fit-5x5/REPORT.md`

Experiment:

1. Added `scripts/recall-segment-task-fit-5x5-experiment.py`.
2. Replayed 200 saved `target/strict-kind-production` anchors across five
   deterministic cohorts.
3. Compared five segment/task-fit variants against current production
   health-action selection: context-weighted rerank, task-fit-required gating,
   wrong-context penalty, segment-evidence gate, and context-weighted top-two.
4. No embedding or LLM provider calls were made. Strategies operated over
   already retrieved candidates, so this tested reranking/selection rather than
   segment-aware candidate generation.
5. Added segment-specific metrics: wrong-context low selections and stale-task
   low selections, both derived from existing oracle rationales for low-scored
   selected memories.

Metrics:

1. `production_health_action`: avg score 2.94, 74 useful selected, 77 low
   selected, 32 wrong-context lows, 43 stale-task lows, 56 useful runs, 58 low
   runs, 2.06 avg memories, 31 empty recalls, 3 missed-useful empties.
2. `segment_context_rerank`: avg score 2.80, 63 useful selected, 75 low
   selected, 37 wrong-context lows, 36 stale-task lows, 52 useful runs, 62 low
   runs, 2.46 avg memories, 15 empty recalls, 2 missed-useful empties.
3. `task_fit_required`: avg score 2.95, 72 useful selected, 75 low selected,
   29 wrong-context lows, 42 stale-task lows, 54 useful runs, 57 low runs, 1.95
   avg memories, 33 empty recalls, 3 missed-useful empties.
4. `wrong_context_penalty`: avg score 2.90, 67 useful selected, 70 low
   selected, 29 wrong-context lows, 40 stale-task lows, 52 useful runs, 59 low
   runs, 1.80 avg memories, 38 empty recalls, 5 missed-useful empties.
5. `segment_evidence_gate`: avg score 2.98, 72 useful selected, 73 low
   selected, 28 wrong-context lows, 40 stale-task lows, 54 useful runs, 55 low
   runs, 1.79 avg memories, 46 empty recalls, 6 missed-useful empties.
6. `segment_context_top2`: avg score 2.96, 52 useful selected, 50 low selected,
   21 wrong-context lows, 26 stale-task lows, 45 useful runs, 47 low runs, 1.78
   avg memories, 15 empty recalls, 2 missed-useful empties.

Lessons:

1. `task_fit_required` was the best conservative segment-fit candidate: it
   reduced wrong-context lows by 3 and average memory count by 0.12 without
   increasing missed-useful empties, but it also lost 2 useful runs.
2. `segment_evidence_gate` improved average score and reduced more lows, but
   increased missed-useful empties from 3 to 6.
3. `segment_context_top2` strongly reduced noise: -27 low selections, -11
   wrong-context lows, -17 stale-task lows, and -0.28 avg memories. It also lost
   too much useful recall: -22 useful selected and -11 useful runs.
4. Naive context-weighted reranking was actively bad for wrong-context recall:
   `segment_context_rerank` increased wrong-context lows from 32 to 37 and
   increased low-selection runs from 58 to 62.
5. Segment/task-fit selection is a precision lever over the existing candidate
   set, but it does not solve candidate retrieval. The useful losses suggest
   that better segment-aware candidate generation is likely more important than
   more aggressive post-retrieval gates.

Decision:

1. Do not ship any of these variants directly.
2. Keep `task_fit_required` as the conservative reranking/gating shape to
   compare against future work.
3. Next segment/task-fit experiment should change candidate generation using
   compact segment summaries or segment keys before vector retrieval, then
   replay against the same metrics.

## 2026-06-26: Broad Codex PreToolUse Hook Removed

Sources:

1. `experiments/recall/README.md`
2. `experiments/recall/2026-06-23-tool-cooldown-5x5/REPORT.md`

Experiment:

1. Installed a broad Codex `PreToolUse` hook that triggered YAAML recall before
   common tool calls, especially Bash.
2. Evaluated tool-origin recall separately from session/manual recall.
3. Replayed five pruning strategies over recent tool-recall cohorts: baseline,
   session cooldown, family cooldown, command-family gate, and targeted
   cooldown.

Metrics:

1. Historical `tool_pre_use` average score was about 2.40 across evaluated
   non-empty runs before removal.
2. Tool-cooldown replay baseline: avg 1.95, 6 useful selections, 33 low
   selections, 6 useful runs, 32 low runs, 20 empty recalls.
3. `session_cooldown_20m`: avg 2.47, 4 useful selections, 11 low selections, 4
   useful runs, 11 low runs, 43 empty recalls, 2 missed-useful empties.
4. `targeted_cooldown`: avg 2.50, 4 useful selections, 10 low selections, 4
   useful runs, 10 low runs, 44 empty recalls, 2 missed-useful empties.
5. `command_family_gate` was too blunt: avg 1.79, 4 useful selections, 20 low
   selections, and 2 missed-useful empties.

Lessons:

1. Broad pre-tool recall produced too much low-value context relative to
   explicit/session recall.
2. Repeated Bash/build/test commands often surfaced plausible but stale task
   memories.
3. Cooldowns helped reduce repeated noise, but the installation/update friction
   and low precision made the hook a poor MVP default.
4. Future tool-triggered recall should be a separate experiment with
   deterministic activation signals and separate metrics, not a broad hook.

Decision:

1. Removed the broad Codex pre-tool hook from default installation.
2. Kept historical `tool_pre_use` stats readable for analysis.

## 2026-06-23: Recall Techniques 5x5

Source: `experiments/recall/2026-06-23-techniques-5x5/REPORT.md`

Experiment:

1. Compared five strategies across five deterministic cohorts for each
   technique family: candidate generation, hard gating, reranking, selection
   budgeting, LLM-filter proxies, memory corpus quality, and eval feedback.
2. Replayed saved `yaaml recall --debug-ranking` outputs and saved eval oracle
   labels for 200 anchors.
3. Treated empty recall as abstention, not as a low score.

Key metrics:

1. Selection budgeting baseline `production_health_action`: score 3.24, 74
   useful selected, 53 low selected, 53 useful runs, 41 low runs, 1.85 avg
   memories.
2. `top_2`: score 3.40, 63 useful selected, 41 low selected, 53 useful runs,
   33 low runs, 1.42 avg memories. This preserved useful-run coverage while
   reducing low selections and context volume.
3. `top_1`: score 3.27, 36 useful selected, 22 low selected, 36 useful runs, 22
   low runs, 0.77 avg memories. It over-pruned useful context.
4. LLM proxy `abstain_if_weak_top`: score 3.28, 74 useful selected, 49 low
   selected, 53 useful runs, 39 low runs, 1.79 avg memories.
5. LLM proxy `llm_strict_proxy`: score 3.45, 64 useful selected, 35 low
   selected, 49 useful runs, 29 low runs, 1.20 avg memories, 17.0%
   missed-useful-empty rate. Its high score came from dropping too much useful
   context.
6. Eval-feedback baseline `strict_kind_no_eval`: score 2.85, 67 useful
   selected, 82 low selected, 53 useful runs, 61 low runs, 2.11 avg memories.
7. `failure_mode_rerank`: score 3.24, 74 useful selected, 53 low selected, 53
   useful runs, 41 low runs, 1.85 avg memories.
8. `precision_abstain`: score 3.36, 73 useful selected, 42 low selected, 52
   useful runs, 35 low runs, 1.52 avg memories, 8.5%
   missed-useful-empty rate.

Lessons:

1. The strongest context-bloat reducer was dynamic selection, especially
   top-two selection.
2. Moderate abstention/filtering can help, but strict variants inflate average
   score by missing useful recall.
3. Project/context-heavy reranking improved some relevance metrics but did not
   reduce context volume; it is a relevance signal, not the main bloat lever.
4. Broad hard gates were not clearly better than narrow evidence gates.
5. Corpus-quality suppression had small replay effects; health labels are useful
   ranking inputs but not sufficient alone.
6. Eval feedback is valuable when it is translated into failure-mode-aware
   reranking rather than blunt suppression.

Decision:

1. Keep failure-mode-aware reranking as the eval-informed baseline.
2. Use dynamic selection/top-two behavior as the default context-volume control.
3. Track missed-useful empty recall separately from score.

## 2026-06-23: Feature Model Recall Selection

Source: `experiments/recall/2026-06-23-feature-model/REPORT.md`

Experiment:

1. Exported candidate-level recall rows from `target/strict-kind-production`.
2. Trained a small local feature model and compared it with production,
   `top_2`, and `weak_top_abstain`.

Dataset:

1. 200 runs.
2. 3200 candidate rows.
3. 413 labeled rows.
4. 106 useful labeled rows.
5. 302 low-scoring labeled rows.

Metrics:

1. `production`: avg score 2.74, 54 useful runs, 69 useful selected, 88 low
   selected, 62 low runs, 2.28 avg memories, 5 empty runs.
2. `top_2`: avg score 2.74, 44 useful runs, 51 useful selected, 67 low
   selected, 54 low runs, 1.69 avg memories, 5 empty runs.
3. `weak_top_abstain`: avg score 2.82, 42 useful runs, 48 useful selected, 57
   low selected, 46 low runs, 1.41 avg memories, 53 empty runs.
4. `feature_model`: avg score 3.30, 39 useful runs, 44 useful selected, 27 low
   selected, 22 low runs, 1.68 avg memories, 12 empty runs.

Lessons:

1. A feature model can sharply reduce known bad recall.
2. The precision gain came with lower useful coverage.
3. Label sparsity is still the main limitation: only 413 of 3200 candidate rows
   had oracle labels, and the feature model still selected 263 unlabeled rows.
4. This was not enough evidence to fine-tune a tiny text model.

Decision:

1. Do not fine-tune a model yet.
2. Keep collecting denser per-candidate labels.
3. Prefer structured runtime changes, such as lower context volume and
   cooldowns, until the feature model plateaus.

## 2026-06-22: Health-Aware Recall 10x10

Source: `experiments/recall/2026-06-22-health-10x10/REPORT.md`

Experiment:

1. Compared ten health-aware selection approaches over ten deterministic
   cohorts using 200 anchors.
2. Tested targeted handling for wrong-context, stale, noisy-metadata,
   context-sensitive, and combined health-action failure modes.

Key metrics:

1. `baseline_strict_kind`: score 2.85, 69 useful selected, 88 low selected, 54
   useful runs, 62 low runs, 2.28 avg memories.
2. `eval_health_rerank`: score 3.03, 74 useful selected, 69 low selected, 56
   useful runs, 54 low runs, 1.97 avg memories.
3. `health_action_rerank`: score 3.20, 75 useful selected, 57 low selected, 54
   useful runs, 44 low runs, 1.88 avg memories.
4. `health_precision_abstain`: score 3.35, 75 useful selected, 45 low selected,
   54 useful runs, 37 low runs, 1.60 avg memories.
5. Abstention for `health_action_rerank`: 22.5% empty rate, 45 empty runs, 39
   clean abstentions, 6 missed-useful empties.
6. Abstention for `health_precision_abstain`: 25.5% empty rate, 51 empty runs,
   44 clean abstentions, 7 missed-useful empties.

Lessons:

1. `health_action_rerank` was the best balanced failure-mode-aware strategy.
2. `health_precision_abstain` had stronger precision but increased
   missed-useful abstentions.
3. Single-mode policies were weaker than combined reranking; failure modes
   interact.

Decision:

1. Implement the combined health-action reranking shape instead of individual
   failure-mode gates.
2. Treat precision-abstain behavior as diagnostic or optional, not default.

## 2026-06-22: Candidate 5x5

Source: `experiments/recall/2026-06-22-candidate-5x5/REPORT.md`

Experiment:

1. Compared five strategy families across five cohorts: candidate pool, dynamic
   recall count, query-signal proxy, hybrid-generation proxy, memory lifecycle,
   and abstention gate.
2. Replayed saved candidates; query-signal and hybrid-generation results were
   proxies over already retrieved candidates.

Key metrics:

1. Candidate pool `pool_3`: score 3.15, 58 useful selected, 49 low selected, 46
   useful runs, 38 low runs, 1.47 avg memories.
2. Candidate pool `pool_8`: score 3.21, 75 useful selected, 57 low selected, 54
   useful runs, 44 low runs, 1.81 avg memories.
3. Dynamic-count baseline `production_health_action`: score 3.21, 75 useful
   selected, 57 low selected, 54 useful runs, 44 low runs, 1.86 avg memories.
4. Dynamic-count `top_2`: score 3.42, 70 useful selected, 40 low selected, 54
   useful runs, 32 low runs, 1.44 avg memories.
5. Query-signal `project_context_proxy`: score 3.26, 77 useful selected, 50 low
   selected, 56 useful runs, 40 low runs, 2.20 avg memories.
6. Query-signal `vector_only_proxy`: score 4.00 but only 1 useful run and 95.5%
   empty recall; this was a misleading high score caused by over-abstention.
7. Abstention `score_floor_1_05`: score 3.35, 75 useful selected, 45 low
   selected, 54 useful runs, 37 low runs, 1.56 avg memories.
8. Abstention `score_floor_1_15`: score 3.44, 67 useful selected, 39 low
   selected, 52 useful runs, 32 low runs, 1.30 avg memories, but 12.8%
   missed-useful empty rate.

Lessons:

1. Increasing the candidate pool from 3 to 8 found more useful memories, but
   also increased low selections and context volume.
2. Top-two dynamic selection was the best early signal for preserving useful
   runs while lowering low selections.
3. Pure vector-only and strict abstention metrics can look good while failing
   the real goal by missing useful recall.
4. Project/context proxies can improve useful coverage but may increase memory
   count.

Decision:

1. Use top-two style dynamic selection as the main production candidate.
2. Keep project/context features as reranking signals, not hard gates.

## 2026-06-22: Recall 10x10

Source: `experiments/recall/2026-06-22-10x10/REPORT.md`

Experiment:

1. Expanded the initial 5x5 into ten selection strategies over ten
   deterministic cohorts.
2. Tested pure score/vector/context variants, task-key ordering, context gates,
   eval-health suppression/reranking, and an eval/context combo.

Key metrics:

1. `baseline_strict_kind`: score 2.85, 69 useful selected, 88 low selected, 54
   useful runs, 62 low runs, 2.28 avg memories.
2. `eval_health_suppress`: score 2.98, 69 useful selected, 69 low selected, 54
   useful runs, 52 low runs, 1.89 avg memories.
3. `eval_health_rerank`: score 3.03, 73 useful selected, 68 low selected, 55
   useful runs, 53 low runs, 1.98 avg memories.
4. `eval_context_combo`: score 3.38, 67 useful selected, 41 low selected, 49
   useful runs, 30 low runs, 1.90 avg memories.
5. `vector_top3`: score 2.56, 38 useful selected, 66 low selected, 33 useful
   runs, 52 low runs, 3.00 avg memories.
6. `context_score_rerank`: score 2.92, 59 useful selected, 64 low selected, 48
   useful runs, 51 low runs, 2.07 avg memories.

Lessons:

1. Eval history was a useful signal: `eval_health_rerank` improved score,
   useful selections, low selections, and average memory count relative to
   baseline.
2. Pure vector and pure context signals were useful diagnostics but poor
   defaults.
3. `eval_context_combo` was a high-precision abstaining strategy, but it reduced
   useful captures and increased missed-useful abstentions.

Decision:

1. Use eval-health reranking/suppression as production-oriented signals.
2. Avoid pure vector, pure context, or high-precision abstention as default
   policies.

## 2026-06-22: Recall 5x5

Source: `experiments/recall/2026-06-22-5x5/REPORT.md`

Experiment:

1. Initial five-strategy replay over 200 anchors.
2. Compared strict-kind baseline, context fit gate, eval-health suppression,
   eval-health rerank, and context-score rerank.

Metrics:

1. `baseline_strict_kind`: score 2.85, 69 useful selected, 88 low selected, 54
   useful runs, 62 low runs, 2.28 avg memories.
2. `context_fit_gate`: score 2.84, 65 useful selected, 82 low selected, 51
   useful runs, 61 low runs, 2.02 avg memories.
3. `eval_health_suppress`: score 2.98, 69 useful selected, 69 low selected, 54
   useful runs, 52 low runs, 1.89 avg memories.
4. `eval_health_rerank`: score 3.03, 73 useful selected, 68 low selected, 55
   useful runs, 53 low runs, 1.97 avg memories.
5. `context_score_rerank`: score 2.92, 59 useful selected, 64 low selected, 48
   useful runs, 51 low runs, 2.07 avg memories.

Lessons:

1. Eval-health signals were immediately better than context-fit hard gating.
2. Context metadata was too lossy to use as a hard gate.
3. Missed-useful abstention needed to be tracked separately before enabling
   abstaining policies.

Decision:

1. Continue with eval-informed ranking.
2. Treat context fit as secondary evidence, not a gate.

## 2026-06-19: Query Cleaning and Cluster Rerank

Source: `RECALL_IMPROVEMENT_REPORT.md`

Experiment:

1. Stripped noisy Codex/environment scaffolding from recall query text.
2. Added `yaaml eval memories`.
3. Built an offline eval-context cluster rerank backtest.

Cluster-rerank metrics on old strict-kind production:

1. Baseline: avg memories 2.28, avg known score 2.85, 69 useful known, 88 low
   known, 54 useful runs, 62 low runs, 5 empty recalls.
2. `cluster_boost`: avg score 2.87, 71 useful known, 88 low known, 56 useful
   runs, 63 low runs, 4 empty recalls.
3. `cluster_gate`: avg score 2.86, 71 useful known, 84 low known, 56 useful
   runs, 64 low runs, 7 empty recalls.
4. `cluster_count_weighted`: avg score 2.83, 71 useful known, 89 low known, 56
   useful runs, 65 low runs, 5 empty recalls.

Cleaned-query metrics:

1. Old strict-kind production: avg memories 2.28, avg known score 2.85, 69
   useful known, 88 low known, 54 useful runs, 62 low runs, 5 empty recalls.
2. Cleaned strict-kind: avg memories 2.28, avg known score 2.81, 65 useful
   known, 84 low known, 51 useful runs, 60 low runs, 5 empty recalls.
3. `cluster_margin_fallback` on cleaned outputs: avg memories 2.31, avg known
   score 2.84, 69 useful known, 82 low known, 53 useful runs, 61 low runs, 4
   empty recalls.

Lessons:

1. Cluster signal exists but was not strong enough to ship by default.
2. Query cleaning removed some bad recall but also removed useful accidental
   signal from YAAML-heavy sessions.
3. `cluster_margin_fallback` looked promising but still trailed the old
   baseline useful-run count.

Decision:

1. Do not enable cluster rerank by default.
2. Keep eval-context clustering as a future experiment once labels are denser.
3. Be cautious with query cleaning because environmental text can carry
   accidental but useful task signal.

## 2026-06-17: Initial Recall Strategy Backtest

Source: `RECALL_STRATEGY_REPORT.md`

Experiment:

1. Tested isolated strategy branches in git worktrees under
   `../yaaml-worktrees/*`.
2. Replayed `yaaml recall --session <id> --turn <ordinal> --json
   --debug-ranking` over 16 historical eval anchors.
3. Compared deterministic top-N, project/task filtering, and LLM selector
   variants.

Metrics:

1. `deterministic-top5`: 80 selected, 5.00 avg per anchor, 56 known selected,
   24 unknown, avg known score 2.68, 18 useful known, 37 low known, useful runs
   captured 12/14, 12 low runs selected.
2. `deterministic-top3`: 48 selected, 3.00 avg per anchor, 35 known selected,
   13 unknown, avg known score 2.92, 13 useful known, 21 low known, useful runs
   captured 11/14, 11 low runs selected.
3. `project-task-filter`: 78 selected, 4.88 avg per anchor, 53 known selected,
   25 unknown, avg known score 2.63, 17 useful known, 35 low known, useful runs
   captured 11/14, 12 low runs selected.
4. `LLM top1 historical run`: 16 selected, 1.00 avg per anchor, 13 known
   selected, 3 unknown, avg known score 3.00, 6 useful known, 7 low known,
   useful runs captured 6/14, 7 low runs selected.
5. `LLM top3 historical run`: 48 selected, 3.00 avg per anchor, 36 known
   selected, 12 unknown, avg known score 2.81, 13 useful known, 22 low known,
   useful runs captured 11/14, 12 low runs selected.

Lessons:

1. `deterministic-top3` was the best immediate tradeoff at that point: it cut
   context volume 40% versus top-five while preserving 11 of 14 useful-run
   opportunities.
2. LLM filtering was not reliable as implemented; binary filtering and
   score-reranking returned empty selections for every anchor in that pass.
3. LLM top-one over-pruned useful context.
4. Project/task filtering did not solve same-project stale memory problems.
5. Top-five captured slightly more useful recall but carried too much low-scoring
   material.

Decision:

1. Default to deterministic top-three at the time.
2. Keep improving deterministic ranking before relying on LLM filtering.
3. Investigate task-fit scoring, exact path/PR/branch/target matches, and stale
   same-project penalties.
