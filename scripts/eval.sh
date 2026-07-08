#!/usr/bin/env bash
set -euo pipefail

# Kungfu AutoResearch — Benchmark Evaluator
# Runs all cases from research/cases/ and produces research/last-eval.json

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
CASES_DIR="$PROJECT_ROOT/research/cases"
OUTPUT_FILE="$PROJECT_ROOT/research/last-eval.json"
KUNGFU="${KUNGFU_BIN:-$PROJECT_ROOT/target/release/kungfu}"

if [[ ! -x "$KUNGFU" ]]; then
  echo "ERROR: kungfu binary not found at $KUNGFU"
  echo "Run: cargo build --release"
  exit 1
fi

# Resolve project directory for a case
resolve_project_dir() {
  local project="$1"
  if [[ "$project" == "kungfu" ]]; then
    echo "$PROJECT_ROOT"
  else
    echo "$PROJECT_ROOT/kungfu-bench/$project"
  fi
}

# Run a single case, output JSON result to stdout
run_case() {
  local case_file="$1"
  local id category project task query command budget
  id=$(jq -r '.id' "$case_file")
  category=$(jq -r '.category' "$case_file")
  project=$(jq -r '.project' "$case_file")
  task=$(jq -r '.task' "$case_file")
  query=$(jq -r '.input.query' "$case_file")
  command=$(jq -r '.input.command // "ask-context"' "$case_file")
  budget=$(jq -r '.input.budget // "small"' "$case_file")

  local project_dir
  project_dir=$(resolve_project_dir "$project")

  if [[ ! -d "$project_dir/.kungfu" ]]; then
    echo "{\"id\":\"$id\",\"status\":\"skip\",\"reason\":\"no index at $project_dir\"}"
    return
  fi

  local start_ms end_ms duration_ms output output_bytes
  start_ms=$(perl -MTime::HiRes=time -e 'printf "%d", time*1000')

  # Run kungfu command
  case "$command" in
    ask-context)
      output=$(cd "$project_dir" && "$KUNGFU" ask-context "$query" --budget "$budget" 2>/dev/null) || output=""
      ;;
    find-symbol)
      output=$(cd "$project_dir" && "$KUNGFU" find-symbol "$query" 2>/dev/null) || output=""
      ;;
    search-text)
      # HARNESS FIX: the CLI subcommand `search-text` was renamed to `search`.
      # The old invocation failed silently (stderr discarded) and scored on empty output.
      output=$(cd "$project_dir" && "$KUNGFU" search "$query" 2>/dev/null) || output=""
      ;;
    context)
      # HARNESS FIX: the `context` subcommand no longer exists; `ask-context` is its successor.
      output=$(cd "$project_dir" && "$KUNGFU" ask-context "$query" --budget "$budget" 2>/dev/null) || output=""
      ;;
    semantic-search)
      output=$(cd "$project_dir" && "$KUNGFU" search "$query" --semantic 2>/dev/null) || output=""
      ;;
    onboard)
      output=$(cd "$project_dir" && "$KUNGFU" onboard 2>/dev/null) || output=""
      ;;
    *)
      output=""
      ;;
  esac

  end_ms=$(perl -MTime::HiRes=time -e 'printf "%d", time*1000')
  duration_ms=$((end_ms - start_ms))
  output_bytes=${#output}

  # Count files in output (lines with file paths)
  local selected_files=0
  selected_files=$(echo "$output" | grep -cE '\[.*\]|\.rs|\.java|\.kt|\.cs|\.ts|\.js|\.py|\.go' || true)

  # Check must_include
  local must_include_count=0 must_include_found=0
  must_include_count=$(jq -r '.expected.must_include // [] | length' "$case_file")
  for pattern in $(jq -r '.expected.must_include // [] | .[]' "$case_file"); do
    if echo "$output" | grep -q "$pattern"; then
      must_include_found=$((must_include_found + 1))
    fi
  done

  # Check should_include (bonus)
  local should_include_count=0 should_include_found=0
  should_include_count=$(jq -r '.expected.should_include // [] | length' "$case_file")
  for pattern in $(jq -r '.expected.should_include // [] | .[]' "$case_file"); do
    if echo "$output" | grep -q "$pattern"; then
      should_include_found=$((should_include_found + 1))
    fi
  done

  # Check should_not_include (penalties)
  local should_not_include_count=0 irrelevant_found=0
  should_not_include_count=$(jq -r '.expected.should_not_include // [] | length' "$case_file")
  for pattern in $(jq -r '.expected.should_not_include // [] | .[]' "$case_file"); do
    if echo "$output" | grep -q "$pattern"; then
      irrelevant_found=$((irrelevant_found + 1))
    fi
  done

  # Check max constraints
  local max_files max_bytes files_ok=true bytes_ok=true
  max_files=$(jq -r '.expected.max_files // 0' "$case_file")
  max_bytes=$(jq -r '.expected.max_bytes // 0' "$case_file")
  if [[ "$max_files" -gt 0 && "$selected_files" -gt "$max_files" ]]; then
    files_ok=false
  fi
  if [[ "$max_bytes" -gt 0 && "$output_bytes" -gt "$max_bytes" ]]; then
    bytes_ok=false
  fi

  # Determine success
  local success=false
  if [[ "$must_include_count" -eq 0 || "$must_include_found" -eq "$must_include_count" ]]; then
    success=true
  fi

  cat <<EOJSON
{
  "id": "$id",
  "category": "$category",
  "project": "$project",
  "status": "done",
  "success": $success,
  "must_include_found": $must_include_found,
  "must_include_total": $must_include_count,
  "should_include_found": $should_include_found,
  "should_include_total": $should_include_count,
  "irrelevant_found": $irrelevant_found,
  "selected_files": $selected_files,
  "output_bytes": $output_bytes,
  "latency_ms": $duration_ms,
  "files_within_limit": $files_ok,
  "bytes_within_limit": $bytes_ok
}
EOJSON
}

