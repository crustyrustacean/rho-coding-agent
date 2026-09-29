# rho-code.dev

Source for the project's landing page, **[rho-code.dev](https://rho-code.dev)**,
built with [taxus](https://github.com/crustyrustacean/taxus) and deployed to
Cloudflare Pages.

It lives in this repository rather than its own so that site changes sit
alongside the code they describe. Commit site work separately from agent
changes — the deploy workflow is path-filtered, so a site-only commit triggers
a deploy and *not* the code CI suite.

## Layout

```
rho-code-dev/
├── site.toml      — site metadata: name, base_url, directory names
├── content/       — markdown pages and posts
├── templates/     — HTML templates (Tera)
├── styles/        — SCSS
├── static/        — assets copied verbatim
└── deploy.ps1     — local build + deploy
```

## Build locally

```sh
taxus build --dir rho-code-dev
```

Output lands in `rho-code-dev/dist/` and is gitignored.

## Deploy

Automated: pushing to `trunk` with changes under `rho-code-dev/` runs
[deploy-rho-code-dev.yml](.github/workflows/deploy-rho-code-dev.yml), which
builds taxus from source, builds the site, and pushes to Cloudflare Pages.

Manually:

```sh
cd rho-code-dev
./deploy.ps1
```

Needs `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID`, or an authenticated
`wrangler login` session.

## Notes

- The Cloudflare Pages project is `rho-code-dev`. It already exists, so the
  workflow only deploys to it and never creates one.
- The deploy pins `--branch=trunk` deliberately. The Pages **production branch is
  set in the Cloudflare dashboard**, not in this repo. It is `trunk` here,
  matching the workflow that has always deployed this site. A wrong value
  publishes to a *preview* URL, which looks like a successful deploy and isn't
  one — check the dashboard if the site 404s after a deploy.
- Taxus is built from `trunk` rather than installed, so the site build is
  cached and reproducible.
