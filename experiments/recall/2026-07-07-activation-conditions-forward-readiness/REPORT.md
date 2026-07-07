# Formation-Time Activation Conditions Forward Readiness

## Question

Do newly written memories have activation-condition metadata, and is there enough downstream recall-eval exposure to evaluate the policy?

## Exposure

- Generated at: `2026-07-07T18:01:00Z`
- Database: `/Users/tbedor/.yaaml/yaaml.db`
- Policy start: `unix:1783407600`
- Activation table present: `True`

## Cohorts

### comparison_no_activation_conditions

- Memories: 3399 (902 active)
- Condition coverage: 0/3399 (0.0%)
- Downstream eval rows: selected=10076, judged=5995, useful=2377, low=3567, insufficient_context=1217
- Average memory score: 2.75
- Creation window: `unix:1780943956` to `unix:1783029106`

### new_policy_activation_conditions

- Memories: 119 (92 active)
- Condition coverage: 119/119 (100.0%)
- Downstream eval rows: selected=3, judged=3, useful=2, low=1, insufficient_context=0
- Average memory score: 3.33
- Creation window: `unix:1783444939` to `unix:1783446134`

## Activation Examples

- Memory 3400 `tf-gcp-org-pol deployments require #platform-security-help approval`: triggers=['tf-gcp-org-pol PR', 'org-policy approval', 'platsec-staff']; anti_triggers=[]
- Memory 3401 `Automated archival delay feature: append-only pending rows + delay metadata`: triggers=['pending_rule_archivals', 'DelayPendingRuleArchival', 'archival date', 'delay reason']; anti_triggers=[]
- Memory 3402 `Riskarbiter code review checklist: streaming, immutability, no temp diagnostics`: triggers=['riskarbiter', 'PR', 'code review', 'Java']; anti_triggers=[]
- Memory 3403 `Riskarbiter test execution: use all_tests_all_shards to bypass shard guessing`: triggers=['riskarbiter', 'test', 'bazel', 'java_test']; anti_triggers=[]
- Memory 3404 `Riskarbiter sparse worktree setup for focused work`: triggers=['riskarbiter', 'worktree', 'git']; anti_triggers=[]
- Memory 3405 `YAAML memory quality evidence: corpus findings and engineering case`: triggers=['working on YAAML memory formulation quality', 'justifying memory work budget to eng team', 'eval-gated consolidation design']; anti_triggers=['after PR#1 is merged into rust-impl']
- Memory 3406 `Updated YAAML memory techniques impact report with corpus evidence`: triggers=['presenting memory work business case internally', 'cross-referencing YAAML experiment corpus']; anti_triggers=[]
- Memory 3407 `Claude Code session format and storage`: triggers=['claude code', 'transcript ingestion', 'session source']; anti_triggers=[]

## Downstream Eval Examples

- Memory 3515 `LLM scoring rerank and learned calibration: selection-limiting backlog decisions`: latest_score=4, latest_run=3573, selected=1, judged=1, useful=1, low=0
- Memory 3424 `Phase 0 completion state: items 1–3 done, 4–5 in progress`: latest_score=4, latest_run=3573, selected=1, judged=1, useful=1, low=0
- Memory 3431 `Phase 0 structure: five validation items before technique decisions`: latest_score=2, latest_run=3569, selected=1, judged=1, useful=0, low=1

## Readiness

- Minimum condition memories: 20
- Minimum judged downstream evals: 20
- Observed condition memories: 119
- Observed judged downstream evals for condition memories: 3
- Pending recall-eval tasks selecting condition memories: 0
- Pending condition eval tasks with later turns: 0
- Ready for decision: `False`

## Decision

Do not retire the backlog item yet: the activation-condition cohort has not accumulated enough forward exposure for a decision.
