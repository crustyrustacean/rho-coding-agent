# Phase 3.12 Tasks

**Status:** 🔜 Planned

---

## Phase A — Provider Registry & Multi-Provider Config

### 1. Extend `ProviderConfig` with `name` field

**File:** `rho-core/src/config.rs`

Add an optional `name` field to `ProviderConfig`:

```rust
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ProviderConfig {
    /// Provider name for display and selection (e.g. "local", "openrouter").
    /// Used by `/model` for disambiguation when multiple providers have the same model.
    /// Defaults to provider index ("0", "1", ...) if not set.
    #[serde(default)]
    pub name: Option<String>,
    /// Provider type label — informational only.
    #[serde(default)]
    pub r#type: Option<String>,
    /// API endpoint URL.
    #[serde(default)]
    pub endpoint: Option<String>,
    /// Environment variable name holding the API key.
    #[serde(default)]
    pub api_key_env: Option<String>,
}
```

- No breaking change — `name` is optional with `#[serde(default)]`
- Update `resolve_api_key()` to work on a single `ProviderConfig` instance instead of `RhoConfig`

**Tests:**
- Deserialize config with `name` field
- Deserialize config without `name` field (default `None`)

### 2. Create `ProviderSettings` wrapper

**File:** `rho-core/src/config.rs`

```rust
/// Multi-provider configuration.
///
/// Wraps an ordered list of [`ProviderConfig`] entries. The first entry is
/// the default provider used by `--endpoint` and `--api-key-env` overrides.
#[derive(Clone, Debug, Default)]
pub struct ProviderSettings {
    pub providers: Vec<ProviderConfig>,
}

impl ProviderSettings {
    /// The default (first) provider, or `None` if empty.
    pub fn default_provider(&self) -> Option<&ProviderConfig> {
        self.providers.first()
    }
}
```

**Tests:**
- Empty settings → `default_provider()` returns `None`
- Single provider → `default_provider()` returns it
- Multiple providers → `default_provider()` returns first

### 3. Add dual-path `WireConfig` deserialization

**File:** `rho-core/src/config.rs`

Support both legacy and new config formats in the same file:

```rust
#[derive(Clone, Debug, Default, Deserialize)]
struct WireConfig {
    // ... existing fields ...

    /// Legacy single-provider config (`[provider]`).
    #[serde(default)]
    provider: Option<ProviderConfig>,

    /// New multi-provider config (`[[providers]]`).
    #[serde(default)]
    providers: Option<Vec<ProviderConfig>>,

    // ... rest unchanged ...
}
```

Note: `toml` handles `[[providers]]` as an array-of-tables. The field name `providers` (plural) distinguishes it from `provider` (singular, legacy).

**Tests:**
- Deserialize legacy `[provider]` → `WireConfig.provider` is `Some`
- Deserialize `[[providers]]` → `WireConfig.providers` is `Some`
- Both present → `providers` takes precedence
- Neither present → both `None`

### 4. Update `ConfigLoader::merge()` for `ProviderSettings`

**File:** `rho-core/src/config.rs`

Merge logic for the new `ProviderSettings`:

```rust
provider: {
    let pp = project.providers.unwrap_or_default();
    let up = user.providers.unwrap_or_default();

    // New format takes precedence over legacy.
    let entries = if !pp.is_empty() {
        pp
    } else if !up.is_empty() {
        up
    } else {
        // Fall back to legacy single-provider config.
        let legacy = project.provider.or(user.provider);
        match legacy {
            Some(p) => vec![p],
            None => vec![],
        }
    };

    ProviderSettings { providers: entries }
}
```

This means:
- Project `[[providers]]` > user `[[providers]]` > project `[provider]` > user `[provider]` > empty
- Project-level `[[providers]]` **replaces** user-level `[[providers]]` (same as other `Vec` fields — no appending)

**Tests:**
- Merge user `[[providers]]` with no project → user entries preserved
- Merge project `[[providers]]` over user `[[providers]]` → project wins (replacement)
- Merge legacy `[provider]` with no `[[providers]]` → promoted to single-element vec
- Merge project `[[providers]]` over user legacy `[provider]` → project wins
- Merge both legacy and new → new wins

### 5. Implement `ProviderRegistry`

**File:** `rho-core/src/provider.rs`

