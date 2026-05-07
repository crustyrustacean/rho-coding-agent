# Run all prompt scenarios against rho.
#
# Usage:
#   ./scenarios/run-all.ps1
#
# Prerequisites:
#   - rho must be on PATH (or built: cargo build -p rho)
#   - A model API must be available (LM Studio, Ollama, etc.)
#
# Each scenario:
#   1. Runs setup.ps1 to create a temp project
#   2. Runs rho --prompt-file with the scenario prompt
#   3. Runs verify.ps1 to check the result

$ErrorActionPreference = "Stop"
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
$scenarios = Get-ChildItem -Path $scriptDir -Directory | Where-Object { $_.Name -match '^\d{2}-' } | Sort-Object Name

$passed = 0
$failed = 0
$total = $scenarios.Count

foreach ($scenario in $scenarios) {
    $name = $scenario.Name
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "Scenario: $name" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Setup
    Write-Host "Setting up..." -ForegroundColor Gray
    & "$($scenario.FullName)/setup.ps1"
    if ($LASTEXITCODE -ne 0) {
        Write-Host "SKIP: setup failed" -ForegroundColor Yellow
        $failed++
        continue
    }

    # Determine the project directory from the scenario number
    $num = $name.Substring(0, 2)
    $projectDir = "$env:TEMP/rho-scenario-$num"

    # Run rho
    Write-Host "Running rho..." -ForegroundColor Gray
    Push-Location $projectDir
    rho --prompt-file "$($scenario.FullName)/prompt.txt" --ephemeral --accept-external-provider
    $rhoCode = $LASTEXITCODE
    Pop-Location

    if ($rhoCode -ne 0) {
        Write-Host "WARN: rho exited with code $rhoCode" -ForegroundColor Yellow
    }

    # Verify
    Write-Host "Verifying..." -ForegroundColor Gray
    & "$($scenario.FullName)/verify.ps1"
    if ($LASTEXITCODE -eq 0) {
        $passed++
    } else {
        $failed++
    }
}

Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Results: $passed/$total passed, $failed/$total failed" -ForegroundColor $(if ($failed -eq 0) { "Green" } else { "Yellow" })
Write-Host "========================================" -ForegroundColor Cyan
