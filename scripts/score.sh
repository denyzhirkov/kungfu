#!/usr/bin/env bash
set -euo pipefail

# Kungfu AutoResearch — Scoring
# Reads research/last-eval.json and computes aggregate score

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
EVAL_FILE="${1:-$PROJECT_ROOT/research/last-eval.json}"

if [[ ! -f "$EVAL_FILE" ]]; then
  echo "ERROR: eval file not found: $EVAL_FILE"
  exit 1
fi

# Use jq to compute all scores
jq '
  # Filter to completed cases only
  .cases | map(select(.status == "done")) |

  # Per-case scoring
  map({
    id: .id,
    category: .category,
    project: .project,
    success: .success,

    # Relevance score: must_include precision (0-1)
    must_score: (if .must_include_total > 0 then (.must_include_found / .must_include_total) else 1 end),

    # Bonus: should_include (0-1)
    should_score: (if .should_include_total > 0 then (.should_include_found / .should_include_total) else 0 end),

    # Penalty: irrelevant files found
    irrelevant: .irrelevant_found,

    # Efficiency metrics
    output_bytes: .output_bytes,
    selected_files: .selected_files,
    latency_ms: .latency_ms,
    files_within_limit: .files_within_limit,
    bytes_within_limit: .bytes_within_limit,

    # Per-case score formula (0-100 scale):
    # Correctness (50pts): must_include precision
    # Relevance bonus (15pts): should_include hits
    # Token efficiency (20pts): lower bytes = better, scaled to 0-4KB range
    # Cleanliness (10pts): irrelevant penalty
    # Constraint compliance (5pts): within file/byte limits
    case_score: (
      (50 * (if .must_include_total > 0 then (.must_include_found / .must_include_total) else 1 end))
      + (15 * (if .should_include_total > 0 then (.should_include_found / .should_include_total) else 0 end))
      + (20 * (1 - ([1, (.output_bytes / 4000)] | min)))
      - (10 * .irrelevant_found)
      - (if .files_within_limit then 0 else 3 end)
      - (if .bytes_within_limit then 0 else 2 end)
    )
  }) as $cases |

  # Aggregate
  {
    total_cases: ($cases | length),
    passed: ([$cases[] | select(.success == true)] | length),
    failed: ([$cases[] | select(.success == false)] | length),

    success_rate: (([$cases[] | select(.success == true)] | length) / ($cases | length) * 100),

    avg_case_score: ([$cases[].case_score] | add / length),
    min_case_score: ([$cases[].case_score] | min),
    max_case_score: ([$cases[].case_score] | max),

    avg_output_bytes: ([$cases[].output_bytes] | add / length),
    avg_selected_files: ([$cases[].selected_files] | add / length),
    avg_latency_ms: ([$cases[].latency_ms] | add / length),
    total_irrelevant: ([$cases[].irrelevant] | add),

    # Aggregate score = average of per-case scores (directly comparable)
    aggregate_score: ([$cases[].case_score] | add / length),

    per_case: $cases
  }
' "$EVAL_FILE"
