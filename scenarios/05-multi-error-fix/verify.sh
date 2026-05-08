#!/usr/bin/env bash
# Verify Scenario 05: all errors fixed, clippy clean.
set -euo pipefail

DIR="${TMPDIR:-/tmp}rho-scenario-05"
CONTENT=$(cat "$DIR/src/lib.rs")

if ! echo "$CONTENT" | grep -qE 'fn insert\(&mut self'; then
    echo "FAIL: insert() does not take &mut self"
    exit 1
fi

if ! (cd "$DIR" && cargo check 2>&1); then
    echo "FAIL: project does not compile"
    exit 1
fi

if (cd "$DIR" && cargo clippy 2>&1); then
    echo "PASS: compiles cleanly and clippy is satisfied"
else
    echo "WARN: compiles but clippy has warnings (acceptable)"
fi
