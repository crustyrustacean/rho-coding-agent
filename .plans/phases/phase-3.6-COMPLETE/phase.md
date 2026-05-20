# Phase 3.6: crates.io Registry Research 🔜 Planned

**Goal:** Give the agent the ability to research crates on crates.io — look up crate metadata, search by keyword, inspect version history, and examine dependency trees — so it can recommend and evaluate third-party crates when solving problems.

**Milestone:** A new `CratesIoLookup` tool with four operations (info, search, versions, deps) that queries the crates.io API through the egress allowlist.

**Depends on:** Phase 3, Phase 3.5 (optional) | **Effort:** 3–5 days

**Full plan (Obsidian):** [[Phase 3.6 — crates.io Registry Research]]

## Motivation

When the model needs a capability not in the standard library (e.g., HTTP clients, serialization, regex), it currently relies entirely on its training data. Failure modes include:
- Stale knowledge — deprecated/superseded crate recommendations
- Wrong version info — latest version in training data may be months old
- No dependency awareness — can't check compatibility
- No reputation signal — can't distinguish popular from abandoned

## Security Architecture

This feature introduces outbound HTTP to `crates.io`, conflicting with rho's defense-in-depth model.

### Layer 1: Dedicated Rust Tool (Not a Shell Escape)
The tool uses `reqwest` directly — it does **not** shell out to `curl` or `wget`. The model never controls the URL; it passes structured parameters mapped to known API paths.

### Layer 2: Egress Allowlist (Opt-In)
The tool respects the existing egress allowlist. Requests to non-allowed hosts are refused before any HTTP is sent. User must opt in via `[egress] allowed_hosts = ["crates.io", "static.crates.io"]`.

### Layer 3: Read-Only API
All crates.io API endpoints used are read-only (`GET`). Risk level is `Read`.

## Design

### Tool: `CratesIoLookup`

**Risk level:** `Read` (auto-approved)

**New file:** `rho-tools/src/registry.rs`

**New dependency:** `reqwest` added to `rho-tools/Cargo.toml`

#### Operations
- `info` — crate metadata (name, version, downloads, license, repo, docs, keywords)
- `search` — search by keyword, ranked by downloads
- `versions` — version history with yanked status
- `deps` — dependency tree grouped by kind (normal, dev, build) with feature mappings

#### Output Framing
All output wrapped in `<crate ...>` tags (consistent with `<context>` framing).

## Exit Criteria
- [ ] `crates_io_lookup` tool performs all four operations (info, search, versions, deps)
- [ ] Egress allowlist enforced before any HTTP request
- [ ] Clear error messages when egress not configured
- [ ] All existing tests pass
- [ ] New tool has ≥ 15 unit tests
- [ ] Egress utility extracted and shared with `LocalChatClient`

## Future Work (Beyond Phase 3.6)
- `crates_add` tool — `cargo add` to add dependencies (Write risk)
- `docs_rs_lookup` — fetched rendered documentation
- Offline caching of crates.io responses
- Workspace-aware search
