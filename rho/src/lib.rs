//! The `rho` binary crate — startup, REPL, and approval gate.
//!
//! This library crate exists to share module structure between `main.rs` and
//! integration tests. The binary entry point (`main.rs`) is thin: parse CLI,
//! call `App::build().run()`.

pub mod app;
pub mod cli;
pub(crate) mod gate;
pub(crate) mod model;
pub(crate) mod presenter;
pub(crate) mod repl;
pub(crate) mod rpc;