```rust
/// A named collection of model providers.
///
/// Owns one or more `Box<dyn Provider>` instances. Provides lookup by name,
/// cross-provider model discovery, and default provider selection.
///
/// Constructed via [`ProviderRegistry::from_config`] from [`ProviderSettings`].
pub struct ProviderRegistry {
    providers: Vec<Box<dyn Provider>>,
}

impl ProviderRegistry {
    /// Construct a registry from config with optional CLI overrides.
    ///
    /// CLI `--endpoint` and `--api-key-env` override the **default (first)**
    /// provider's endpoint and API key. Additional providers use their
    /// configured values.
    ///
    /// If no providers are configured, returns an empty registry.
    pub fn from_config(
        settings: &ProviderSettings,
        endpoint_override: Option<&str>,
        api_key_env_override: Option<&str>,
    ) -> Self { ... }

    /// All registered providers.
    pub fn providers(&self) -> &[Box<dyn Provider>] { ... }

    /// Find a provider by name (exact match).
    pub fn get(&self, name: &str) -> Option<&dyn Provider> { ... }

    /// The default (first) provider.
    ///
    /// # Panics
    ///
    /// Panics if the registry is empty. Callers should check `is_empty()`
    /// or handle the error from `from_config`.
    pub fn default(&self) -> &dyn Provider { ... }

    /// Whether the registry has any providers.
    pub fn is_empty(&self) -> bool { ... }

    /// Collect all models across all providers.
    ///
    /// Queries each provider's `list_models()` endpoint. Returns a vec of
    /// `(provider_name, ModelInfo)` pairs for disambiguation.
    ///
    /// Providers that are unreachable are silently skipped (with a warning
    /// logged). Returns an empty vec if all providers fail.
    pub async fn list_all_models(&self) -> Vec<(&str, ModelInfo)> { ... }

    /// Find which provider has a given model ID.
    ///
    /// Searches providers in order. Returns the first provider that has
    /// a model matching the given ID.
    pub async fn find_model(&self, model_id: &str) -> Option<(&dyn Provider, ModelInfo)> { ... }
}
```

**Key design decisions:**
- `list_all_models()` silently skips unreachable providers (with `tracing::warn!`) rather than failing — a misconfigured secondary provider shouldn't block the whole app
- `from_config()` constructs an `OpenAiCompatibleProvider` for each entry — all current providers are OpenAI-compatible
- CLI overrides only affect the default provider — this matches the existing `--endpoint` UX

**Tests:**
- Empty settings → empty registry
- Single provider → `default()` returns it, `is_empty()` is false
- Multiple providers → `get("openrouter")` finds by name, `get("nonexistent")` returns `None`
- CLI endpoint override → applied to default provider only
- `list_all_models()` — mock/stub test (requires async, may need test helpers)

### 6. Update `App` to hold `ProviderRegistry`

**File:** `rho/src/app.rs`

```rust
pub struct App {
    pub(crate) session: Session,
    pub(crate) registry: ProviderRegistry,  // was: provider: Box<dyn Provider>
    pub(crate) registry: ToolRegistry,
    // ... rest unchanged ...
}
```

**Changes in `App::build()`:**
- Step 4: `provider_factory()` → `ProviderRegistry::from_config()`
- Step 6: `check_provider_consent(provider.as_ref(), &cli)` → `check_provider_consent(&registry, &cli)`
- Step 10: `resolve_model(&config, cli.model.as_ref(), provider.as_ref())` → `resolve_model(&config, cli.model.as_ref(), &registry)`

**Empty registry handling:**
- If `from_config()` returns empty registry and no `--endpoint` override, fall back to `provider_factory()` for a default localhost provider (maintains zero-config UX)
- Or: `from_config()` always includes a default localhost entry if the vec is empty

**Recommendation:** `from_config()` appends a default localhost provider when the settings are empty. This matches the current behavior (no config → localhost:1234).

### 7. Update `resolve_model()` for multi-provider

**File:** `rho/src/app.rs`

```rust
async fn resolve_model(
    config: &RhoConfig,
    cli_model: Option<&String>,
    registry: &ProviderRegistry,
) -> Result<String> {
    // 1. CLI flag (unchanged).
    // 2. Config (unchanged).
    // 3. Auto-detect across all providers.
    let all_models = registry.list_all_models().await;
    if all_models.is_empty() {
        anyhow::bail!("no models available from any provider. ...");
    }
    let (provider_name, model) = &all_models[0];
    eprintln!("auto-detected model: {} (from provider: {})", model.id, provider_name);
    Ok(model.id.clone())
}
```

### 8. Consolidate external provider consent

