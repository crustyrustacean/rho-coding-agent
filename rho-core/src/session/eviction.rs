//! Turn-internal eviction planner.
//!
//! Phase 3: Instead of evicting entire turns when the context window is full,
//! this module plans selective downgrades of individual entries *within* turns.
//!
//! The planner function takes the current entry path and budget,
//! and returns a list of downgrade actions specifying which entries
//! should be downgraded and to what resolution. The caller (typically
//! `Session::path_messages`) applies these actions.
//!
//! # Downgrade priority
//!
//! 1. Tool results in the oldest turns first (oldest-first eviction order)
//! 2. Within a turn, the largest tool results first (biggest savings)
//! 3. Outline first, then Summarize if still over budget
//!
//! # Invariants preserved
//!
//! - System message never downgraded
//! - Assistant messages with `tool_calls`: content is outlined but `tool_calls`
//!   are preserved (handled by the existing `render_reduced_fidelity`)
//! - First and last user turns are protected (existing invariant from
//!   `SlidingWindowContextManager`)
//! - Tool-pair integrity: if an Assistant message is outlined, its Tool
//!   results can also be outlined without breaking the pair

use crate::context::{TokenBudget, approximate_tokens, estimate_tool_schema_overhead};
use crate::message::ChatMessage;
use crate::session::Entry;
use crate::session::entry::EntryPayload;
use crate::session::entry::EntryResolution;
use crate::session::estimator::TokenEstimator;

/// An action to downgrade an entry's resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DowngradeAction {
    /// The entry to downgrade.
    pub entry_id: crate::newtypes::EntryId,
    /// The target resolution.
    pub target: DowngradeTarget,
}

/// The target resolution for a downgrade.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DowngradeTarget {
    /// Replace with a structural outline (~10-20% of original tokens).
    Outline,
    /// Replace with a short summary (~5-10% of original tokens).
    Summarize,
}

/// The result of the downgrade planner.
#[derive(Clone, Debug, Default)]
pub(crate) struct DowngradePlan {
    /// The planned downgrade actions, in priority order.
    pub actions: Vec<DowngradeAction>,
}

impl DowngradePlan {
    /// Returns `true` if the plan contains no actions.
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    /// Estimated tokens saved by applying this plan.
    ///
    /// This is a rough estimate: each Outline saves ~80% and each Summarize
    /// saves ~90% of the entry's token cost. The exact savings depend on the
    /// generated outline/summary text.
    #[allow(dead_code)]
    pub fn estimated_savings(&self, entries: &[&Entry]) -> usize {
        let entry_map: std::collections::HashMap<_, _> = entries
            .iter()
            .map(|e| (&e.id, approximate_tokens_for_entry(e)))
            .collect();

        self.actions
            .iter()
            .map(|a| {
                let original = entry_map.get(&a.entry_id).copied().unwrap_or(0);
                match a.target {
                    DowngradeTarget::Outline => original * 8 / 10, // ~80% savings
                    DowngradeTarget::Summarize => original * 9 / 10, // ~90% savings
                }
            })
            .sum()
    }
}

/// Group entries into turns for eviction planning.
///
/// A turn is either:
/// - A single `User` or `System` message
/// - An `Assistant { tool_calls }` message plus all its matching `Tool` results
///
/// Returns a list of turns, where each turn is a list of entry indices
/// into the original `entries` slice.
fn group_entries_into_turns(entries: &[&Entry]) -> Vec<Vec<usize>> {
    let mut turns: Vec<Vec<usize>> = Vec::new();
    let mut i = 0;

    while i < entries.len() {
        if let EntryPayload::Message(ChatMessage::Assistant { tool_calls, .. }) =
            &entries[i].payload
        {
            let ids: Vec<&str> = tool_calls.iter().map(|c| c.id.as_ref()).collect();
            let mut turn = vec![i];
            i += 1;
            // Absorb matching Tool results
            while i < entries.len() {
                if let EntryPayload::Message(ChatMessage::Tool { tool_call_id, .. }) =
                    &entries[i].payload
                    && ids.contains(&tool_call_id.as_ref())
                {
                    turn.push(i);
                    i += 1;
                    continue;
                }
                break;
            }
            turns.push(turn);
        } else {
            turns.push(vec![i]);
            i += 1;
        }
    }

    turns
}

