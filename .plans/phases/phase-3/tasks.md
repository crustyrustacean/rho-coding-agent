# Phase 3 Tasks

1. **Add `CargoCheck` tool:**
   - Run `cargo check --message-format=json`
   - Parse the NDJSON stream into structured `Diagnostic` types
   - Return: error code, message, file, line, column, suggested replacements
   - Filter to the relevant crate/project (not dependency noise)

2. **Add `CargoClippy` tool:**
   - Same as `CargoCheck` but with `cargo clippy --message-format=json`
   - Include lint name and severity

3. **Add `RustcExplain` tool:**
   - Run `rustc --explain E0XXX`
   - Return the formatted explanation text

4. **Add `CargoTest` tool:**
   - Run `cargo test --message-format=json`
   - Parse test results: which passed, which failed, failure output

5. **Add `CargoFix` tool:**
   - Run `cargo fix --allow-dirty` for machine-applicable suggestions
   - Or: apply individual `MachineApplicable` suggestions from check/clippy output directly (more surgical)

6. **Define `rho-tools::rust` types** — these are part of the data model and should be designed with the same care as `rho-core` types:
   - `Diagnostic` — a structured compiler diagnostic
   - `DiagnosticSpan` — file, line range, column range
   - `DiagnosticSuggestion` — suggested replacement text for a span
   - `TestResult` — pass/fail with output
   - All types round-trip through serde, use newtypes where appropriate

7. **Use `rho-highlight` to map diagnostic spans to AST nodes:**
   - When a diagnostic points to a span, query the tree-sitter tree for the enclosing syntax node
   - Include the enclosing node type in the tool result (e.g., "this error is inside a `fn` item")
   - This gives the model richer context than raw line/column numbers

8. **Add tree-sitter node-splitting validation to `EditFile`** (deferred from Phase 2):
   - Warn if a replacement would split a syntax node (e.g., replacing half a string literal)
   - This was deferred from Phase 2 to keep the initial feedback loop fast; now that `rho-highlight` is more mature with structural queries, it can be integrated cleanly
   - Ensure `EditFile` tests cover: syntax-node-splitting warning, split across node boundaries

9. **Compose a Rust-aware system prompt extension:**
   - "You have access to structured Rust compiler diagnostics."
   - "When code fails to compile, use CargoCheck before attempting fixes."
   - "Trust machine-applicable suggestions from the compiler."

10. **Add a `CargoCheck` → `EditFile` → `CargoCheck` integration test loop.**

11. **Create the `rho-eval` behavioural benchmark suite:**
    - Define 10–20 canonical coding tasks (fix this compile error, refactor this function, add this test) with known correct outcomes
    - Implement automated scoring: run each task, compare the agent's result against the expected outcome, produce a pass/fail report
    - This provides a quantitative measure of agent quality that persists across phases

12. **Test suite audit:**
    - Promote `cargo * --message-format=json` output parsing into shared test helpers in `rho-test-helpers`
    - Extract compiler message JSON fixtures (check, clippy, test) into `tests/fixtures/`
    - Ensure `Diagnostic` / `DiagnosticSpan` / `DiagnosticSuggestion` types have round-trip serde tests
    - Audit tree-sitter structural query tests — ensure they cover edge cases (empty files, malformed syntax, multi-byte characters)
    - Review test naming: adopt a consistent convention (e.g., `deserializes_X`, `executes_X_correctly`, `rejects_invalid_X`)
