//! Presentation layer implementations.
//!
//! Each mode (REPL, RPC, future TUI) provides its own presenter that
//! handles all formatted output. Business logic calls presenter methods
//! instead of raw `println!`/`eprintln!`.

pub(crate) mod repl;

pub(crate) use repl::ReplPresenter;
