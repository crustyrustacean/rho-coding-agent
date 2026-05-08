#!/usr/bin/env bash
# Scenario 05: Multiple errors requiring iterative fix cycle
# Creates a Rust project with 3 different compilation errors.
set -euo pipefail

DIR="${TMPDIR:-/tmp}rho-scenario-05"

rm -rf "$DIR"
cargo init --lib "$DIR"

cat > "$DIR/src/lib.rs" << 'EOF'
use std::collections::HashMap;

/// A simple key-value store.
pub struct Store {
    data: HashMap<String, String>,
}

impl Store {
    /// Create a new empty store.
    pub fn new() -> Store {
        Store {
            data: HashMap::new(),
        }
    }

    /// Insert a key-value pair.
    pub fn insert(&self, key: String, value: String) {
        self.data.insert(key, value);
    }

    /// Get a value by key. Returns the value, not an Option.
    pub fn get(&self, key: &str) -> String {
        self.data.get(key)
    }

    /// Count the number of entries.
    pub fn len(&self) -> usize {
        self.data.len()
    }
}
EOF

# Copy the auto-approval config into the temp project so rho runs non-interactively
mkdir -p "$DIR/.rho"
cp "$(dirname "$0")/.rho/config.toml" "$DIR/.rho/config.toml"

echo "Scenario 05 created at: $DIR"
echo "Three errors:"
echo "  1. insert() takes &self but needs &mut self"
echo "  2. get() returns HashMap's Option<&String> but declares -> String"
echo "  3. len() without is_empty() will trigger a clippy warning"
