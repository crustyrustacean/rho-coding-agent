# Model Resolution — Design Notes

## Current approach (v0.72)

rho uses **config-as-truth** model resolution. No network calls at startup; the config file is the source of truth.

Resolution priority:
1. `--model <id>` (CLI flag)
2. `agent.model` (config)
3. `agent.provider` → that provider's `default_model` (config)
4. First provider's `default_model` (config fallback)

Misconfiguration surfaces as a clear HTTP error at request time (e.g. 404 for a typo'd model ID).

## What changed from the old approach

The old approach queried `/v1/models` at startup to "auto-detect" the model. This was removed because:
- External providers may return large or non-standard model lists
- Added latency to every startup even with a config file
- Broke the principle of "config is truth, validate at request time"

The `/v1/models` endpoint is still called at **runtime** for:
- `/models` RPC command (list available models)
- `/model <id>` command (switch models mid-session, with provider auto-detection)

## Future improvements

- **`--list-models` CLI flag** — non-interactive way to discover models without starting a session
- **Fuzzy suggestions** — when a model ID returns 404, suggest similar model names from `/v1/models`
- **Model aliases** — short names (e.g. `sonnet` → `anthropic/claude-sonnet-4`) via `~/.rho/models.toml`
