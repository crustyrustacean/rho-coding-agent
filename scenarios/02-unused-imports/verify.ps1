# Verify Scenario 02: unused imports should be removed.
$ErrorActionPreference = "Stop"
$dir = "$env:TEMP/rho-scenario-02"

$content = Get-Content "$dir/src/lib.rs" -Raw

$unused = @("use std::collections::HashMap", "use std::io", "use std::fmt", "use std::path::PathBuf")
$remaining = $unused | Where-Object { $content -match [regex]::Escape($_) }

if ($remaining.Count -gt 0) {
    Write-Host "FAIL: unused imports still present: $($remaining -join ', ')" -ForegroundColor Red
    exit 1
}

# The actual functions should still be there
if ($content -notmatch 'fn add' -or $content -notmatch 'fn double') {
    Write-Host "FAIL: functions were accidentally removed" -ForegroundColor Red
    exit 1
}

Push-Location $dir
cargo check 2>&1 | Out-Null
Pop-Location

if ($LASTEXITCODE -eq 0) {
    Write-Host "PASS: unused imports removed, project compiles" -ForegroundColor Green
} else {
    Write-Host "FAIL: project does not compile after fix" -ForegroundColor Red
    exit 1
}