**File:** `rho/src/app.rs`

```rust
fn check_provider_consent(registry: &ProviderRegistry, cli: &Cli) -> Result<()> {
    let external: Vec<&str> = registry.providers()
        .iter()
        .filter(|p| p.is_external())
        .map(|p| p.name())
        .collect();

    if external.is_empty() || cli.accept_external_provider || cli.endpoint.is_some() {
        return Ok(());
    }

    eprintln!();
    eprintln!("  ⚠  External provider(s) detected");
    eprintln!("      Providers: {}", external.join(", "));
    eprintln!();
    eprintln!("      Your prompts and code will be sent to external servers.");
    // ... rest of consent prompt ...
}
```

Single prompt for all external providers instead of one per provider.

### 9. Update `repl.rs` call sites

**File:** `rho/src/repl.rs`

Two changes (Phase A):
- `app.provider.chat_client()` → `app.registry.default().chat_client()` in `run_repl`
- `app.provider.chat_client()` → `app.registry.default().chat_client()` in `run_prompt_file`

Phase B will change these to `app.active_provider().chat_client()`.

### 10. Update re-exports in `rho-core/src/lib.rs`

Add:
```rust
pub use config::ProviderSettings;
pub use provider::ProviderRegistry;
```

Remove or keep `provider_factory` (deprecated but public).

### 11. Add `ProviderRegistry` unit tests

**File:** `rho-core/src/provider.rs` (in `#[cfg(test)]` block)

- `registry_empty_settings` — empty vec → empty registry
- `registry_single_provider` — one entry → default() works
- `registry_multiple_providers` — three entries → get() finds by name
- `registry_get_nonexistent` — returns None
- `registry_from_config_default_localhost` — empty settings → default localhost entry
- `registry_from_config_endpoint_override` — override applied to first provider only
- `registry_from_config_api_key_override` — override applied to first provider only

### 12. Add config tests for `[[providers]]`

**File:** `rho-core/src/config.rs` (in `#[cfg(test)]` block)

- `load_multi_provider_config` — deserialize `[[providers]]` format
- `load_legacy_provider_config` — existing test still passes
- `legacy_promoted_to_vec` — `[provider]` → `ProviderSettings { providers: [..] }`
- `providers_project_replaces_user` — project `[[providers]]` replaces user `[[providers]]`
- `providers_with_names` — deserialize `name` field
- `providers_with_all_fields` — full config with name, type, endpoint, api_key_env

### 13. Deprecate `provider_factory()`

**File:** `rho-core/src/provider.rs`

Add deprecation notice to doc comment:
```rust
/// Construct a single [`OpenAiCompatibleProvider`] from [`RhoConfig`].
///
/// **Deprecated:** Use [`ProviderRegistry::from_config`] for new code.
/// This function is preserved for backward compatibility with
/// `rho-bench` and test code.
#[deprecated(
    since = "0.42.0",
    note = "use ProviderRegistry::from_config() for multi-provider support"
)]
pub fn provider_factory(...) -> Box<dyn Provider> { ... }
```

Also deprecate `client_factory()`, `is_local_endpoint()`, `resolve_api_key()` on `RhoConfig` (keep as free functions for backward compat).

### 14. Update `RhoConfig` field

**File:** `rho-core/src/config.rs`

```rust
pub struct RhoConfig {
    pub agent: AgentLoopConfig,
    pub provider: ProviderSettings,  // was: ProviderConfig
    // ... rest unchanged ...
}
```

Update all `config.provider.endpoint` → `config.provider.default_provider().and_then(|p| p.endpoint.as_deref())` or add a convenience accessor.

**Convenience accessors on `ProviderSettings`:**
```rust
impl ProviderSettings {
    /// The default provider's endpoint, if configured.
    pub fn default_endpoint(&self) -> Option<&str> { ... }

    /// The default provider's API key env var, if configured.
    pub fn default_api_key_env(&self) -> Option<&str> { ... }
}
```

**Impact analysis — all `config.provider.xxx` references:**
- `rho-core/src/client/mod.rs` line ~569: `config.provider.endpoint.clone()` → `config.provider.default_endpoint()`
- `rho-core/src/client/mod.rs` line ~582: `config.provider.api_key_env.as_deref()` → `config.provider.default_api_key_env()`
- `rho/src/app.rs`: accesses via `ProviderRegistry::from_config()` — no direct config.provider access
- `rho-bench/src/main.rs`: uses `provider_factory()` — no direct config.provider access
- Config tests: update field access

