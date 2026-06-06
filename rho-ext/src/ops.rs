//! Helper macros for rho host op boilerplate reduction.
//!
//! Phase 3 of the rho-ext improvement plan: reduce the "five-file ceremony"
//! of adding a new op to a single declaration where possible.
//!
//! # Provided macros
//!
//! - `err!` — return an `__ERROR__`-prefixed string
//! - `json!` — serialize to JSON string (errors on failure)
//! - `require_perm!` — permission guard using a `HostState` field
//! - `require_field!` — extract a required field from JSON input
//! - `js_fn!` — generate JS wrapper function strings (fixed arity)
//! - `js_json_fn!` — generate JS wrapper that marshals JS objects to JSON strings

/// Return an error string with the `__ERROR__` prefix.
///
/// This is the standard error convention for rho ops — the JS shim's
/// `unwrapResult()` checks for this prefix and throws a JS Error.
#[macro_export]
macro_rules! err {
    ($($arg:tt)*) => {
        format!("__ERROR__{}", format_args!($($arg)*))
    };
}

/// Serialize a value as a JSON string.
///
/// Returns an `__ERROR__` string if serialization fails.
#[macro_export]
macro_rules! json {
    ($($arg:tt)*) => {
        serde_json::to_string(&serde_json::json!($($arg)*))
            .unwrap_or_else(|e| err!("serialization failed: {e}"))
    };
}

/// Check a permission on `HostState` and return early with an error
/// if the permission is not granted.
///
/// # Examples
///
/// ```ignore
/// fn op_rho_fetch_url(state: &mut OpState, #[string] opts: &str) -> String {
///     require_perm!(state, allow_network, "network");
///     // ...
/// }
/// ```
#[macro_export]
macro_rules! require_perm {
    ($state:expr, $field:ident, $perm_name:expr) => {
        if !$state
            .try_borrow::<$crate::host::HostState>()
            .is_some_and(|h| h.$field)
        {
            return $crate::err!(
                "rho op: extension does not have {} permission \
                 (enable with `{} = true` in config)",
                $perm_name,
                stringify!($field)
            );
        }
    };
}

/// Extract a required string field from a JSON object.
///
/// Returns an `__ERROR__` string if the field is missing or not a string.
///
/// # Examples
///
/// ```ignore
/// let url = require_field!(opts_json, "url");
/// ```
#[macro_export]
macro_rules! require_field {
    ($json_str:expr, $field:literal) => {
        match serde_json::from_str::<serde_json::Value>($json_str)
            .ok()
            .and_then(|v| v[$field].as_str().map(String::from))
        {
            Some(val) => val,
            None => return err!("missing required field '{}'", $field),
        }
    };
}

