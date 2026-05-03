# Phase 2.5: Adaptive-Resolution Context

**Goal:** Replace the linear `Vec<ChatMessage>` conversation model with a tree-shaped session of typed entries, organized around the principle of *adaptive resolution* — preserve fine detail where it matters to the model's task and coarse detail where it doesn't. The conversation sent to the model becomes a budget-aware path through the tree, where each entry can be at full resolution, summarized, or attached out-of-band as structured detail.

**Milestone:** A user can run a multi-turn conversation, branch at any earlier point, and return with a generated summary attached. Tool results that exceed budget are truncated rather than dropped. Token estimates self-correct against API ground truth. Extensions can attach structured detail to messages without consuming context budget. The amnesia reproducer in `test_scenarios/` passes — the secret survives a 6-file read on both Gemma and Qwen.

## The framing: context as adaptive mesh refinement

CFD simulations of fire don't run on uniform grids. A 1m × 1m × 1m grid covers the room, but it smears the features that matter — the flame front, the boundary layer, the eddies near the obstruction. Refining everything to 1cm uniformly is computationally infeasible. The discipline of CFD is *adaptive mesh refinement* (AMR): keep fine resolution where the physics is interesting, coarsen aggressively where it isn't, and let the algorithm decide which is which based on the features being predicted.

The current `SlidingWindowContextManager` is a uniform-coarse grid. It treats all messages as interchangeable units of "tokens consumed" and applies one operation — keep or evict — uniformly across the conversation. That works while the conversation is short and homogeneous. It fails the moment the conversation has structure: a stated goal, a series of investigations, a key piece of data, intermediate scaffolding. The grid cell is too big to distinguish them.

Today's reproducer makes this concrete. With Gemma 4, after 5 file reads the budget tips and `fit` evicts a complete turn — user message, assistant tool-call, tool result — together freeing 10,685 tokens. With Qwen 2.5 Coder 14B, the same prompt evicts after only 3 reads because Qwen's tokenizer counts twice as many tokens per character; eviction frees 227 tokens and drops only the user message. Same code, same prompt, two completely different failure shapes — because uniform-coarse eviction is a function of *when* the budget tips relative to *what* happens to be at the head of the queue. There is no principle behind which content survives.

Phase 2.5 rebuilds the context layer around three resolution-aware primitives:

1. **Tree shape** — entries form a parent-linked tree with a movable leaf, so context becomes a *path* not a buffer. This separates "what's in the model's view" from "what's in the conversation history."
2. **Resolution levels** — each entry carries an explicit resolution (full, compacted, attached-only). Compaction becomes a refinement operation rather than a destructive one. Tool results gain a structured-detail slot that lives outside the model's context but is available to other tools.
3. **Calibrated budget** — token estimates self-correct against the API's actual `prompt_tokens` count, and tool schemas + system overhead are subtracted from the budget *before* path-fitting. The current heuristic isn't wrong by 20% — Qwen showed it can be wrong by 100%+. A model-blind heuristic cannot survive contact with multiple models.

The session tree is the data structure that makes the rest possible. Pi got the data structure right; the framing here is what makes it more than a port.

## Why now

Phase 2 closed the security surface and made the agent useful as a daily tool. Phase 3 will introduce structured cargo diagnostics, tree-sitter, and tools that produce and consume rich derived data. Without adaptive resolution, every Phase 3 tool output gets stringified and pushed through the same uniform grid, and the same eviction pathologies recur — only with bigger payloads.

**The bug is shipping today.** Two reproducers in `test_scenarios/` show it firing on local-model harnesses that any user could hit:

- `amnesia_test_small.md` (6 files, ~30K tokens) reliably evicts the user turn on both Gemma 4 and Qwen 2.5 Coder.
- `Context_Window_Amnesia_Bug.md` documents a single `Get-Process | Format-List` invocation whose tool result alone exceeds budget.

Both failures have the same root cause and the same fix: tool results need bounded resolution before they enter history, and the heuristic that decides "is the budget tight" needs to be calibrated against reality.

Doing this before Phase 3 means tree-sitter and Rust tooling get to use the entry types and resolution levels from day one. Doing it after means retrofitting every tool and the context manager mid-stream while the bug continues to ship.

This is not "go full git DAG." Pi's tree is one parent per entry, a single mutable leaf, and a context-building function that walks from leaf to root. We adopt the same data structure and add the resolution-aware framing on top.

## Scope

