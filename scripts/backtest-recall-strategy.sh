#!/usr/bin/env bash
set -euo pipefail

repo="${1:-$(git rev-parse --show-toplevel)}"
label="${2:-$(basename "$repo")}"
oracle_bin="${YAAML_ORACLE_BIN:-yaaml}"
out_dir="${BACKTEST_OUT_DIR:-$repo/target/recall-backtests}"
anchor_source="${BACKTEST_ANCHOR_SOURCE:-fixed}"
anchor_limit="${BACKTEST_ANCHOR_LIMIT:-200}"
db_path="${BACKTEST_DB:-$HOME/.yaaml/yaaml.db}"
exclude_context_embedded="${BACKTEST_EXCLUDE_CONTEXT_EMBEDDED:-1}"
anchors_file="${BACKTEST_ANCHORS_FILE:-}"
mkdir -p "$out_dir"

anchors_tsv="$out_dir/anchors.tsv"
details_jsonl="$out_dir/$label.details.jsonl"
summary_json="$out_dir/$label.summary.json"
: >"$details_jsonl"

if [[ -n "$anchors_file" ]]; then
  cp "$anchors_file" "$anchors_tsv"
elif [[ "$anchor_source" == "eval-library" ]]; then
  if [[ ! -f "$db_path" ]]; then
    echo "database not found: $db_path" >&2
    exit 1
  fi
  limit_clause=""
  if [[ "$anchor_limit" != "all" ]]; then
    limit_clause="LIMIT $anchor_limit"
  fi
  leakage_clause=""
  if [[ "$exclude_context_embedded" == "1" ]]; then
    leakage_clause="AND NOT EXISTS (
      SELECT 1
      FROM eval_results er
      JOIN eval_context_embeddings ece ON ece.eval_result_id = er.id
      WHERE er.eval_run_id = r.id
    )"
  fi
  sqlite3 -separator $'\t' "$db_path" "
    WITH eligible AS (
      SELECT
        r.id,
        json_extract(r.config_json, '$.session_id') AS session_id,
        CAST(json_extract(r.config_json, '$.turn_ordinal') AS INTEGER) AS turn_ordinal
      FROM eval_runs r
      WHERE r.completed_at IS NOT NULL
        AND json_extract(r.config_json, '$.session_id') IS NOT NULL
        AND json_extract(r.config_json, '$.turn_ordinal') IS NOT NULL
        $leakage_clause
    ),
    latest_per_anchor AS (
      SELECT
        MAX(id) AS run_id,
        session_id,
        turn_ordinal
      FROM eligible
      GROUP BY session_id, turn_ordinal
    )
    SELECT
      run_id,
      session_id,
      turn_ordinal,
      'eval-library-' || run_id
    FROM latest_per_anchor
    ORDER BY run_id DESC
    $limit_clause;
  " >"$anchors_tsv"
else
  cat >"$anchors_tsv" <<'ANCHORS'
945	019ed725-d1bc-7e62-860f-0315f2af147a	1	java-acl-all-bad
944	019ed6d1-2c21-7480-abd9-77e59d52df20	11	java-ci-mixed
943	019ea810-ee08-7cb3-a7ac-5bcd76670996	123	yaaml-good
942	019ea810-ee08-7cb3-a7ac-5bcd76670996	122	yaaml-mixed
941	019ed725-d1bc-7e62-860f-0315f2af147a	0	java-acl-low
931	019ed6d1-2c21-7480-abd9-77e59d52df20	7	java-risk-mixed
930	019ed6d1-2c21-7480-abd9-77e59d52df20	6	java-risk-mixed
929	019ed6d1-2c21-7480-abd9-77e59d52df20	5	java-review-mixed
928	019ed6d1-2c21-7480-abd9-77e59d52df20	4	java-review-mixed
921	019ea810-ee08-7cb3-a7ac-5bcd76670996	120	yaaml-backtest
917	019ed6df-0d7b-7210-b6eb-9da62527d7f1	5	tools-mixed
906	019ea810-ee08-7cb3-a7ac-5bcd76670996	118	yaaml-quality-mixed
900	019ea810-ee08-7cb3-a7ac-5bcd76670996	116	yaaml-good
890	019ed6cc-4fe5-7d61-8a85-dd7ec0c3fb4f	0	forge-review-mixed
887	019ebd43-5d50-7c42-98f8-1ddd93912a8d	43	java-package-good
881	019ebd43-5d50-7c42-98f8-1ddd93912a8d	42	java-package-mixed
ANCHORS
fi

