# Phase 2.5 Tasks

The tasks below implement the Phase 2.5 plan. They are roughly ordered so that each task's prerequisites are landed before it; deviations from this order risk having to redo work. Two tasks (the calibrator and tool-result bounding) are marked as **early-priority** because they together close the shipping amnesia bug and could plausibly be released as a 0.11.x patch independent of the larger refactor.

---

1. **Add the `uuid` dependency to `rho-core`.**
   - Single addition: `uuid = { version = "1", features = ["v4"] }`.
   - Document in `phase.md` and the workspace dependency philosophy section.

2. **Define `EntryId` as a newtype in `rho-core`:**
   - `EntryId(String)` holding an 8-char hex prefix of a UUID v4 (matches pi's format).
   - Implements `Display`, `Deref<Target = str>`, `From<&str>`, `Serialize`, `Deserialize`, `Hash`, `Eq`.
   - `EntryId::new()` generates a fresh ID. `EntryId::from_uuid(Uuid)` for explicit construction.
   - Round-trip serde test, hash-collision sanity test (generate 10,000 IDs, assert no collisions).
   - Lives in the existing `newtypes.rs` alongside `FilePath`, `ToolName`, `ToolCallId`.

3. **Define the `Entry`, `EntryPayload`, and `EntryResolution` types in a new `rho-core/src/session/entry.rs`:**
   ```rust
   pub struct Entry {
       pub id: EntryId,
       pub parent_id: Option<EntryId>,
       pub timestamp: SystemTime,
       pub resolution: EntryResolution,
       pub payload: EntryPayload,
   }

   pub enum EntryResolution {
       Full,
       Compacted { into: EntryId },
       Attached,
   }

   pub enum EntryPayload {
       Message(ChatMessage),
       Compaction { summary: CompactionSummary, first_kept: EntryId, tokens_before: usize },
       BranchSummary { summary: CompactionSummary, from_id: EntryId },
       ModelChange { model: String },
       Label { target_id: EntryId, label: Option<String> },
       SessionInfo { name: String },
       LeafMoved { from: Option<EntryId>, to: EntryId },
       Custom { kind: String, data: serde_json::Value },
       CustomMessage { kind: String, content: Vec<ContentBlock> },
   }
   ```
   - `CompactionSummary` is defined in Task 9; this task can stub it out with the struct definition only.
   - All variants `Serialize`/`Deserialize`-able; pi's wire format is informative, not binding; we use snake_case throughout.
   - Document each variant with its purpose, its default `EntryResolution`, and whether it participates in LLM context.
   - Round-trip serde tests for each variant including each `EntryResolution` arm.

4. **Define `Session` in `rho-core/src/session/mod.rs`:**
   - `Session` owns: `header: SessionHeader`, `entries: HashMap<EntryId, Entry>`, `leaf: Option<EntryId>`, `estimator: Box<dyn TokenEstimator>`, plus the existing `tools: Vec<ToolSchema>`, `model: String`, `context_manager: Box<dyn ContextManager>`, `token_budget: TokenBudget`, `redactor: Redactor`.
   - `SessionHeader` holds: `id: SessionId`, `version: u32` (start at 1), `created_at: SystemTime`, `cwd: PathBuf`, `parent_session: Option<PathBuf>`.
   - Constructor: `Session::new(model, system_prompt, tools, cwd) -> Self`. The system prompt becomes the first `Message` entry with `parent_id = None`, `resolution: Full`.
   - Builder methods preserved from `Conversation`: `with_context_manager`, `with_token_budget`, `with_redactor`. New: `with_estimator`.
   - Document the leaf-pointer contract in the type-level doc.

5. **Implement append operations on `Session`, including bounded tool-result handling [early-priority]:**

   This task contains the fix for the shipping amnesia bug (P2.5-9). The bounded tool-result behaviour is the *most important* sub-item; everything else is plumbing.

   - `append_user_message(text: &str) -> EntryId`
   - `append_assistant_message(msg: ChatMessage) -> EntryId` — used by `send_current` to persist model responses
   - `append_tool_result(call_id: ToolCallId, result: &ToolResult) -> EntryId` —
       - runs the redactor first, same as today
       - **bounded resolution:** if the redacted content would consume more than `budget.prompt_budget() / 2` tokens (per the calibrated estimator), truncate it at a UTF-8-safe character boundary; the truncated content goes into the entry's `Message` payload, the *full* content goes into `details: ToolResultDetails::FullOutput { original_size, content }` on the `ToolResult`, and the entry's resolution remains `Full` (the truncated text is what the model sees).
       - the truncation footer reads: `\n\n... [truncated; original size: {N} bytes — re-read the source with offset to access more].`
       - emit a `warn!` log line with the original and truncated sizes.
   - `append_compaction(summary: CompactionSummary, first_kept: EntryId, tokens_before: usize) -> EntryId`
   - `append_branch_summary(summary: CompactionSummary, from_id: EntryId) -> EntryId`
   - `append_label(target_id, label: Option<String>) -> EntryId`
   - `append_custom_state<E: ExtensionEntry>(data: &E) -> EntryId` — resolution defaults to `Attached`
   - `append_custom_message<E: ExtensionEntry>(data: &E) -> EntryId` — resolution defaults to `Full`
   - All operations: assign a fresh `EntryId`, set `parent_id = self.leaf`, set timestamp, insert into `entries`, update `self.leaf`. Return the new ID.
   - Each operation that changes the leaf writes a `LeafMoved` entry *only* when the leaf moves to a non-adjacent position (i.e., during branch operations, not during normal append). Document this clearly.
   - Make the equivalents of `Conversation::push_*` `pub(crate)` per the M3 fix from the review; only the high-level `send`/`submit_tool_results` and the typed `append_custom*` are `pub`.
   - **Tests for this task** (directly mirror the amnesia reproducer):
     - Append a tool result whose redacted content exceeds the threshold; verify the entry's `Message` payload contains the truncated content + footer, and the original is in `ToolResultDetails::FullOutput`.
     - Append a tool result that fits comfortably; verify no truncation, `details: None`.
     - Verify the truncation point is a UTF-8 character boundary (use a multi-byte test fixture).

6. **Implement the `TokenEstimator` and calibrator in `rho-core/src/session/estimator.rs` [early-priority]:**

   This task contains the second half of the shipping amnesia fix (P2.5-1). It can land independently of, and before, the rest of Phase 2.5 — the existing `fit` can switch to using a `TokenEstimator` instance even before the tree refactor.

   ```rust
   pub trait TokenEstimator: Send + Sync {
       fn estimate(&self, content: &str) -> usize;
       fn calibrate(&mut self, model: &str, estimated: usize, actual: usize);
   }

   pub struct HeuristicEstimator {
       /// Per-model chars-per-token ratio. Bootstrapped from defaults; refined by calibrate().
       ratios: HashMap<String, f32>,
   }
   ```
   - Bootstrap defaults: Gemma 3.5, Qwen 2.5, Claude 3.7, GPT-4 4.0, Llama 3.5, unknown 2.5 (conservative).
   - `estimate(content)` uses the *current* model's ratio. The current model comes from the most recent `Session` request; for an estimator without context, fall back to the unknown ratio.
   - `calibrate(model, estimated, actual)` updates the per-model ratio using exponential moving average: `ratios[model] = α * (chars_in_request / actual) + (1 - α) * ratios[model]`. Use α = 0.3 — fast enough to converge in a few requests, slow enough to not whiplash on outliers.
   - Optional: `tiktoken-rs` integration behind a `tiktoken` feature flag. When enabled, `HeuristicEstimator` is replaced by `TiktokenEstimator` for known model families (OpenAI, Claude). Local models fall back to the heuristic.
   - **Wire-up:** `Session::send_current` records the response's `prompt_tokens` and calls `estimator.calibrate(model, estimated_for_request, actual_prompt_tokens)`. The instrumentation log gains an `estimator_error` field showing percent error per request.
   - **Tests:**
     - Bootstrap test: a fresh estimator returns reasonable values for known models.
     - Convergence test: simulate 10 calibrate() calls with a known true ratio; assert error drops below 10% by call 3.
     - Round-trip test: estimator state can be serialized to JSON and back.

7. **Implement tree navigation on `Session`:**
   - `leaf() -> Option<EntryId>` — current position.
   - `entry(id: EntryId) -> Option<&Entry>` — lookup by ID.
   - `path_to_root() -> Vec<&Entry>` — walks `self.leaf` back through `parent_id` chains.
   - `children(id: EntryId) -> Vec<EntryId>` — direct children of an entry, sorted by timestamp.
   - `branch_to(id: EntryId) -> Result<()>` — moves the leaf to `id`, writes a `LeafMoved` entry, errors if `id` does not exist.
   - `branch_with_summary(id: EntryId, summary: CompactionSummary, from_id: EntryId) -> Result<()>` — combines `branch_to` with a `BranchSummary` append at the new leaf.
   - These are programmatic only in 2.5; the TUI in Phase 4 wires them to `/tree`, `/fork`, `/clone`.

8. **Implement context building (`fit_path`):**
   - Replace `Conversation::send_current`'s body. New flow:
     1. Walk `self.leaf` to root, collecting entries.
     2. Reverse to get chronological order.
     3. Pass to `context_manager.fit_path(entries, budget, estimator, tool_schemas) -> Vec<ChatMessage>`.
     4. Build `ChatRequest` from the resulting messages plus tools.
     5. Call client; on response, persist as appended `Message` entry and call `estimator.calibrate(...)`.
   - `ContextManager` gains:
     ```rust
     fn fit_path(
         &self,
         entries: &[&Entry],
         budget: TokenBudget,
         estimator: &dyn TokenEstimator,
         tool_schemas: &[ToolSchema],
     ) -> Vec<ChatMessage>;
     ```
     with a default impl that:
     - Filters out entries with resolution `Compacted` or `Attached` (they don't reach the model).
     - Filters payload kinds that don't become messages: `Custom`, `Label`, `LeafMoved`, `ModelChange`, `SessionInfo`.
     - Renders `Compaction { summary, .. }` and `BranchSummary { summary, .. }` as synthetic `ChatMessage::User` entries with deterministic framing of the `CompactionSummary` (see Task 9 for the rendering contract).
     - Converts remaining `Message` and `CustomMessage` entries to `ChatMessage`.
     - **Computes tool-schema overhead** using `estimator` and subtracts it from `budget.prompt_budget()` before delegating to `fit`.
     - **Computes system-message overhead** and subtracts it as well (P2.5-3).
     - Delegates the final budget enforcement to the existing `fit(&[ChatMessage], budget)`.
   - The existing `SlidingWindowContextManager::fit` is unchanged. The default `fit_path` implementation gives every existing context manager tree-awareness *and* calibrated overhead handling for free.

9. **Define `CompactionStrategy`, `CompactionSummary`, and `MechanicalCompactionStrategy`:**
   ```rust
   pub struct CompactionSummary {
       pub original_request: Option<String>,
       pub tool_calls: BTreeMap<ToolName, Vec<String>>,
       pub tokens_compacted: usize,
       pub entry_count: usize,
       pub time_span: Duration,
       pub notes: Option<String>,
   }

   #[async_trait]
   pub trait CompactionStrategy: Send + Sync {
       async fn compact(&self, entries: &[&Entry]) -> Result<CompactionSummary>;
   }
   ```
   - `MechanicalCompactionStrategy` walks the entries, extracts:
     - `original_request`: the first `ChatMessage::User` text encountered in the range (P2.5-6).
     - `tool_calls`: groups `ChatMessage::Assistant { tool_calls }` entries by tool name; for each call, formats a one-line argument summary.
     - `tokens_compacted`: sum of estimated tokens across all compacted entries.
     - `entry_count`, `time_span`: trivial walks.
     - `notes`: `None` (LLM strategies populate this).
   - **Rendering contract** for `path_messages` / `fit_path`: a `CompactionSummary` is rendered as a `ChatMessage::User` with body:
     ```
     [Compacted: {entry_count} entries, {tokens_compacted} tokens, span {duration}]
     Original request: "{original_request, if present}"
     Tool activity:
       - {tool_name}: {N} calls — {args_summary_1}, {args_summary_2}, ...
     {notes, if present}
     ```
     This rendering is stable and tested as part of the `fit_path` test suite.
   - `Session::compact_older_than(threshold: usize, strategy: &dyn CompactionStrategy) -> Result<EntryId>` selects the oldest entries whose total tokens exceed `threshold`, generates a summary, appends a `Compaction` entry, and **transitions the compacted entries' resolution to `Compacted { into }`**. Does not modify or delete the original entries — they stay in the tree at lower resolution, just bypassed by `fit_path`.

10. **Define the `ExtensionEntry` trait:**
    ```rust
    pub trait ExtensionEntry: Serialize + DeserializeOwned + 'static {
        const KIND: &'static str;
    }
    ```
    - `Session::append_custom_state<E: ExtensionEntry>(data: &E) -> EntryId` serializes `data`, tags with `E::KIND`, stores as `Custom { kind, data }` with resolution `Attached`.
    - `Session::read_custom_state<E: ExtensionEntry>(id: EntryId) -> Option<E>` looks up the entry, verifies the `kind` field matches `E::KIND`, deserializes. Returns `None` on kind mismatch (schema version skew).
    - Same pair for `CustomMessage` (resolution `Full`).
    - Document the `KIND` versioning convention: `"<author>.<feature>.v<n>"` (e.g., `"rho.diagnostics.v1"`).
    - Add a usage example in the module-level doc.

11. **Implement JSONL persistence:**
    - `Session::open(path: &Path) -> Result<Session>` — reads JSONL, reconstructs `entries` map and `leaf` from the most recent `LeafMoved` entry (or the last appended entry if none).
    - `Session::save_path() -> Option<&Path>` — returns where the session would persist, or `None` for in-memory.
    - `Session::flush() -> Result<()>` — appends any unwritten entries to the JSONL file.
    - Auto-flush on each append: writes are buffered to disk after every append operation. A crashed process loses at most one in-flight entry.
    - Path layout: `~/.rho/sessions/<project-hash>/<timestamp>_<session-id>.jsonl`. Project hash is sha256 of canonical CWD, base32-encoded, first 16 chars. This avoids pi's `--<path>--` filename hack and works on Windows.
    - In-memory mode: `Session::in_memory(...)` constructor that skips disk operations entirely. Used by tests.
    - Deleted sessions: out of scope for 2.5.

12. **Wire the agent loop:**
    - `run_loop` signature changes from `&mut Conversation` to `&mut Session`. Otherwise the body is unchanged — it still calls `session.send_current(client)`, still appends tool results via `session.append_tool_result(...)`.
    - `send_current` records `prompt_tokens` from the response and calls `estimator.calibrate(...)`.
    - Verify the persistence-invariant fix from Phase 1a still holds: assistant `tool_calls` messages are appended as entries before their matching tool result entries.
    - All multi-tool-call iteration logic from Phase 2 Task 5 stays as-is — just `append_tool_result` instead of `push_tool_result`.

13. **Add `ToolResultDetails` as an extensible enum:**
    ```rust
    pub enum ToolResultDetails {
        None,
        FullOutput { original_size: usize, content: String },
        // Phase 3 will add: Diagnostics(Vec<Diagnostic>), FileSnapshot { ... }, etc.
    }
    ```
    - Add a `details: ToolResultDetails` field to `ToolResult` (default `None`).
    - This field is *not* sent to the model and *not* part of the `ChatMessage` content. It travels alongside the result for tools and extensions to consume.
    - `FullOutput` is the variant used by Task 5's bounded-tool-result handling.
    - Phase 2.5 ships `None` and `FullOutput`; Phase 3 grows the enum.
    - Update `RunCommand`, `ReadFile`, `WriteFile`, `EditFile`, `ListDir` to construct `ToolResult` with `details: ToolResultDetails::None`. No behaviour change in those tools.

14. **Update the binary:**
    - `rho/src/main.rs` constructs a `Session` instead of a `Conversation`. The startup flow is otherwise identical.
    - **Startup overhead breakdown** (P2.5-3): after constructing the session, compute system + tool-schema + context-file token cost using the calibrated estimator. If it exceeds 50% of budget, log a `warn!` with a breakdown. This is the early-warning system for "budget is mostly overhead before the user has typed a word."
    - Add a `--session <path>` CLI flag for opening an existing session file. Defaults to creating a fresh in-project session.
    - Add a `--ephemeral` flag for `Session::in_memory` mode (no persistence).
    - The REPL slash commands (`/quit`, `/clear`) continue to work. `/clear` becomes "branch back to the system message" — same effect from the user's perspective, but the old conversation tree is preserved on disk.

15. **Migrate existing tests:**
    - `Conversation::messages()` callers get `session.path_messages()` (returns the leaf-to-root path as `Vec<ChatMessage>`).
    - `Conversation::push_*` callers get the corresponding `session.append_*` methods.
    - `Conversation::clear` callers get `session.clear()` (which branches to the system message, same effect).
    - Mock `ChatClient`, approval gates, fixture loaders are unchanged.
    - The integration test `assistant_tool_call_message_persisted_before_tool_result` translates verbatim — the structural invariant is unchanged.

16. **Add Phase 2.5–specific tests:**
    - **Entry round-trip:** every `EntryPayload` and `EntryResolution` variant survives JSONL serialize/deserialize.
    - **Tree integrity:** appending entries always produces a valid tree (every `parent_id` references an existing entry, the leaf is reachable from root).
    - **Path building:** `path_to_root()` returns entries in correct order with no duplicates and no orphans.
    - **Resolution filtering:** `fit_path` skips `Compacted` and `Attached` entries; verifies they are still present in the tree.
    - **Branching:** after `branch_to(earlier_id)`, `path_to_root()` reflects the new path and old branch is unreachable from leaf but still in `entries`.
    - **Branch summary:** `branch_with_summary` produces a `BranchSummary` entry at the new leaf with correct `from_id`.
    - **Compaction:** `MechanicalCompactionStrategy` produces a stable, deterministic `CompactionSummary` for a fixed input. `original_request` is correctly populated when the user message is in range, `None` when it isn't. `compact_older_than` produces a `Compaction` entry whose `first_kept` references an existing entry, transitions the compacted entries' resolution to `Compacted`, and the next `path_messages()` call expands the summary correctly.
    - **Rendering contract:** the `CompactionSummary` rendering is byte-stable across runs for a fixed input.
    - **Persistence round-trip:** open a session, append entries, save, reopen, verify identical structure including leaf position and resolution levels.
    - **In-memory mode:** `Session::in_memory` performs no disk I/O.
    - **Extension entries:** typed read/write through `ExtensionEntry` trait works; kind mismatch returns `None`; bumping the version constant produces a clean break.

17. **Add session-tree-pressure integration tests (P2.5-7 from the followups):**

    Three scenarios must be covered. The first is the documented amnesia reproducer; the second is the Get-Process scenario; the third is the long-conversation pressure case.

    **(a) Amnesia reproducer — `test_scenarios/amnesia_test_small.md`.**

    Run the reproducer with a mock client whose responses match the captured Gemma trace and the captured Qwen trace from `Amnesia_Test_-_Context_Window_Eviction.md` and `Amnesia_Test_-_Qwen2_5-Coder-14B.md`. Assert: the final assistant reply contains the secret `PLUM-BLOSSOM-8834`. This test is the binary pass/fail for the bug.

    **(b) Single-oversized-tool-result test — Get-Process scenario.**

    This is the regression test from `Context_Window_Amnesia_Bug.md`, promoted verbatim into the suite. It must pass before Phase 2.5 is considered complete.

    ```rust
    #[test]
    fn fitter_never_evicts_the_most_recent_tool_call_pair() {
        let huge = "x".repeat(200_000); // ~50K tokens
        let messages = vec![
            ChatMessage::system_text("sys"),
            ChatMessage::user_text("do it"),
            assistant_with_tool_call("call_1"),
            ChatMessage::tool_result(ToolCallId::from("call_1"), &huge),
        ];
        let cm = SlidingWindowContextManager::new();
        let result = cm.fit(&messages, TokenBudget::new(32_768));

        // The most recent tool-call pair must survive in *some* form,
        // even if its content has to be truncated.
        let has_assistant = result.iter().any(|m|
            matches!(m, ChatMessage::Assistant { tool_calls, .. } if !tool_calls.is_empty())
        );
        let has_tool = result.iter().any(|m| matches!(m, ChatMessage::Tool { .. }));
        assert!(has_assistant && has_tool, "fitter dropped the in-flight tool call");
    }
    ```

    Translate this to the new `Session` API: construct a session with the equivalent entries, walk to leaf, verify the resulting message path contains both the tool-calling assistant and its matching (truncated) tool result. The truncation that makes this pass comes from Task 5's bounded-tool-result handling.

    **(c) Long-conversation pressure test.**

    Construct a session with 50 turns, force eviction with a small budget, verify:
    - The first user message survives (or gets compacted into a `Compaction` entry whose `original_request` field carries it verbatim).
    - No tool-call assistant message gets separated from its matching tool result anywhere on the leaf path.
    - The model receives a coherent path even after compaction (no orphan tool results, no dangling tool_call_ids).

    Together these tests catch the worst failure modes from both Phase 2 (the amnesia bug currently shipping in 0.11.0) and Phase 3 (the cargo-output cases that will appear once tree-sitter and `cargo check --message-format=json` start producing structured output).

18. **Add `compact-and-resume` end-to-end test:**
    - Run a real agent loop (with mock client) until eviction triggers compaction.
    - Verify the next `send_current` request contains the compaction summary, not the evicted messages.
    - Verify the model's response is correctly appended after the compaction entry.
    - Verify a subsequent `path_messages` call still includes the compaction summary (it doesn't get re-evicted).
    - Verify the original entries are still in the tree at `Compacted` resolution and accessible via `entry(id)`.

19. **Add token-estimator convergence test:**
    - With a mock client that returns increasing `prompt_tokens` values, run 5 round-trips against a fictional model.
    - Verify that the estimator's per-model ratio converges within 10% of the true ratio by the third round-trip.
    - Verify that an unknown model bootstraps with the conservative ratio and converges to the correct one.

20. **Update `AGENTS.md`:**
    - Replace the `Conversation` references with `Session`.
    - Add `Entry`, `EntryPayload`, `EntryResolution`, `EntryId`, `ExtensionEntry`, `CompactionStrategy`, `CompactionSummary`, `TokenEstimator`, `ToolResultDetails` to the Key Types table.
    - Add a "Session tree" section briefly explaining the leaf/path model and the resolution-aware framing, pointing readers at `phase-2.5/phase.md` for the design rationale and the CFD analogy.
    - Note in the architecture diagram that `Session` replaces `Conversation` at the same layer position — no dependency direction changes.

21. **Update the roadmap:**
    - Insert Phase 2.5 between Phase 2 and Phase 3 in `roadmap.md`.
    - Update Design Decision #15 (context window management strategy) to reflect adaptive resolution: tree shape, resolution levels, and calibrated budget.
    - Update Design Decision #6 (multi-turn tool composition) — note the data model now supports parallel exploration via branching.
    - Add new Design Decisions:
      - "Tree-shaped session model — pi-inspired, simpler than git, explicit leaf, JSONL persistence."
      - "Adaptive resolution — entries carry explicit resolution, compaction is a refinement not a deletion, tool results bounded at append time."
      - "Calibrated token budget — per-model estimator self-corrects against API ground truth."

22. **Test suite audit:**
    - Promote `Session` test helpers into `rho-test-helpers`: `in_memory_session()`, `seeded_session(entries)`, `path_messages_of(session) -> Vec<ChatMessage>`.
    - Audit the test suite for direct `Vec<ChatMessage>` construction — most tests should now construct sessions and inspect via `path_messages()`.
    - Document the entry-vs-message distinction in the test conventions.
    - Verify the security tests from Phase 1b/2 still pass without modification (the security surface is orthogonal to the session topology).

---

## Optional pre-Phase-2.5 patch release

Tasks 5 (bounded tool results) and 6 (calibrator) together close the shipping amnesia bug. They could plausibly ship as a 0.11.x patch release before the rest of Phase 2.5 lands, on the existing `Vec<ChatMessage>` data model:

- The `TokenEstimator` trait can be applied to the existing `SlidingWindowContextManager::fit` directly — replace the hard-coded `chars/4` with a call to `estimator.estimate(...)`.
- Tool-result bounding can be applied at `Conversation::push_tool_result` rather than `Session::append_tool_result` — same logic, different call site.
- The `ToolResultDetails::FullOutput` variant requires only the `details` field on `ToolResult`, which is a small addition without the rest of the entry-type infrastructure.

This is a contingency, not the recommendation. The full Phase 2.5 is the right shape and worth landing whole. But if user pressure makes a patch release necessary, this is the path.
