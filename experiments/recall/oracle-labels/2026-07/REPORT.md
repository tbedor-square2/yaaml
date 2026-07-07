# Dense Oracle Labels

Date: 2026-07-07
Adjudicator model: `claude-haiku-4-5-20251001`
Input directory: `target/anchor-refresh-2026-07`

## Method

Scored the saved ranked candidate pool (top 16, plus production-selected and historical-oracle memory ids) for every anchor with the calibration adjudicator prompt (query + memory + rubric only; no rank metadata, no prior judge score). Dense per-run oracle files replace historical production-judge rows when experiments set `BACKTEST_ORACLE_DIR`.

## Coverage

- Anchors: 300
- Anchors with at least one label: 299
- Anchors with a useful (>=4) label: 281
- Total labels: 5007
- Useful labels (>=4): 1808
- Low labels (<=2): 3167
- Skipped candidates missing from the memory database: 0

## Score Distribution

| Score | Labels |
| ---: | ---: |
| 1 | 1083 |
| 2 | 2084 |
| 3 | 32 |
| 4 | 1018 |
| 5 | 790 |

## Caveats

- Labels are LLM adjudication, not human ground truth; the adjudicator's own validity rests on the 2026-07 judge-calibration study.
- Labels cover the saved candidate pools only. A future strategy that surfaces memories outside these pools needs a label top-up run against its own recall artifacts (the cache is append-only and keyed by run/memory/model).

## Artifacts

- `labels.jsonl`: append-only adjudicator score cache.
- `dense-oracles/oracle-<run_id>.json`: per-run dense oracle files (`BACKTEST_ORACLE_DIR` target).
- `manifest.json`: inputs, parameters, and coverage stats.
