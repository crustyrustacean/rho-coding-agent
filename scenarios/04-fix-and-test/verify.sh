#!/usr/bin/env bash
# Verify Scenario 04: code compiles and all tests pass.
set -euo pipefail

DIR="${TMPDIR:-/tmp}rho-scenario-04"
CONTENT=$(cat "$DIR/src/lib.rs")

if ! echo "$CONTENT" | grep -qE 'let mut result'; then
    echo "FAIL: 'result' is not declared as mutable"
    exit 1
fi

if (cd "$DIR" && cargo test 2>&1); then
    echo "PASS: all tests pass"
else
    echo "FAIL: tests did not pass"
    exit 1
fi
