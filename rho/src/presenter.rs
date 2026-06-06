//! Presentation layer.
//!
//! All formatted output goes through [`RpcPresenter`], which writes
//! diagnostics to stderr (separate from the JSONL stdout channel).

pub(crate) mod rpc;

pub(crate) use rpc::RpcPresenter;
