//! Session phase detection.
//!
//! Phase 4: Track which phase of work the coding agent is in
//! (Exploration, Execution, Verification, Conclusion) so that
//! downstream systems (compaction, eviction, context stats) can make
//! phase-aware decisions.

/// The current phase of a coding session.
///
/// Phases are ordered and transitions follow a state machine:
///
/// ```text
/// Exploration → Execution → Verification → (Conclusion)
///      ↑              ↑             │
///      └──────────────┘─────────────┘
/// ```
///
/// The agent can revisit earlier phases (e.g., Verification → Execution
/// if more edits are needed). `Conclusion` is terminal within a
/// `run_loop` invocation.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SessionPhase {
    /// Reading files, listing directories, running initial diagnostics,
    /// looking up documentation. The model is gathering information.
    #[default]
    Exploration,

    /// Making changes: editing files, writing new files, running `cargo_fix`.
    /// The model is actively modifying the codebase.
    Execution,

    /// Confirming that changes work: running `cargo_check`, `cargo_test`,
    /// `cargo_clippy` after edits. The model is validating its work.
    Verification,

    /// The model has produced a final text reply with no further tool calls.
    /// Terminal within a `run_loop` invocation.
    Conclusion,
}

impl SessionPhase {
    /// Returns the phase name as a static string.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Exploration => "exploration",
            Self::Execution => "execution",
            Self::Verification => "verification",
            Self::Conclusion => "conclusion",
        }
    }
}

impl std::fmt::Display for SessionPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Classify a tool call's phase contribution.
///
/// Returns `Some(SessionPhase)` if the tool call suggests a specific phase.
/// Returns `None` for tools that don't clearly indicate a phase (e.g.,
/// extension tools, or tools whose phase depends on context).
#[must_use]
pub fn classify_tool_phase(tool_name: &str, has_had_edits: bool) -> Option<SessionPhase> {
    match tool_name {
        // Pure exploration tools — always Exploration
        "read_file" | "list_dir" | "rustdoc_lookup" | "crates_io_lookup" | "rustc_explain" => {
            Some(SessionPhase::Exploration)
        }

        // Pure execution tools — always Execution
        "edit_file" | "write_file" | "cargo_fix" | "run_command" => Some(SessionPhase::Execution),

        // Verification tools: Verification if edits have been made,
        // Exploration if this is the initial diagnostic run.
        "cargo_check" | "cargo_test" | "cargo_clippy" => {
            if has_had_edits {
                Some(SessionPhase::Verification)
            } else {
                Some(SessionPhase::Exploration)
            }
        }

        // Unknown / extension tools — no phase signal
        _ => None,
    }
}