/// Generate a JS wrapper function string for the `rho` global object.
///
/// Fixed-arity variants handle the most common patterns:
///
/// - `js_fn!(name, op, void)` — call op, no return
/// - `js_fn!(name, op, void, p1, p2)` — call op(p1, p2), no return
/// - `js_fn!(name, op, unwrap)` — call op, return unwrapped result
/// - `js_fn!(name, op, unwrap, p1)` — call op(p1), return unwrapped result
/// - `js_fn!(name, op, json)` — call op, return JSON.parse(unwrapped)
/// - `js_fn!(name, op, json, p1)` — call op(p1), return JSON.parse(unwrapped)
/// - `js_fn!(name, op, void_unwrap)` — call op, unwrap (throws on error), no return
/// - `js_fn!(name, op, void_unwrap, p1, p2)` — call op(p1, p2), unwrap, no return
#[macro_export]
macro_rules! js_fn {
    // ── 0-param variants ─────────────────────────────────────────────────
    (   $name:literal, $op:literal, void ) => {
        concat!("  ", $name, "() {\n", "    ops.", $op, "();\n", "  },\n")
    };
    (   $name:literal, $op:literal, unwrap ) => {
        concat!(
            "  ",
            $name,
            "() {\n",
            "    return unwrapResult(ops.",
            $op,
            "());\n",
            "  },\n"
        )
    };
    (   $name:literal, $op:literal, json ) => {
        concat!(
            "  ",
            $name,
            "() {\n",
            "    return JSON.parse(unwrapResult(ops.",
            $op,
            "()));\n",
            "  },\n"
        )
    };
    (   $name:literal, $op:literal, void_unwrap ) => {
        concat!(
            "  ",
            $name,
            "() {\n",
            "    unwrapResult(ops.",
            $op,
            "());\n",
            "  },\n"
        )
    };

    // ── 1-param variants ──────────────────────────────────────────────────
    (   $name:literal, $op:literal, void,       $p1:literal ) => {
        concat!(
            "  ", $name, "(", $p1, ") {\n", "    ops.", $op, "(", $p1, ");\n", "  },\n"
        )
    };
    (   $name:literal, $op:literal, unwrap,     $p1:literal ) => {
        concat!(
            "  ",
            $name,
            "(",
            $p1,
            ") {\n",
            "    return unwrapResult(ops.",
            $op,
            "(",
            $p1,
            "));\n",
            "  },\n"
        )
    };
    (   $name:literal, $op:literal, json,       $p1:literal ) => {
        concat!(
            "  ",
            $name,
            "(",
            $p1,
            ") {\n",
            "    return JSON.parse(unwrapResult(ops.",
            $op,
            "(",
            $p1,
            ")));\n",
            "  },\n"
        )
    };
    (   $name:literal, $op:literal, void_unwrap, $p1:literal ) => {
        concat!(
            "  ",
            $name,
            "(",
            $p1,
            ") {\n",
            "    unwrapResult(ops.",
            $op,
            "(",
            $p1,
            "));\n",
            "  },\n"
        )
    };

    // ── 2-param variants ──────────────────────────────────────────────────
    (   $name:literal, $op:literal, void,       $p1:literal, $p2:literal ) => {
        concat!(
            "  ", $name, "(", $p1, ", ", $p2, ") {\n", "    ops.", $op, "(", $p1, ", ", $p2,
            ");\n", "  },\n"
        )
    };
    (   $name:literal, $op:literal, unwrap,     $p1:literal, $p2:literal ) => {
        concat!(
            "  ",
            $name,
            "(",
            $p1,
            ", ",
            $p2,
            ") {\n",
            "    return unwrapResult(ops.",
            $op,
            "(",
            $p1,
            ", ",
            $p2,
            "));\n",
            "  },\n"
        )
    };
    (   $name:literal, $op:literal, json,       $p1:literal, $p2:literal ) => {
        concat!(
            "  ",
            $name,
            "(",
            $p1,
            ", ",
            $p2,
            ") {\n",
            "    return JSON.parse(unwrapResult(ops.",
            $op,
            "(",
            $p1,
            ", ",
            $p2,
            ")));\n",
            "  },\n"
        )
    };
    (   $name:literal, $op:literal, void_unwrap, $p1:literal, $p2:literal ) => {
        concat!(
            "  ",
            $name,
            "(",
            $p1,
            ", ",
            $p2,
            ") {\n",
            "    unwrapResult(ops.",
            $op,
            "(",
            $p1,
            ", ",
            $p2,
            "));\n",
            "  },\n"
        )
    };

    // ── 3-param variants ──────────────────────────────────────────────────
    (   $name:literal, $op:literal, void,       $p1:literal, $p2:literal, $p3:literal ) => {
        concat!(
            "  ", $name, "(", $p1, ", ", $p2, ", ", $p3, ") {\n", "    ops.", $op, "(", $p1, ", ",
            $p2, ", ", $p3, ");\n", "  },\n"
        )
    };
    (   $name:literal, $op:literal, unwrap,     $p1:literal, $p2:literal, $p3:literal ) => {
        concat!(
            "  ",
            $name,
            "(",
            $p1,
            ", ",
            $p2,
            ", ",
            $p3,
            ") {\n",
            "    return unwrapResult(ops.",
            $op,
            "(",
            $p1,
            ", ",
            $p2,
            ", ",
            $p3,
            "));\n",
            "  },\n"
        )
    };
    (   $name:literal, $op:literal, json,       $p1:literal, $p2:literal, $p3:literal ) => {
        concat!(
            "  ",
            $name,
            "(",
            $p1,
            ", ",
            $p2,
            ", ",
            $p3,
            ") {\n",
            "    return JSON.parse(unwrapResult(ops.",
            $op,
            "(",
            $p1,
            ", ",
            $p2,
            ", ",
            $p3,
            ")));\n",
            "  },\n"
        )
    };
}

/// Generate a JS wrapper that takes a JS object, converts it to a JSON
/// string, calls the op, and returns the parsed result.
///
/// This is the standard pattern for ops like `fetchUrl` and `runCommand`
/// that accept structured options.
#[macro_export]
macro_rules! js_json_fn {
    // With JSON.parse on return
    (   $name:literal, $op:literal ) => {
        concat!(
            "  ",
            $name,
            "(opts) {\n",
            "    const optsJson = typeof opts === \"string\" ? opts : JSON.stringify(opts);\n",
            "    const result = unwrapResult(ops.",
            $op,
            "(optsJson));\n",
            "    return JSON.parse(result);\n",
            "  },\n"
        )
    };
    // With unwrap only (no JSON.parse)
    (   $name:literal, $op:literal, unwrap ) => {
        concat!(
            "  ",
            $name,
            "(opts) {\n",
            "    const optsJson = typeof opts === \"string\" ? opts : JSON.stringify(opts);\n",
            "    return unwrapResult(ops.",
            $op,
            "(optsJson));\n",
            "  },\n"
        )
    };
    // With void_unwrap (throw on error, no return)
    (   $name:literal, $op:literal, void_unwrap ) => {
        concat!(
            "  ",
            $name,
            "(opts) {\n",
            "    const optsJson = typeof opts === \"string\" ? opts : JSON.stringify(opts);\n",
            "    unwrapResult(ops.",
            $op,
            "(optsJson));\n",
            "  },\n"
        )
    };
}

