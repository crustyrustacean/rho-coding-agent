#!/usr/bin/env bash
# Scenario 01: Fix E0308 type mismatch
# Creates a Rust project where a function returns an integer but is declared to return String.
set -euo pipefail

DIR="${TMPDIR:-/tmp}rho-scenario-01"

rm -rf "$DIR"
cargo init --lib "$DIR"

cat > "$DIR/src/lib.rs" << 'EOF'
/// Returns a greeting message.
pub fn greet(name: &str) -> String {
    42
}

/// Formats a farewell message.
pub fn farewell(name: &str) -> String {
    format!("Goodbye, {name}!")
}
EOF

# Copy the auto-approval config into the temp project so rho runs non-interactively
mkdir -p "$DIR/.rho"
cp "$(dirname "$0")/.rho/config.toml" "$DIR/.rho/config.toml"

echo "Scenario 01 created at: $DIR"
echo "The function 'greet' returns an integer but should return a String."