### 15. Run full CI

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace
cargo test --workspace
```

All four stages must pass.

---

## Phase B — In-Session Model Switching (next PR)

### B.1. Add `App.active_provider_index` tracking

**File:** `rho/src/app.rs`

```rust
pub struct App {
    pub(crate) registry: ProviderRegistry,
    pub(crate) active_provider_index: usize,  // NEW
    // ...
}
```

Initialize to `0` (default provider). Updated by `/model` command.

### B.2. Implement `/models` REPL command

**File:** `rho/src/repl.rs`

Add a match arm in the REPL loop:

```rust
"/models" => {
    let all = app.registry.list_all_models().await;
    if all.is_empty() {
        println!("No models available.");
    } else {
        for (provider_name, model) in &all {
            println!("  {}/{}", provider_name, model.id);
        }
    }
    continue;
}
```

### B.3. Implement `/model <id>` with fuzzy matching

**File:** `rho/src/repl.rs`

```rust
"/model" | arg if arg.starts_with("/model ") => {
    let query = arg.strip_prefix("/model ").unwrap().trim();
    // Try exact provider/model syntax first
    if let Some((provider, model)) = query.split_once('/') {
        if let Some(p) = app.registry.get(provider) {
            if let Some((_, info)) = p.find_model_by_id(model).await {
                switch_provider(&mut app, p, &info.id);
                continue;
            }
        }
    }
    // Fuzzy match across all providers
    let all = app.registry.list_all_models().await;
    let matches: Vec<_> = all.iter()
        .filter(|(_, m)| m.id.contains(query))
        .collect();
    match matches.len() {
        0 => println!("No model matching \"{}\".", query),
        1 => {
            let (name, info) = matches[0];
            let provider = app.registry.get(name).unwrap();
            switch_provider(&mut app, provider, &info.id);
        }
        _ => {
            println!("Ambiguous. Matches:");
            for (name, info) in &matches {
                println!("  {}/{}", name, info.id);
            }
        }
    }
    continue;
}
```

### B.4. Implement `/model <provider>/<id>` explicit selection

Covered by B.3 (split on `/` first).

### B.5. Session model/provider update integration

```rust
fn switch_provider(app: &mut App, provider: &dyn Provider, model_id: &str) {
    // Find the provider's index in the registry.
    let idx = app.registry.providers().iter()
        .position(|p| p.name() == provider.name())
        .unwrap();
    app.active_provider_index = idx;
    app.session.set_model(model_id);
    println!("Switched to {} (provider: {})", model_id, provider.name());
}
```

### B.6. Tests for model switching

- Unit test for `ProviderRegistry::find_model()` (exact match, no match, first-match semantics)
- Integration test for `/models` output format
- Integration test for `/model` exact match
- Integration test for `/model` fuzzy match (single result, ambiguous, no match)
- Integration test for `/model provider/id` explicit syntax

---

## Testing Strategy

### Unit Tests

- `ProviderRegistry` construction, lookup, default selection
- `ProviderSettings` convenience accessors
- Config deserialization (legacy, new, mixed, empty)
- Config merging (project overrides user, legacy promotion)

### Integration Tests

- Full `App::build()` with multi-provider config
- `resolve_model()` across multiple providers
- Consent prompt with multiple external providers
- CLI `--endpoint` override with multi-provider config

### Regression Tests

- All existing tests pass (single-provider users unaffected)
- `rho-bench` continues to work with `provider_factory()`
- Legacy `[provider]` config format works unchanged

---

## Success Criteria

### Phase A

1. ✅ `[[providers]]` array in config with `name`, `type`, `endpoint`, `api_key_env`
2. ✅ Legacy `[provider]` transparently promoted
3. ✅ `ProviderRegistry` with `from_config()`, `default()`, `get()`, `list_all_models()`
4. ✅ `App` holds `ProviderRegistry`, auto-detect queries all providers
5. ✅ Single consent prompt for all external providers
6. ✅ `--endpoint`/`--api-key-env` override default provider
7. ✅ All tests pass, new registry + config tests added
8. ✅ `rho-bench` unchanged
9. ✅ Zero behavior change for single-provider users

### Phase B

1. ✅ `/models` lists all models with provider prefix
2. ✅ `/model <id>` fuzzy-matches and switches
3. ✅ `/model <provider>/<id>` explicit selection
4. ✅ Session updated without restart
5. ✅ Ambiguous matches handled gracefully
