#!/usr/bin/env bash
# Verify Scenario 02: unused imports should be removed.
set -euo pipefail

DIR="${TMPDIR:-/tmp}rho-scenario-02"
CONTENT=$(cat "$DIR/src/lib.rs")

FAIL=0
for import in "use std::collections::HashMap" "use std::io" "use std::fmt" "use std::path::PathBuf"; do
    if echo "$CONTENT" | grep -qF "$import"; then
        echo "FAIL: unused import still present: $import"
        FAIL=1
    fi
done

# The actual functions should still be there
if ! echo "$CONTENT" | grep -q 'fn add'; then
    echo "FAIL: fn add was accidentally removed"
    FAIL=1
fi
if ! echo "$CONTENT" | grep -q 'fn double'; then
    echo "FAIL: fn double was accidentally removed"
    FAIL=1
fi

if [ "$FAIL" -eq 1 ]; then
    exit 1
fi

if (cd "$DIR" && cargo check 2>&1); then
    echo "PASS: unused imports removed, project compiles"
else
    echo "FAIL: project does not compile after fix"
    exit 1
fi
