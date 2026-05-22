# Model Validation Improvements

## Current Problems

Looking at the code, there are three concrete gaps in model handling:

### 1. No validation of `--model` or config model

In [`resolve_model`](rho/src/app.rs), when a model is specified via `--model` or config, it's used verbatim with zero validation against the provider's `/v1/models` endpoint. A typo like `--model qwen3-8` (when the real ID is `qwen3-8b`) silently passes until the first API call returns a cryptic 404 from the server.

### 2. Auto-detect silently picks first model, no choice

When no model is specified, [`resolve_model`](rho/src/app.rs) queries all providers and grabs the first model. The user has no way to pick — they must restart with `--model` after manually discovering the ID.

### 3. No quick way to discover available models

To find available models, users must start rho, type `/models`, then restart. There's no `--list-models` flag.

## Proposed Improvements

### A. Validate model existence at startup (`app.rs`)

When `--model` or config specifies a model, query the provider(s) and verify it exists. If not, show available models and error out with a helpful message (including fuzzy suggestions).

### B. Interactive model selection when no model specified

When auto-detecting and multiple models are available, show a numbered list and let the user pick. Single-model auto-detect can still silently proceed.

### C. `--list-models` CLI flag (`cli.rs`)

A quick non-interactive way to discover models without starting a REPL.

### D. Fuzzy model suggestions on miss (`app.rs` or new `model_match` module)

When a specified model isn't found, use a simple string similarity metric to suggest likely alternatives.

## Where Changes Land

| Change | Crate | Files |
|---|---|---|
| A. Validate at startup | `rho` | `app.rs` (`resolve_model`) |
| B. Interactive selection | `rho` | `app.rs` (`resolve_model`) |
| C. `--list-models` flag | `rho` | `cli.rs`, `app.rs`, `main.rs` |
| D. Fuzzy suggestions | `rho` | new `model_match.rs` or inline in `app.rs` |

All changes are in the `rho` binary crate — no changes needed to `rho-core` since `ProviderRegistry::list_all_models()` and `ProviderRegistry::find_model()` already exist.

## Implementation Plan

- **A + D** first (biggest bang for the buck)
- **C** (`--list-models`) as a quick follow-up
- **B** (interactive selection) last (most UX polish)