/// Estimate the token cost of a single entry's message content.
fn approximate_tokens_for_entry(entry: &Entry) -> usize {
    match &entry.payload {
        EntryPayload::Message(msg) => approximate_tokens(msg),
        _ => 1,
    }
}

/// Estimate the token cost of entries at their current resolution.
///
/// Outlined/Summarized entries count their reduced text. Full entries
/// count their full content.
fn estimate_tokens_at_resolution(entries: &[&Entry]) -> usize {
    entries.iter().map(|e| estimate_entry_token_cost(e)).sum()
}

/// Estimate the token cost of a single entry at its current resolution.
fn estimate_entry_token_cost(entry: &Entry) -> usize {
    match &entry.resolution {
        EntryResolution::Full | EntryResolution::Pinned => approximate_tokens_for_entry(entry),
        EntryResolution::Outlined { outline } => {
            approximate_tokens(&ChatMessage::user_text(outline))
        }
        EntryResolution::Summarized { summary } => {
            approximate_tokens(&ChatMessage::user_text(summary))
        }
        EntryResolution::Compacted { .. } | EntryResolution::Attached => 0,
    }
}

/// Plan selective downgrades to bring the entry path within budget.
///
/// Analyzes the entry path (chronological order) and returns a list of
/// downgrade actions that, when applied, should bring the total token
/// count within the given budget.
///
/// The planner:
/// 1. Computes the adjusted budget (subtracting tool schema + system overhead)
/// 2. Estimates current token usage at current resolutions
/// 3. If within budget, returns an empty plan
/// 4. Otherwise, iterates turns oldest-first, targeting tool results
///    (largest first) for downgrade: Outline first, then Summarize
/// 5. Stops when estimated savings cover the excess
///
/// # Invariants
///
/// - Never downgrades the system message (first entry)
/// - Never downgrades the last turn (the active request)
/// - Never downgrades the first user turn (original request)
/// - Never downgrades pinned entries
/// - Never downgrades already-downgraded entries
pub(crate) fn plan_downgrades(
    entries: &[&Entry],
    budget: TokenBudget,
    estimator: &dyn TokenEstimator,
    tool_schemas: &[rho_ai::ToolDefinition],
) -> DowngradePlan {
    if entries.len() <= 2 {
        return DowngradePlan::default();
    }

    // Step 1: Compute adjusted budget
    let schema_overhead = estimate_tool_schema_overhead(tool_schemas, estimator);
    let system_overhead = entries
        .first()
        .map_or(0, |e| approximate_tokens_for_entry(e));
    let available = budget
        .prompt_budget()
        .saturating_sub(schema_overhead)
        .saturating_sub(system_overhead);

    // Step 2: Estimate current token usage (skip system entry at index 0)
    let non_system_entries: Vec<&Entry> = entries[1..].to_vec();
    let current_usage = estimate_tokens_at_resolution(&non_system_entries);

    let excess = current_usage.saturating_sub(available);
    if excess == 0 {
        return DowngradePlan::default();
    }

    // Step 3: Group into turns
    let turns = group_entries_into_turns(entries);

    // Step 4: Identify protected turn indices
    let last_turn_idx = turns.len().saturating_sub(1);
    let first_user_turn = turns.iter().position(|t| {
        t.first().is_some_and(|&idx| {
            matches!(
                &entries[idx].payload,
                EntryPayload::Message(ChatMessage::User { .. })
            )
        })
    });

    // Step 5: Collect downgradeable entries across all turns, oldest first
    let mut candidates: Vec<(usize, usize)> = Vec::new(); // (turn_idx, entry_index_in_entries)

    for (turn_idx, turn) in turns.iter().enumerate() {
        // Skip protected turns
        if turn_idx == 0 {
            // System message turn
            continue;
        }
        if turn_idx == last_turn_idx {
            // Active request turn
            continue;
        }
        if first_user_turn == Some(turn_idx) {
            // First user turn (original request)
            continue;
        }

        for &entry_idx in turn {
            let entry = entries[entry_idx];

            // Skip already-downgraded, compacted, attached, or pinned entries
            if !matches!(entry.resolution, EntryResolution::Full) {
                continue;
            }

            // Only downgrade entries with meaningful content
            let tokens = approximate_tokens_for_entry(entry);
            if tokens <= 10 {
                continue; // Too small to bother
            }

            candidates.push((turn_idx, entry_idx));
        }
    }

    // Step 6: Sort candidates — oldest turn first, largest entry first within a turn
    candidates.sort_by(|a, b| {
        a.0.cmp(&b.0).then_with(|| {
            let tokens_a = approximate_tokens_for_entry(entries[a.1]);
            let tokens_b = approximate_tokens_for_entry(entries[b.1]);
            tokens_b.cmp(&tokens_a) // largest first
        })
    });

    // Step 7: Greedily select downgrades until excess is covered
    let mut actions = Vec::new();
    let mut covered: std::collections::HashSet<crate::newtypes::EntryId> =
        std::collections::HashSet::new();
    let mut estimated_savings: usize = 0;

    for &(_turn_idx, entry_idx) in &candidates {
        if estimated_savings >= excess {
            break;
        }

        let entry = entries[entry_idx];
        if covered.contains(&entry.id) {
            continue;
        }
        covered.insert(entry.id.clone());

        let tokens = approximate_tokens_for_entry(entry);
        let action = DowngradeAction {
            entry_id: entry.id.clone(),
            target: DowngradeTarget::Outline,
        };
        estimated_savings += tokens * 8 / 10;
        actions.push(action);

        // If outlining alone won't cover the excess, add a Summarize step
        // for this entry as a second action (will be applied after Outline)
        if estimated_savings < excess {
            let extra = tokens / 10; // additional savings from Outline→Summarize
            actions.push(DowngradeAction {
                entry_id: entry.id.clone(),
                target: DowngradeTarget::Summarize,
            });
            estimated_savings += extra;
        }
    }

    DowngradePlan { actions }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::needless_borrows_for_generic_args)]
