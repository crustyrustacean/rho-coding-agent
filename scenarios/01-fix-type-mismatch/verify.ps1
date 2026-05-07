# Verify Scenario 01: the greet function should now return a String, not an integer.
$ErrorActionPreference = "Stop"
$dir = "$env:TEMP/rho-scenario-01"

$content = Get-Content "$dir/src/lib.rs" -Raw

# Check that the integer literal is gone
if ($content -match '\b42\b' -and $content -notmatch 'to_string|String::from|format!') {
    Write-Host "FAIL: greet() still returns an integer" -ForegroundColor Red
    exit 1
}

# Check it compiles
Push-Location $dir
$result = cargo check 2>&1
Pop-Location

if ($LASTEXITCODE -eq 0) {
    Write-Host "PASS: project compiles cleanly" -ForegroundColor Green
} else {
    Write-Host "FAIL: project does not compile" -ForegroundColor Red
    Write-Host $result
    exit 1
}
