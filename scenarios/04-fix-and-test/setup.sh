#!/usr/bin/env bash
# Scenario 04: Fix compilation error and verify tests pass
# Creates a Rust project with a mutability error and existing tests.
set -euo pipefail

DIR="${TMPDIR:-/tmp}rho-scenario-04"

rm -rf "$DIR"
cargo init --lib "$DIR"

cat > "$DIR/src/lib.rs" << 'EOF'
/// Collects even numbers from a range into a vector.
pub fn collect_evens(max: i32) -> Vec<i32> {
    let result = Vec::new();
    for i in 0..max {
        if i % 2 == 0 {
            result.push(i);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_evens_up_to_10() {
        assert_eq!(collect_evens(10), vec![0, 2, 4, 6, 8]);
    }

    #[test]
    fn collects_evens_up_to_1() {
        assert_eq!(collect_evens(1), vec![0]);
    }

    #[test]
    fn collects_evens_up_to_0() {
        assert_eq!(collect_evens(0), vec![]);
    }
}
EOF

# Copy the auto-approval config into the temp project so rho runs non-interactively
mkdir -p "$DIR/.rho"
cp "$(dirname "$0")/.rho/config.toml" "$DIR/.rho/config.toml"

echo "Scenario 04 created at: $DIR"
echo "The variable 'result' needs to be mutable. Tests exist to verify the fix."
