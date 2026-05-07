# Scenario 04: Fix compilation error and verify tests pass
# Creates a Rust project with a mutability error and existing tests.

$ErrorActionPreference = "Stop"
$dir = "$env:TEMP/rho-scenario-04"

if (Test-Path $dir) { Remove-Item -Recurse -Force $dir }

cargo init --lib $dir
Set-Content -Path "$dir/src/lib.rs" -Value @"
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
"@

Write-Host "Scenario 04 created at: $dir"
Write-Host "The variable 'result' needs to be mutable. Tests exist to verify the fix."