mod tests {
    use super::*;
    use crate::message::{ChatMessage, ModelToolCall, ToolCallFunction};
    use crate::newtypes::{EntryId, ToolCallId, ToolName};
    use crate::session::HeuristicEstimator;
    use crate::session::entry::{Entry, EntryPayload, EntryResolution};
    use std::time::SystemTime;

    fn test_entry_full(payload: EntryPayload) -> Entry {
        Entry {
            id: EntryId::new(),
            parent_id: None,
            timestamp: SystemTime::UNIX_EPOCH,
            resolution: EntryResolution::Full,
            payload,
        }
    }

    fn system_entry() -> Entry {
        test_entry_full(EntryPayload::Message(ChatMessage::system_text(
            "you are rho",
        )))
    }

    fn user_entry(text: &str) -> Entry {
        test_entry_full(EntryPayload::Message(ChatMessage::user_text(text)))
    }

    fn assistant_entry_with_tool(call_id: &str) -> Entry {
        test_entry_full(EntryPayload::Message(ChatMessage::Assistant {
            content: vec![],
            tool_calls: vec![ModelToolCall {
                id: ToolCallId::from(call_id),
                call_type: "function".to_owned(),
                function: ToolCallFunction {
                    name: ToolName::from("read_file"),
                    arguments: "{}".to_owned(),
                },
            }],
        }))
    }

    fn tool_entry(call_id: &str, content: &str) -> Entry {
        test_entry_full(EntryPayload::Message(ChatMessage::tool_result(
            ToolCallId::from(call_id),
            content,
        )))
    }

    fn as_refs(entries: &[Entry]) -> Vec<&Entry> {
        entries.iter().collect()
    }

