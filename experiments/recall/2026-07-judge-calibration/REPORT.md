# Judge Calibration

Date: 2026-07-06
Labels file: `experiments/recall/judge-calibration/2026-07-labels.jsonl`

## Results

- Rows: 100
- Labeled rows: 100
- Label field: `adjudicator_score`
- Missing labels: 0
- Label sources: `{'anthropic:claude-haiku-4-5-20251001': 100}`
- Exact agreement: 0.34
- Within-1 agreement: 0.58
- Useful binary agreement: 0.58
- Low binary agreement: 0.58
- Cohen's kappa, exact 1-5: 0.13522012578616355
- Cohen's kappa, useful binary: 0.20574886535552186
- Cohen's kappa, low binary: 0.20574886535552186

## Interpretation

These metrics compare the production recall judge against an independent adjudicator label set. They validate judge agreement for tuning purposes, not human ground truth.
