# Feature Model Recall Selection

This experiment exports candidate-level recall rows from the strict-kind
production backtest, trains a small local feature model, and compares it against
the existing production selector plus two simple heuristics.

## Dataset

1. Input backtest: `target/strict-kind-production`
2. Exported dataset: `target/recall-training-dataset.jsonl`
3. Runs: 200
4. Candidate rows: 3200
5. Labeled rows: 413
6. Useful labeled rows: 106
7. Low-scoring labeled rows: 302

The exported dataset is intentionally left under `target/`; the committed
artifact is `dataset-summary.json`.

## Results

| Strategy | Avg known score | Useful runs | Useful selected | Low selected | Low runs | Avg memories | Empty runs |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| production | 2.74 | 54 | 69 | 88 | 62 | 2.28 | 5 |
| top_2 | 2.74 | 44 | 51 | 67 | 54 | 1.69 | 5 |
| weak_top_abstain | 2.82 | 42 | 48 | 57 | 46 | 1.41 | 53 |
| feature_model | 3.30 | 39 | 44 | 27 | 22 | 1.68 | 12 |

## Readout

1. `feature_model` sharply reduced known bad recall: 27 low-scoring selections
   versus 88 for production and 67 for `top_2`.
2. The precision gain came with lower useful coverage: 39 useful runs versus 54
   for production and 44 for `top_2`.
3. Label sparsity remains the main limitation: only 413 of 3200 candidate rows
   have oracle labels, and `feature_model` still selected 263 unlabeled rows.
4. This is not enough evidence to fine-tune a tiny text model. The feature model
   has not plateaued on a dense enough labeled set, and the current evidence
   does not prove that text-level judgment is the missing signal.

## Recommendation

1. Keep the runtime change focused on lower context volume: top-two selection
   plus session cooldown.
2. Use this exporter to collect more per-candidate labels before training
   anything more complex.
3. Revisit a tiny fine-tuned model only after the feature model plateaus on a
   denser labeled corpus and error analysis shows failures that structured
   recall features cannot explain.