    fn tiny_budget() -> TokenBudget {
        // Prompt budget = 100 - 50 (reserve) = 50
        // With system overhead ~20, available for messages ~30
        TokenBudget::with_reserve(100, 50)
    }

    // ── group_entries_into_turns ──────────────────────────────────────

    #[test]
    fn group_simple_conversation() {
        let entries: Vec<Entry> = vec![
            system_entry(),
            user_entry("hello"),
            test_entry_full(EntryPayload::Message(ChatMessage::assistant_text("hi"))),
            user_entry("bye"),
        ];
        let refs = as_refs(&entries);
        let turns = group_entries_into_turns(&refs);

        // System, User("hello"), Assistant("hi"), User("bye") — 4 separate turns
        assert_eq!(turns.len(), 4);
        assert_eq!(turns[0], vec![0]);
        assert_eq!(turns[1], vec![1]);
        assert_eq!(turns[2], vec![2]);
        assert_eq!(turns[3], vec![3]);
    }

    #[test]
    fn group_tool_call_turn() {
        let entries: Vec<Entry> = vec![
            system_entry(),
            user_entry("read files"),
            assistant_entry_with_tool("call_1"),
            tool_entry("call_1", "file content here"),
            user_entry("done"),
        ];
        let refs = as_refs(&entries);
        let turns = group_entries_into_turns(&refs);

        // Turn 0: System, Turn 1: User, Turn 2: Assistant+Tool, Turn 3: User
        assert_eq!(turns.len(), 4);
        assert_eq!(turns[2], vec![2, 3]); // Assistant + Tool grouped
    }

    #[test]
    fn group_multiple_tool_calls_in_one_turn() {
        let entries: Vec<Entry> = vec![
            system_entry(),
            user_entry("go"),
            test_entry_full(EntryPayload::Message(ChatMessage::Assistant {
                content: vec![],
                tool_calls: vec![
                    ModelToolCall {
                        id: ToolCallId::from("c1"),
                        call_type: "function".to_owned(),
                        function: ToolCallFunction {
                            name: ToolName::from("read_file"),
                            arguments: "{}".to_owned(),
                        },
                    },
                    ModelToolCall {
                        id: ToolCallId::from("c2"),
                        call_type: "function".to_owned(),
                        function: ToolCallFunction {
                            name: ToolName::from("run_command"),
                            arguments: "{}".to_owned(),
                        },
                    },
                ],
            })),
            tool_entry("c1", "file1"),
            tool_entry("c2", "output"),
            user_entry("next"),
        ];
        let refs = as_refs(&entries);
        let turns = group_entries_into_turns(&refs);

        assert_eq!(turns.len(), 4);
        assert_eq!(turns[2], vec![2, 3, 4]); // Assistant + Tool1 + Tool2
    }

    // ── plan_downgrades ──────────────────────────────────────────────

    #[test]
    fn plan_downgrades_returns_empty_when_within_budget() {
        let entries: Vec<Entry> = vec![system_entry(), user_entry("hello")];
        let refs = as_refs(&entries);
        let estimator = HeuristicEstimator::new();

        // Generous budget
        let plan = plan_downgrades(&refs, TokenBudget::new(32_768), &estimator, &[]);
        assert!(plan.is_empty());
    }

    #[test]
    fn plan_downgrades_targets_tool_results_in_old_turns() {
        // Build a scenario where a tool result in a middle turn is over budget
        let entries: Vec<Entry> = vec![
            system_entry(),                         // Turn 0 (protected)
            user_entry("fix the bug"),              // Turn 1 (first user, protected)
            assistant_entry_with_tool("call_1"),    // Turn 2
            tool_entry("call_1", &"x".repeat(500)), // Turn 2 — large tool result
            user_entry("current question"),         // Turn 3 (last, protected)
        ];
        let refs = as_refs(&entries);
        let estimator = HeuristicEstimator::new();

        // Very tight budget — should downgrade the tool result in turn 2
        let plan = plan_downgrades(&refs, tiny_budget(), &estimator, &[]);

        // Should have at least one action targeting the tool result
        assert!(!plan.is_empty(), "plan should have downgrade actions");

        let tool_action = plan.actions.iter().find(|a| {
            let entry = refs.iter().find(|e| e.id == a.entry_id);
            entry.is_some_and(|e| {
                matches!(&e.payload, EntryPayload::Message(ChatMessage::Tool { .. }))
            })
        });
        assert!(
            tool_action.is_some(),
            "should target a tool result for downgrade"
        );
    }

