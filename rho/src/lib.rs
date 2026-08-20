//! The `rho` binary crate — startup and RPC protocol.
//!
//! This library crate exists to share module structure between `main.rs` and
//! integration tests. The binary entry point (`main.rs`) is thin: parse CLI,
//! call `App::build().run()`.

pub mod app;
pub mod cli;
pub mod ext_cli;
pub(crate) mod ext_observer;
pub(crate) mod presenter;
pub(crate) mod rpc;
pub use wire_conversions::{risk_label, state_name};
pub(crate) mod wire_conversions;
