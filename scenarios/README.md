# Prompt Scenarios

These scenarios exercise rho's Rust tooling features without user input.
Each scenario is a self-contained directory with:

- `setup.ps1` — creates a temporary Rust project with a known defect
- `prompt.txt` — the prompt file passed to `rho --prompt-file`
- `verify.ps1` — checks whether the agent fixed the issue

## Usage

```powershell
# From the rho-coding-agent repo root:
./scenarios/01-fix-type-mismatch/setup.ps1
cd $env:TEMP/rho-scenario-01
rho --prompt-file ../rho-coding-agent/scenarios/01-fix-type-mismatch/prompt.txt --ephemeral
./scenarios/01-fix-type-mismatch/verify.ps1
```

Or run all scenarios:

```powershell
./scenarios/run-all.ps1
```

## Scenarios

| # | Name | Tools Exercised |
|---|---|---|
| 01 | Fix type mismatch (E0308) | `cargo_check`, `edit_file` |
| 02 | Remove unused imports | `cargo_clippy`, `cargo_fix` |
| 03 | Explain error code | `cargo_check`, `rustc_explain`, `edit_file` |
| 04 | Fix and verify with tests | `cargo_check`, `edit_file`, `cargo_test` |
| 05 | Multi-error fix cycle | `cargo_check`, `edit_file` (iterative) |