    #[test]
    fn plan_downgrades_never_targets_system_message() {
        let sys_id;
        let entries: Vec<Entry> = {
            let sys = system_entry();
            sys_id = sys.id.clone();
            vec![
                sys,
                user_entry(&"x".repeat(500)),
                test_entry_full(EntryPayload::Message(ChatMessage::assistant_text(
                    &"x".repeat(500),
                ))),
                user_entry(&"y".repeat(500)),
            ]
        };
        let refs = as_refs(&entries);
        let estimator = HeuristicEstimator::new();

        let plan = plan_downgrades(&refs, tiny_budget(), &estimator, &[]);
        assert!(
            !plan.actions.iter().any(|a| a.entry_id == sys_id),
            "should never downgrade the system message"
        );
    }

    #[test]
    fn plan_downgrades_never_targets_first_user_turn() {
        let first_user_id;
        let entries: Vec<Entry> = {
            let u = user_entry("remember the secret: TIGER-7742");
            first_user_id = u.id.clone();
            vec![
                system_entry(),
                u,
                test_entry_full(EntryPayload::Message(ChatMessage::assistant_text("ok"))),
                user_entry(&"x".repeat(500)),
                test_entry_full(EntryPayload::Message(ChatMessage::assistant_text("ok2"))),
                user_entry("current"),
            ]
        };
        let refs = as_refs(&entries);
        let estimator = HeuristicEstimator::new();

        let plan = plan_downgrades(&refs, tiny_budget(), &estimator, &[]);
        assert!(
            !plan.actions.iter().any(|a| a.entry_id == first_user_id),
            "should never downgrade the first user turn"
        );
    }

    #[test]
    fn plan_downgrades_never_targets_last_turn() {
        let last_user_id;
        let entries: Vec<Entry> = {
            let u = user_entry("current question");
            last_user_id = u.id.clone();
            vec![
                system_entry(),
                user_entry("original"),
                test_entry_full(EntryPayload::Message(ChatMessage::assistant_text(
                    &"x".repeat(500),
                ))),
                u,
            ]
        };
        let refs = as_refs(&entries);
        let estimator = HeuristicEstimator::new();

        let plan = plan_downgrades(&refs, tiny_budget(), &estimator, &[]);
        assert!(
            !plan.actions.iter().any(|a| a.entry_id == last_user_id),
            "should never downgrade the last turn"
        );
    }

    #[test]
    fn plan_downgrades_never_targets_pinned_entries() {
        let pinned_id = EntryId::new();
        let mut pinned_entry = user_entry("important plan");
        pinned_entry.id = pinned_id.clone();
        pinned_entry.resolution = EntryResolution::Pinned;

        let entries: Vec<Entry> = vec![
            system_entry(),
            pinned_entry,
            test_entry_full(EntryPayload::Message(ChatMessage::assistant_text(
                &"x".repeat(500),
            ))),
            user_entry("current"),
        ];
        let refs = as_refs(&entries);
        let estimator = HeuristicEstimator::new();

        let plan = plan_downgrades(&refs, tiny_budget(), &estimator, &[]);
        assert!(
            !plan.actions.iter().any(|a| a.entry_id == pinned_id),
            "should never downgrade pinned entries"
        );
    }

