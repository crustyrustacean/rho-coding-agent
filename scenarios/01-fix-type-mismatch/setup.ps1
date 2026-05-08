# Scenario 01: Fix E0308 type mismatch
# Creates a Rust project where a function returns an integer but is declared to return String.

$ErrorActionPreference = "Stop"
$dir = "$env:TEMP/rho-scenario-01"

if (Test-Path $dir) { Remove-Item -Recurse -Force $dir }

cargo init --lib $dir
Set-Content -Path "$dir/src/lib.rs" -Value @"
/// Returns a greeting message.
pub fn greet(name: &str) -> String {
    42
}

/// Formats a farewell message.
pub fn farewell(name: &str) -> String {
    format!("Goodbye, {name}!")
}
"@

# Copy the auto-approval config into the temp project so rho runs non-interactively
New-Item -ItemType Directory -Force -Path "$dir/.rho" | Out-Null
Copy-Item -Path "$PSScriptRoot/.rho/config.toml" -Destination "$dir/.rho/config.toml"

Write-Host "Scenario 01 created at: $dir"
Write-Host "The function 'greet' returns an integer but should return a String."
