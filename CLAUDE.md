After making code changes in this repo, run `just quality`. It checks formatting, typechecks, strict clippy linting, the full test suite, and coverage.

## Experiments

- Recall/memory experiment methodology lives in `experiments/recall/README.md`. Follow its sampling, significance, holdout, and replay-vs-rerun rules for any backtest: screen on the frozen ~200-anchor library, replay saved candidates when only reranking/selection changed, rerun retrieval only when candidate generation or query construction changed, and reserve full-corpus replays for corpus-wide lifecycle effects.
- Report paired bootstrap confidence intervals; treat deltas whose interval includes zero as no detectable effect. Confirm winners on the holdout anchor set before shipping.
- The experiment backlog and durable results live in `EXPERIMENTS_LOG.md`. Phase 0 (validation infrastructure) items run first, in order; technique items are blocked on Phase 0. New experiment ideas are appended to the backlog with a hypothesis, target metric, and measurement mode — experiment-sized ideas go there, not in `ROADMAP.md`.
- After running an experiment: delete its backlog entry, append a decision-record entry (date, sources, experiment, metrics, lessons, decision), and commit the run artifacts under `experiments/recall/<date>-<name>/`.
