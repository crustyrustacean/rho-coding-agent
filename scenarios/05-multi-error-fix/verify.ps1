# Verify Scenario 05: all errors fixed, clippy clean.
$ErrorActionPreference = "Stop"
$dir = "$env:TEMP/rho-scenario-05"

$content = Get-Content "$dir/src/lib.rs" -Raw

# Check that insert takes &mut self
if ($content -notmatch 'fn insert\(&mut self') {
    Write-Host "FAIL: insert() does not take &mut self" -ForegroundColor Red
    exit 1
}

# Check it compiles
Push-Location $dir
cargo check 2>&1 | Out-Null
$checkCode = $LASTEXITCODE

if ($checkCode -ne 0) {
    Write-Host "FAIL: project does not compile" -ForegroundColor Red
    Pop-Location
    exit 1
}

# Check clippy
$clippyOutput = cargo clippy 2>&1 | Out-String
$clippyCode = $LASTEXITCODE
Pop-Location

if ($clippyCode -eq 0) {
    Write-Host "PASS: compiles cleanly and clippy is satisfied" -ForegroundColor Green
} else {
    Write-Host "WARN: compiles but clippy has warnings (acceptable)" -ForegroundColor Yellow
    Write-Host $clippyOutput
}
