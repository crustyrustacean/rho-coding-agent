# Benchmarking with `rho-bench`

`rho-bench` is a standalone binary that runs rho's eval tasks against one or more local models and produces structured comparison reports. It automates the "set up a broken project → run rho → check if it fixed it" loop, capturing timing and token metrics along the way.

## Quick start

```sh
# Build
cargo build -p rho-bench

# Run all 5 tasks against a single model
rho-bench --models qwen3-8b

# Compare multiple models side-by-side
rho-bench --models qwen3-6b,qwen3-32b,llama3-8b-instruct-q4
```

## CLI reference

```
rho-bench [OPTIONS]

Options:
  -m, --models <MODELS>           Comma-separated model IDs (default: auto-detect from server)
  -t, --tasks <TASKS>             Comma-separated task IDs (default: all tasks)
  -r, --repeats <REPEATS>         Repeats per (model, task) pair (default: 1)
      --endpoint <ENDPOINT>       Model API endpoint URL (default: localhost:1234)
  -o, --output <OUTPUT>           Output format: "table" or "json" (default: table)
      --results-dir <DIR>         Directory for JSON result files (default: bench-results/)
      --compact                   Use compact system prompt (for small-context models)
      --token-budget <TOKENS>     Context window token budget
      --max-iterations <N>        Max agent loop iterations per task (default: 32)
```

## How it works

For each `(model, task, repeat)` triple, `rho-bench`:

1. **Creates an isolated temp directory** — runs `cargo init --lib` and writes the task's initial source files (with known defects).
2. **Spins up an in-memory `Session`** — with the specified model, auto-approve policy, and full tool registry (`CargoCheck`, `EditFile`, etc.).
3. **Drives `run_loop`** — sends the task's user prompt to the agent. A `CountingClient` wrapper accumulates token usage across all requests.
4. **Verifies the result** — reads back the modified files from disk and calls the task's `verify()` function.
5. **Records metrics** — wall-clock time, prompt/completion tokens, agent iterations, and pass/fail verdict.
6. **Cleans up** — removes the temp directory.

### Architecture

```
rho-bench (binary)
├── main.rs          CLI parsing, model resolution, dispatch
├── harness.rs       CountingClient, BenchApprovalGate, per-task execution
├── comparison.rs    Terminal table formatting, multi-model breakdown
└── persistence.rs   JSON result files
```

The harness uses `rho-core` and `rho-tools` directly (not the `rho` binary), so it can capture `ModelUsage` statistics without parsing logs.

## Output formats

### Table (default)

```
╭───────────────────────────────────────────────────────────────╮
│                    rho-bench results                          │
╰───────────────────────────────────────────────────────────────╯

Model                            Pass    Fail    Err     Time      Tokens
──────────────────────────────── ────── ────── ────── ──────── ──────────
qwen3-8b                          4/5    1/5    0/5     42.3s      18.1k
qwen3-32b                         5/5    0/5    0/5     28.1s      22.4k

Task                                 qwen3-8b           qwen3-32b
──────────────────────────────────── ──────────────── ────────────────
fix_e0308_type_mismatch              ✅  8200ms  3it    ✅  5100ms  2it
fix_e0425_unresolved_name            ❌ 15000ms  5it    ✅  7200ms  3it
fix_unused_import                    ✅  5400ms  2it    ✅  4800ms  2it
add_missing_derive_debug             ✅  6100ms  2it    ✅  5500ms  2it
fix_mutable_borrow                   ✅  7600ms  3it    ✅  5500ms  2it
```

### JSON

```sh
rho-bench --models qwen3-8b --output json
```

Produces a JSON array of `EvalRun` objects, each containing:
- `model_id`, `timestamp`, prompt hashes
- `outcomes[]` — per-task `TaskOutcome` with `verdict`, `explanation`, and `metrics`

## Result persistence

Results are written to `bench-results/` (configurable via `--results-dir`):

| File | Purpose |
|---|---|
| `bench-results/latest.json` | Most recent run (always overwritten) |
| `bench-results/20260509-143052.json` | Timestamped archive (never overwritten) |

This makes it easy to compare across sessions:

```sh
# Compare latest against a baseline
diff bench-results/20260508-100000.json bench-results/latest.json
```

## Built-in tasks

| Task ID | Description | Tools exercised |
|---|---|---|
| `fix_e0308_type_mismatch` | Function returns `i32` but is declared `-> String` | `cargo_check`, `edit_file` |
| `fix_e0425_unresolved_name` | Function calls an undefined helper | `cargo_check`, `edit_file` |
| `fix_unused_import` | Remove an unused `use std::io` import | `cargo_clippy`, `cargo_fix` |
| `add_missing_derive_debug` | Struct needs `#[derive(Debug)]` for `format!("{:?}", …)` | `cargo_check`, `edit_file` |
| `fix_mutable_borrow` | Variable needs `let mut` to call `.push()` | `cargo_check`, `edit_file` |

## Writing new tasks

Tasks implement the `EvalTask` trait from `rho-eval`:

```rust
// In rho-eval/src/tasks.rs

struct MyNewTask;

impl EvalTask for MyNewTask {
    fn id(&self) -> &str { "my_new_task" }
    fn name(&self) -> &str { "My New Task" }
    fn description(&self) -> &str { "Fix the off-by-one error in the loop." }
    fn initial_files(&self) -> Vec<(&str, &str)> {
        vec![("src/lib.rs", "pub fn sum(n: usize) -> usize {\n    (0..n).sum()\n}\n")]
    }
    fn user_prompt(&self) -> &str { "Fix the bug in src/lib.rs." }
    fn verify(&self, files: &[(&str, &str)]) -> TaskOutcome {
        // Check that the fix is correct
        let lib = files.iter().find(|(p, _)| *p == "src/lib.rs");
        match lib {
            Some((_, content)) if content.contains("0..=n") => {
                TaskOutcome::new(self.id(), self.name(),
                    TaskVerdict::Pass, "off-by-one fixed")
            }
            Some((_, content)) => {
                TaskOutcome::new(self.id(), self.name(),
                    TaskVerdict::Fail, format!("unexpected content: {content}"))
            }
            None => {
                TaskOutcome::new(self.id(), self.name(),
                    TaskVerdict::Error, "src/lib.rs not found")
            }
        }
    }
}
```

Register it in `all_tasks()` and it's automatically available to `rho-bench`.

## Tips for reliable benchmarks

- **Temperature 0** — quantized local models are inherently non-deterministic, but loading at temperature 0 minimizes variance. Use `--repeats 3+` and average.
- **Isolate the server** — close other applications that compete for GPU memory during benchmarks.
- **Warm up** — run a single task once before the timed sweep to populate model caches.
- **Compact prompts for small models** — use `--compact` for models with < 8K context windows.
- **Match real usage** — set `--token-budget` to the same value you use in production.
