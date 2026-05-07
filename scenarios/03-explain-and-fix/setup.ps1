# Scenario 03: Explain error code, then fix
# Creates a Rust project with an E0277 trait bound error (missing Debug derive).

$ErrorActionPreference = "Stop"
$dir = "$env:TEMP/rho-scenario-03"

if (Test-Path $dir) { Remove-Item -Recurse -Force $dir }

cargo init --lib $dir
Set-Content -Path "$dir/src/lib.rs" -Value @"
pub struct Config {
    pub name: String,
    pub verbose: bool,
}

/// Print the config for debugging.
pub fn debug_config(cfg: &Config) {
    println!("{:?}", cfg);
}
"@

Write-Host "Scenario 03 created at: $dir"
Write-Host "Config struct is missing #[derive(Debug)] needed by println!({:?})."
