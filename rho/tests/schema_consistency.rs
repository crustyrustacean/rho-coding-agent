//! Dispatch ↔ schema consistency test.
//!
//! The `OpenRPC` schema at `docs/rpc-schema/openrpc.json` is *generated* from
//! the wire structs (`cargo xtask schema` → `rho-schema-gen`), but nothing
//! forces a regeneration after the code changes — exactly the failure mode
//! that produced the old hand-maintained schema drifting out of sync with
//! the dispatch layer. This test is that force.
//!
//! It compares the **committed** schema file against `rho_protocol::schema`'s
//! registry (the same source the generator reads). If they differ, the fix is
//! always the same one-liner: run `cargo xtask schema` and commit the result.
//!
//! A second check pins the registry itself to the RPC dispatch table's
//! method set in `rho/src/rpc.rs` — the dispatch strings are duplicated in
//! this test's expectation list, so adding a method without registering it
//! fails here rather than silently vanishing from the generated schema.

use std::collections::BTreeSet;

/// The methods the RPC dispatch layer in `rho/src/rpc.rs` actually handles.
///
/// `prompt`, `abort`, and `approvalResponse` are routed by the concurrent
/// reader (`demux_request`), never reaching `dispatch_request`; the rest are
/// dispatched directly. Both sets belong here.
const DISPATCHED_METHODS: &[&str] = &[
    "prompt",
    "abort",
    "approvalResponse",
    "clear",
    "newSession",
    "getState",
    "getMessages",
    "setModel",
    "listModels",
    "listProviders",
    "getSessionStats",
    "listSessions",
    "listExtensions",
    "reloadExtensions",
    "compact",
    "resumeSession",
    "listTools",
];

/// The notifications the RPC observer actually emits (see `rpc.rs` and the
/// wire types).
const EMITTED_NOTIFICATIONS: &[&str] = &[
    "ready",
    "agent/start",
    "agent/end",
    "agent/error",
    "state/change",
    "message/delta",
    "reasoning/delta",
    "tool/call",
    "tool/result",
    "tool/denied",
    "approval/request",
    "usage",
];

fn load_committed_spec() -> serde_json::Value {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/rpc-schema/openrpc.json");
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    serde_json::from_str(&content)
        .unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()))
}

fn names_of(spec: &serde_json::Value, key: &str) -> BTreeSet<String> {
    spec[key]
        .as_array()
        .expect("schema key present")
        .iter()
        .map(|m| m["name"].as_str().expect("name is a string").to_owned())
        .collect()
}

#[test]
fn registry_covers_every_dispatched_method() {
    let registry: BTreeSet<_> = rho_protocol::schema::method_names().into_iter().collect();
    let dispatched: BTreeSet<_> = DISPATCHED_METHODS.iter().copied().collect();

    let missing: Vec<_> = dispatched.difference(&registry).collect();
    assert!(
        missing.is_empty(),
        "dispatch table has methods the schema registry lacks (run `cargo xtask schema` \
         after registering them in rho-protocol/src/schema.rs): {missing:?}"
    );
    let extra: Vec<_> = registry.difference(&dispatched).collect();
    assert!(
        extra.is_empty(),
        "schema registry advertises methods the dispatch table doesn't handle: {extra:?}"
    );
}

#[test]
fn registry_covers_every_emitted_notification() {
    let registry: BTreeSet<_> = rho_protocol::schema::notification_names()
        .into_iter()
        .collect();
    let emitted: BTreeSet<_> = EMITTED_NOTIFICATIONS.iter().copied().collect();

    let missing: Vec<_> = emitted.difference(&registry).collect();
    assert!(
        missing.is_empty(),
        "observer emits notifications the schema registry lacks: {missing:?}"
    );
    let extra: Vec<_> = registry.difference(&emitted).collect();
    assert!(
        extra.is_empty(),
        "schema registry advertises notifications nothing emits: {extra:?}"
    );
}

#[test]
fn committed_schema_matches_registry() {
    let spec = load_committed_spec();
    let spec_methods = names_of(&spec, "methods");
    let registry_methods: BTreeSet<String> = rho_protocol::schema::method_names()
        .into_iter()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        spec_methods, registry_methods,
        "committed openrpc.json is stale — run `cargo xtask schema` and commit the result"
    );

    let spec_notifs = names_of(&spec, "notifications");
    let registry_notifs: BTreeSet<String> = rho_protocol::schema::notification_names()
        .into_iter()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        spec_notifs, registry_notifs,
        "committed openrpc.json is stale — run `cargo xtask schema` and commit the result"
    );
}

#[test]
fn committed_schema_refs_resolve() {
    let spec = load_committed_spec();
    let text = serde_json::to_string(&spec).expect("serialize spec");
    let defs: BTreeSet<&str> = spec["$defs"]
        .as_object()
        .map(|m| m.keys().map(String::as_str).collect())
        .unwrap_or_default();

    let re = regex::Regex::new(r##"\$ref": "#/\$defs/([A-Za-z]+)"##).expect("valid regex");
    for cap in re.captures_iter(&text) {
        let name = cap.get(1).expect("capture group").as_str();
        assert!(
            defs.contains(name),
            "schema $ref #{name} has no matching $defs entry — generation bug"
        );
    }
}
