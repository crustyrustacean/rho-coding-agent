# Verify Scenario 04: code compiles and all tests pass.
$ErrorActionPreference = "Stop"
$dir = "$env:TEMP/rho-scenario-04"

$content = Get-Content "$dir/src/lib.rs" -Raw

if ($content -notmatch 'let mut result') {
    Write-Host "FAIL: 'result' is not declared as mutable" -ForegroundColor Red
    exit 1
}

Push-Location $dir
$testOutput = cargo test 2>&1 | Out-String
Pop-Location

if ($LASTEXITCODE -eq 0) {
    Write-Host "PASS: all tests pass" -ForegroundColor Green
} else {
    Write-Host "FAIL: tests did not pass" -ForegroundColor Red
    Write-Host $testOutput
    exit 1
}