    #[test]
    fn plan_downgrades_never_targets_already_downgraded() {
        let outlined_id = EntryId::new();
        let mut outlined_entry = tool_entry("call_1", "big content");
        outlined_entry.id = outlined_id.clone();
        outlined_entry.resolution = EntryResolution::Outlined {
            outline: "already outlined".to_owned(),
        };

        let entries: Vec<Entry> = vec![
            system_entry(),
            user_entry("original"),
            assistant_entry_with_tool("call_1"),
            outlined_entry,
            user_entry("current"),
        ];
        let refs = as_refs(&entries);
        let estimator = HeuristicEstimator::new();

        let plan = plan_downgrades(&refs, tiny_budget(), &estimator, &[]);
        assert!(
            !plan.actions.iter().any(|a| a.entry_id == outlined_id),
            "should not downgrade already-outlined entries"
        );
    }

    #[test]
    fn plan_downgrades_prefers_largest_entries_first() {
        let small_id;
        let big_id;
        let entries: Vec<Entry> = {
            let small = tool_entry("c1", "tiny");
            small_id = small.id.clone();
            let big = tool_entry("c2", &"x".repeat(2000));
            big_id = big.id.clone();
            vec![
                system_entry(),
                user_entry("original"),
                // Turn 2: assistant + 2 tool results
                test_entry_full(EntryPayload::Message(ChatMessage::Assistant {
                    content: vec![],
                    tool_calls: vec![
                        ModelToolCall {
                            id: ToolCallId::from("c1"),
                            call_type: "function".to_owned(),
                            function: ToolCallFunction {
                                name: ToolName::from("read_file"),
                                arguments: "{}".to_owned(),
                            },
                        },
                        ModelToolCall {
                            id: ToolCallId::from("c2"),
                            call_type: "function".to_owned(),
                            function: ToolCallFunction {
                                name: ToolName::from("read_file"),
                                arguments: "{}".to_owned(),
                            },
                        },
                    ],
                })),
                small,
                big,
                user_entry("current"),
            ]
        };
        let refs = as_refs(&entries);
        let estimator = HeuristicEstimator::new();

        // Tight budget — should target the large tool result first
        let plan = plan_downgrades(&refs, tiny_budget(), &estimator, &[]);

        if plan.actions.len() >= 2 {
            // The big entry should come before the small entry
            let big_pos = plan.actions.iter().position(|a| a.entry_id == big_id);
            let small_pos = plan.actions.iter().position(|a| a.entry_id == small_id);
            assert!(big_pos.is_some(), "should target the big tool result");
            if let (Some(bp), Some(sp)) = (big_pos, small_pos) {
                assert!(
                    bp < sp,
                    "large entries should be downgraded before small ones"
                );
            }
        }
    }

    #[test]
    fn plan_downgrades_targets_oldest_turns_first() {
        let old_tool_id;
        let new_tool_id;
        let entries: Vec<Entry> = {
            let old = tool_entry("c1", &"x".repeat(500));
            old_tool_id = old.id.clone();
            let new = tool_entry("c2", &"x".repeat(500));
            new_tool_id = new.id.clone();
            vec![
                system_entry(),
                user_entry("original"),
                // Old turn
                assistant_entry_with_tool("c1"),
                old,
                // Middle turn
                user_entry("middle question"),
                test_entry_full(EntryPayload::Message(ChatMessage::assistant_text("ok"))),
                // New turn
                assistant_entry_with_tool("c2"),
                new,
                user_entry("current"),
            ]
        };
        let refs = as_refs(&entries);
        let estimator = HeuristicEstimator::new();

        let plan = plan_downgrades(&refs, tiny_budget(), &estimator, &[]);

        let old_pos = plan.actions.iter().position(|a| a.entry_id == old_tool_id);
        let new_pos = plan.actions.iter().position(|a| a.entry_id == new_tool_id);
        assert!(old_pos.is_some(), "should target tool result in older turn");
        if let (Some(op), Some(np)) = (old_pos, new_pos) {
            assert!(
                op < np,
                "older turn entries should be downgraded before newer ones"
            );
        }
    }

