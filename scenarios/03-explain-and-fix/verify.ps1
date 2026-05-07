# Verify Scenario 03: Config should now derive Debug.
$ErrorActionPreference = "Stop"
$dir = "$env:TEMP/rho-scenario-03"

$content = Get-Content "$dir/src/lib.rs" -Raw

if ($content -notmatch 'derive.*Debug') {
    Write-Host "FAIL: Config struct does not derive Debug" -ForegroundColor Red
    exit 1
}

Push-Location $dir
cargo check 2>&1 | Out-Null
Pop-Location

if ($LASTEXITCODE -eq 0) {
    Write-Host "PASS: Config derives Debug, project compiles" -ForegroundColor Green
} else {
    Write-Host "FAIL: project does not compile" -ForegroundColor Red
    exit 1
}
