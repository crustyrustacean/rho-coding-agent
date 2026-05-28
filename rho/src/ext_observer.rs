//! Composite observer that delegates to multiple [`AgentObserver`] instances.
//!
//! The agent loop takes a single `&dyn AgentObserver`. When extensions are
//! loaded, we need to forward events to both the REPL's built-in observer
//! and any extension observers. [`CompositeObserver`] holds a list of
//! observers and fans out every call.

use rho_core::agent::{AgentState, InterceptResult};
use rho_core::tool::{ToolResult, ToolRisk};
use rho_core::AgentObserver;

/// An [`AgentObserver`] that delegates to zero or more inner observers.
///
/// For interception, the **first `Block` wins** — if any observer blocks
/// a tool call, the call is denied immediately.
pub struct CompositeObserver<'a> {
    observers: Vec<&'a dyn AgentObserver>,
}

impl<'a> CompositeObserver<'a> {
    /// Create a composite from a list of observers.
    pub fn new(observers: Vec<&'a dyn AgentObserver>) -> Self {
        Self { observers }
    }
}

impl AgentObserver for CompositeObserver<'_> {
    fn on_state_change(&self, state: AgentState) {
        for obs in &self.observers {
            obs.on_state_change(state.clone());
        }
    }

    fn on_text_delta(&self, delta: &str) {
        for obs in &self.observers {
            obs.on_text_delta(delta);
        }
    }

    fn on_reasoning_delta(&self, delta: &str) {
        for obs in &self.observers {
            obs.on_reasoning_delta(delta);
        }
    }

    fn on_tool_call(&self, name: &str, arguments: &str) {
        for obs in &self.observers {
            obs.on_tool_call(name, arguments);
        }
    }

    fn on_tool_result(&self, name: &str, result: &ToolResult) {
        for obs in &self.observers {
            obs.on_tool_result(name, result);
        }
    }

    fn on_tool_denied(&self, name: &str) {
        for obs in &self.observers {
            obs.on_tool_denied(name);
        }
    }

    fn on_approval_requested(&self, tool_name: &str, risk: ToolRisk) {
        for obs in &self.observers {
            obs.on_approval_requested(tool_name, risk.clone());
        }
    }

    fn on_tool_call_intercept(&self, name: &str, arguments: &str) -> Option<InterceptResult> {
        for obs in &self.observers {
            if let Some(result) = obs.on_tool_call_intercept(name, arguments) {
                // First Block wins — return immediately.
                if matches!(result, InterceptResult::Block { .. }) {
                    return Some(result);
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rho_core::agent::NopObserver;

    #[test]
    fn composite_delegates_state_change() {
        struct Recording(std::sync::Mutex<Vec<String>>);
        impl AgentObserver for Recording {
            fn on_state_change(&self, state: AgentState) {
                self.0.lock().unwrap().push(format!("{state:?}"));
            }
        }

        let r1 = Recording(std::sync::Mutex::new(vec![]));
        let r2 = Recording(std::sync::Mutex::new(vec![]));
        let composite = CompositeObserver::new(vec![&r1, &r2]);
        composite.on_state_change(AgentState::Thinking);

        assert_eq!(*r1.0.lock().unwrap(), vec!["Thinking"]);
        assert_eq!(*r2.0.lock().unwrap(), vec!["Thinking"]);
    }

    #[test]
    fn composite_first_block_wins() {
        struct AlwaysBlock;
        impl AgentObserver for AlwaysBlock {
            fn on_tool_call_intercept(
                &self,
                _name: &str,
                _arguments: &str,
            ) -> Option<InterceptResult> {
                Some(InterceptResult::Block {
                    reason: "blocked".into(),
                })
            }
        }

        let nop = NopObserver;
        let blocker = AlwaysBlock;
        let composite = CompositeObserver::new(vec![&nop, &blocker]);
        let result = composite.on_tool_call_intercept("test", "{}");
        assert!(matches!(result, Some(InterceptResult::Block { .. })));
    }
}
