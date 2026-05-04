# Phase 5 Tasks

1. **Create the `rho-ext` crate.**

2. **Define the extension API:**
   - `ExtTool` trait — user-defined tools with name, description, schema, and execute
   - Initial implementation: tools defined in TOML config files (command + args template)
   - Example: a `DockerRun` tool that runs `docker run` with the provided arguments
   - **Extension argument safety** — command templates use structured argument substitution. Each argument is passed as a separate parameter to `Command::arg()`, never shell-interpolated into a single string. This prevents the most common injection vector where model-provided arguments escape the intended command structure.
   - Custom slash commands — extensions can register commands that appear in the TUI's slash-command autocomplete and invoke extension-provided logic

3. **Extend project prompt trust:** (Phase 1b already implements core trust model; this task extends it)
   - Review trust model coverage: are there new context-file patterns (e.g., `.claude/rules/`, `.github/copilot-instructions.md`) that should be added to the scan list?
   - Tool descriptions from extensions are auditable — the TUI shows what each registered tool's description says

4. **Extend project-level configuration** (`.rho/config.toml` — config loading already exists from Phase 2):
   - Custom tool definitions
   - Enabled/disabled tools
   - Approval policies — already loaded from Phase 2, now extended with tool-specific rules for extension tools

5. **Extend global configuration** (`~/.rho/config.toml` — already loaded from Phase 2):
   - Theme preferences
   - Keybindings

6. **Prompt composition with budget awareness** (the full composition chain, building on Phase 1's project context file scanning):
   - Base identity prompt
   + Project context files (in scan-list order: `AGENTS.md`, `.agents.md`, `CLAUDE.md`, `.cursorrules`, `.rho/prompt.md`) — each as a clearly delimited section in the system prompt
   + PowerShell guidance
   + Rust tooling guidance
   + Extension tool descriptions (auto-generated from the registry)
   + Tool schemas (auto-generated from the registry)
   - Precedence: the base identity prompt is always first and cannot be overridden by a context file. Context files are instructions the user intentionally placed in the project; they extend but do not replace the agent's core identity.
   - **Budget-aware composition:** measure the token cost of each layer (base prompt, each context file, shell guidance, Rust guidance, extensions) and log it at startup. If the composed system prompt exceeds a configurable fraction of the token budget (default: 50%), warn the user with a breakdown by layer — e.g., "System prompt uses 8,200 / 32,768 tokens (25%). Breakdown: base 2,230, AGENTS.md 2,465, shell guidance 1,500, Rust guidance 2,005." This makes prompt bloat visible and gives the user actionable information (which context file to remove, which guidance layer to trim). The threshold and logging are controlled by `[context] prompt_budget_warning` in config (default: `0.5`).
   - The token measurement uses the calibrated `HeuristicEstimator` (Phase 2.5), which self-corrects per-model against API ground truth. Phase 4's model-aware sizing means this fraction is relative to the *actual* model context window.

7. **Polish:**
   - Comprehensive error messages (no panics, all errors surfaced)
   - Logging (file-based, for debugging)
   - Performance profiling (tool execution times, token usage tracking)

8. **Snapshot test for the composed system prompt:**
   Given a fixed tool registry, a fixed set of project context files, a fixed shell-guidance string, a fixed Rust-guidance string, and the bundled base prompt, the output of the composition function must be byte-for-byte stable. The expected output is stored as a snapshot file at `rho-core/tests/fixtures/prompts/composed_full.md`; the test fails when the live output diverges.

   This catches three classes of bug that are otherwise invisible:
   - **Reordering** — a refactor of the composition function causes layers to come out in a different order. Agent behaviour shifts but no other test fails.
   - **Whitespace drift** — a stray newline or section delimiter changes the prompt's shape. Models can be sensitive to formatting; the snapshot makes this visible.
   - **Unintended inclusion** — an extension or context file that should have been filtered out makes it into the composed prompt. Surfaces immediately.

   Additionally, the budget-awareness layer adds a **token cost assertion**: the test verifies that the composed prompt's measured token cost is within expected bounds (e.g., "system prompt must be < 12,000 tokens"). If a prompt layer grows beyond the asserted bound, the test fails — catching prompt bloat at the test level before it reaches real usage.

   The full composition chain (per task 6 above) is:
   1. Base identity prompt (from `rho-core/src/prompts/base.md`)
   2. Project context files in scan-list order
   3. Shell guidance (PowerShell idioms, from Phase 2)
   4. Rust guidance (compiler diagnostic conventions, from Phase 3)
   5. Extension tool descriptions (auto-generated)
   6. Tool schemas (auto-generated)

   Updating the snapshot is a deliberate action (e.g., `INSTA_UPDATE=1 cargo test` or a dedicated `cargo xtask` subcommand); the diff in the snapshot file goes through code review like any other change. Component-level tests (each layer in isolation) are also useful but smaller in scope — the snapshot test is the integration-level contract.

9. **Test suite audit:**
   - Review the entire test suite across all workspace crates for consistency
   - Ensure extension/tool registration tests cover: duplicate names, invalid schemas, missing dependencies
   - Verify config loading tests cover: missing files, malformed TOML, unknown keys (config loading tests should already exist from Phase 2 — extend for extension-specific keys)
   - Consider whether any integration tests should be promoted to property-based tests (proptest) for type serialization
   - Final naming convention check — all tests follow the established pattern
   - Document the testing conventions in `AGENTS.md` and the crate-level doc comments