    #[test]
    fn plan_downgrades_estimated_savings_is_positive() {
        let entries: Vec<Entry> = vec![
            system_entry(),
            user_entry("original"),
            assistant_entry_with_tool("call_1"),
            tool_entry("call_1", &"x".repeat(500)),
            user_entry("current"),
        ];
        let refs = as_refs(&entries);
        let estimator = HeuristicEstimator::new();

        let plan = plan_downgrades(&refs, tiny_budget(), &estimator, &[]);
        if !plan.is_empty() {
            assert!(
                plan.estimated_savings(&refs) > 0,
                "non-empty plan should have positive estimated savings"
            );
        }
    }

    #[test]
    fn plan_downgrades_with_tiny_session() {
        // Only system + 1 user — nothing to downgrade
        let entries: Vec<Entry> = vec![system_entry(), user_entry("hello")];
        let refs = as_refs(&entries);
        let estimator = HeuristicEstimator::new();

        let plan = plan_downgrades(&refs, tiny_budget(), &estimator, &[]);
        assert!(plan.is_empty());
    }

    #[test]
    fn plan_downgrades_empty_entries() {
        let refs: Vec<&Entry> = vec![];
        let estimator = HeuristicEstimator::new();

        let plan = plan_downgrades(&refs, TokenBudget::default(), &estimator, &[]);
        assert!(plan.is_empty());
    }

    // ── DowngradePlan ─────────────────────────────────────────────────

    #[test]
    fn downgrade_plan_is_empty_for_default() {
        let plan = DowngradePlan::default();
        assert!(plan.is_empty());
        assert_eq!(plan.estimated_savings(&[]), 0);
    }

    #[test]
    fn downgrade_plan_is_empty_when_no_actions() {
        let plan = DowngradePlan { actions: vec![] };
        assert!(plan.is_empty());
    }

    #[test]
    fn downgrade_plan_not_empty_with_actions() {
        let id = EntryId::new();
        let plan = DowngradePlan {
            actions: vec![DowngradeAction {
                entry_id: id,
                target: DowngradeTarget::Outline,
            }],
        };
        assert!(!plan.is_empty());
    }

    // ── estimate_tokens_at_resolution ────────────────────────────────

    #[test]
    fn estimate_tokens_skips_compacted() {
        let entry = Entry {
            id: EntryId::new(),
            parent_id: None,
            timestamp: SystemTime::UNIX_EPOCH,
            resolution: EntryResolution::Compacted {
                into: EntryId::new(),
            },
            payload: EntryPayload::Message(ChatMessage::user_text("x".repeat(1000))),
        };
        assert_eq!(estimate_entry_token_cost(&entry), 0);
    }

    #[test]
    fn estimate_tokens_uses_outline_for_outlined() {
        let entry = Entry {
            id: EntryId::new(),
            parent_id: None,
            timestamp: SystemTime::UNIX_EPOCH,
            resolution: EntryResolution::Outlined {
                outline: "short".to_owned(),
            },
            payload: EntryPayload::Message(ChatMessage::user_text("x".repeat(1000))),
        };
        let tokens = estimate_entry_token_cost(&entry);
        // "short" should be very few tokens (< 10)
        assert!(
            tokens < 10,
            "outlined entry should use ~1 token for 'short', got {tokens}"
        );
    }

    // ── DowngradeTarget and DowngradeAction ────────────────────────────

    #[test]
    fn downgrade_action_equality() {
        let id = EntryId::new();
        let a1 = DowngradeAction {
            entry_id: id.clone(),
            target: DowngradeTarget::Outline,
        };
        let a2 = DowngradeAction {
            entry_id: id.clone(),
            target: DowngradeTarget::Outline,
        };
        assert_eq!(a1, a2);
    }

    #[test]
    fn downgrade_target_inequality() {
        let id = EntryId::new();
        let a1 = DowngradeAction {
            entry_id: id.clone(),
            target: DowngradeTarget::Outline,
        };
        let a2 = DowngradeAction {
            entry_id: id,
            target: DowngradeTarget::Summarize,
        };
        assert_ne!(a1, a2);
    }
}
