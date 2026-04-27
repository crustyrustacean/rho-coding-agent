## [0.3.0] - 2026-04-26

### 🚀 Features

- Make model and system prompt configurable via CLI arguments (`--model`, `--system`)
- Add tool definition types (`Tool`, `ToolFunction`, `ToolParameters`, `ToolParameterProperty`)
- Add `AssistantResponse` enum to distinguish text replies from tool call requests
- Add tool definitions to `ChatRequest` and `Conversation`
- Handle `FinishReason::ToolCalls` in `Conversation::send`

### 📚 Documentation

- Add comprehensive doc comments to all public types, fields, and variants in `rho-core`
- Document behavioral difference between `AssistantResponse::Message` and `AssistantResponse::ToolCall`

### 🧹 Miscellaneous

- Resolve Clippy lints (`single_match_else`, `uninlined_format_args`)
- Bump version to 0.3.0

## [0.2.2] - 2026-04-26

### 💼 Other

- Program functions end to end, takes a chat message, send it to the model, returns the response and prints it to the console

### ⚙️ Miscellaneous Tasks

- Initial project scaffold
- Bump version to 0.1.1
- Resolve clippy lints from CI
