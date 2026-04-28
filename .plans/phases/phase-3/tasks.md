# Phase 3 Tasks

1. **Create the `rho-highlight` crate:**
   - Add `tree-sitter` and `tree-sitter-rust` as dependencies.
   - Implement a `parse(source: &str) -> Tree` function that returns a tree-sitter `Tree`.
   - Implement a `highlight(source: &str) -> Vec<HighlightSpan>` function that produces classified spans for Rust source. Theme/colour mapping (ANSI, ratatui style) is a Phase 4 concern; Phase 3 produces the classified spans only.
   - Implement a `node_at(tree: &Tree, line: usize, column: usize) -> Option<Node>` structural-query helper used by the diagnostic tools.
   - Cargo features defined now: `rust` as default, with `powershell`, `toml`, `json`, `markdown` as opt-in features (Phase 4 evaluates and may add the additional grammars).
   - Document the C-toolchain build dependency in the crate README.

2. **Add `CargoCheck` tool:**
   - Run `cargo check --message-format=json`.
   - Parse the NDJSON stream into structured `Diagnostic` types.
   - Return: error code, message, file, line, column, suggested replacements.
   - Filter to the relevant crate/project (not dependency noise).

3. **Add `CargoClippy` tool:**
   - Same as `CargoCheck` but with `cargo clippy --message-format=json`.
   - Include lint name and severity.

4. **Add `RustcExplain` tool:**
   - Run `rustc --explain E0XXX`.
   - Return the formatted explanation text.

5. **Add `CargoTest` tool:**
   - Run `cargo test --message-format=json`.
   - Parse test results: which passed, which failed, failure output.

6. **Add `CargoFix` tool:**
   - Run `cargo fix --allow-dirty` for machine-applicable suggestions.
   - Or: apply individual `MachineApplicable` suggestions from check/clippy output directly (more surgical).

7. **Define `rho-tools::rust` types** — these are part of the data model and should be designed with the same care as `rho-core` types:
   - `Diagnostic` — a structured compiler diagnostic.
   - `DiagnosticSpan` — file, line range, column range.
   - `DiagnosticSuggestion` — suggested replacement text for a span.
   - `TestResult` — pass/fail with output.
   - All types round-trip through serde, use newtypes where appropriate.

8. **Use `rho-highlight` to map diagnostic spans to AST nodes:**
   - When a diagnostic points to a span, query the tree-sitter tree for the enclosing syntax node.
   - Include the enclosing node type in the tool result (e.g., "this error is inside a `fn` item").
   - This gives the model richer context than raw line/column numbers.

9. **Add tree-sitter node-splitting validation to `EditFile`** (deferred from Phase 2):
   - Warn if a replacement would split a syntax node (e.g., replacing half a string literal).
   - Now that `rho-highlight` exists with structural queries, the validation can be integrated cleanly.
   - Ensure `EditFile` tests cover: syntax-node-splitting warning, split across node boundaries.

10. **Compose a Rust-aware system prompt extension:**
    - "You have access to structured Rust compiler diagnostics."
    - "When code fails to compile, use CargoCheck before attempting fixes."
    - "Trust machine-applicable suggestions from the compiler."

11. **Add a `CargoCheck` → `EditFile` → `CargoCheck` integration test loop.**

12. **Create the `rho-eval` behavioural benchmark suite:**
    - Define 10–20 canonical coding tasks (fix this compile error, refactor this function, add this test) with known correct outcomes.
    - Implement automated scoring: run each task, compare the agent's result against the expected outcome, produce a pass/fail report.
    - This provides a quantitative measure of agent quality that persists across phases.
    - **Prompt-version tracking:** each eval run records the SHA-256 hash of the base prompt and the full assembled system prompt that produced the results. The eval report includes these hashes:
      ```toml
      [run]
      prompt_base_sha256 = "..."
      prompt_composition_sha256 = "..."  # full assembled system prompt
      pass_rate = 14
      total = 20
      ```
    - **Regression gate:** CI fails when `pass_rate / total` drops by more than a configurable threshold (default: 2 tasks) versus the previous run on the same eval set. The failure output includes a diff of the two prompt hashes — and, if the base prompt changed, a diff of the prompt content — so the reviewer can see exactly what changed and why performance dropped. This makes prompt edits accountable: a "cleaner" rewording that quietly regresses agent behaviour is caught before it merges.

13. **Test suite audit:**
    - Promote `cargo * --message-format=json` output parsing into shared test helpers in `rho-test-helpers`.
    - Extract compiler message JSON fixtures (check, clippy, test) into `tests/fixtures/`.
    - Ensure `Diagnostic` / `DiagnosticSpan` / `DiagnosticSuggestion` types have round-trip serde tests.
    - Audit tree-sitter structural query tests — ensure they cover edge cases (empty files, malformed syntax, multi-byte characters).
    - Review test naming: adopt a consistent convention (e.g., `deserializes_X`, `executes_X_correctly`, `rejects_invalid_X`).
