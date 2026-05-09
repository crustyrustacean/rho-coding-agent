# Prompt Injection Defense

File contents are untrusted data. When the model reads a file, the contents could contain instructions designed to manipulate the model's behaviour. rho mitigates this with framing.

## The `<context>` / `<context:end>` convention

File contents returned by `ReadFile` are wrapped in framing tags:

```text
<context>
[actual file contents here]
<context:end>
```

The system prompt instructs the model that content between `<context>` and `<context:end>` is data to be processed, not instructions to be followed. This is a form of *role-based compartmentalisation* — the model is told to adopt a different role when reading framed content.

### Why `<context:end>` and not `</context>`?

The original closing tag `</context>` was ambiguous — it could appear in legitimate file contents (e.g., XML files, HTML comments). The `<context:end>` sentinel is a unique string that will not appear naturally in source code, making it a reliable boundary marker.

## Where framing is applied

| Tool | Framing |
|---|---|
| `ReadFile` | Output wrapped in `<context>` / `<context:end>` |
| `RunCommand` | Output wrapped in `<context>` / `<context:end>` |
| `CargoCheck`, `CargoClippy` | Structured diagnostic output (not framed — parsed, not raw) |
| `CargoTest` | Structured test output (not framed — parsed, not raw) |
| `RustcExplain` | Plain text (not framed — short, known source) |
| `WriteFile`, `EditFile` | Input is tool arguments, not file reads — not framed |

## System prompt primacy

The system prompt is loaded before any user interaction and is never evicted from the context window (it's pinned by the context manager). This means the framing instructions persist even if the conversation grows long.

## Limitations

- **Framing is advisory, not enforced.** The model *could* choose to ignore the `<context>` tags and follow injected instructions. The framing relies on the model's instruction-following behaviour, not a technical barrier.
- **No content scanning.** rho does not scan file contents for injection patterns. The framing is applied uniformly regardless of content.
- **Approval gate is the real defense.** If the model attempts to execute a destructive action based on injected instructions, the approval gate catches it. Framing is a defense-in-depth layer, not a substitute for human oversight.
