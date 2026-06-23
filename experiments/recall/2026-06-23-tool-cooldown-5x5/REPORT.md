# Tool Recall Narrowing 5x5

This replay compares five pruning strategies across five recent tool-recall cohorts.
It uses post-fix `tool_pre_use` eval runs from the local YAAML SQLite store.

Important limitation: this backtest can only remove memories that the current retrieval selected; it cannot score alternate memories that a different retriever would have found.

## Strategies

- `baseline`: current selected memories.
- `session_cooldown_20m`: suppress a memory if it was already kept in the session in the last 20 minutes.
- `family_cooldown_20m`: suppress a memory if it was already kept for the same command family in the last 20 minutes.
- `command_family_gate`: keep only memories with deterministic command-family evidence.
- `targeted_cooldown`: session cooldown plus suppress patch-text/tmp patch recalls.

## Overall Results

| strategy | avg | useful | low | useful_runs | low_runs | empty | missed_useful_empty | delta_low | delta_useful |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| baseline | 1.95 | 6 | 33 | 6 | 32 | 20 | 0 | 0 | 0 |
| session_cooldown_20m | 2.47 | 4 | 11 | 4 | 11 | 43 | 2 | -22 | -2 |
| family_cooldown_20m | 2.39 | 4 | 14 | 4 | 13 | 41 | 2 | -19 | -2 |
| command_family_gate | 1.79 | 4 | 20 | 4 | 20 | 33 | 2 | -13 | -2 |
| targeted_cooldown | 2.50 | 4 | 10 | 4 | 10 | 44 | 2 | -23 | -2 |

## Cohort Results

### git
| strategy | avg | useful | low | empty | missed_useful_empty |
| --- | --- | --- | --- | --- | --- |
| baseline | 2.27 | 2 | 9 | 9 | 0 |
| session_cooldown_20m | 2.43 | 2 | 5 | 13 | 0 |
| family_cooldown_20m | 2.43 | 2 | 5 | 13 | 0 |
| command_family_gate | 2.00 | 0 | 1 | 18 | 2 |
| targeted_cooldown | 2.43 | 2 | 5 | 13 | 0 |

### yarn
| strategy | avg | useful | low | empty | missed_useful_empty |
| --- | --- | --- | --- | --- | --- |
| baseline | 1.56 | 3 | 15 | 0 | 0 |
| session_cooldown_20m | 2.33 | 1 | 2 | 15 | 2 |
| family_cooldown_20m | 2.33 | 1 | 2 | 15 | 2 |
| command_family_gate | 1.56 | 3 | 15 | 0 | 0 |
| targeted_cooldown | 2.33 | 1 | 2 | 15 | 2 |

### apply_patch_source
| strategy | avg | useful | low | empty | missed_useful_empty |
| --- | --- | --- | --- | --- | --- |
| baseline | 2.60 | 1 | 4 | 7 | 0 |
| session_cooldown_20m | 2.75 | 1 | 3 | 8 | 0 |
| family_cooldown_20m | 2.60 | 1 | 4 | 7 | 0 |
| command_family_gate | 2.60 | 1 | 4 | 7 | 0 |
| targeted_cooldown | 2.75 | 1 | 3 | 8 | 0 |

### apply_patch_text
| strategy | avg | useful | low | empty | missed_useful_empty |
| --- | --- | --- | --- | --- | --- |
| baseline | 2.00 | 0 | 5 | 1 | 0 |
| session_cooldown_20m | 2.00 | 0 | 1 | 4 | 0 |
| family_cooldown_20m | 2.00 | 0 | 3 | 3 | 0 |
| command_family_gate | n/a | 0 | 0 | 5 | 0 |
| targeted_cooldown | n/a | 0 | 0 | 5 | 0 |

### other
| strategy | avg | useful | low | empty | missed_useful_empty |
| --- | --- | --- | --- | --- | --- |
| baseline | n/a | 0 | 0 | 3 | 0 |
| session_cooldown_20m | n/a | 0 | 0 | 3 | 0 |
| family_cooldown_20m | n/a | 0 | 0 | 3 | 0 |
| command_family_gate | n/a | 0 | 0 | 3 | 0 |
| targeted_cooldown | n/a | 0 | 0 | 3 | 0 |

## Findings

- `session_cooldown_20m` is the best balanced candidate in this replay: it removes repeated noise while preserving most useful recalls.
- `family_cooldown_20m` is slightly worse here because the repeated noise is mostly same-session, not only same command family.
- `command_family_gate` is too blunt as tested: it removes apply-patch text noise, but it also misses useful git memories and does not improve yarn recalls.
- `targeted_cooldown` keeps the useful/low tradeoff close to session cooldown while suppressing the patch-text/tmp patch cohort completely.
- The next production candidate should start with session-level memory cooldown and a narrow tool-input suppression for synthetic patch text, not broad command-family gating.

## Artifacts

- Manifest: `experiments/recall/2026-06-23-tool-cooldown-5x5/manifest.json`
- Summary JSON: `experiments/recall/2026-06-23-tool-cooldown-5x5/summary.json`
- Per-run JSONL: `experiments/recall/2026-06-23-tool-cooldown-5x5/details.jsonl`
