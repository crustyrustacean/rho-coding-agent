# Scenario 05: Multiple errors requiring iterative fix cycle
# Creates a Rust project with 3 different compilation errors.

$ErrorActionPreference = "Stop"
$dir = "$env:TEMP/rho-scenario-05"

if (Test-Path $dir) { Remove-Item -Recurse -Force $dir }

cargo init --lib $dir
Set-Content -Path "$dir/src/lib.rs" -Value @"
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
"@

Write-Host "Scenario 05 created at: $dir"
Write-Host "Three errors:"
Write-Host "  1. insert() takes &self but needs &mut self"
Write-Host "  2. get() returns HashMap's Option<&String> but declares -> String"
Write-Host "  3. len() without is_empty() will trigger a clippy warning"
