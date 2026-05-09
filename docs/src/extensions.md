# Extensions

> Extensions are planned for Phase 5. This page describes the architecture and the foundation that's already in place.

## Extension entries

rho's session tree supports typed extension entries that are versioned and schema-skew-safe. This is the mechanism that future extensions will build on.

```rust
// Write a typed state entry (never sent to the model)
session.write_custom_state("rho.diagnostics.v1", &json!({ "errors": 3 }))?;

// Write a custom message entry (sent to the model as a synthetic user message)
session.write_custom_message("rho.diagnostics.v1", vec![
    ContentBlock::Text { text: "3 compilation errors found".into() },
])?;

// Read back a custom state entry
if let Some(state) = session.read_custom_state("rho.diagnostics.v1")? {
    println!("errors: {}", state["errors"]);
}
```

## Kind versioning

Extension entry kinds use the format `<author>.<feature>.v<n>`. When rho encounters an entry with an unknown kind version, it returns `None` (graceful degradation) rather than panicking. This allows extensions to evolve their schema without breaking older rho versions.

## Current uses

Extension entries are currently used by:

- **Diagnostic extensions** — `ToolResultDetails::Diagnostics` carries structured compiler output
- **Custom messages** — the system can inject synthetic messages into the conversation
- **Custom state** — tools and extensions can persist opaque state in the session tree

## Planned (Phase 5)

- **Custom tools via TOML** — define tools declaratively in `.rho/tools.toml`
- **Custom slash commands** — user-defined REPL commands
- **Extension API** — a crate (`rho-ext`) providing a stable interface for third-party extensions
