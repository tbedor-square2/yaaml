# YAAML Experiments Log

This log records durable lessons from YAAML recall and memory-quality
experiments. Keep generated run artifacts under `experiments/recall/<date>-...`
and summarize the decision-level result here so later experiments do not have
to rediscover the same tradeoffs. Methodology (sampling, significance,
holdout, and replay-vs-rerun rules) lives in `experiments/recall/README.md`.

## Experiment Backlog

This is the single backlog for recall/memory experiments. `ROADMAP.md` holds
product direction; anything experiment-sized (has a hypothesis and a metric)
belongs here, not there.

How to use this backlog:

1. **Adding an idea**: append it to the appropriate section below with a
   short hypothesis, the metric it should move, and how it would be measured
   (replay saved candidates vs. rerun retrieval vs. full corpus — see
   `experiments/recall/README.md`). Date the addition. Diagnostic or
   exploratory items are equally valid backlog entries: their "metric" may be
   the question they answer (for example "what fraction of misses are
   retrieval misses?"), with a report as the deliverable.
2. **Running an item**: follow the methodology in
   `experiments/recall/README.md`. Phase 0 gates *decisions*, not
   exploration: prototyping, diagnostics, and idea-scouting may run at any
   time, and diagnostics that do not depend on Phase 0 artifacts (for
   example formation-miss mining, which only reads transcripts) may complete
   early. What Phase 0 blocks is treating a technique result as a ship/no-ship
   decision input — those need the refreshed anchor library, CI tooling, a
   known pool ceiling, and a calibrated judge to be readable.
3. **Retiring an item**: follow the "When an item is addressed" policy below.

### When an Item Is Addressed

- **Phase 0 prerequisite completed**: keep it in the Phase 0 list, prefix it
  with "DONE YYYY-MM-DD", keep any residual notes inline, and add a decision
  record only if it produced experiment evidence (tooling work does not need
  one).
- **Backlog experiment or diagnostic completed**: delete the backlog entry
  and add a dated decision record (date, sources, experiment, metrics or the
  answered question, lessons, decision). The decision record is the durable
  artifact; the backlog entry is disposable.
- **Idea invalidated without a run**: delete the entry and add a one-line
  dated "Rejected without run" decision record stating the reason, so the
  idea is not re-proposed.
- **Forward/memory-write experiment completed**: same as a technique
  experiment, and the run directory must satisfy the forward-experiment
  artifact contract in `experiments/recall/README.md` (exposure window and
  cohort definitions in the manifest).

### Next Run

The next action is always the lowest Phase 0 item not marked DONE; once
Phase 0 is complete, it is the first technique-backlog entry. The Phase 0
list below is canonical — this section intentionally names no items so there
is only one place to update.

No technique result becomes a ship/no-ship decision until Phase 0 is
complete; exploration and Phase-0-independent diagnostics may run earlier
(see "Running an item" above).

### Phase 0: Validation Infrastructure (run first, in order)

These are prerequisites, not experiments that can "win." They exist because
the methodology in `experiments/recall/README.md` requires artifacts and
tooling that do not exist yet, and because several shipped decisions predate
the confidence-interval and holdout rules. Each item lists its done criteria;
an item is complete when its artifacts are committed.

Out-of-order completion is allowed when an item has no dependency on the
items before it (item 2, pure tooling, was completed before item 1 this
way); the next action is always the lowest incomplete item.

1. DONE 2026-07-06 — **Anchor library and holdout refresh** (added
   2026-07-02). Generated `2026-07` libraries from 300 current-corpus
   eval-library anchors selected newest-first, context-embedding-leakage
   filtered, limited to `session_background` and `tool_pre_use` origins, and
   requiring a completed turn at or before the anchor turn. Artifacts:
   `experiments/recall/anchor-libraries/2026-07-screening.tsv`,
   `experiments/recall/anchor-libraries/2026-07-holdout.tsv`, and
   `experiments/recall/anchor-libraries/2026-07-manifest.json`.
   *Result*: screening has 200 anchors, 90.5% known-score coverage, and 102
   oracle-useful anchors; holdout has 100 anchors, 91.0% known-score coverage,
   and 54 oracle-useful anchors; zero anchor overlap.
2. DONE 2026-07-06 — **Bootstrap CI tooling** (added 2026-07-06). Shared
   paired-bootstrap implementation in
   `scripts/recall_experiment_stats.py` (anchors paired on `run_id`,
   deterministic seed, 2000 resamples); `recall-5x5-worktree-metrics.py` now
   emits `delta_ci_95` and `delta_verdicts` in `summary.json` and a
   Significance section in `REPORT.md` labeling each delta `confirmed`,
   `no detectable effect`, or `needs larger sample`.
   *Residual*: the older standalone `recall-*-experiment.py` replay scripts
   still emit point deltas only; import the shared helper into any of them
   before using their output for a decision.
3. DONE 2026-07-06 — **Pool-recall ceiling oracle** (added 2026-07-02).
   Diagnostic report:
   `experiments/recall/2026-07-06-pool-recall-oracle/REPORT.md`.
   *Result*: 102 screening anchors had a known-useful memory; 18 had a
   known-useful memory in the 16-candidate pool (`pool_recall_rate` 17.6%),
   84 misses were due to retrieval, and 15 were due to selection.
   *Decision*: future recall-quality work should target candidate generation
   before another reranking/selection-only variant.
4. DONE 2026-07-06 — **Judge calibration set** (added 2026-07-02).
   Adjudicated 100 recall candidates with an independent LLM pass that saw
   only the query, memory, and rubric, not the production judge score or
   rationale. Artifacts:
   `experiments/recall/judge-calibration/2026-07-labels.jsonl`,
   `experiments/recall/judge-calibration/2026-07-labeling-guide.md`, and
   `experiments/recall/2026-07-judge-calibration/REPORT.md`.
   *Result*: exact agreement 34%, within-1 agreement 58%, useful binary
   agreement 58%, low binary agreement 58%, exact-score Cohen's kappa 0.135,
   useful-binary kappa 0.206, and low-binary kappa 0.206.
   *Decision*: the current production judge is not acceptable as the sole
   tuning target; use the adjudicator labels/report as the baseline for
   judge-prompt/model alignment before trusting ranking decisions.
5. DONE 2026-07-06 — **Re-validate shipped pre-CI decisions** (added
   2026-07-03). Report:
   `experiments/recall/2026-07-06-pre-ci-revalidation/REPORT.md`.
   *Decision*: tune `health_action_rerank` for over-abstention; keep
   recall-query cleaning. Requires item 4 before either result is used as a
   ship/no-ship input beyond this Phase 0 audit.
6. DONE 2026-07-07 — **Dense oracle labels** (added 2026-07-07). The
   2026-07-06/07 technique sprint landed "no detectable effect" on every
   shipping metric because only ~3-4 useful-known selections existed per arm.
   `scripts/densify-oracle-labels.py` adjudicator-scored the full saved
   candidate pools for screening + holdout (5,007 labels, 300 anchors, 299
   with at least one label) into
   `experiments/recall/oracle-labels/2026-07/dense-oracles`, consumed via
   `BACKTEST_ORACLE_DIR`. See the 2026-07-07 decision record and the "Dense
   oracle labels" section of `experiments/recall/README.md`.
   *Residual*: strategies that surface memories outside the saved pools need
   a label top-up run before their unknowns are readable.

### Technique Backlog (decisions blocked on Phase 0)

Ordered roughly by expected value. This list is a starting point, not a
boundary — materially different ideas that fit the intake rules are welcome
additions, and the 5x5 loop's "propose 5 materially different approaches"
step is expected to generate candidates not listed here.

1. **Lineage-aware retrieval for superseded useful memories** (added
   2026-07-07). Diagnostic first: 98 of 104 raw pool-oracle retrieval misses
   were oracle-useful memories that are now inactive. Question: do their
   consolidated/refined successors carry the useful content, and do those
   successors appear in (and get selected from) the pool? Walk
   `superseded_by_memory_id`/lineage refs from the inactive-useful set and
   report successor pool/selection rates. If successors are missing or
   unranked, follow with a technique experiment: map inactive vector hits to
   their active successor at retrieval time, or revisit consolidation
   aggressiveness.

## 2026-07-29: Automatic Background Recall Retired

Evidence:

1. In the trailing seven-day production slice, `session_background` had 32
   useful runs out of 98 evaluated runs (32.7%); 32 of 111 judged memories
   were useful (28.8%), 76 were low (68.5%), and the average score was 2.43.
2. The preceding seven-day slice was also weak and better than the current
   one: 43 useful runs out of 102 (42.2%); 46 of 118 judged memories were
   useful (39.0%), 69 were low (58.5%), and the average score was 2.69.
3. Explicit `manual_query` recall was materially cleaner in the trailing
   slice: 14 useful runs out of 16 evaluated, with 14 of 16 judged memories
   useful (87.5%) and an average score of 4.25.
4. The source-overlap adjudicator gate did not provide convincing forward
   confirmation: among eight isolated restored runs, four were useful and
   four were low. This operational slice is small and unpaired, so it is not
   treated as a statistically confirmed technique result.
5. Activation-condition rows were associated with worse background outcomes
   in the same observational slice (10 of 53 judged memories useful with
   conditions versus 22 of 58 without). That comparison is unpaired and
   confounded, so it diagnoses rather than estimates a causal effect.

Decision:

1. Remove automatic recall generation from completed-turn processing,
   including its queued-task kind, dispatcher, ranking path, file writes, and
   eval scheduling.
2. Reject bare `yaaml recall` and remove the manual
   `--origin session-background` compatibility surface. Keep only focused
   explicit queries and historical replay.
3. Retire the runtime source-overlap forward rollout and formation-time
   activation-condition forward experiment as automatic-recall shipping
   items. Their existing artifacts and historical metrics remain for
   analysis; shared ranking metadata may still be evaluated for explicit
   recall.
4. This is a product stop-loss based on sustained poor absolute background
   recall quality, not a claim that the unpaired week-over-week or metadata
   slices establish a causal treatment effect.

## 2026-07-07: Adjudicator-Gated Source-Overlap Restoration

Sources:

1. `scripts/source-overlap-gate-experiment.py`
2. `experiments/recall/2026-07-07-source-overlap-gate/REPORT.md`
3. `experiments/recall/2026-07-07-source-overlap-gate-holdout/REPORT.md`
4. `experiments/recall/2026-07-07-source-overlap-redundancy-check/cases.jsonl`

Experiment:

1. Calibration first: token containment of memory body in query text does
   not separate incremental from redundant restored selections (means 0.444
   vs 0.481 on the 82 labeled cases; at containment 0.7 the gate keeps
   30/31 incremental but still admits 45/51 redundant). The lexical-gate
   hypothesis was rejected without a full run.
2. Simulated the instrument that does discriminate: restore a
   source-overlap-dropped candidate only when its cached redundancy-aware
   adjudicator score is >= 4; unscored candidates stay dropped. Replayed
   selection on screening and holdout, reporting deltas under both the raw
   dense oracle and the redundancy-adjusted oracle.

Metrics:

1. Screening: +14 useful capture runs (95% CI +7 to +22, confirmed under
   both oracles), 0 added low selections (exactly zero under both oracles),
   missed-useful empties -14 (CI -22 to -7), +0.08 average memories.
2. Holdout: +9 useful capture runs (CI +4 to +15), 0 added low selections,
   missed-useful empties -9 (CI -15 to -4).
3. Contrast under the adjusted oracle: blanket restoration drops to average
   score 3.19 with 70 low selections, while the gated arm holds 3.75 with
   35 — the gate harvests the incremental half and excludes the redundant
   half.

Lessons:

1. Lexical containment is not a viable redundancy signal; redundancy here
   is semantic (the query context already covers the memory's insight).
2. The raw dense oracle and the redundancy-adjusted oracle agree on the
   gated arm's deltas, which mitigates (but does not eliminate) the
   shared-instrument concern — the gate uses the redundancy-aware prompt
   while the raw dense oracle uses the plain calibrated adjudicator.
3. The gate is cheap at runtime because recall is async: only candidates
   dropped solely for source overlap need scoring, ~1-3 per recall.

Decision:

1. Reject the lexical-containment gate without a full experiment.
2. Promote the adjudicator gate to a runtime implementation item behind an
   env flag, with forward production evals as the independent confirmation
   before default-on.

## 2026-07-07: Selection Tuning on Dense Labels

Sources:

1. `scripts/selection-tuning-experiment.py`
2. `experiments/recall/2026-07-07-selection-tuning/REPORT.md`
3. `experiments/recall/2026-07-07-selection-tuning-holdout/REPORT.md`
4. `experiments/recall/2026-07-07-source-overlap-redundancy-check/REPORT.md`

Experiment:

1. Replayed saved production candidate rankings through parameterized
   strict-kind selection variants (abstention threshold 0.75 -> 0.55/0.40,
   second-slot threshold 0.90 -> 0.75, negative health-rerank deltas scaled
   by 0.5, combinations), a source-overlap arm, and a cached-LLM-score arm,
   against the dense oracle. Default-parameter replay reproduced production
   selections on 100% of anchors after excluding candidates with any
   `drop:` filter reason.
2. Confirmed the winning arm on the holdout, then re-scored its restored
   selections with a redundancy-aware adjudicator variant.

Metrics:

1. Every threshold/health arm was byte-identical to the default replay:
   zero delta on all metrics. The strict-kind thresholds and health-rerank
   deltas never bind on these pools; abstention comes from the hard filter,
   not from score thresholds.
2. `allow_source_overlap` (ignore `drop:source_turn_already_in_query`):
   screening +40 useful capture runs (CI +29 to +52), missed-useful empties
   -47 (CI -60 to -35), low selections +8 (CI +3 to +14), +0.26 avg
   memories. Holdout confirmed: +21 useful capture runs (CI +13 to +29),
   missed-useful empties -24 (CI -33 to -16), low selections +3 (CI 0 to
   +7, not confirmed).
3. Redundancy check on the 82 restored selections: 70 were dense-useful but
   only 31 (44.3%) stayed useful under an incremental-value instruction; 39
   were demoted as already visible in the query.
4. `llm_t2` (cached LLM scores, threshold 2) was slightly worse than
   production on the anchor-refresh pools; retired.

Lessons:

1. The over-abstention diagnosis was right but the mechanism was wrong:
   production's missed-useful empties come almost entirely from the
   source-overlap hard drop, not from selection thresholds or health-rerank
   penalties. Threshold tuning is a dead end on current pools.
2. The dense oracle is redundancy-blind: it credits memories whose content
   is already visible in the query. Any experiment touching
   source-overlap/dedup behavior must re-score differing selections with a
   redundancy-aware adjudicator before trusting dense-label deltas.
3. About half the suppressed-then-restored memories are genuinely
   incremental — a real, holdout-confirmed win pool behind a smarter gate.

Decision:

1. Do not ship blanket removal of the source-overlap drop; roughly half of
   what it restores is redundant context.
2. Retire threshold/health-scale tuning and the cached-LLM-score arm as no
   detectable effect / worse.
3. Queue the incremental-value source-overlap gate as the top technique
   item: content-overlap-conditional dropping, evaluated with
   redundancy-aware re-scoring.

## 2026-07-07: Dense Oracle Labels

Sources:

1. `scripts/densify-oracle-labels.py`
2. `experiments/recall/oracle-labels/2026-07/REPORT.md`
3. `experiments/recall/oracle-labels/2026-07/labels.jsonl`
4. `experiments/recall/oracle-labels/2026-07/dense-oracles/`

Question:

1. Can adjudicator-scored candidate pools replace the sparse, weakly
   calibrated historical production-judge oracle and make useful-capture
   deltas readable?

Metrics:

1. 5,007 labels across 300 anchors (200 screening + 100 holdout), top-16
   saved pools plus production-selected and historical-oracle memory ids;
   0 candidates missing from the memory database.
2. 299 of 300 anchors have at least one label; 281 have a useful (>=4)
   label. Score histogram: 1,083 ones, 2,084 twos, 32 threes, 1,018 fours,
   790 fives.
3. Production baseline recomputed under the dense oracle (saved selections,
   no reruns): 106 selected memories, all 106 labeled (100% known-selected
   coverage vs single digits before), 70 useful known selected, 35 low known
   selected, 65 useful capture runs, 101 empty recalls, 93 missed-useful
   empties out of 189 dense-oracle-useful anchors.

Lessons:

1. The power problem is solved: useful-known selections per arm went from
   ~3-4 to ~70, so paired CIs on useful capture become readable.
2. Production's dominant failure under the dense oracle is over-abstention
   (93 of 101 empty recalls had a useful memory available), consistent with
   the pre-CI revalidation finding on `health_action_rerank`.
3. The adjudicator is decisive (almost no 3s), which sharpens useful/low
   binary metrics but means borderline cases resolve to one side; the
   labeling instrument is single-sourced from `judge-calibration.py` so it
   cannot drift from the calibration study.

Decision:

1. Use `BACKTEST_ORACLE_DIR=experiments/recall/oracle-labels/2026-07/dense-oracles`
   for all experiments on the 2026-07 libraries; never mix dense and
   historical labels in one comparison.
2. Run the selection/health-rerank tuning backlog item next against the
   dense oracle.

## 2026-07-07: Activation Conditions Forward Readiness

Sources:

1. `scripts/activation-condition-forward-report.py`
2. `experiments/recall/2026-07-07-activation-conditions-forward-readiness/manifest.json`
3. `experiments/recall/2026-07-07-activation-conditions-forward-readiness/cohorts.jsonl`
4. `experiments/recall/2026-07-07-activation-conditions-forward-readiness/summary.json`
5. `experiments/recall/2026-07-07-activation-conditions-forward-readiness/REPORT.md`

Question:

1. Do newly written memories have activation-condition metadata, and is there
   enough downstream recall-eval exposure to evaluate the policy?

Metrics:

1. Activation table present after normal CLI migration: yes.
2. Memories scanned: 3,518.
3. Active memories scanned: 994.
4. Memories with activation-condition rows in the artifact snapshot: 119.
5. Judged downstream evals for activation-condition memories: 3 (2 useful,
   1 low).
6. Pending recall-eval tasks selecting activation-condition memories: 0.
7. Readiness threshold: at least 20 condition memories and 20 judged
   downstream evals.

Lessons:

1. The schema, measurement path, and formulation exposure are working: the
   first new-policy cohort has at least 119 memories with
   activation-condition rows in the artifact snapshot.
2. Early downstream evidence is mixed-positive: 2 useful scores and 1 low
   score, but 3 judged evals remains far below the 20-judged-eval readiness
   threshold.
3. The forward experiment cannot be retired from memory creation alone; it
   still needs enough later recall selections and judged eval outcomes for
   those memories.

Decision:

1. Keep the formation-time activation conditions backlog item open.
2. Re-run the forward-readiness reporter after the daemon has written
   activation-condition memories and recall evals have accumulated.

## 2026-07-07: Session-Level Context Dedup

Sources:

1. `target/recall-backtests/wide-multi-query-candidate-generation-v2/wide-multi-query-candidate-generation-v2.details.jsonl`
2. `target/recall-backtests/wide-multi-query-candidate-generation-v2/wide-multi-query-candidate-generation-v2.recall-*.json`
3. `scripts/session-context-dedup-experiment.py`
4. `experiments/recall/2026-07-07-session-context-dedup/REPORT.md`

Experiment:

1. Replayed saved wide-v2 selections and dropped a selected memory when it
   was already selected earlier in the same session or when the memory text
   had strong visible overlap with the current recall query.
2. Compared the drop-only session-context policy against the wide-v2
   baseline on the frozen 200-anchor screening set with paired bootstrap CIs.

Metrics:

1. Selected memories: 122 -> 36; average selected per anchor -0.43, 95% CI
   -0.505 to -0.365, confirmed.
2. Empty recall runs: 89 -> 166; delta +77, CI +64 to +90, confirmed.
3. Missed-useful empty runs: 42 -> 81; delta +39, CI +29 to +50,
   confirmed.
4. Useful known selected: 3 -> 2; delta -1, CI -3 to 0, no detectable
   effect.
5. Low known selected remained 0.
6. Dropped selected memories: 86 total; 44 previously recalled in the same
   session and 42 visible in the current query.

Lessons:

1. A hard drop-only context-dedup gate is far too aggressive on the current
   screening set. It saves context but converts many potentially useful
   recalls into empty recalls.
2. Prior same-session recall is not enough to prove redundancy; repeated
   recall of the same memory can still be useful later in a long task.
3. The drop cases are useful diagnostics for a softer context feature, but
   not for a production hard gate.

Decision:

1. Retire the session-level context dedup backlog item as completed.
2. Do not ship the drop-only dedup policy.
3. If revisited, use context visibility as a rank feature or LLM evidence
   feature, not an unconditional suppression rule.

## 2026-07-07: Formation-Miss Mining

Sources:

1. `scripts/formation-miss-mining.py`
2. `experiments/recall/2026-07-07-formation-miss-mining/REPORT.md`
3. `experiments/recall/2026-07-07-formation-miss-mining/cases.jsonl`
4. `/Users/tbedor/.yaaml/yaaml.db`

Question:

1. Where do repeated user corrections appear across sessions without an
   active memory that appears to cover the correction?

Metrics:

1. Sessions scanned: 381.
2. Transcript files read: 380.
3. Correction-like user turns: 175.
4. Repeated correction clusters: 11.
5. Clusters with apparent active-memory coverage: 7.
6. Missed-formation clusters: 4.
7. Missed correction turns: 36.

Lessons:

1. Transcript-only mining can identify repeated correction clusters without a
   forward cohort, but it remains heuristic: duplicate agent runs can amplify
   a single user correction across sessions.
2. The highest-signal missed clusters are concentrated in Java/riskarbiter
   review threads, especially PR-comment follow-up, rollout flag defaults,
   and legacy partition initialization questions.
3. Broad correction phrases such as "actually" and "do not edit files" are
   too noisy; the diagnostic uses stricter correction patterns and requires
   repeated-session evidence before counting a cluster.

Decision:

1. Retire the formation-miss mining backlog item as completed.
2. Use the 4 missed clusters as seed examples for the formation-time
   activation-conditions experiment, but do not treat this diagnostic as a
   ship/no-ship evaluation of the write policy.

## 2026-07-07: Segment-Aware Candidate Generation Reconciliation

Sources:

1. `experiments/recall/2026-07-06-hybrid-candidate-generation/REPORT.md`
2. `experiments/recall/2026-07-06-wide-multi-query-candidate-generation/REPORT.md`
3. `experiments/recall/2026-07-06-wide-multi-query-candidate-generation-v2-pool-oracle/REPORT.md`

Question:

1. Is there a pending segment-aware candidate-generation experiment distinct
   from the completed hybrid and wide multi-query runs?

Decision:

1. No separate backlog item is needed. Segment-aware candidate generation was
   already exercised by hybrid retrieval over active-segment summary/task-key
   embeddings and by wide multi-query retrieval over active segment summary
   plus identity-key text.
2. Do not duplicate that retrieval experiment. The latest wide-v2 pool oracle
   showed active-only pool recall improved to 22/28 (78.6%) but useful
   selection stayed at 3/28, so the next segment-aware work should be a
   selection/context-dedup or formation-metadata experiment rather than
   another broad candidate-generation rerun.

## 2026-07-06: Pool-Recall Ceiling Oracle

Sources:

1. `experiments/recall/anchor-libraries/2026-07-screening.tsv`
2. `target/anchor-refresh-2026-07/anchor-refresh.recall-*.json`
3. `target/anchor-refresh-2026-07/oracle-*.json`
4. `experiments/recall/2026-07-06-pool-recall-oracle/REPORT.md`

Question:

1. For screening anchors with a known-useful oracle memory, does that useful
   memory appear in the 16-candidate production ranking pool at all?

Metrics:

1. Screening anchors: 200.
2. Oracle-useful anchors: 102.
3. Known-useful in pool: 18 (`pool_recall_rate` 17.6%).
4. Known-useful selected: 3 (2.9% of oracle-useful anchors).
5. Misses due to retrieval: 84.
6. Misses due to selection: 15.

Lessons:

1. Candidate generation is the binding ceiling on the refreshed screening
   library. Most known-useful memories are absent from the pool, so reranking
   cannot recover them.
2. Selection still leaves some known-useful memories unselected, but it is the
   smaller failure mode.

Decision:

1. Prioritize candidate-generation work next, especially hybrid retrieval
   over active-segment/query embeddings plus identity-key lexical inclusion.
2. Do not spend another default experiment on reranking-only variants until
   the pool ceiling improves.

## 2026-07-06: LLM Judge Calibration

Sources:

1. `experiments/recall/judge-calibration/2026-07-labels.jsonl`
2. `experiments/recall/judge-calibration/2026-07-labeling-guide.md`
3. `experiments/recall/2026-07-judge-calibration/REPORT.md`

Experiment:

1. Sampled 100 production-judge-scored recall candidates from the refreshed
   screening artifacts.
2. Labeled each case with an independent Anthropic
   `claude-haiku-4-5-20251001` adjudicator prompt that saw only the recall
   query, stored memory, and 1-5 rubric, not the production judge score or
   rationale.
3. Compared production `judge_score` against `adjudicator_score`.

Metrics:

1. Labeled rows: 100; missing labels: 0.
2. Exact agreement: 34%.
3. Within-1 agreement: 58%.
4. Useful binary agreement: 58%; low binary agreement: 58%.
5. Cohen's kappa: 0.135 exact 1-5, 0.206 useful binary, 0.206 low binary.

Lessons:

1. The current production judge has weak agreement with the LLM adjudicator.
2. This is judge-vs-adjudicator validation for tuning discipline, not human
   ground truth.

Decision:

1. Phase 0 calibration infrastructure is complete.
2. Do not treat the current production judge as an acceptable sole tuning
   target. Align the judge prompt/model against the adjudicator-labeled set
   before trusting new ranking ship/no-ship decisions.

## 2026-07-06: Judge Prompt Alignment

Sources:

1. `experiments/recall/judge-calibration/2026-07-labels.jsonl`
2. `experiments/recall/2026-07-06-judge-prompt-alignment/details.jsonl`
3. `experiments/recall/2026-07-06-judge-prompt-alignment/REPORT.md`
4. `crates/yaaml/src/main.rs`

Experiment:

1. Replayed three candidate prompt shapes over the same 100-row
   adjudicator-labeled calibration set: the current offline eval prompt
   shape, a query-plus-memory rubric prompt, and a stricter recall-decision
   prompt.
2. Compared each candidate score against `adjudicator_score` using exact,
   within-1, useful/low binary agreement, Cohen's kappa, MAE, and bias.

Metrics:

1. `query_memory_rubric`: exact 82%, within-1 96%, useful binary 95%, low
   binary 96%, useful-binary kappa 0.865, MAE 0.260, bias +0.020.
2. `strict_recall_decision`: exact 60%, within-1 92%, useful binary 92%, low
   binary 92%, useful-binary kappa 0.769, MAE 0.510, bias -0.390.
3. `production_eval_candidate`: exact 39%, within-1 63%, useful binary 61%,
   low binary 62%, useful-binary kappa 0.164, MAE 1.180, bias +0.620.
4. Existing stored production scores remained weakest: exact 34%, within-1
   58%, useful/low binary 58%, MAE 1.420, bias +0.940.

Lessons:

1. The old offline eval prompt was asking an after-the-fact "helped after
   recall" question even when the calibration target is pre-injection
   query-plus-memory usefulness.
2. Aligning the prompt to the calibration question removes most of the
   positive-score bias and substantially improves agreement with the
   adjudicator labels.

Decision:

1. Adopt the `query_memory_rubric` prompt for offline eval candidate judging.
2. Keep the empty-recall abstention judge on the old after-the-fact prompt,
   because abstention scoring still depends on subsequent conversation.

Addendum (2026-07-07): the daemon's online per-memory recall evals also moved
to the aligned instrument, single-sourced as
`candidate_judge_system_prompt`/`candidate_judge_prompt` in
`crates/yaaml/src/llm_judge.rs` and shared with the offline path so the
prompts cannot drift. The online judge scores each recalled memory against
the anchor turn text pre-injection-style; abstention judging (and per-memory
judging when the anchor turn has no stored display text) stays on the
after-the-fact prompt. Consequence: online per-memory scores before and after
this date come from different instruments — do not trend them across the
boundary.

## 2026-07-06: Hybrid Candidate Generation

Sources:

1. `experiments/recall/anchor-libraries/2026-07-screening.tsv`
2. `target/anchor-refresh-2026-07/anchor-refresh.details.jsonl`
3. `target/recall-backtests/hybrid-candidate-generation-v2/hybrid-candidate-generation-v2.details.jsonl`
4. `experiments/recall/2026-07-06-hybrid-candidate-generation/REPORT.md`
5. `experiments/recall/2026-07-06-hybrid-candidate-generation-v2-pool-oracle/REPORT.md`

Experiment:

1. Added an env-gated hybrid candidate pool behind
   `YAAML_EXPERIMENT_HYBRID_RECALL=1`.
2. The pool unions primary active-segment vector hits, active-segment
   summary/task-key vector hits, and exact identity-key memory hits. The v2
   rerun also extracts identity keys from memory titles and bodies before
   matching.
3. Reran retrieval on the frozen 200-anchor screening library and replayed
   existing production selection.

Metrics:

1. Useful known selected: +1, 95% CI -2 to +4, no detectable effect.
2. Useful capture runs: +1, CI -2 to +4, no detectable effect.
3. Missed-useful empty runs: -9, CI -16 to -2, confirmed.
4. Empty recall runs: -28, CI -39 to -18, confirmed.
5. Average selected per anchor: +0.22, CI +0.15 to +0.295, confirmed.
6. Low known selected remained 0.
7. Pool recall improved only from 18/102 (17.6%) to 21/102 (20.6%);
   retrieval remained the larger miss class with 81 retrieval misses and 17
   selection misses.

Lessons:

1. Hybrid identity-key inclusion and segment-summary embedding mostly reduced
   abstention by selecting unjudged memories; it did not measurably increase
   judged useful capture.
2. Candidate generation is still the ceiling, but this specific hybrid shape
   is too weak. The next candidate-generation run should widen semantic
   retrieval itself rather than only unioning identity-key and summary
   channels at the same 16-candidate pool size.

Decision:

1. Do not ship this hybrid variant.
2. Keep the implementation behind `YAAML_EXPERIMENT_HYBRID_RECALL` as
   experiment scaffolding, not production behavior.
3. Replace the completed backlog item with a wide multi-query candidate
   generation follow-up.

## 2026-07-06: Wide Multi-Query Candidate Generation

Sources:

1. `experiments/recall/anchor-libraries/2026-07-screening.tsv`
2. `experiments/recall/2026-07-06-wide-multi-query-candidate-generation/REPORT.md`
3. `target/recall-backtests/wide-multi-query-candidate-generation-v2/wide-multi-query-candidate-generation-v2.details.jsonl`
4. `experiments/recall/2026-07-06-wide-multi-query-candidate-generation-v2-pool-oracle/REPORT.md`

Experiment:

1. Added an env-gated wide retrieval experiment behind
   `YAAML_EXPERIMENT_WIDE_RECALL=1`.
2. The variant searches pool 64 at similarity threshold 0.20 over the current
   recall query, active segment summary, and active identity-key text. The v2
   rerun also widens the selector/debug pool to 64 while leaving the final
   selection limit unchanged.
3. Reran retrieval on the frozen 200-anchor screening library and replayed the
   production selector.

Metrics:

1. Useful known selected: 0 delta, 95% CI 0 to 0, no detectable effect.
2. Useful capture runs: 0 delta, CI 0 to 0, no detectable effect.
3. Missed-useful empty runs: -3, CI -8 to +2, needs larger sample.
4. Empty recall runs: -12, CI -20 to -4, confirmed.
5. Average selected per anchor: +0.08, CI +0.035 to +0.125, confirmed.
6. Low known selected remained 0.
7. Raw pool recall improved from 18/102 (17.6%) to 22/102 (21.6%). After
   excluding currently inactive memories, production active-only pool recall
   was 18/28 (64.3%) and wide v2 active-only pool recall was 22/28 (78.6%).
   Wide v2 still selected only 3/28 active useful anchors, leaving 6 active
   retrieval misses and 19 active selection misses.

Lessons:

1. Naive wider semantic retrieval reduces abstention but does not improve
   judged useful capture on the screening library.
2. The wider selector pool improved active-only pool recall, but selected
   useful coverage did not move; the added candidates are not being promoted
   by the current selection policy.
3. Active-only analysis is required before deciding whether retrieval or
   selection is binding, because the raw oracle labels include inactive
   memories that production recall intentionally excludes.

Decision:

1. Do not ship this wide multi-query variant.
2. Keep the implementation behind `YAAML_EXPERIMENT_WIDE_RECALL` as experiment
   scaffolding, not production behavior.
3. Use active-only pool metrics for the next decision. The active-only result
   points back to selection/ranking calibration rather than another broad
   candidate-generation heuristic.

## 2026-07-06: Retrieval-Miss Diagnosis

Sources:

1. `experiments/recall/2026-07-06-wide-multi-query-candidate-generation-v2-pool-oracle/cases.jsonl`
2. `target/recall-backtests/wide-multi-query-candidate-generation-v2/wide-multi-query-candidate-generation-v2.recall-*.json`
3. `experiments/recall/2026-07-06-retrieval-miss-diagnosis/REPORT.md`
4. `scripts/diagnose-retrieval-misses.py`
5. `scripts/pool-recall-oracle.py`

Experiment:

1. Joined raw wide-v2 pool-oracle retrieval misses against the current YAAML
   memory database.
2. Classified useful memory instances by current active status, project match,
   task-key overlap, lexical overlap, and query-noise features.
3. Updated `pool-recall-oracle.py` to emit active-only pool metrics so future
   reports distinguish true current retrieval misses from inactive oracle
   labels.

Metrics:

1. Raw wide-v2 retrieval misses: 80 anchors, 104 useful-memory instances.
2. Cause counts across raw retrieval-miss instances: 98 `memory_inactive`, 5
   `missing_identity_key_in_query`, 1 `query_noise_dominates`.
3. Active oracle-useful anchors: 28.
4. Active wide-v2 known-useful in pool: 22/28 (78.6%).
5. Active wide-v2 known-useful selected: 3/28 (10.7%).
6. Active remaining misses: 6 due to retrieval, 19 due to selection.

Lessons:

1. The raw pool-recall ceiling overstated retrieval failure because most raw
   misses refer to memories that are now inactive.
2. For currently active useful memories, selection/ranking is the larger
   observed gap.
3. Active-only pool metrics should be reported for future recall experiments
   whenever the oracle comes from historical eval labels.

Decision:

1. Mark retrieval-miss diagnosis complete.
2. Prioritize learned weight calibration next; the active-only diagnosis says
   ranking/selection is the current binding gap.

## 2026-07-06: Learned Weight Calibration

Sources:

1. `target/recall-backtests/wide-multi-query-candidate-generation-v2/`
2. `target/recall-training-datasets/wide-v2-candidates.jsonl`
3. `experiments/recall/2026-07-06-learned-weight-calibration/REPORT.md`
4. `scripts/export-recall-training-data.py`
5. `scripts/recall-feature-model-experiment.py`

Experiment:

1. Upgraded `recall-feature-model-experiment.py` from point-delta output to a
   decision-grade saved-candidate replay: logistic feature scoring, isotonic
   calibration on training-fold predictions, threshold tuning, held-out
   per-anchor details, paired bootstrap CIs, and `REPORT.md` output.
2. Exported candidate-level rows from the wide-v2 saved recall artifacts and
   replayed learned selection against the saved production selections.
3. Limited model selection to the production-eligible filter pool rather than
   all debug-ranking rows.

Metrics:

1. Dataset: 12,480 candidate rows from 195 replayable anchors.
2. Labeled rows: 46 total, with 24 useful and 22 low labels.
3. Useful known selected: +1, 95% CI 0 to +3, no detectable effect.
4. Useful capture runs: +1, CI 0 to +3, no detectable effect.
5. Low known selected remained 0.
6. Average selected per anchor: +1.374, CI +1.292 to +1.456, confirmed.
7. Empty recall runs: -84, CI -98 to -71, confirmed; missed-useful empty runs:
   -11, CI -18 to -5, confirmed.

Lessons:

1. The current labeled candidate set is too sparse for a stable local learned
   selector. Isotonic calibration collapsed into a permissive boundary and
   the tuned model selected two memories for every replayable anchor.
2. The apparent abstention improvement is mostly a context-volume tradeoff,
   not a confirmed useful-capture gain.
3. A local feature model needs either denser labels or a stronger externally
   judged scoring signal before it should replace deterministic selection.

Decision:

1. Do not ship the learned local selector.
2. Mark learned weight calibration addressed for the current label set.
3. Prioritize the LLM scoring rerank backlog item next, because it can score
   unlabeled active pool candidates directly instead of relying on sparse
   historical candidate labels.

## 2026-07-07: LLM Scoring Rerank

Sources:

1. `target/recall-backtests/wide-multi-query-candidate-generation-v2/`
2. `experiments/recall/2026-07-07-llm-scoring-rerank/REPORT.md`
3. `experiments/recall/2026-07-07-llm-scoring-rerank/llm_scores.jsonl`
4. `scripts/llm-rerank-recall-candidates.py`

Experiment:

1. Added a saved-candidate LLM scoring replay harness. It hydrates top
   production-eligible candidates from the current YAAML memory database,
   scores them with the aligned query-plus-memory rubric prompt, caches
   scores, tunes a threshold on training folds, and compares held-out
   selections with paired bootstrap CIs.
2. Scored the top 8 production-eligible candidates per screening anchor from
   the wide-v2 saved recall artifacts using `claude-haiku-4-5-20251001`.
3. Replayed selection with final selection limit 2 and fold-tuned score
   thresholds.

Metrics:

1. Scored candidates: 1,544 across 200 anchors.
2. Fold-tuned thresholds: 5, 5, 5, 5, 5.
3. Useful known selected: -1, 95% CI -6 to +3, needs larger sample.
4. Useful capture runs: -1, CI -6 to +3, needs larger sample.
5. Low known selected: +1, CI 0 to +3, no detectable effect.
6. Empty recall runs: +24, CI +5 to +42, confirmed.
7. Missed-useful empty runs: +16, CI +2 to +31, confirmed.
8. Fixed-threshold diagnostics also had no hidden win: thresholds 1-4 kept
   useful capture flat while selecting far more context and one known-low
   memory; threshold 5 over-abstained.

Lessons:

1. The aligned single-candidate LLM scorer does not solve selection in this
   saved-candidate setup. It either behaves too permissively at low thresholds
   or over-abstains when tuned against the sparse labels.
2. More scoring intelligence over the existing pool is not enough with the
   current memory payloads and labels; formation-time metadata and activation
   semantics are likely a better next lever.
3. Cached LLM score artifacts are reusable for future scoring diagnostics, but
   should not be treated as a production policy.

Decision:

1. Do not ship LLM scoring rerank.
2. Mark the LLM scoring rerank backlog item addressed.
3. Prioritize formation-time activation conditions next.

## 2026-07-06: Pre-CI Decision Revalidation

Sources:

1. `experiments/recall/anchor-libraries/2026-07-screening.tsv`
2. `target/anchor-refresh-2026-07/anchor-refresh.details.jsonl`
3. `target/revalidate-no-query-cleaning/no-query-cleaning.details.jsonl`
4. `experiments/recall/2026-07-06-pre-ci-revalidation/REPORT.md`

Experiment:

1. Revalidated `health_action_rerank` against a saved-candidate no-health
   proxy on the refreshed screening library. The proxy reverses encoded
   health deltas from saved debug rankings; noisy-metadata task-key
   restoration is not reconstructable from saved JSON, so this is conservative
   rather than exact.
2. Revalidated recall-query cleaning by rerunning retrieval with
   `YAAML_DISABLE_RECALL_QUERY_CLEANING=1` over the same screening anchors and
   comparing current production cleaning against that no-cleaning baseline.

Metrics:

1. `health_action_rerank` vs no-health proxy: average known score +2.42
   (95% CI +0.67 to +4.00, confirmed), average selected per anchor -0.36
   (CI -0.44 to -0.27, confirmed), clean abstentions +32 (CI +22 to +42,
   confirmed), missed-useful empty runs +34 (CI +23 to +45, confirmed).
   Useful selected +2 and useful capture runs +2 were not confirmed wins.
2. Query cleaning vs no-cleaning: useful selected -1 (CI -3 to 0, no
   detectable effect), useful capture runs -1 (CI -3 to 0, no detectable
   effect), average selected per anchor unchanged, low selections unchanged,
   missed-useful empty runs +2 (CI 0 to +5, needs larger sample).

Lessons:

1. `health_action_rerank` improves precision and context volume but is now
   clearly over-abstaining on the refreshed screening library.
2. Recall-query cleaning no longer shows a confirmed useful-recall regression
   on this library, but its effects are small and should remain monitored.

Decision:

1. Tune `health_action_rerank`, not a clean keep: preserve its precision and
   context-volume gains while reducing missed-useful abstention.
2. Keep recall-query cleaning. The refreshed replay does not confirm the
   historical small useful-recall regression.

## 2026-07-01: Persisted Segment Labels Initial Backtest

Sources:

1. `target/recall-backtests/segment-labels-before-41/segment-labels-before-41.summary.json`
2. `target/recall-backtests/segment-labels-after-81/segment-labels-after-81.summary.json`
3. `target/recall-backtests/segment-labels-anchors/anchors-41.tsv`

Experiment:

1. Added explicit persisted segment labels through `segment_labels` and
   `conversation_segment_labels`.
2. Added label status on `conversation_segments` so oracle abstention
   (`labels: []`) is persisted and not requeued forever.
3. Materialized labels into segment and memory task keys as `label:<normalized>`
   weak recall evidence, not hard task identity.
4. Changed segment-label backfill queueing to newest-first so bounded batches
   cover current recall/eval work before old backlog.
5. Ran a bounded label backfill batch. It was stopped after 81 stable segments
   were labeled, leaving 60 distinct labels and 185 segment-label links.

Metrics:

1. Baseline over 41 fixed eval-library anchors: 20 selected memories, 0.49
   selected per anchor, 5 known selected, 15 unknown selected, average known
   score 4.6, 5 useful known, 0 low known, 5 useful-capture runs, 23 empty
   recalls.
2. After 81 labeled stable segments: 21 selected memories, 0.51 selected per
   anchor, 5 known selected, 16 unknown selected, average known score 4.6, 5
   useful known, 0 low known, 5 useful-capture runs, 22 empty recalls.
3. Only one anchor changed: run 3298 moved from empty recall to memory 3255.
   That memory was unknown to the historical oracle rows, while the anchor did
   have a known useful memory available.
4. For run 3298, labels added weak key overlap
   `label:square-console-recently-visited-resource-table-ui` and
   `label:pr-review-and-screenshot-management`, raising the selected memory's
   task-key bonus from 0.12 to 0.24 and allowing it to survive strict-kind
   selection.

Lessons:

1. The label plumbing works end-to-end: persisted labels are visible in segment
   listing, materialized into segment/memory task keys, and can change recall
   selection.
2. A small recent-only backfill is not enough to prove a recall-quality lift.
   Most anchors were unchanged because their relevant query/source segments
   were still unlabeled.
3. The initial effect was precision-neutral on known scores: no new known low
   selections, but also no additional known useful capture.
4. Label generation quality needs monitoring. Some labels are useful
   workstream/task labels, while raw mechanical topic keys remain noisy.

Decision:

1. Keep persisted labels as weak recall evidence.
2. Continue with denser backfill and/or targeted anchor-session labeling before
   drawing stronger conclusions about recall-quality impact.
3. Do not promote labels to hard task identity until they have broader
   backtest support.

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
