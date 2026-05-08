#!/usr/bin/env bash
# Run all prompt scenarios against rho.
#
# Usage:
#   ./scenarios/run-all.sh [--model <model-id>]
#
# Prerequisites:
#   - rho must be on PATH or built: cargo build -p rho
#   - A model API must be available (LM Studio, Ollama, etc.)
#
# Each scenario:
#   1. Runs setup.sh to create a temp project
#   2. Runs rho --prompt-file with the scenario prompt
#   3. Runs verify.sh to check the result
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RHO="${RHO:-rho}"

# Optional --model flag
MODEL_ARGS=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --model)
            MODEL_ARGS=(--model "$2")
            shift 2
            ;;
        *)
            echo "Unknown argument: $1" >&2
            exit 1
            ;;
    esac
done

PASSED=0
FAILED=0
TOTAL=0

for scenario_dir in "$SCRIPT_DIR"/[0-9][0-9]-*/; do
    name="$(basename "$scenario_dir")"
    num="${name:0:2}"
    project_dir="${TMPDIR:-/tmp}rho-scenario-$num"

    echo ""
    echo "========================================"
    echo "Scenario: $name"
    echo "========================================"
    TOTAL=$((TOTAL + 1))

    # Setup
    echo "Setting up..."
    if ! bash "$scenario_dir/setup.sh"; then
        echo "SKIP: setup failed"
        FAILED=$((FAILED + 1))
        continue
    fi

    # Run rho
    echo "Running rho..."
    if ! (cd "$project_dir" && "$RHO" \
            --prompt-file "$scenario_dir/prompt.txt" \
            --ephemeral \
            "${MODEL_ARGS[@]+"${MODEL_ARGS[@]}"}"); then
        echo "WARN: rho exited with non-zero status"
    fi

    # Verify
    echo "Verifying..."
    if bash "$scenario_dir/verify.sh"; then
        PASSED=$((PASSED + 1))
    else
        FAILED=$((FAILED + 1))
    fi
done

echo ""
echo "========================================"
echo "Results: $PASSED/$TOTAL passed, $FAILED/$TOTAL failed"
echo "========================================"

[ "$FAILED" -eq 0 ]