The conversation type is replaced with `Session`. Entries are typed: `Message`, `Compaction`, `BranchSummary`, `ModelChange`, `Label`, `SessionInfo`, `Custom`, `CustomMessage`. Each entry carries a `resolution: EntryResolution` field that records its current level of detail. The agent loop continues to drive the conversation via append operations, but those operations now write entries to a tree, return entry IDs, and may transition resolution levels.

Token estimation is reworked to self-correct: the chat client returns `prompt_tokens` on every response, and the context manager records the ratio of estimated-vs-actual to recalibrate per-model. The `chars/4` heuristic stays as a bootstrap value; once the system has seen one real response from a model, it switches to a model-specific calibration.

Compaction is introduced as a node type and a structured strategy. The Phase 2.5 `MechanicalCompactionStrategy` is deterministic — no LLM calls — and produces *structured* output (verbatim original request, aggregate stats, list of tool calls) rather than the prose summary the original plan sketched. The hook for replacing this with model-generated summaries lands in this phase as a `CompactionStrategy` trait. The structured output makes mechanical and LLM-driven strategies substitutable at the data level.

The TUI does not exist yet, so `/tree` and `/fork` slash commands are not in scope here. The data model fully supports them, so Phase 4 is a UI layer over machinery that already works.

## New Dependencies

| Crate | For | Decision |
|---|---|---|
| `uuid` | `rho-core` | Entry IDs need to be stable across sessions and unique across processes. 8-char hex from a UUID is what pi uses; we'll do the same. The `uuid` crate is the standard choice and pulls in nothing surprising |
| `tiktoken-rs` | `rho-core` (optional, feature-gated) | Real tokenization for OpenAI-format models. Heavy dep (~2MB compiled), but the only way to make the heuristic provably correct for OpenAI/Claude-format models. Feature-gated so users on local-only models can opt out |

The `tiktoken-rs` decision is a partial commitment: we make it available behind a feature flag so the calibration loop can prefer real tokenization when it's compiled in, falling back to the heuristic+feedback approach otherwise. This is a hedge — if the feedback loop alone proves sufficient, the dep can be removed. If users are running a mix of local and frontier models, having both options matters.

JSONL persistence uses the existing `serde_json` already in the tree. No graph crates, no merkle DAG libraries, no new serialization formats.

## Decisions

**Entry IDs are explicit, not content-addressed.** Pi uses 8-char hex IDs from a UUID source. We do the same. Content addressing (Merkle DAG style) would buy deduplication and stronger immutability guarantees, but it complicates testing (entries with timestamps would have unstable hashes), complicates extensions (custom content needs canonical serialization), and pays off only at scales we don't expect to hit.

**The leaf pointer is explicit, not implicit.** Pi appears to derive leaf position from the most-recently-appended entry. We make it an explicit field on `Session`, persisted as a `LeafMoved` event in the log. This makes branch operations auditable ("when did the leaf move and from where to where?") and makes loading deterministic. Cost is one extra field and one extra entry type. Worth it.

