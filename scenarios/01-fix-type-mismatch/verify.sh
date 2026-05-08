#!/usr/bin/env bash
# Verify Scenario 01: the greet function should now return a String, not an integer.
set -euo pipefail

DIR="${TMPDIR:-/tmp}rho-scenario-01"
CONTENT=$(cat "$DIR/src/lib.rs")

# Check that the integer literal 42 is gone (or replaced with a proper String expression)
if echo "$CONTENT" | grep -qE '\b42\b' && ! echo "$CONTENT" | grep -qE 'to_string\(\)|String::from|format!'; then
    echo "FAIL: greet() still returns an integer"
    exit 1
fi

# Check it compiles
if (cd "$DIR" && cargo check 2>&1); then
    echo "PASS: project compiles cleanly"
else
    echo "FAIL: project does not compile"
    exit 1
fi
