//! Composite observer that delegates to multiple [`AgentObserver`] instances.
//!
//! The agent loop takes a single `&dyn AgentObserver`. When extensions are
//! loaded, we need to forward events to both the REPL's built-in observer
//! and any extension observers. [`CompositeObserver`] holds a list of
//! observers and fans out every call.

use async_trait::async_trait;
use rho_core::AgentObserver;
use rho_core::agent::{AgentState, InterceptResult};
use rho_core::tool::{ToolResult, ToolRisk};

/// An [`AgentObserver`] that delegates to zero or more inner observers.
///
/// For interception, the **first `Block` wins** — if any observer blocks
/// a tool call, the call is denied immediately.
pub struct CompositeObserver<'a> {
    /// Inner observers.
    observers: Vec<&'a dyn AgentObserver>,
}

impl<'a> CompositeObserver<'a> {
    /// Create a composite from a list of observers.
    pub fn new(observers: Vec<&'a dyn AgentObserver>) -> Self {
        Self { observers }
    }
}

#[async_trait]
impl AgentObserver for CompositeObserver<'_> {
    async fn on_state_change(&self, state: AgentState) {
        for obs in &self.observers {
            obs.on_state_change(state.clone()).await;
        }
    }

    async fn on_text_delta(&self, delta: &str) {
        for obs in &self.observers {
            obs.on_text_delta(delta).await;
        }
    }

    async fn on_reasoning_delta(&self, delta: &str) {
        for obs in &self.observers {
            obs.on_reasoning_delta(delta).await;
        }
    }

    async fn on_tool_call(&self, name: &str, arguments: &str) {
        for obs in &self.observers {
            obs.on_tool_call(name, arguments).await;
        }
    }

    async fn on_tool_result(&self, name: &str, result: &ToolResult) {
        for obs in &self.observers {
            obs.on_tool_result(name, result).await;
        }
    }

    async fn on_tool_denied(&self, name: &str) {
        for obs in &self.observers {
            obs.on_tool_denied(name).await;
        }
    }

    async fn on_approval_requested(&self, tool_name: &str, risk: ToolRisk) {
        for obs in &self.observers {
            obs.on_approval_requested(tool_name, risk).await;
        }
    }

    async fn on_usage(
        &self,
        iteration: u32,
        usage: &rho_core::IterationUsage,
        context: &rho_core::session::ContextStats,
    ) {
        for obs in &self.observers {
            obs.on_usage(iteration, usage, context).await;
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

    #[tokio::test]
    async fn composite_delegates_state_change() {
        struct Recording(std::sync::Mutex<Vec<String>>);
        #[async_trait]
        impl AgentObserver for Recording {
            async fn on_state_change(&self, state: AgentState) {
                self.0.lock().unwrap().push(format!("{state:?}"));
            }
        }

        let r1 = Recording(std::sync::Mutex::new(vec![]));
        let r2 = Recording(std::sync::Mutex::new(vec![]));
        let composite = CompositeObserver::new(vec![&r1, &r2]);
        composite.on_state_change(AgentState::Thinking).await;

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
