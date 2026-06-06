//! The `rho` binary crate — startup and RPC protocol.
//!
//! This library crate exists to share module structure between `main.rs` and
//! integration tests. The binary entry point (`main.rs`) is thin: parse CLI,
//! call `App::build().run()`.

pub mod app;
pub mod cli;
pub(crate) mod ext_observer;
pub(crate) mod model;
pub(crate) mod presenter;
pub(crate) mod rpc;
