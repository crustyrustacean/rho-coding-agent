# Introduction

**rho** is a Rust coding agent that runs against local LLMs on Windows.

It connects to OpenAI-compatible endpoints (LM Studio, Ollama) on `localhost`, gives the model access to tools for reading and writing files and executing PowerShell commands, and runs an autonomous agent loop that the user supervises through an approval gate.

Design priorities:

- **Local first** — your code stays on your machine by default
- **Safe by default** — destructive actions require your approval
- **Rust-native** — structured compiler diagnostics, not text scraping
- **Extensible** — custom tools via config, provider-agnostic core

This book documents the architecture, security model, configuration, and development of rho.
