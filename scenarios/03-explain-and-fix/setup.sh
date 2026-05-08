#!/usr/bin/env bash
# Scenario 03: Explain error code, then fix
# Creates a Rust project with an E0277 trait bound error (missing Debug derive).
set -euo pipefail

DIR="${TMPDIR:-/tmp}rho-scenario-03"

rm -rf "$DIR"
cargo init --lib "$DIR"

cat > "$DIR/src/lib.rs" << 'EOF'
pub struct Config {
    pub name: String,
    pub verbose: bool,
}

/// Print the config for debugging.
pub fn debug_config(cfg: &Config) {
    println!("{:?}", cfg);
}
EOF

# Copy the auto-approval config into the temp project so rho runs non-interactively
mkdir -p "$DIR/.rho"
cp "$(dirname "$0")/.rho/config.toml" "$DIR/.rho/config.toml"

echo "Scenario 03 created at: $DIR"
echo "Config struct is missing #[derive(Debug)] needed by println!({:?})."