/// Transition the current phase based on a new tool call.
///
/// Rules:
/// - `Conclusion` is absorbing — once reached, no transitions out.
/// - Any edit tool (`edit_file`, `write_file`, `cargo_fix`) forces `Execution`.
/// - Verification tools (`cargo_check`, etc.) go to `Verification` only if
///   `has_had_edits` is true; otherwise `Exploration`.
/// - Exploration tools (`read_file`, etc.) never downgrade from `Execution`
///   or `Verification` back to `Exploration`.
/// - Unknown tools don't change the phase.
pub fn transition_phase(
    current: SessionPhase,
    tool_name: &str,
    has_had_edits: bool,
) -> SessionPhase {
    // Conclusion is terminal
    if current == SessionPhase::Conclusion {
        return current;
    }

    match classify_tool_phase(tool_name, has_had_edits) {
        Some(target) => match target {
            SessionPhase::Execution => SessionPhase::Execution,
            SessionPhase::Verification => SessionPhase::Verification,
            SessionPhase::Exploration => {
                // Exploration tools don't downgrade from Execution/Verification.
                match current {
                    SessionPhase::Execution | SessionPhase::Verification => current,
                    _ => SessionPhase::Exploration,
                }
            }
            // Conclusion can never come from classify_tool_phase, but handle it.
            SessionPhase::Conclusion => current,
        },
        // Unknown tool — no transition
        None => current,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_exploration() {
        assert_eq!(SessionPhase::default(), SessionPhase::Exploration);
    }

    #[test]
    fn display_matches_as_str() {
        for phase in [
            SessionPhase::Exploration,
            SessionPhase::Execution,
            SessionPhase::Verification,
            SessionPhase::Conclusion,
        ] {
            assert_eq!(phase.as_str(), phase.to_string());
        }
    }

    // ── classify_tool_phase ─────────────────────────────────────────────

    #[test]
    fn read_file_is_exploration() {
        assert_eq!(
            classify_tool_phase("read_file", false),
            Some(SessionPhase::Exploration)
        );
    }

    #[test]
    fn list_dir_is_exploration() {
        assert_eq!(
            classify_tool_phase("list_dir", false),
            Some(SessionPhase::Exploration)
        );
    }

    #[test]
    fn rustdoc_lookup_is_exploration() {
        assert_eq!(
            classify_tool_phase("rustdoc_lookup", false),
            Some(SessionPhase::Exploration)
        );
    }

    #[test]
    fn crates_io_lookup_is_exploration() {
        assert_eq!(
            classify_tool_phase("crates_io_lookup", false),
            Some(SessionPhase::Exploration)
        );
    }

    #[test]
    fn rustc_explain_is_exploration() {
        assert_eq!(
            classify_tool_phase("rustc_explain", false),
            Some(SessionPhase::Exploration)
        );
    }

    #[test]
    fn edit_file_is_execution() {
        assert_eq!(
            classify_tool_phase("edit_file", false),
            Some(SessionPhase::Execution)
        );
    }

    #[test]
    fn write_file_is_execution() {
        assert_eq!(
            classify_tool_phase("write_file", false),
            Some(SessionPhase::Execution)
        );
    }

    #[test]
    fn cargo_fix_is_execution() {
        assert_eq!(
            classify_tool_phase("cargo_fix", false),
            Some(SessionPhase::Execution)
        );
    }

    #[test]
    fn run_command_is_execution() {
        assert_eq!(
            classify_tool_phase("run_command", false),
            Some(SessionPhase::Execution)
        );
    }

    #[test]
    fn cargo_check_exploration_without_edits() {
        assert_eq!(
            classify_tool_phase("cargo_check", false),
            Some(SessionPhase::Exploration)
        );
    }

    #[test]
    fn cargo_check_verification_with_edits() {
        assert_eq!(
            classify_tool_phase("cargo_check", true),
            Some(SessionPhase::Verification)
        );
    }

    #[test]
    fn cargo_test_exploration_without_edits() {
        assert_eq!(
            classify_tool_phase("cargo_test", false),
            Some(SessionPhase::Exploration)
        );
    }

    #[test]
    fn cargo_test_verification_with_edits() {
        assert_eq!(
            classify_tool_phase("cargo_test", true),
            Some(SessionPhase::Verification)
        );
    }

    #[test]
    fn cargo_clippy_verification_with_edits() {
        assert_eq!(
            classify_tool_phase("cargo_clippy", true),
            Some(SessionPhase::Verification)
        );
    }

    #[test]
    fn unknown_tool_is_none() {
        assert_eq!(classify_tool_phase("my_custom_tool", false), None);
    }

    // ── transition_phase ────────────────────────────────────────────────

    #[test]
    fn exploration_to_execution_on_edit() {
        assert_eq!(
            transition_phase(SessionPhase::Exploration, "edit_file", false),
            SessionPhase::Execution
        );
    }

    #[test]
    fn exploration_to_exploration_on_read() {
        assert_eq!(
            transition_phase(SessionPhase::Exploration, "read_file", false),
            SessionPhase::Exploration
        );
    }

    #[test]
    fn exploration_to_exploration_on_initial_cargo_check() {
        assert_eq!(
            transition_phase(SessionPhase::Exploration, "cargo_check", false),
            SessionPhase::Exploration
        );
    }

    #[test]
    fn execution_stays_on_read() {
        // Reading files during Execution doesn't downgrade to Exploration
        assert_eq!(
            transition_phase(SessionPhase::Execution, "read_file", false),
            SessionPhase::Execution
        );
    }

    #[test]
    fn execution_to_verification_on_cargo_check() {
        assert_eq!(
            transition_phase(SessionPhase::Execution, "cargo_check", true),
            SessionPhase::Verification
        );
    }

    #[test]
    fn verification_stays_on_read() {
        assert_eq!(
            transition_phase(SessionPhase::Verification, "read_file", false),
            SessionPhase::Verification
        );
    }

    #[test]
    fn verification_to_execution_on_edit() {
        assert_eq!(
            transition_phase(SessionPhase::Verification, "edit_file", false),
            SessionPhase::Execution
        );
    }

    #[test]
    fn verification_to_verification_on_cargo_test() {
        assert_eq!(
            transition_phase(SessionPhase::Verification, "cargo_test", true),
            SessionPhase::Verification
        );
    }

    #[test]
    fn conclusion_is_absorbing() {
        assert_eq!(
            transition_phase(SessionPhase::Conclusion, "edit_file", false),
            SessionPhase::Conclusion
        );
        assert_eq!(
            transition_phase(SessionPhase::Conclusion, "cargo_check", true),
            SessionPhase::Conclusion
        );
        assert_eq!(
            transition_phase(SessionPhase::Conclusion, "read_file", false),
            SessionPhase::Conclusion
        );
    }

    #[test]
    fn unknown_tool_preserves_current_phase() {
        assert_eq!(
            transition_phase(SessionPhase::Exploration, "ext_custom", false),
            SessionPhase::Exploration
        );
        assert_eq!(
            transition_phase(SessionPhase::Execution, "ext_custom", false),
            SessionPhase::Execution
        );
        assert_eq!(
            transition_phase(SessionPhase::Verification, "ext_custom", false),
            SessionPhase::Verification
        );
    }

    // ── Full workflow simulation ─────────────────────────────────────────

    #[test]
    fn typical_fix_workflow() {
        let mut phase = SessionPhase::Exploration;
        let mut has_edits = false;

        // Initial exploration: read file, check errors
        phase = transition_phase(phase, "read_file", has_edits);
        assert_eq!(phase, SessionPhase::Exploration);

        phase = transition_phase(phase, "cargo_check", has_edits);
        assert_eq!(phase, SessionPhase::Exploration);

        // Edit to fix
        phase = transition_phase(phase, "edit_file", has_edits);
        assert_eq!(phase, SessionPhase::Execution);
        has_edits = true;

        // Verify fix
        phase = transition_phase(phase, "cargo_check", has_edits);
        assert_eq!(phase, SessionPhase::Verification);

        // More verification
        phase = transition_phase(phase, "cargo_test", has_edits);
        assert_eq!(phase, SessionPhase::Verification);
    }

    #[test]
    fn edit_verify_reedit_cycle() {
        let mut phase = SessionPhase::Exploration;
        let mut has_edits = false;

        // Explore
        phase = transition_phase(phase, "read_file", has_edits);
        assert_eq!(phase, SessionPhase::Exploration);

        // Edit
        phase = transition_phase(phase, "edit_file", has_edits);
        assert_eq!(phase, SessionPhase::Execution);
        has_edits = true;

        // Verify — still broken
        phase = transition_phase(phase, "cargo_check", has_edits);
        assert_eq!(phase, SessionPhase::Verification);

        // Re-edit
        phase = transition_phase(phase, "edit_file", has_edits);
        assert_eq!(phase, SessionPhase::Execution);

        // Verify again — now clean
        phase = transition_phase(phase, "cargo_check", has_edits);
        assert_eq!(phase, SessionPhase::Verification);
    }

    #[test]
    fn exploration_only_workflow() {
        let mut phase = SessionPhase::Exploration;

        phase = transition_phase(phase, "read_file", false);
        assert_eq!(phase, SessionPhase::Exploration);

        phase = transition_phase(phase, "rustdoc_lookup", false);
        assert_eq!(phase, SessionPhase::Exploration);

        phase = transition_phase(phase, "cargo_check", false);
        assert_eq!(phase, SessionPhase::Exploration);
    }
}