#[cfg(test)]
mod tests {
    #[test]
    fn err_macro_basic() {
        let s = err!("something went wrong");
        assert_eq!(s, "__ERROR__something went wrong");
    }

    #[test]
    fn err_macro_formatted() {
        let s = err!("code {}: {}", 404, "not found");
        assert_eq!(s, "__ERROR__code 404: not found");
    }

    #[test]
    fn json_macro_basic() {
        let s = json!({ "key": "value" });
        let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed["key"], "value");
    }

    #[test]
    fn json_macro_nested() {
        let s = json!({ "a": { "b": 1 }, "c": [1, 2, 3] });
        let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed["a"]["b"], 1);
        assert_eq!(parsed["c"][2], 3);
    }

    #[test]
    fn require_field_found() {
        fn helper(json: &str) -> String {
            require_field!(json, "url")
        }
        let val = helper(r#"{"url": "https://example.com"}"#);
        assert_eq!(val, "https://example.com");
    }

    #[test]
    fn require_field_missing() {
        fn helper(json: &str) -> String {
            require_field!(json, "url")
        }
        let result = helper(r#"{"other": "value"}"#);
        assert!(result.starts_with("__ERROR__missing required field 'url'"));
    }

    // ── js_fn! tests ──

    #[test]
    fn js_fn_void_0param() {
        let js = js_fn!("log", "op_rho_log", void);
        assert_eq!(js, "  log() {\n    ops.op_rho_log();\n  },\n");
    }

    #[test]
    fn js_fn_void_2param() {
        let js = js_fn!("log", "op_rho_log", void, "level", "message");
        assert_eq!(
            js,
            "  log(level, message) {\n    ops.op_rho_log(level, message);\n  },\n"
        );
    }

    #[test]
    fn js_fn_unwrap_0param() {
        let js = js_fn!("getCwd", "op_rho_get_cwd", unwrap);
        assert_eq!(
            js,
            "  getCwd() {\n    return unwrapResult(ops.op_rho_get_cwd());\n  },\n"
        );
    }

    #[test]
    fn js_fn_unwrap_1param() {
        let js = js_fn!("readFile", "op_rho_read_file", unwrap, "path");
        assert_eq!(
            js,
            "  readFile(path) {\n    return unwrapResult(ops.op_rho_read_file(path));\n  },\n"
        );
    }

    #[test]
    fn js_fn_json_0param() {
        let js = js_fn!("getModel", "op_rho_get_model", json);
        assert_eq!(
            js,
            "  getModel() {\n    return JSON.parse(unwrapResult(ops.op_rho_get_model()));\n  },\n"
        );
    }

    #[test]
    fn js_fn_void_unwrap_1param() {
        let js = js_fn!(
            "writeFile",
            "op_rho_write_file",
            void_unwrap,
            "path",
            "content"
        );
        assert_eq!(
            js,
            "  writeFile(path, content) {\n    unwrapResult(ops.op_rho_write_file(path, content));\n  },\n"
        );
    }

    #[test]
    fn js_fn_3param() {
        let js = js_fn!(
            "setModel",
            "op_rho_set_model",
            void,
            "name",
            "version",
            "extra"
        );
        assert_eq!(
            js,
            "  setModel(name, version, extra) {\n    ops.op_rho_set_model(name, version, extra);\n  },\n"
        );
    }

    // ── js_json_fn! tests ──

    #[test]
    fn js_json_fn_basic() {
        let js = js_json_fn!("fetchUrl", "op_rho_fetch_url");
        assert!(js.contains("fetchUrl(opts)"));
        assert!(js.contains("typeof opts === \"string\""));
        assert!(js.contains("JSON.parse(result)"));
    }

    #[test]
    fn js_json_fn_unwrap() {
        let js = js_json_fn!("writeConfig", "op_rho_write_config", unwrap);
        assert!(js.contains("return unwrapResult"));
        assert!(!js.contains("JSON.parse"));
    }
}
