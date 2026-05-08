# Scenario 02: Remove unused imports
# Creates a Rust project with several unused imports that clippy/fix can clean up.

$ErrorActionPreference = "Stop"
$dir = "$env:TEMP/rho-scenario-02"

if (Test-Path $dir) { Remove-Item -Recurse -Force $dir }

cargo init --lib $dir
Set-Content -Path "$dir/src/lib.rs" -Value @"
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
"@

# Copy the auto-approval config into the temp project so rho runs non-interactively
New-Item -ItemType Directory -Force -Path "$dir/.rho" | Out-Null
Copy-Item -Path "$PSScriptRoot/.rho/config.toml" -Destination "$dir/.rho/config.toml"

Write-Host "Scenario 02 created at: $dir"
Write-Host "The file has 4 unused imports that should be removed."
