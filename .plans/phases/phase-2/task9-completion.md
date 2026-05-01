# Task 9: `ChatRequest.tools` Serialization — Completion Report

**Date:** 2026-04-29 (retroactive)
**Status:** ✅ Complete — implemented during Phase 1a, verified during Phase 2

---

## What Was Done

Task 9 asked to "add the `ChatRequest.tools` serialization so tool definitions are sent to the model API." This was already implemented during Phase 1a as part of the core conversation/request infrastructure. Phase 2 verified that it works end-to-end through the agent loop.

### Existing Implementation

**`rho-core/src/request.rs`:**
- `ChatRequest` carries a `tools: Vec<ToolSchema>` field, serialized via `#[derive(Serialize)]`
- The `tools` field is included in every JSON request body sent to the model API

**`rho-core/src/schema.rs`:**
- `ToolSchema` and `ToolSchemaFunction` define the wire-format tool definitions
- `ToolSchema::new(name, description, parameters)` builds the standard OpenAI-compatible tool schema
- Both derive `Serialize` for JSON serialization

**`rho-core/src/conversation.rs`:**
- `Conversation` stores `tools: Vec<ToolSchema>` from construction
- `Conversation::send_current()` builds `ChatRequest { model, messages, tools: self.tools.clone() }` and sends it to the client

**`rho-core/src/tool.rs`:**
- `ToolRegistry::tool_schemas()` extracts `ToolSchema` values from all registered tools
- The binary passes `registry.tool_schemas()` to `Conversation::new()`

### End-to-End Flow

1. Binary registers tools via `register_all()` → `ToolRegistry`
2. Binary calls `registry.tool_schemas()` → `Vec<ToolSchema>`
3. Binary constructs `Conversation::new(model, prompt, tool_schemas)`
4. Agent loop calls `conversation.send_current(&client)` → `ChatRequest { tools }`
5. `LocalChatClient::chat(request)` serializes the full request including `tools` as JSON
6. Model receives tool definitions and can return `finish_reason: "tool_calls"`

---

## Verification

No new tests were needed — the existing integration tests for the agent loop (Tasks 1–8) all exercise tool-call flows that depend on `ChatRequest.tools` being serialized correctly. Specifically:

- `tool_call_response` and `multi_tool_call_response` test helpers produce model responses with `finish_reason: "tool_calls"`, which only works when the model received tool schemas
- All agent loop integration tests that involve tool calls implicitly verify that tool schemas are sent

---

## Design Decisions

1. **Tool schemas in the API `tools` field, not in the system prompt** — The OpenAI API provides a dedicated `tools` field in the chat completion request. Tool schemas are sent there, not embedded in the prompt text. This is the standard approach and allows the model to generate structured tool calls.

2. **`ToolSchema` is distinct from `Tool` trait** — The `Tool` trait defines the interface for *executing* tools. `ToolSchema` defines the *description* the model sees. This separation means the model never sees execution logic, only the interface contract.

3. **Tools are cloned per request** — `self.tools.clone()` on every `send_current()` call. This is cheap (tool schemas are small and few) and avoids borrow-checker issues with the conversation's mutable state.

---

## File Changes

No files were changed for this task. The implementation pre-existed from Phase 1a.
