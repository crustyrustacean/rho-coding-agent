# How Rho Works: End-to-End Flow

`rho` operates as a continuous loop of **Reasoning $\rightarrow$ Action $\rightarrow$ Observation**. It acts as a bridge between a Large Language Model (LLM) and your local file system/shell.

## 1. Initialization & Configuration
When you start `rho`, it performs several setup steps:
*   **Environment Discovery:** It identifies the project root (searching for markers like `Cargo.toml`).
*   **Config Loading:** It merges user-level configuration (`~/.rho/config.toml`) with project-specific configuration (`.rho/config.toml`). This determines things like which model to use, how strictly to enforce tool approvals, and what commands are blacklisted in the shell.
*   **Tool Registry:** The `ToolRegistry` (in `rho-core`) is populated with available tools (from `rho-tools`), such as `ReadFile`, `WriteFile`, and `RunCommand`. Each tool is paired with a JSON schema so the LLM knows how to call it.

## 2. The Agent Loop (`run_loop`)
This is the "heartbeat" of the program, located in `rho-core/src/agent.rs`. The loop follows these steps:

### A. Context Preparation (The Prompt)
Before sending anything to the LLM, `rho` constructs a `ChatRequest`. This isn't just your last message; it includes:
*   **System Prompt:** Instructions on how to behave as an API-driven agent.
*   **Tool Schemas:** A definition of every tool available, telling the model "You can call `edit_file` if you need to change code."
*   **Context Management:** The `ContextManager` ensures the conversation history stays within the LLM's token limit by using a sliding window (evicting old turns) while keeping important parts like the system prompt pinned.

### B. Model Inference (`ChatClient`)
The request is sent via the `ChatClient` trait to an OpenAI-compatible API (like LM Studio or Ollama). The model processes the context and returns either:
1.  **Text Content:** A direct response to you (e.g., "I've finished updating the tests.").
2.  **Tool Calls:** A structured request to execute one or more tools (e.g., "Call `run_command` with `cargo test`").

### C. The Approval Gate & Execution
If the model requests a tool call, `rho` does not execute it immediately. Instead:
1.  **Risk Assessment:** It checks the `ToolRisk` level (`Read`, `Write`, or `Destructive`).
2.  **Approval Policy:** The `ApprovalPolicy` determines if this specific tool requires human intervention. If configured to `Ask`, the agent pauses and waits for you to confirm.
3.  **Execution:** Once approved, the `ToolRegistry` dispatches the call to the actual implementation (e.g., `rho-tools/src/shell.rs`).
4.  **Safety Layers:** 
    *   **Sandbox Check:** File tools verify paths are within the allowed directory (`SandboxRoot`).
    *   **Redaction:** The `Redactor` scans the output for potential secrets (like API keys) and replaces them with `[REDACTED]` before you or the model see them.

### D. Observation & Feedback
The result of the tool execution (stdout, stderr, or error messages) is wrapped in a `Tool` message type and appended to the conversation history. 

## 3. Iteration vs. Completion
*   **Iteration:** The loop repeats. The model now sees its previous action *and* the result of that action as part of its context. It "observes" that `cargo test` failed and decides it needs to call `edit_file` to fix the code.
*   **Completion:** The loop terminates only when the model provides a plain text response without any pending tool calls, or if a safety limit is reached (like `MaxIterationsExceeded`).

---
**Summary of Data Flow:**
`User Input` $\rightarrow$ `Conversation History` $\rightarrow$ `LLM` $\rightarrow$ `Tool Call Request` $\rightarrow$ `Approval Gate` $\rightarrow$ `Tool Execution (with Sandbox/Redaction)` $\rightarrow$ `Tool Result` $\rightarrow$ `Back to Conversation History`.
