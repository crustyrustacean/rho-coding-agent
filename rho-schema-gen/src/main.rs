//! Generator binary for `docs/rpc-schema/openrpc.json`.
//!
//! Invoked by `cargo xtask schema` (and safe to run directly). Emits the
//! `OpenRPC` document from [`rho_protocol::schema::openrpc`] — the same wire
//! structs the dispatch layer deserializes — and pretty-prints it to the
//! schema path. The version field is injected by xtask afterwards.

use std::path::Path;

fn main() {
    let spec = rho_protocol::schema::openrpc();
    let out = serde_json::to_string_pretty(&spec).expect("serialize openrpc spec");
    let path = Path::new("docs/rpc-schema/openrpc.json");
    std::fs::create_dir_all(path.parent().expect("parent dir")).expect("create docs/rpc-schema");
    std::fs::write(path, out.as_bytes()).expect("write openrpc.json");

    let methods = spec["methods"].as_array().map_or(0, Vec::len);
    let notifications = spec["notifications"].as_array().map_or(0, Vec::len);
    println!(
        "schema: {methods} methods, {notifications} notifications -> {}",
        path.display()
    );
}
