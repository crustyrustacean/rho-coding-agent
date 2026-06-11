//! rho-highlight — tree-sitter parsing, syntax classification, and structural
//! code queries for the rho coding agent.
//!
//! Provides two capabilities:
//!
//! 1. **Structural understanding** — tools use tree-sitter to reason about code
//!    (identify node types at a position, validate edit safety). [`node_at`] is
//!    the primary entry point.
//!
//! 2. **Syntax classification** — produces [`HighlightSpan`] values that a
//!    renderer maps to ANSI colours or `ratatui` styles. Theme/colour mapping is
//!    out of scope; this crate only classifies spans.
//!
//! # Feature flags
//!
//! | Feature | Grammar | Default |
//! |---|---|---|
//! | `rust` | [`tree-sitter-rust`](https://crates.io/crates/tree-sitter-rust) | ✅ yes |
//! | `powershell` | *planned* | — |
//! | `toml` | *planned* | — |
//! | `json` | *planned* | — |
//! | `markdown` | *planned* | — |
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
//! | [`parse()`] | [`mod@parse`] — produce a tree-sitter `Tree` from source |
//! | [`highlight()`] | [`mod@highlight`] — produce classified [`HighlightSpan`]s |
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