anchor_count="$(wc -l <"$anchors_tsv" | tr -d ' ')"
if [[ "$anchor_count" == "0" ]]; then
  echo "no anchors selected" >&2
  exit 1
fi

(
  cd "$repo"
  cargo build -q -p yaaml
)
recall_bin="$repo/target/debug/yaaml"

run_with_retry() {
  local attempt
  for attempt in 1 2 3 4 5; do
    if "$@"; then
      return 0
    fi
    sleep "$attempt"
  done
  return 1
}

while IFS=$'\t' read -r run_id session_id turn_ordinal case_label; do
  oracle_file="$out_dir/oracle-$run_id.json"
  recall_file="$out_dir/$label.recall-$run_id.json"

  if [[ ! -s "$oracle_file" ]]; then
  run_with_retry "$oracle_bin" eval show "$run_id" --json >"$oracle_file" 2>"$oracle_file.stderr"
  fi

  run_with_retry bash -c \
    'cd "$1" && "$2" recall --session "$3" --turn "$4" --json --debug-ranking' \
    bash "$repo" "$recall_bin" "$session_id" "$turn_ordinal" >"$recall_file" 2>"$recall_file.stderr"

  if ! jq -e type "$recall_file" >/dev/null; then
    echo "recall command did not emit valid JSON for run $run_id; see $recall_file.stderr" >&2
    exit 1
  fi

  jq -n -c \
    --arg label "$label" \
    --arg case_label "$case_label" \
    --argjson run_id "$run_id" \
    --slurpfile oracle "$oracle_file" \
    --slurpfile recall "$recall_file" '
      def score_map:
        ($oracle[0].results // [])
        | map(select(.judge_score | test("^[1-5]$")))
        | map({key: (.memory_id | tostring), value: {
            score: (.judge_score | tonumber),
            title: .memory_title
          }})
        | from_entries;
      def avg(xs):
        if (xs | length) == 0 then null else ((xs | add) / (xs | length)) end;
      score_map as $scores
      | ($recall[0].selected_memory_ids // []) as $selected
      | ($selected | map(. as $id | $scores[($id | tostring)] // null)) as $selected_known
      | ($selected_known | map(select(. != null).score)) as $known_scores
      | {
          strategy: $label,
          case: $case_label,
          run_id: $run_id,
          session_id: $oracle[0].run.session_id,
          turn_ordinal: $oracle[0].run.turn_ordinal,
          selected_memory_ids: $selected,
          selected_count: ($selected | length),
          known_selected_count: ($known_scores | length),
          unknown_selected_count: (($selected | length) - ($known_scores | length)),
          average_known_score: avg($known_scores),
          useful_known_selected: ($known_scores | map(select(. >= 4)) | length),
          low_known_selected: ($known_scores | map(select(. <= 2)) | length),
          captured_any_known_useful: any($known_scores[]?; . >= 4),
          selected_any_known_low: any($known_scores[]?; . <= 2),
          oracle_has_useful: any(($scores | to_entries[]?.value.score); . >= 4),
          oracle_best_score: ([$scores | to_entries[]?.value.score] | max),
          empty_recall: (($selected | length) == 0),
          filter_telemetry: $recall[0].filter_telemetry
        }
    ' >>"$details_jsonl"
done <"$anchors_tsv"

jq -s \
  --arg label "$label" \
  --arg anchor_source "$anchor_source" '
    def avg(xs):
      if (xs | length) == 0 then null else ((xs | add) / (xs | length)) end;
    {
      strategy: $label,
      anchor_source: $anchor_source,
      anchors: length,
      selected_memories: (map(.selected_count) | add),
      average_selected_per_anchor: avg(map(.selected_count)),
      known_selected_memories: (map(.known_selected_count) | add),
      unknown_selected_memories: (map(.unknown_selected_count) | add),
      average_known_score: avg(map(.average_known_score) | map(select(. != null))),
      useful_known_selected: (map(.useful_known_selected) | add),
      low_known_selected: (map(.low_known_selected) | add),
      useful_capture_runs: (map(select(.captured_any_known_useful)) | length),
      low_selection_runs: (map(select(.selected_any_known_low)) | length),
      oracle_useful_runs: (map(select(.oracle_has_useful)) | length),
      empty_recall_runs: (map(select(.empty_recall)) | length),
      llm_attempted_runs: (map(select(.filter_telemetry.llm_attempted == true)) | length),
      llm_applied_runs: (map(select(.filter_telemetry.llm_applied == true)) | length),
      cases: .
    }
  ' "$details_jsonl" >"$summary_json"

cat "$summary_json"