**Persistence is JSONL, append-only.** No database, no custom binary format. JSONL is text-friendly, diffable, and survives partial writes (a crashed write loses the last entry, not the whole session). Sessions live at `~/.rho/sessions/<project-hash>/<timestamp>_<session-id>.jsonl`. Project hash is sha256 of the canonical project root, base32-encoded; we do not encode the path in the filename (pi's `--<path>--` scheme breaks on Windows).

**Resolution is a first-class entry property.** Each entry carries a `resolution: EntryResolution` field:

```rust
pub enum EntryResolution {
    /// Full content participates in the model's context.
    Full,
    /// Content has been summarized into a Compaction or BranchSummary entry;
    /// the original is still in the tree but bypassed by fit_path.
    Compacted { into: EntryId },
    /// Content is preserved as structured ToolResultDetails but doesn't
    /// participate in the model's context. Tools can read it; the LLM doesn't.
    Attached,
}
```

This makes refinement explicit. The context-building function `fit_path` reads the resolution field to decide what to include. Compaction transitions entries from `Full` to `Compacted`. Tool results may start at `Full` and transition to `Attached` once their structured details have been consumed by a downstream tool. The whole "what does the model see" question becomes a function of the resolution levels along the leaf-to-root path.

**Token budgets are calibrated, not estimated.** The `TokenBudget` type splits into a *prompt* budget and a *completion reserve*. The prompt-side estimator is no longer a fixed `chars/4` heuristic — it's a per-model calibrator that starts with a conservative bootstrap value and updates from the API's actual `prompt_tokens` field on every response. After the first real call, estimation error for that specific model drops to whatever the calibrator can achieve, typically <5%.

```rust
pub struct TokenBudget {
    pub context_window: usize,
    pub completion_reserve: usize,  // default 4096
}

pub trait TokenEstimator {
    fn estimate(&self, content: &str) -> usize;
    fn calibrate(&mut self, model: &str, estimated: usize, actual: usize);
}
```

The default `HeuristicEstimator` starts with model-specific bootstrap ratios (Gemma ~3.5 chars/token, Qwen ~2.5, Claude ~3.7, GPT-4 ~4.0) and converges from there. Models we haven't seen before bootstrap conservatively at 2.5. The heuristic is no longer a fixed truth; it's a prior that gets corrected.

**Tool schemas are subtracted from budget *before* path-fitting.** This was P2.5-2 in the followups: `Conversation::send_current` sends `tools` *outside* the budget enforcement, so a fully-budgeted message path plus tools could overflow. Fix: `fit_path` computes tool-schema tokens (using the calibrated estimator) and subtracts them from the budget before deciding what fits.

**Tool results are bounded at append time.** This was P2.5-9 — the immediate bug. In `Session::append_tool_result`, after redaction, if the result exceeds half the prompt budget, it is truncated to that boundary with a footer noting the original size and pointing the model at re-reading with offset. The truncated content is what enters context; the *full* content is preserved out-of-band as `ToolResultDetails::FullOutput { original_size, content }`. This is the AMR move at the leaf level: keep coarse content (truncated prose) on the model's grid, attach fine content (full output) at higher resolution where it can be retrieved if needed.

**Compaction is a strategy trait with structured output.** Compacting a long conversation requires LLM calls in the limit. Phase 2.5 ships a `MechanicalCompactionStrategy` that produces deterministic, *structured* summaries without any model calls. The output isn't prose — it's a typed summary that the path-builder renders into a synthetic message:

```rust
pub struct CompactionSummary {
    /// Verbatim original user request, if present in the compacted range.
    pub original_request: Option<String>,
    /// Aggregate counts and identifiers.
    pub tool_calls: BTreeMap<ToolName, Vec<String>>,  // tool -> [args summary]
    pub tokens_compacted: usize,
    pub entry_count: usize,
    pub time_span: Duration,
    /// Optional free-form notes (used by LLM strategies, empty for mechanical).
    pub notes: Option<String>,
}
```

The structured form does several jobs the prose form couldn't: P2.5-6 (preserve the original user request) is enforced by the type rather than by convention, P2.5-8 (signal what was evicted) reads from the same type, and an `LlmCompactionStrategy` later can populate `notes` without changing the consumers. The `path_messages` function renders this into a `ChatMessage::User` with a deterministic framing the model can rely on.

**Branch summaries reuse the compaction infrastructure.** When the leaf moves to a different branch, an optional `BranchSummary` entry can be inserted at the new position summarizing the abandoned branch. Same `CompactionSummary` type, same strategy trait. Phase 2.5 does not implement automatic insertion (no UI yet) but the entry type, the API method, and the strategy are all in place.

**Extension state vs. extension context is a type-level distinction.** `Custom` entries hold serialized state that does not become part of the LLM context (resolution = Attached). `CustomMessage` entries hold content that does (resolution = Full). This is pi's distinction and it's the right one — but where pi uses untyped JSON for both, we expose a typed trait:

```rust
pub trait ExtensionEntry: Serialize + DeserializeOwned + 'static {
    const KIND: &'static str;  // namespaced: "rho.diagnostics.v1"
}
```

Extension authors implement `ExtensionEntry` for their state types and get typed `read`/`write` access. Schema versioning is encoded in the `KIND` constant — bump the version, and consumers either match or get `None` on read. This is the foundation pi cannot give in TypeScript without runtime validation overhead.

**The agent loop's surface is unchanged in shape.** `run_loop` still takes a session-like object, still pushes user messages, still appends tool results. The methods change name (from `push_user_text` to `append_user_message` etc.) and return entry IDs, but the control flow is identical. No new states in the state machine. No changes to the approval gate. No changes to tools — tools that want to populate `ToolResultDetails` opt in by setting the `details` field on `ToolResult`, but existing tools work unchanged.

**Tool results gain a `details` slot.** `ChatMessage::Tool` already carries `Vec<ContentBlock>`. We add a sibling `details: Option<ToolResultDetails>` field where `ToolResultDetails` is an extensible enum admitting tool-specific structured payloads:

```rust
pub enum ToolResultDetails {
    None,
    FullOutput { original_size: usize, content: String },  // preserves bounded content
    // Phase 3 will add: Diagnostics(Vec<Diagnostic>), FileSnapshot { ... }, etc.
}
```

The wire format ignores `details` (it is not sent to the model). Tools that consume each other's output read the `details`. This is the structured-attachment hook the review identified, and it's also the storage layer for the bounded-tool-result preservation strategy.

## Migration Strategy

This is a refactor, not a rewrite. The existing types — `ChatMessage`, `ContentBlock`, `ModelToolCall`, `ToolResult`, `Redactor`, the agent loop, every tool — all stay intact. The only structural changes are wrapping `ChatMessage` in `Entry`, adding the resolution field, replacing `TokenBudget`'s heuristic with a calibrator, and bounding tool results at append time.

Specifically:

1. `Conversation` becomes `Session`, stored as `HashMap<EntryId, Entry>` plus a `leaf: Option<EntryId>` field plus a `TokenEstimator`.
2. `Conversation::push_user_text(msg)` becomes `Session::append_user_message(msg) -> EntryId`. The body of the function changes; the call sites barely do.
3. `Conversation::send_current` becomes `Session::send_current`. Its body now walks `leaf → root → reverse → ContextManager::fit_path → ChatRequest`, and on response the calibrator is updated with `(estimated, actual)` for the model.
4. `ContextManager::fit(&[ChatMessage], budget) -> Vec<ChatMessage>` gains a default-implemented sibling: `fit_path(&[Entry], budget, estimator) -> Vec<ChatMessage>` that filters by resolution, expands compactions, subtracts tool-schema overhead, and delegates the final budget enforcement to existing `fit`.
5. `ToolResult` gains a `details` field defaulting to `None`. `Session::append_tool_result` truncates the content if it exceeds the per-result threshold and stuffs the original into `details`.

Test impact is small. Existing tests construct messages and call agent-loop functions; they still do. Integration tests that inspect `Conversation::messages()` get a thin wrapper (`session.path_messages()`) returning the leaf-to-root message sequence. Mock client and approval gate are untouched. The amnesia reproducers in `test_scenarios/` become end-to-end regression tests.

## Exit Criteria

The agent runs as before, with conversation backed by a tree session. Sessions persist to `~/.rho/sessions/` as JSONL and reload cleanly. The leaf can be moved programmatically (no UI yet) and branching produces correct context on the next request. Compaction-as-node works with a structured mechanical strategy and the `CompactionStrategy` trait is in place for Phase 4+ replacements. Extensions can read and write typed `Custom` entries via the `ExtensionEntry` trait.

The two shipped reproducers must pass:

1. `amnesia_test_small.md` recovers the secret `PLUM-BLOSSOM-8834` on both `google/gemma-4-26b-a4b` and `Qwen/Qwen2.5-Coder-14B-Instruct`.
2. `Context_Window_Amnesia_Bug.md`'s `Get-Process | Format-List` scenario produces a coherent reply that references the process list, even after truncation.

The token estimator must converge: after one round-trip with a model, the heuristic error on the *next* request for that model drops below 10%.

All Phase 1 and Phase 2 tests pass with the new model.

## Non-Goals (deferred)

- Slash commands for tree navigation (`/tree`, `/fork`, `/clone`) — Phase 4
- LLM-driven compaction — Phase 4 or 5
- Session selector UI (`/resume`) — Phase 4
- Branch labels and named bookmarks (data model exists, no UI) — Phase 4
- Cross-session forking (pi's `pi --fork`) — Phase 5
- Sharing/export to HTML — Phase 5
- Refinement-aware tools (tools that decide what resolution to emit at) — Phase 3+
- Adaptive resolution policies (when to compact, what to attach) beyond the simple budget-driven default — Phase 4+

## Open questions for future iteration

This plan is the first deep pass. Several questions are deferred but worth flagging:

- **Resolution transitions.** Currently entries can move from `Full` to `Compacted` or `Attached`. Should the reverse be possible (un-compact, re-attach)? Probably yes for branching, no for normal flow, but the API surface for it is unspecified.
- **Calibrator persistence.** Should the per-model calibration be saved across sessions? A user who runs rho daily against the same model shouldn't have to re-bootstrap the estimator each launch. Probably yes, in `~/.rho/calibration.json`. Not in 2.5; flag for 2.6.
- **Multi-model sessions.** A session can change models mid-stream (`ModelChange` entry). The calibrator handles this correctly, but the path-fitting may need to know which model the *current* request is targeting to subtract the right schema overhead. The current plan assumes one model per request, which is true today.
- **`Compacted` entries during branching.** If the leaf moves backward to before a compaction point, does the compaction still apply? The clean answer is "compactions are scoped to a specific leaf path, branching creates a new path, recompute from scratch on the new path." Worth specifying explicitly when implementation hits it.