# Main
echo "Running benchmark evaluation..."
echo "Cases: $CASES_DIR"
echo "Binary: $KUNGFU"
echo ""

results=()
total=0
passed=0
failed=0
skipped=0

for case_file in "$CASES_DIR"/case-*.json; do
  [[ -f "$case_file" ]] || continue
  total=$((total + 1))
  id=$(jq -r '.id' "$case_file")
  echo -n "  $id ... "

  result=$(run_case "$case_file")
  results+=("$result")

  status=$(echo "$result" | jq -r '.status')
  if [[ "$status" == "skip" ]]; then
    echo "SKIP"
    skipped=$((skipped + 1))
  elif [[ "$(echo "$result" | jq -r '.success')" == "true" ]]; then
    echo "PASS ($(echo "$result" | jq -r '.output_bytes')B, $(echo "$result" | jq -r '.latency_ms')ms)"
    passed=$((passed + 1))
  else
    echo "FAIL (must_include: $(echo "$result" | jq -r '.must_include_found')/$(echo "$result" | jq -r '.must_include_total'))"
    failed=$((failed + 1))
  fi
done

# Build JSON output
{
  echo "{"
  echo "  \"timestamp\": \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\","
  echo "  \"kungfu_version\": \"$($KUNGFU --version 2>/dev/null || echo unknown)\","
  echo "  \"total\": $total,"
  echo "  \"passed\": $passed,"
  echo "  \"failed\": $failed,"
  echo "  \"skipped\": $skipped,"
  echo "  \"cases\": ["
  first=true
  for r in "${results[@]}"; do
    if $first; then first=false; else echo ","; fi
    echo -n "    $r"
  done
  echo ""
  echo "  ]"
  echo "}"
} > "$OUTPUT_FILE"

echo ""
echo "Results: $passed/$total passed, $failed failed, $skipped skipped"
echo "Saved to: $OUTPUT_FILE"
