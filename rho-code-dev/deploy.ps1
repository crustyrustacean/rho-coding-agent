#!/usr/bin/env pwsh
<#
.SYNOPSIS
  Deploy rho-code.dev to Cloudflare Pages.
.DESCRIPTION
  Builds the site with taxus and deploys the dist/ directory to Cloudflare Pages.
  Requires CLOUDFLARE_API_TOKEN and CLOUDFLARE_ACCOUNT_ID environment variables,
  or an authenticated wrangler session (wrangler login).

  Run from anywhere; the script changes to its own directory so the site
  builds correctly now that it lives in a subdirectory of the repo.
#>

$ErrorActionPreference = "Stop"

# The site lives in this directory. Resolve relative to the script, not the
# caller's cwd, so `taxus build` reads the right site.toml.
Push-Location $PSScriptRoot

try {
    # ── Build ──────────────────────────────────────────────────────────────
    Write-Host "==> Building site with taxus..." -ForegroundColor Cyan
    taxus build
    if ($LASTEXITCODE -ne 0) {
        Write-Error "Build failed."
        exit 1
    }

    # ── Deploy ─────────────────────────────────────────────────────────────
    # --branch must match the Pages project's production branch, set in the
    # Cloudflare dashboard (not this repo). It is `trunk`.
    Write-Host "==> Deploying to Cloudflare Pages..." -ForegroundColor Cyan
    npx wrangler pages deploy dist/ --project-name=rho-code-dev --branch=trunk
    if ($LASTEXITCODE -ne 0) {
        Write-Error "Deploy failed."
        exit 1
    }

    Write-Host "==> Done! Site deployed to https://rho-code.dev" -ForegroundColor Green
}
finally {
    Pop-Location
}