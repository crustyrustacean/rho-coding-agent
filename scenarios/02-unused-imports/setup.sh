#!/usr/bin/env bash
# Scenario 02: Remove unused imports
# Creates a Rust project with several unused imports that clippy/fix can clean up.
set -euo pipefail

DIR="${TMPDIR:-/tmp}rho-scenario-02"

rm -rf "$DIR"
cargo init --lib "$DIR"

cat > "$DIR/src/lib.rs" << 'EOF'
use std::collections::HashMap;
use std::io;
use std::fmt;
use std::path::PathBuf;

/// Adds two numbers together.
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

/// Doubles a number.
pub fn double(x: i32) -> i32 {
    x * 2
}
EOF

# Copy the auto-approval config into the temp project so rho runs non-interactively
mkdir -p "$DIR/.rho"
cp "$(dirname "$0")/.rho/config.toml" "$DIR/.rho/config.toml"

echo "Scenario 02 created at: $DIR"
echo "The file has 4 unused imports that should be removed."
