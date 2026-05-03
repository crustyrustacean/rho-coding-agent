# Test Scenarios

Prompt files for manual integration testing of `rho` via `--prompt-file`.

## Running

Run the **small test first** — it's faster and validates the methodology.
If it triggers eviction, the full test is a stronger reproducer.

```sh
# 1. Small amnesia test (6 files, ~30K tokens — quick validation)
RUST_LOG=rho=debug,info cargo run -p rho -- \
  --prompt-file test_scenarios/amnesia_test_small.md \
  --model google/gemma-4-26b-a4b \
  2>&1 | tee runs/amnesia-small-$(date +%s).log

# 2. Small test with reduced token budget (forces earlier eviction)
RUST_LOG=rho=debug,info cargo run -p rho -- \
  --prompt-file test_scenarios/amnesia_test_small.md \
  --model google/gemma-4-26b-a4b \
  --token-budget 8192 \
  2>&1 | tee runs/amnesia-small-tiny-budget-$(date +%s).log

# 3. Full amnesia test (20 files, ~63K tokens — strong reproducer)
RUST_LOG=rho=debug,info cargo run -p rho -- \
  --prompt-file test_scenarios/amnesia_test.md \
  --model google/gemma-4-26b-a4b \
  2>&1 | tee runs/amnesia-full-$(date +%s).log
```

## Amnesia Tests

These tests check whether the sliding window context manager evicts the
initial user message (containing a secret code) when tool-call results
push the conversation past the token budget.

### How it works

1. The prompt embeds a unique secret code (e.g. `MANGO-TANGO-4729`) in the
   first user turn.
2. The agent is instructed to read many source files (generating large
   tool-call results that accumulate in the context window).
3. After all reads, the agent is asked to recall the secret code.
4. The check is binary: did the final assistant reply contain the literal
   secret string? If yes → no amnesia. If no → amnesia detected.

### What to look for in logs

**Note:** Structured tracing output goes to `logs/rho.log`, not stderr.
Use `grep` on that file for the diagnostics below.

- `context window: turns evicted` — confirms the sliding window kicked in
- `dropped_user=` in the structured fields — confirms a user turn was evicted
  (the first user turn, containing the secret, is the oldest and evicted first)
- `Assistant:` in stdout — the final reply; check whether it contains the
  secret code
