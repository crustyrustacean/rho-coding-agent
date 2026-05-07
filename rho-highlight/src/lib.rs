//! rho-highlight — tree-sitter parsing, syntax classification, and structural
//! code queries for the rho coding agent.
//!
//! # Purpose
//!
//! This crate sits between `rho-core` and the tool/TUI layers, providing two
//! roles:
//!
//! 1. **Structural understanding** — tools use tree-sitter to reason about code
//!    (find function boundaries, identify node types at a position, validate
//!    edit safety). [`node_at`] is the primary entry point for Phase 3.
//!
//! 2. **Rendering** (Phase 4) — the TUI uses tree-sitter to syntax-highlight code
//!    blocks by producing [`HighlightSpan`] values that a renderer maps to ANSI
//!    colours or `ratatui` styles. Theme/colour mapping is out of scope for Phase 3;
//!    this crate only classifies spans.
//!
//! # Feature flags
//!
//! | Feature | Grammar | Default |
//! |---|---|---|
//! | `rust` | [`tree-sitter-rust`](https://crates.io/crates/tree-sitter-rust) | ✅ yes |
//! | `powershell` | *Phase 4 evaluation* | — |
//! | `toml` | *Phase 4 evaluation* | — |
//! | `json` | *Phase 4 evaluation* | — |
//! | `markdown` | *Phase 4 evaluation* | — |
//!
//! # Build requirement
//!
//! Tree-sitter grammars compile embedded C sources at build time via the `cc`
//! crate. A C compiler must be available:
//!
//! - **Windows** — Visual Studio Build Tools (MSVC) or LLVM/clang
//! - **macOS** — Xcode Command Line Tools (`xcode-select --install`)
//! - **Linux** — `gcc` or `clang` (`apt install build-essential` / equivalent)
//!
//! # Module layout
//!
//! | Module | Contents |
//! |---|---|
//! | [`parse`] | [`parse`] — produce a tree-sitter `Tree` from source |
//! | [`highlight`] | [`highlight`] — produce classified [`HighlightSpan`]s |
//! | [`query`] | [`node_at`] — structural node queries |
//! | [`lang`] | [`Language`] enum — grammar selection |
//! | [`error`] | [`HighlightError`] |

pub mod error;
pub mod highlight;
pub mod lang;
pub mod parse;
pub mod query;

pub use error::HighlightError;
pub use highlight::{HighlightSpan, HighlightTag, highlight};
pub use lang::Language;
pub use parse::parse;
pub use query::{NodeInfo, node_at};
