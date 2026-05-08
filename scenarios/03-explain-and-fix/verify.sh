#!/usr/bin/env bash
# Verify Scenario 03: Config should now derive Debug.
set -euo pipefail

DIR="${TMPDIR:-/tmp}rho-scenario-03"
CONTENT=$(cat "$DIR/src/lib.rs")

if ! echo "$CONTENT" | grep -qE 'derive.*Debug'; then
    echo "FAIL: Config struct does not derive Debug"
    exit 1
fi

if (cd "$DIR" && cargo check 2>&1); then
    echo "PASS: Config derives Debug, project compiles"
else
    echo "FAIL: project does not compile"
    exit 1
fi
