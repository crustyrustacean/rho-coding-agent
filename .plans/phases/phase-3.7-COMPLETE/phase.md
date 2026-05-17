# Phase 3.7: Multi-Model Benchmark Harness ✅ COMPLETE

**Goal:** Provide a structured benchmarking tool that evaluates rho's coding ability across multiple local models, capturing not just pass/fail but performance metrics (wall time, token usage, agent iterations) and persisting results for comparison over time.

**Milestone:** `rho-bench` binary drives `rho-eval` tasks through the agent loop programmatically, captures metrics via `CountingClient`, and persists structured results as JSON.

**Depends on:** Phase 3 (specifically `rho-eval`) | **Effort:** 1 day

**Full plan (Obsidian):** [[Phase 3.7 — Multi-Model Benchmark Harness]]

## What Was Delivered

### 1. Enriched `rho-eval` Types

- `TaskMetrics` — duration_ms, token_input, token_output, agent_iterations, finish_reason
- `TaskOutcome` — now carries optional `metrics: TaskMetrics`
- `EvalRun` — added `model_id`, `timestamp`, aggregate helpers (`total_duration_ms`, `total_token_input`, etc.)

### 2. `rho-bench` Binary

**New crate:** `rho-bench/`

```
rho-bench/src/
├── main.rs          CLI parsing, model resolution, dispatch
├── harness.rs       CountingClient, BenchApprovalGate, per-task execution
├── comparison.rs    Terminal table and multi-model breakdown display
└── persistence.rs   JSON result files
```

### 3. Key Components

- **`CountingClient`** — Wraps `LocalChatClient` with `AtomicU32` counters for prompt/completion tokens. Zero-overhead and thread-safe.
- **`BenchApprovalGate`** — Always returns `true` for non-interactive benchmark runs.
- **Isolated temp projects** — Each `(model, task, repeat)` triple gets a fresh `cargo init --lib` temp directory.
- **Result persistence** — `bench-results/latest.json` (overwritten) + timestamped archives.

### 4. CLI

```
rho-bench [OPTIONS]
  -m, --models <MODELS>           Comma-separated model IDs
  -t, --tasks <TASKS>             Comma-separated task IDs
  -r, --repeats <REPEATS>         Repeats per (model, task) pair
      --endpoint <ENDPOINT>       Model API endpoint (default: localhost:1234)
  -o, --output <OUTPUT>           "table" or "json" (default: table)
      --results-dir <DIR>         JSON output directory (default: bench-results/)
      --compact                   Use compact system prompt
      --token-budget <TOKENS>     Context window token budget
      --max-iterations <N>        Max agent iterations per task
```

## Exit Criteria ✅

- [x] `rho-bench` binary exists and runs eval tasks
- [x] `CountingClient` captures token counts
- [x] Multi-model sweeps via `--models` flag
- [x] Terminal table output with per-task breakdown
- [x] JSON output for CI
- [x] Timestamped result persistence
- [x] All existing tests pass (no regressions)
- [x] 8 unit tests in `rho-bench`, 26 in `rho-eval`
