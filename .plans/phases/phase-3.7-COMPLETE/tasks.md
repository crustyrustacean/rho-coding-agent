# Phase 3.7 Tasks ✅ ALL COMPLETE

**Completed:** 2026-05-09 | **Version:** 0.31.0

---

### Task 1: Enrich `rho-eval` Types

- [x] Create `TaskMetrics` struct with duration, token counts, iterations
- [x] Add `metrics: TaskMetrics` field to `TaskOutcome`
- [x] Add `model_id`, `timestamp` fields to `EvalRun`
- [x] Add aggregate helper methods to `EvalRun`
- [x] All new fields use `#[serde(default)]` for backward compatibility

### Task 2: Create `rho-bench` Crate

- [x] `cargo new rho-bench` with `clap`, `async-trait`, `anyhow`, `chrono`, `serde_json` deps
- [x] Create module structure: `main.rs`, `harness.rs`, `comparison.rs`, `persistence.rs`

### Task 3: `CountingClient` Implementation

- [x] Wrap `LocalChatClient` with `AtomicU32` counters
- [x] `chat()` atomically increments prompt/completion tokens
- [x] `snapshot()` returns current totals
- [x] Thread-safe for future parallel execution

### Task 4: `BenchApprovalGate`

- [x] Always returns `true`
- [x] Combined with `AutoApprovePolicy` for non-interactive runs

### Task 5: Isolated Temp Project Execution

- [x] Each `(model, task, repeat)` gets a fresh temp dir
- [x] `cargo init --lib` then populate with task `initial_files()`
- [x] After agent runs, read files back and pass to `task.verify()`
- [x] Clean up temp dir after run

### Task 6: CLI and Options

- [x] `--models` comma-separated model list
- [x] `--tasks` comma-separated task filter
- [x] `--repeats` for reliability
- [x] `--endpoint` custom endpoint
- [x] `--output` table/json
- [x] `--results-dir` persistence directory
- [x] `--compact`, `--token-budget`, `--max-iterations`

### Task 7: Output Formats

- [x] Terminal table: summary + per-task breakdown across models
- [x] JSON output as `EvalRun` array for CI pipelines

### Task 8: Result Persistence

- [x] `bench-results/latest.json` — always overwritten
- [x] `bench-results/YYYYMMDD-HHMMSS.json` — timestamped, never overwritten

### Task 9: Documentation

- [x] `docs/src/development/benchmarking.md` — full usage guide
- [x] Updated architecture docs, dependency flow, crate responsibilities
- [x] Updated `.gitignore` for `bench-results/`
