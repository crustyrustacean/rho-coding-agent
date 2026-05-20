# Phase 3.6 Tasks 🔜 Planned

**Estimated effort:** 3–5 days

Full plan in Obsidian: [[Phase 3.6 — crates.io Registry Research]]

---

### Task 1: Egress Utility Extraction

**Files:** `rho-core/src/egress.rs` (new), `rho-core/src/client.rs` (modified)

- [ ] Create `rho-core/src/egress.rs` with `check_egress()` function
- [ ] Add `RhoError::EgressDenied { host }` variant
- [ ] Refactor `LocalChatClient::check_egress()` to delegate to `check_egress()`
- [ ] Export from `rho-core/src/lib.rs`

**Tests:**
- `check_egress("localhost", default_config)` → `Ok(())`
- `check_egress("crates.io", { allowed_hosts: ["crates.io"] })` → `Ok(())`
- `check_egress("evil.com", default_config)` → `Err(EgressDenied)`

### Task 2: `CratesIoLookup` Tool — `info` Operation

**File:** `rho-tools/src/registry.rs` (new)

- [ ] Add `reqwest = { version = "0.13", features = ["json"] }` to `rho-tools/Cargo.toml`
- [ ] Create `CratesIoLookup` struct holding `reqwest::Client` and `EgressConfig`
- [ ] Implement `execute_info(crate_name: &str) -> Result<String>`
- [ ] `GET /api/v1/crates/{name}`
- [ ] Parse and format into `<crate info>` template

### Task 3: `search` Operation

- [ ] Implement `execute_search(query: &str) -> Result<String>`
- [ ] `GET /api/v1/crates?q={query}&per_page=10&sort=downloads`
- [ ] URL-encode query string
- [ ] Format ranked results

### Task 4: `versions` Operation

- [ ] Implement `execute_versions(crate_name: &str) -> Result<String>`
- [ ] `GET /api/v1/crates/{crate_name}/versions`
- [ ] Show latest 20 versions with yanked status

### Task 5: `deps` Operation

- [ ] Implement `execute_deps(crate_name: &str, version: Option<&str>) -> Result<String>`
- [ ] `GET /api/v1/crates/{crate_name}/{version}/dependencies`
- [ ] Group by kind: normal, dev, build
- [ ] Show feature-to-dependency mappings

### Task 6: Registration, Config, and System Prompt

- [ ] Register `CratesIoLookup` in `register_all()` with risk `Read`
- [ ] Tool constructor receives `EgressConfig` from `RhoConfig`
- [ ] Add system prompt instructions for when to use the tool

### Task 7: Integration Tests and Evaluation Scenario

- [ ] Add scenario: "Search crates.io for a lightweight HTTP client crate. Look up metadata and dependencies. Add to Cargo.toml and write a simple GET request. Verify it compiles."
