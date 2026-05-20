//! Built-in eval task definitions.
//!
//! Each task represents a canonical coding scenario with a known correct
//! outcome. Run `rho-bench --tasks all` to execute the full suite.

use crate::task::{EvalTask, TaskOutcome, TaskVerdict};

/// Returns all built-in eval tasks.
pub fn all_tasks() -> Vec<Box<dyn EvalTask>> {
    vec![
        Box::new(Scenario01FixTypeMismatch),
        Box::new(Scenario02UnusedImports),
        Box::new(Scenario03ExplainAndFix),
        Box::new(Scenario04FixAndTest),
        Box::new(Scenario05MultiErrorFix),
        Box::new(Scenario06RustdocApiLookup),
        Box::new(Scenario07CrateLookup),
        Box::new(Scenario08FixMissingLifetime),
        Box::new(Scenario09FixBorrowConflict),
        Box::new(Scenario10FixTraitBound),
    ]
}

// ── Scenario 01: Fix E0308 type mismatch ────────────────────────────────────

/// A function returns an integer but is declared to return `String`.
struct Scenario01FixTypeMismatch;

impl EvalTask for Scenario01FixTypeMismatch {
    fn id(&self) -> &str {
        "scenario_01_fix_type_mismatch"
    }

    fn name(&self) -> &str {
        "Fix type mismatch (E0308)"
    }

    fn description(&self) -> &str {
        "Fix E0308: greet() returns an integer but is declared to return String."
    }

    fn initial_files(&self) -> Vec<(&str, &str)> {
        vec![(
            "src/lib.rs",
            "/// Returns a greeting message.\n\
             pub fn greet(name: &str) -> String {\n\
             \x20   42\n\
             }\n\
             \n\
             /// Formats a farewell message.\n\
             pub fn farewell(name: &str) -> String {\n\
             \x20   format!(\"Goodbye, {name}!\")\n\
             }\n",
        )]
    }

    fn user_prompt(&self) -> &str {
        "The file src/lib.rs has a compilation error. Use the cargo_check tool \
         to see the error, then use edit_file to fix it. After fixing, run \
         cargo_check again to confirm the fix compiles cleanly."
    }

    fn verify(&self, files: &[(&str, &str)]) -> TaskOutcome {
        let Some((_, lib)) = files.iter().find(|(p, _)| *p == "src/lib.rs") else {
            return TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Error,
                "src/lib.rs not found in output",
            );
        };

        // The bare integer 42 must be replaced with a String expression.
        let has_bare_42 = lib.contains("    42\n") || lib.contains("\t42\n");
        let has_string_conversion =
            lib.contains("to_string()") || lib.contains("String::from") || lib.contains("format!(");

        if has_string_conversion && !has_bare_42 {
            TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Pass,
                "function now returns a String",
            )
        } else if has_bare_42 {
            TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Fail,
                "greet() still returns a bare integer",
            )
        } else {
            TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Fail,
                "expected String return expression not found",
            )
        }
    }
}

// ── Scenario 02: Remove unused imports ─────────────────────────────────────

/// Multiple unused imports that should be removed via clippy/fix.
struct Scenario02UnusedImports;

impl EvalTask for Scenario02UnusedImports {
    fn id(&self) -> &str {
        "scenario_02_unused_imports"
    }

    fn name(&self) -> &str {
        "Remove unused imports"
    }

    fn description(&self) -> &str {
        "Fix unused imports detected by clippy, using cargo_fix."
    }

    fn initial_files(&self) -> Vec<(&str, &str)> {
        vec![(
            "src/lib.rs",
            "use std::collections::HashMap;\n\
             use std::io;\n\
             use std::fmt;\n\
             use std::path::PathBuf;\n\
             \n\
             /// Adds two numbers together.\n\
             pub fn add(a: i32, b: i32) -> i32 {\n\
             \x20   a + b\n\
             }\n\
             \n\
             /// Doubles a number.\n\
             pub fn double(x: i32) -> i32 {\n\
             \x20   x * 2\n\
             }\n",
        )]
    }

    fn user_prompt(&self) -> &str {
        "The file src/lib.rs has unused imports. Use the cargo_clippy tool to \
         check for lint warnings, then use cargo_fix with the clippy option to \
         automatically remove them. After fixing, run cargo_clippy again to \
         confirm all warnings are resolved."
    }

    fn verify(&self, files: &[(&str, &str)]) -> TaskOutcome {
        let Some((_, lib)) = files.iter().find(|(p, _)| *p == "src/lib.rs") else {
            return TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Error,
                "src/lib.rs not found in output",
            );
        };

        let unused = [
            "use std::collections::HashMap",
            "use std::io",
            "use std::fmt",
            "use std::path::PathBuf",
        ];

        let mut still_present = Vec::new();
        for imp in &unused {
            if lib.contains(imp) {
                still_present.push(*imp);
            }
        }

        // The actual functions should still be there.
        let has_add = lib.contains("fn add");
        let has_double = lib.contains("fn double");

        if !still_present.is_empty() && (has_add || has_double) {
            TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Fail,
                format!("unused imports still present: {}", still_present.join(", ")),
            )
        } else if !has_add || !has_double {
            TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Fail,
                "required functions were accidentally removed",
            )
        } else {
            TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Pass,
                "unused imports removed, functions preserved",
            )
        }
    }
}

// ── Scenario 03: Explain error code and fix ────────────────────────────────

/// A struct missing `#[derive(Debug)]` — the prompt asks the model to use
/// `rustc_explain` before fixing.
struct Scenario03ExplainAndFix;

impl EvalTask for Scenario03ExplainAndFix {
    fn id(&self) -> &str {
        "scenario_03_explain_and_fix"
    }

    fn name(&self) -> &str {
        "Explain error code and fix"
    }

    fn description(&self) -> &str {
        "Fix E0277: Config struct missing #[derive(Debug)] needed by println!({:?})."
    }

    fn initial_files(&self) -> Vec<(&str, &str)> {
        vec![(
            "src/lib.rs",
            "pub struct Config {\n\
             \x20   pub name: String,\n\
             \x20   pub verbose: bool,\n\
             }\n\
             \n\
             /// Print the config for debugging.\n\
             pub fn debug_config(cfg: &Config) {\n\
             \x20   println!(\"{:?}\", cfg);\n\
             }\n",
        )]
    }

    fn user_prompt(&self) -> &str {
        "The file src/lib.rs has a compilation error. Use the cargo_check tool \
         to see the error. If you get an error code, use the rustc_explain tool \
         to understand what it means. Then use edit_file to fix the issue and \
         verify with cargo_check that it compiles."
    }

    fn verify(&self, files: &[(&str, &str)]) -> TaskOutcome {
        let Some((_, lib)) = files.iter().find(|(p, _)| *p == "src/lib.rs") else {
            return TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Error,
                "src/lib.rs not found in output",
            );
        };

        if lib.contains("derive") && lib.contains("Debug") {
            TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Pass,
                "Config derives Debug",
            )
        } else {
            TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Fail,
                "derive(Debug) not found on Config",
            )
        }
    }
}

// ── Scenario 04: Fix and verify with tests ─────────────────────────────────

/// A variable needs `mut` and existing tests must pass.
struct Scenario04FixAndTest;

impl EvalTask for Scenario04FixAndTest {
    fn id(&self) -> &str {
        "scenario_04_fix_and_test"
    }

    fn name(&self) -> &str {
        "Fix and verify with tests"
    }

    fn description(&self) -> &str {
        "Fix E0382: variable needs `mut`, then verify all tests pass."
    }

    fn initial_files(&self) -> Vec<(&str, &str)> {
        vec![(
            "src/lib.rs",
            "/// Collects even numbers from a range into a vector.\n\
             pub fn collect_evens(max: i32) -> Vec<i32> {\n\
             \x20   let result = Vec::new();\n\
             \x20   for i in 0..max {\n\
             \x20       if i % 2 == 0 {\n\
             \x20           result.push(i);\n\
             \x20       }\n\
             \x20   }\n\
             \x20   result\n\
             }\n\
             \n\
             #[cfg(test)]\n\
             mod tests {\n\
             \x20   use super::*;\n\
             \n\
             \x20   #[test]\n\
             \x20   fn collects_evens_up_to_10() {\n\
             \x20       assert_eq!(collect_evens(10), vec![0, 2, 4, 6, 8]);\n\
             \x20   }\n\
             \n\
             \x20   #[test]\n\
             \x20   fn collects_evens_up_to_1() {\n\
             \x20       assert_eq!(collect_evens(1), vec![0]);\n\
             \x20   }\n\
             \n\
             \x20   #[test]\n\
             \x20   fn collects_evens_up_to_0() {\n\
             \x20       assert_eq!(collect_evens(0), vec![]);\n\
             \x20   }\n\
             }\n",
        )]
    }

    fn user_prompt(&self) -> &str {
        "The file src/lib.rs has a compilation error. Use the cargo_check tool \
         to see the error, then use edit_file to fix it. After fixing, use \
         cargo_test to make sure all tests pass."
    }

    fn verify(&self, files: &[(&str, &str)]) -> TaskOutcome {
        let Some((_, lib)) = files.iter().find(|(p, _)| *p == "src/lib.rs") else {
            return TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Error,
                "src/lib.rs not found in output",
            );
        };

        // The fix should declare result as mutable.
        if !lib.contains("let mut result") {
            return TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Fail,
                "'result' is not declared as mutable",
            );
        }

        // Tests should still be present.
        if !lib.contains("#[test]") || !lib.contains("collects_evens_up_to_10") {
            return TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Fail,
                "tests were accidentally removed",
            );
        }

        TaskOutcome::new(
            self.id(),
            self.name(),
            TaskVerdict::Pass,
            "result declared mutable, tests preserved",
        )
    }
}

// ── Scenario 05: Multi-error fix cycle ─────────────────────────────────────

/// Three different compilation errors requiring an iterative fix cycle.
struct Scenario05MultiErrorFix;

impl EvalTask for Scenario05MultiErrorFix {
    fn id(&self) -> &str {
        "scenario_05_multi_error_fix"
    }

    fn name(&self) -> &str {
        "Multi-error fix cycle"
    }

    fn description(&self) -> &str {
        "Fix three errors: &mut self on insert, Option return on get, clippy len/is_empty."
    }

    fn initial_files(&self) -> Vec<(&str, &str)> {
        vec![(
            "src/lib.rs",
            "use std::collections::HashMap;\n\
             \n\
             /// A simple key-value store.\n\
             pub struct Store {\n\
             \x20   data: HashMap<String, String>,\n\
             }\n\
             \n\
             impl Store {\n\
             \x20   /// Create a new empty store.\n\
             \x20   pub fn new() -> Store {\n\
             \x20       Store {\n\
             \x20           data: HashMap::new(),\n\
             \x20       }\n\
             \x20   }\n\
             \n\
             \x20   /// Insert a key-value pair.\n\
             \x20   pub fn insert(&self, key: String, value: String) {\n\
             \x20       self.data.insert(key, value);\n\
             \x20   }\n\
             \n\
             \x20   /// Get a value by key.\n\
             \x20   pub fn get(&self, key: &str) -> String {\n\
             \x20       self.data.get(key)\n\
             \x20   }\n\
             \n\
             \x20   /// Count the number of entries.\n\
             \x20   pub fn len(&self) -> usize {\n\
             \x20       self.data.len()\n\
             \x20   }\n\
             }\n",
        )]
    }

    fn user_prompt(&self) -> &str {
        "The file src/lib.rs has multiple compilation errors. Use cargo_check to \
         see all errors, then read_file to see the full source. Apply ALL fixes \
         in a single edit_file call with multiple edits — do not fix them one \
         at a time. Then run cargo_check and cargo_clippy to verify everything \
         is clean. Fix any remaining clippy warnings too."
    }

    fn verify(&self, files: &[(&str, &str)]) -> TaskOutcome {
        let Some((_, lib)) = files.iter().find(|(p, _)| *p == "src/lib.rs") else {
            return TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Error,
                "src/lib.rs not found in output",
            );
        };

        let mut issues = Vec::new();

        // Error 1: insert should take &mut self.
        if !lib.contains("fn insert(&mut self") {
            issues.push("insert() does not take &mut self");
        }

        // Error 2: get() return type must handle Option.
        // The original had `-> String` with body `self.data.get(key)` which
        // returns Option<&String>. A valid fix changes the return type to
        // Option<...> or adds `.unwrap()`, `.unwrap_or_default()`, etc.
        let get_line = lib.lines().find(|l| l.contains("fn get("));
        if let Some(line) = get_line {
            // Reject the original broken signature: `-> String` without unwrap/expect.
            if line.contains("-> String") && !line.contains("unwrap") && !line.contains("expect") {
                let body_has_unwrap = lib.contains(".unwrap()") || lib.contains(".expect(");
                if !body_has_unwrap {
                    issues.push("get() still returns String without Option handling");
                }
            }
        } else {
            issues.push("get() method was removed");
        }

        // The Store struct and its methods should still exist.
        if !lib.contains("struct Store") {
            issues.push("Store struct was removed");
        }

        if issues.is_empty() {
            TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Pass,
                "all errors fixed",
            )
        } else {
            TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Fail,
                format!("issues: {}", issues.join("; ")),
            )
        }
    }
}

// ── Scenario 06: Fix stdlib API misuse with rustdoc ───────────────────────

/// The code misuses `HashMap::get`, ignoring its `Option` return type.
/// The model should use `rustdoc_lookup` to check the API, then fix it.
struct Scenario06RustdocApiLookup;

impl EvalTask for Scenario06RustdocApiLookup {
    fn id(&self) -> &str {
        "scenario_06_rustdoc_api_lookup"
    }

    fn name(&self) -> &str {
        "Fix stdlib API misuse with rustdoc"
    }

    fn description(&self) -> &str {
        "Fix HashMap::get return-type confusion using rustdoc_lookup."
    }

    fn initial_files(&self) -> Vec<(&str, &str)> {
        vec![(
            "src/lib.rs",
            "use std::collections::HashMap;\n\
             \n\
             /// Merge `other` into `base`, summing values for duplicate keys.\n\
             pub fn merge_sum(base: &mut HashMap<String, i64>, other: &HashMap<String, i64>) {\n\
             \x20   for (key, val) in other {\n\
             \x20       let existing = base.get(key);\n\
             \x20       base.insert(key.to_string(), existing + val);\n\
             \x20   }\n\
             }\n",
        )]
    }

    fn user_prompt(&self) -> &str {
        "The file src/lib.rs has a compilation error related to HashMap's API. \
         Use cargo_check to see the error, then use rustdoc_lookup to look up \
         HashMap::get and HashMap::entry so you understand the correct return \
         types. Fix the code and verify with cargo_check."
    }

    fn verify(&self, files: &[(&str, &str)]) -> TaskOutcome {
        let Some((_, lib)) = files.iter().find(|(p, _)| *p == "src/lib.rs") else {
            return TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Error,
                "src/lib.rs not found in output",
            );
        };

        let mut issues = Vec::new();

        // The function must still exist.
        if !lib.contains("fn merge_sum") {
            issues.push("merge_sum function was removed");
        }

        // The original buggy pattern: bare .get(key) result used in arithmetic.
        // If the file still has the raw `let existing = base.get(key);` line
        // AND uses `existing + val`, it hasn't been fixed.
        let has_bare_get = lib.contains("base.get(key);") || lib.contains("base.get(key));");
        if has_bare_get && lib.contains("existing + val") {
            issues.push("existing is still Option, not unwrapped");
        }

        // A correct fix must handle the Option somehow.
        let handles_option = lib.contains("or_insert(0)")
            || lib.contains("or_insert_with")
            || lib.contains("unwrap_or(0)")
            || lib.contains("unwrap_or_default")
            || lib.contains("or_default()");

        if !handles_option {
            issues.push("no Option handling found (expected or_insert/unwrap_or/etc)");
        }

        if issues.is_empty() {
            TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Pass,
                "HashMap::get Option handled correctly",
            )
        } else {
            TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Fail,
                format!("issues: {}", issues.join("; ")),
            )
        }
    }
}

// ── Scenario 07: Add a crate dependency via crates.io lookup ────────────

/// The model must use `crates_io_lookup` to find serde's latest version,
/// then add it to Cargo.toml.
struct Scenario07CrateLookup;

impl EvalTask for Scenario07CrateLookup {
    fn id(&self) -> &str {
        "scenario_07_crate_lookup"
    }

    fn name(&self) -> &str {
        "Add a crate dependency via crates.io lookup"
    }

    fn description(&self) -> &str {
        "Use crates_io_lookup to find serde, then add it to Cargo.toml with the derive feature."
    }

    fn initial_files(&self) -> Vec<(&str, &str)> {
        vec![
            (
                "Cargo.toml",
                "[package]\n\
                 name = \"scenario_07_crate_lookup\"\n\
                 version = \"0.1.0\"\n\
                 edition = \"2021\"\n\
                 \n\
                 [dependencies]\n",
            ),
            (
                "src/lib.rs",
                "use serde::{Deserialize, Serialize};\n\
                 \n\
                 /// A user record that can be serialized and deserialized.\n\
                 #[derive(Serialize, Deserialize, Debug)]\n\
                 pub struct User {\n\
                 \x20   pub id: u64,\n\
                 \x20   pub name: String,\n\
                 \x20   pub email: String,\n\
                 }\n\
                 \n\
                 /// Create a new user.\n\
                 pub fn new_user(id: u64, name: &str, email: &str) -> User {\n\
                 \x20   User {\n\
                 \x20       id,\n\
                 \x20       name: name.to_string(),\n\
                 \x20       email: email.to_string(),\n\
                 \x20   }\n\
                 }\n",
            ),
        ]
    }

    fn user_prompt(&self) -> &str {
        "The file src/lib.rs uses the `serde` crate for serialization, but serde is \
         not listed as a dependency in Cargo.toml. Use the `crates_io_lookup` tool \
         to find the latest version of serde and its available features, then edit \
         Cargo.toml to add serde with the `derive` feature. After editing, run \
         `cargo check` to make sure the project compiles."
    }

    fn verify(&self, files: &[(&str, &str)]) -> TaskOutcome {
        let Some((_, toml)) = files.iter().find(|(p, _)| *p == "Cargo.toml") else {
            return TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Error,
                "Cargo.toml not found in output",
            );
        };

        let Some((_, lib)) = files.iter().find(|(p, _)| *p == "src/lib.rs") else {
            return TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Error,
                "src/lib.rs not found in output",
            );
        };

        let mut issues = Vec::new();

        // Check serde is in Cargo.toml under [dependencies].
        let deps_section_idx = toml.find("[dependencies]");
        if let Some(idx) = deps_section_idx {
            let after_deps = &toml[idx..];
            if !after_deps.contains("serde") {
                issues.push("serde not found under [dependencies] in Cargo.toml");
            }
        } else {
            issues.push("[dependencies] section not found in Cargo.toml");
        }

        // Check the derive feature is mentioned.
        let has_derive_feature =
            toml.contains("derive") || toml.contains("\"derive\"") || toml.contains("'derive'");
        if !has_derive_feature {
            issues.push("serde's derive feature not found");
        }

        // Check src/lib.rs still has the User struct.
        if !lib.contains("struct User") {
            issues.push("User struct was removed from src/lib.rs");
        }

        if issues.is_empty() {
            TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Pass,
                "serde added to Cargo.toml with derive feature",
            )
        } else {
            TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Fail,
                format!("issues: {}", issues.join("; ")),
            )
        }
    }
}

// ── Scenario 08: Fix E0106 missing lifetime specifier ───────────────────────

/// A struct holds a `&str` reference but is missing the lifetime parameter.
/// The model must add `<'a>` to the struct, annotate the field, and update
/// the `impl` block and constructor — preserving reference semantics.
struct Scenario08FixMissingLifetime;

impl EvalTask for Scenario08FixMissingLifetime {
    fn id(&self) -> &str {
        "scenario_08_fix_missing_lifetime"
    }

    fn name(&self) -> &str {
        "Fix missing lifetime specifier (E0106)"
    }

    fn description(&self) -> &str {
        "Fix E0106: TextCursor holds &str but is missing a lifetime parameter."
    }

    fn initial_files(&self) -> Vec<(&str, &str)> {
        vec![(
            "src/lib.rs",
            "/// A cursor that walks through a text buffer by character.\n\
             pub struct TextCursor {\n\
             \x20   source: &str,\n\
             \x20   position: usize,\n\
             }\n\
             \n\
             impl TextCursor {\n\
             \x20   /// Create a new cursor for the given source text.\n\
             \x20   pub fn new(source: &str) -> TextCursor {\n\
             \x20       TextCursor { source, position: 0 }\n\
             \x20   }\n\
             \n\
             \x20   /// Peek at the current character without advancing.\n\
             \x20   pub fn peek(&self) -> Option<char> {\n\
             \x20       self.source.chars().nth(self.position)\n\
             \x20   }\n\
             \n\
             \x20   /// Advance the cursor by one character.\n\
             \x20   pub fn advance(&mut self) {\n\
             \x20       if self.position < self.source.len() {\n\
             \x20           self.position += 1;\n\
             \x20       }\n\
             \x20   }\n\
             \n\
             \x20   /// Return the remaining text from the current position.\n\
             \x20   pub fn remaining(&self) -> &str {\n\
             \x20       &self.source[self.position..]\n\
             \x20   }\n\
             }\n",
        )]
    }

    fn user_prompt(&self) -> &str {
        "The file src/lib.rs has a compilation error. Use cargo_check to see \
         the error, then use edit_file to fix it. After fixing, run cargo_check \
         again to confirm the fix compiles cleanly."
    }

    fn verify(&self, files: &[(&str, &str)]) -> TaskOutcome {
        let Some((_, lib)) = files.iter().find(|(p, _)| *p == "src/lib.rs") else {
            return TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Error,
                "src/lib.rs not found in output",
            );
        };

        let mut issues = Vec::new();

        // The struct must have a lifetime parameter.
        let struct_has_lifetime =
            lib.contains("struct TextCursor<") || lib.contains("struct TextCursor <");
        if !struct_has_lifetime {
            issues.push("TextCursor struct is missing a lifetime parameter");
        }

        // The source field must use a lifetime (not owned String).
        // Accept &'a str, '_ str, or any explicit lifetime.
        let field_has_lifetime_ref =
            (lib.contains("source: &'") || lib.contains("source: &'_")) && lib.contains("str");
        let field_is_owned_string =
            lib.contains("source: String") || lib.contains("source : String");

        if field_is_owned_string {
            issues.push("source field was changed to String — must remain &str with a lifetime");
        } else if !field_has_lifetime_ref {
            issues.push("source field is still &str without a lifetime annotation");
        }

        // The impl block must include the lifetime.
        let impl_has_lifetime = lib.contains("impl<") && lib.contains("TextCursor");
        if !impl_has_lifetime {
            issues.push("impl block is missing the lifetime parameter");
        }

        // The struct and its methods should still exist.
        if !lib.contains("fn new(") {
            issues.push("new() method was removed");
        }
        if !lib.contains("fn peek(") {
            issues.push("peek() method was removed");
        }
        if !lib.contains("fn advance(") {
            issues.push("advance() method was removed");
        }
        if !lib.contains("fn remaining(") {
            issues.push("remaining() method was removed");
        }

        if issues.is_empty() {
            TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Pass,
                "lifetime parameter added correctly, reference semantics preserved",
            )
        } else {
            TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Fail,
                format!("issues: {}", issues.join("; ")),
            )
        }
    }
}

// ── Scenario 09: Fix E0502 borrow conflict ─────────────────────────────────

/// Two methods call `get()` then `insert()` on the same `HashMap` — the
/// immutable reference from `get()` is still alive when `insert()` needs a
/// mutable borrow. The model must break the overlap by copying the value
/// out before mutating, or by using the entry API.
struct Scenario09FixBorrowConflict;

impl EvalTask for Scenario09FixBorrowConflict {
    fn id(&self) -> &str {
        "scenario_09_fix_borrow_conflict"
    }

    fn name(&self) -> &str {
        "Fix borrow conflict (E0502)"
    }

    fn description(&self) -> &str {
        "Fix E0502: get() holds an immutable reference while insert() needs a mutable borrow."
    }

    fn initial_files(&self) -> Vec<(&str, &str)> {
        vec![(
            "src/lib.rs",
            "use std::collections::HashMap;\n\
             \n\
             /// A registry that tracks scores for named participants.\n\
             pub struct Scoreboard {\n\
             \x20   scores: HashMap<String, i64>,\n\
             }\n\
             \n\
             impl Scoreboard {\n\
             \x20   /// Create a new empty scoreboard.\n\
             \x20   pub fn new() -> Scoreboard {\n\
             \x20       Scoreboard {\n\
             \x20           scores: HashMap::new(),\n\
             \x20       }\n\
             \x20   }\n\
             \n\
             \x20   /// Add points to a participant's score.\n\
             \x20   pub fn add_score(&mut self, name: &str, points: i64) {\n\
             \x20       let current = self.scores.get(name).unwrap_or(&0);\n\
             \x20       self.scores.insert(name.to_string(), *current + points);\n\
             \x20   }\n\
             \n\
             \x20   /// Get a participant's current score.\n\
             \x20   pub fn get_score(&self, name: &str) -> i64 {\n\
             \x20       *self.scores.get(name).unwrap_or(&0)\n\
             \x20   }\n\
             \n\
             \x20   /// Record a penalty by subtracting points.\n\
             \x20   pub fn penalize(&mut self, name: &str, penalty: i64) {\n\
             \x20       let current = self.scores.get(name).unwrap_or(&0);\n\
             \x20       self.scores.insert(name.to_string(), *current - penalty);\n\
             \x20   }\n\
             }\n",
        )]
    }

    fn user_prompt(&self) -> &str {
        "The file src/lib.rs has a compilation error. Use cargo_check to see the \
         error, then use edit_file to fix it. After fixing, run cargo_check again \
         to confirm the fix compiles cleanly."
    }

    fn verify(&self, files: &[(&str, &str)]) -> TaskOutcome {
        let Some((_, lib)) = files.iter().find(|(p, _)| *p == "src/lib.rs") else {
            return TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Error,
                "src/lib.rs not found in output",
            );
        };

        let mut issues = Vec::new();

        // Both add_score and penalize must still exist.
        if !lib.contains("fn add_score") {
            issues.push("add_score() method was removed");
        }
        if !lib.contains("fn penalize") {
            issues.push("penalize() method was removed");
        }

        // The Scoreboard struct and HashMap must still be present.
        if !lib.contains("struct Scoreboard") {
            issues.push("Scoreboard struct was removed");
        }
        if !lib.contains("HashMap") {
            issues.push("HashMap usage was removed");
        }

        // The original borrow-conflict pattern: `let current = self.scores.get(...)`
        // followed by `self.scores.insert(...)` in the same block. If `current`
        // is still a reference (`&i64`), the borrow conflict persists.
        //
        // Valid fixes:
        //   1. Copy the value out: `let current = *self.scores.get(...).unwrap_or(&0);`
        //   2. Use entry API: `self.scores.entry(...).and_modify(...).or_insert(...)`
        //
        // The broken pattern: `let current = self.scores.get(name).unwrap_or(&0);`
        // where `current` is `&i64` — still borrowing `self.scores`.

        let has_broken_get_pattern =
            lib.contains("let current = self.scores.get(name).unwrap_or(&0);");
        let has_deref_fix = lib.contains("let current = *self.scores.get(");
        let has_entry_fix = lib.contains("entry(");

        if has_broken_get_pattern && !has_deref_fix && !has_entry_fix {
            issues.push("add_score/penalize still have overlapping borrow pattern");
        }

        // The get_score method should still work (it has no borrow conflict).
        if !lib.contains("fn get_score") {
            issues.push("get_score() method was removed");
        }

        if issues.is_empty() {
            TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Pass,
                "borrow conflict resolved",
            )
        } else {
            TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Fail,
                format!("issues: {}", issues.join("; ")),
            )
        }
    }
}

// ── Scenario 10: Fix E0277 trait bound not satisfied ────────────────────────

/// A generic `verify<T>` function and a `Container<T>` struct both call
/// `.checksum()` on `T`, but neither has a `T: Checksum` bound. The model
/// must add the trait bound to both the function and the impl block.
struct Scenario10FixTraitBound;

impl EvalTask for Scenario10FixTraitBound {
    fn id(&self) -> &str {
        "scenario_10_fix_trait_bound"
    }

    fn name(&self) -> &str {
        "Fix trait bound not satisfied (E0277)"
    }

    fn description(&self) -> &str {
        "Fix E0277: add the `T: Checksum` bound to a generic function and struct impl."
    }

    fn initial_files(&self) -> Vec<(&str, &str)> {
        vec![(
            "src/lib.rs",
            "/// A trait for types that can produce a checksum.\n\
             pub trait Checksum {\n\
             \x20   /// Return a simple checksum value.\n\
             \x20   fn checksum(&self) -> u64;\n\
             }\n\
             \n\
             impl Checksum for String {\n\
             \x20   fn checksum(&self) -> u64 {\n\
             \x20       self.bytes().fold(0u64, |acc, b| acc.wrapping_add(b as u64))\n\
             \x20   }\n\
             }\n\
             \n\
             impl Checksum for Vec<u8> {\n\
             \x20   fn checksum(&self) -> u64 {\n\
             \x20       self.iter().fold(0u64, |acc, b| acc.wrapping_add(*b as u64))\n\
             \x20   }\n\
             }\n\
             \n\
             /// Verify that a value's checksum matches the expected value.\n\
             pub fn verify<T>(data: &T, expected: u64) -> bool {\n\
             \x20   data.checksum() == expected\n\
             }\n\
             \n\
             /// A container that holds a checksummable value.\n\
             pub struct Container<T> {\n\
             \x20   pub value: T,\n\
             }\n\
             \n\
             impl<T> Container<T> {\n\
             \x20   /// Create a new container.\n\
             \x20   pub fn new(value: T) -> Container<T> {\n\
             \x20       Container { value }\n\
             \x20   }\n\
             \n\
             \x20   /// Check if the stored value's checksum matches the expected value.\n\
             \x20   pub fn validate(&self, expected: u64) -> bool {\n\
             \x20       self.value.checksum() == expected\n\
             \x20   }\n\
             }\n",
        )]
    }

    fn user_prompt(&self) -> &str {
        "The file src/lib.rs has compilation errors. Use cargo_check to see the \
         errors, then use edit_file to fix them. After fixing, run cargo_check again \
         to confirm the fix compiles cleanly."
    }

    fn verify(&self, files: &[(&str, &str)]) -> TaskOutcome {
        let Some((_, lib)) = files.iter().find(|(p, _)| *p == "src/lib.rs") else {
            return TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Error,
                "src/lib.rs not found in output",
            );
        };

        let mut issues = Vec::new();

        // The Checksum trait and its methods must still exist.
        if !lib.contains("trait Checksum") {
            issues.push("Checksum trait was removed");
        }
        if !lib.contains("fn checksum") {
            issues.push("checksum() method was removed");
        }

        // The verify function must exist with a Checksum bound on T.
        if !lib.contains("fn verify") {
            issues.push("verify() function was removed");
        } else if !lib.contains("Checksum") {
            // If Checksum was removed entirely, already flagged above.
        } else {
            // Check that verify has T: Checksum (inline or where clause).
            let verify_has_bound = lib.contains("fn verify<T: Checksum>")
                || (lib.contains("fn verify<T>") && lib.contains("where T: Checksum"));
            if !verify_has_bound {
                issues.push("verify() is missing T: Checksum bound");
            }
        }

        // The Container struct and its methods must still exist.
        if !lib.contains("struct Container") {
            issues.push("Container struct was removed");
        }
        if !lib.contains("fn validate") {
            issues.push("validate() method was removed");
        }

        // The Container impl must have T: Checksum (inline or where clause).
        let impl_has_bound = lib.contains("impl<T: Checksum> Container")
            || (lib.contains("impl<T> Container") && lib.contains("where T: Checksum"));
        if !impl_has_bound {
            issues.push("Container impl is missing T: Checksum bound");
        }

        if issues.is_empty() {
            TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Pass,
                "trait bounds added correctly",
            )
        } else {
            TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Fail,
                format!("issues: {}", issues.join("; ")),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_tasks_returns_ten_tasks() {
        let tasks = all_tasks();
        assert_eq!(tasks.len(), 10);
    }

    #[test]
    fn all_tasks_have_unique_ids() {
        let tasks = all_tasks();
        let mut ids: Vec<&str> = tasks.iter().map(|t| t.id()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), tasks.len());
    }

    #[test]
    fn all_tasks_have_initial_files() {
        for task in all_tasks() {
            assert!(
                !task.initial_files().is_empty(),
                "task {} has no initial files",
                task.id()
            );
        }
    }

    // ── Scenario 01 ─────────────────────────────────────────────────────

    #[test]
    fn scenario_01_passes_with_string_return() {
        let task = Scenario01FixTypeMismatch;
        let files = vec![(
            "src/lib.rs",
            "/// Returns a greeting message.\n\
             pub fn greet(name: &str) -> String {\n\
             \x20   format!(\"Hello, {name}!\")\n\
             }\n\
             \n\
             /// Formats a farewell message.\n\
             pub fn farewell(name: &str) -> String {\n\
             \x20   format!(\"Goodbye, {name}!\")\n\
             }\n",
        )];
        assert_eq!(task.verify(&files).verdict, TaskVerdict::Pass);
    }

    #[test]
    fn scenario_01_fails_with_integer_return() {
        let task = Scenario01FixTypeMismatch;
        let files = vec![(
            "src/lib.rs",
            "/// Returns a greeting message.\n\
             pub fn greet(name: &str) -> String {\n\
             \x20   42\n\
             }\n",
        )];
        assert_eq!(task.verify(&files).verdict, TaskVerdict::Fail);
    }

    // ── Scenario 02 ─────────────────────────────────────────────────────

    #[test]
    fn scenario_02_passes_with_imports_removed() {
        let task = Scenario02UnusedImports;
        let files = vec![(
            "src/lib.rs",
            "/// Adds two numbers together.\n\
             pub fn add(a: i32, b: i32) -> i32 {\n\
             \x20   a + b\n\
             }\n\
             \n\
             /// Doubles a number.\n\
             pub fn double(x: i32) -> i32 {\n\
             \x20   x * 2\n\
             }\n",
        )];
        assert_eq!(task.verify(&files).verdict, TaskVerdict::Pass);
    }

    #[test]
    fn scenario_02_fails_with_imports_present() {
        let task = Scenario02UnusedImports;
        let files = vec![(
            "src/lib.rs",
            "use std::collections::HashMap;\n\
             use std::io;\n\
             \n\
             pub fn add(a: i32, b: i32) -> i32 { a + b }\n",
        )];
        assert_eq!(task.verify(&files).verdict, TaskVerdict::Fail);
    }

    // ── Scenario 03 ─────────────────────────────────────────────────────

    #[test]
    fn scenario_03_passes_with_derive_debug() {
        let task = Scenario03ExplainAndFix;
        let files = vec![(
            "src/lib.rs",
            "#[derive(Debug)]\n\
             pub struct Config {\n\
             \x20   pub name: String,\n\
             \x20   pub verbose: bool,\n\
             }\n",
        )];
        assert_eq!(task.verify(&files).verdict, TaskVerdict::Pass);
    }

    #[test]
    fn scenario_03_fails_without_derive() {
        let task = Scenario03ExplainAndFix;
        let files = vec![(
            "src/lib.rs",
            "pub struct Config {\n\
             \x20   pub name: String,\n\
             }\n",
        )];
        assert_eq!(task.verify(&files).verdict, TaskVerdict::Fail);
    }

    // ── Scenario 04 ─────────────────────────────────────────────────────

    #[test]
    fn scenario_04_passes_with_mut_result() {
        let task = Scenario04FixAndTest;
        let files = vec![(
            "src/lib.rs",
            "pub fn collect_evens(max: i32) -> Vec<i32> {\n\
             \x20   let mut result = Vec::new();\n\
             \x20   for i in 0..max {\n\
             \x20       if i % 2 == 0 {\n\
             \x20           result.push(i);\n\
             \x20       }\n\
             \x20   }\n\
             \x20   result\n\
             }\n\
             \n\
             #[cfg(test)]\n\
             mod tests {\n\
             \x20   use super::*;\n\
             \x20   #[test]\n\
             \x20   fn collects_evens_up_to_10() {\n\
             \x20       assert_eq!(collect_evens(10), vec![0, 2, 4, 6, 8]);\n\
             \x20   }\n\
             }\n",
        )];
        assert_eq!(task.verify(&files).verdict, TaskVerdict::Pass);
    }

    #[test]
    fn scenario_04_fails_without_mut() {
        let task = Scenario04FixAndTest;
        let files = vec![(
            "src/lib.rs",
            "pub fn collect_evens(max: i32) -> Vec<i32> {\n\
             \x20   let result = Vec::new();\n\
             \x20   result\n\
             }\n",
        )];
        assert_eq!(task.verify(&files).verdict, TaskVerdict::Fail);
    }

    // ── Scenario 05 ─────────────────────────────────────────────────────

    #[test]
    fn scenario_05_passes_with_all_fixes() {
        let task = Scenario05MultiErrorFix;
        let files = vec![(
            "src/lib.rs",
            "use std::collections::HashMap;\n\
             \n\
             pub struct Store {\n\
             \x20   data: HashMap<String, String>,\n\
             }\n\
             \n\
             impl Store {\n\
             \x20   pub fn new() -> Store {\n\
             \x20       Store { data: HashMap::new() }\n\
             \x20   }\n\
             \n\
             \x20   pub fn insert(&mut self, key: String, value: String) {\n\
             \x20       self.data.insert(key, value);\n\
             \x20   }\n\
             \n\
             \x20   pub fn get(&self, key: &str) -> Option<&String> {\n\
             \x20       self.data.get(key)\n\
             \x20   }\n\
             \n\
             \x20   pub fn len(&self) -> usize {\n\
             \x20       self.data.len()\n\
             \x20   }\n\
             \n\
             \x20   pub fn is_empty(&self) -> bool {\n\
             \x20       self.data.is_empty()\n\
             \x20   }\n\
             }\n",
        )];
        assert_eq!(task.verify(&files).verdict, TaskVerdict::Pass);
    }

    #[test]
    fn scenario_05_fails_with_original_errors() {
        let task = Scenario05MultiErrorFix;
        let files = task.initial_files();
        assert_eq!(task.verify(&files).verdict, TaskVerdict::Fail);
    }

    // ── Scenario 06 ─────────────────────────────────────────────────────

    #[test]
    fn scenario_06_passes_with_entry_api() {
        let task = Scenario06RustdocApiLookup;
        let files = vec![(
            "src/lib.rs",
            "use std::collections::HashMap;\n\
             \n\
             pub fn merge_sum(base: &mut HashMap<String, i64>, other: &HashMap<String, i64>) {\n\
             \x20   for (key, val) in other {\n\
             \x20       *base.entry(key.to_string()).or_insert(0) += val;\n\
             \x20   }\n\
             }\n",
        )];
        assert_eq!(task.verify(&files).verdict, TaskVerdict::Pass);
    }

    #[test]
    fn scenario_06_passes_with_get_unwrap_or() {
        let task = Scenario06RustdocApiLookup;
        let files = vec![(
            "src/lib.rs",
            "use std::collections::HashMap;\n\
             \n\
             pub fn merge_sum(base: &mut HashMap<String, i64>, other: &HashMap<String, i64>) {\n\
             \x20   for (key, val) in other {\n\
             \x20       let existing = base.get(key).copied().unwrap_or(0);\n\
             \x20       base.insert(key.to_string(), existing + val);\n\
             \x20   }\n\
             }\n",
        )];
        assert_eq!(task.verify(&files).verdict, TaskVerdict::Pass);
    }

    #[test]
    fn scenario_06_fails_with_original_bug() {
        let task = Scenario06RustdocApiLookup;
        let files = task.initial_files();
        assert_eq!(task.verify(&files).verdict, TaskVerdict::Fail);
    }

    // ── General ─────────────────────────────────────────────────────────

    #[test]
    fn missing_file_returns_error_verdict() {
        let task = Scenario01FixTypeMismatch;
        let files: Vec<(&str, &str)> = vec![];
        assert_eq!(task.verify(&files).verdict, TaskVerdict::Error);
    }

    // ── Scenario 07 ─────────────────────────────────────────────────────

    #[test]
    fn scenario_07_passes_with_serde_in_cargo_toml() {
        let task = Scenario07CrateLookup;
        let files = vec![
            (
                "Cargo.toml",
                "[package]\n\
                 name = \"test\"\n\
                 version = \"0.1.0\"\n\
                 [dependencies]\n\
                 serde = { version = \"1\", features = [\"derive\"] }\n",
            ),
            (
                "src/lib.rs",
                "use serde::{Deserialize, Serialize};\n\
                 pub struct User { pub id: u64, pub name: String, pub email: String }\n",
            ),
        ];
        assert_eq!(task.verify(&files).verdict, TaskVerdict::Pass);
    }

    #[test]
    fn scenario_07_fails_without_serde() {
        let task = Scenario07CrateLookup;
        let files = vec![
            (
                "Cargo.toml",
                "[package]\n\
                 name = \"test\"\n\
                 version = \"0.1.0\"\n\
                 [dependencies]\n",
            ),
            (
                "src/lib.rs",
                "use serde::{Deserialize, Serialize};\n\
                 pub struct User { pub id: u64, pub name: String, pub email: String }\n",
            ),
        ];
        assert_eq!(task.verify(&files).verdict, TaskVerdict::Fail);
    }

    #[test]
    fn scenario_07_fails_missing_cargo_toml() {
        let task = Scenario07CrateLookup;
        let files = vec![];
        assert_eq!(task.verify(&files).verdict, TaskVerdict::Error);
    }

    // ── Scenario 08 ─────────────────────────────────────────────────────

    #[test]
    fn scenario_08_passes_with_correct_lifetime() {
        let task = Scenario08FixMissingLifetime;
        let files = vec![(
            "src/lib.rs",
            "pub struct TextCursor<'a> {\n\
             \x20   source: &'a str,\n\
             \x20   position: usize,\n\
             }\n\
             \n\
             impl<'a> TextCursor<'a> {\n\
             \x20   pub fn new(source: &'a str) -> TextCursor<'a> {\n\
             \x20       TextCursor { source, position: 0 }\n\
             \x20   }\n\
             \n\
             \x20   pub fn peek(&self) -> Option<char> {\n\
             \x20       self.source.chars().nth(self.position)\n\
             \x20   }\n\
             \n\
             \x20   pub fn advance(&mut self) {\n\
             \x20       if self.position < self.source.len() {\n\
             \x20           self.position += 1;\n\
             \x20       }\n\
             \x20   }\n\
             \n\
             \x20   pub fn remaining(&self) -> &'a str {\n\
             \x20       &self.source[self.position..]\n\
             \x20   }\n\
             }\n",
        )];
        assert_eq!(task.verify(&files).verdict, TaskVerdict::Pass);
    }

    #[test]
    fn scenario_08_passes_with_underscore_lifetime() {
        let task = Scenario08FixMissingLifetime;
        let files = vec![(
            "src/lib.rs",
            "pub struct TextCursor<'_> {\n\
             \x20   source: &'_ str,\n\
             \x20   position: usize,\n\
             }\n\
             \n\
             impl<'a> TextCursor<'a> {\n\
             \x20   pub fn new(source: &'a str) -> TextCursor<'a> {\n\
             \x20       TextCursor { source, position: 0 }\n\
             \x20   }\n\
             \x20   pub fn peek(&self) -> Option<char> {\n\
             \x20       self.source.chars().nth(self.position)\n\
             \x20   }\n\
             \x20   pub fn advance(&mut self) {}\n\
             \x20   pub fn remaining(&self) -> &str { &self.source[self.position..] }\n\
             }\n",
        )];
        // The underscore lifetime on the struct is valid Rust syntax.
        // The impl uses 'a so impl_has_lifetime should still pass.
        assert_eq!(task.verify(&files).verdict, TaskVerdict::Pass);
    }

    #[test]
    fn scenario_08_fails_with_original_code() {
        let task = Scenario08FixMissingLifetime;
        let files = task.initial_files();
        assert_eq!(task.verify(&files).verdict, TaskVerdict::Fail);
    }

    #[test]
    fn scenario_08_fails_if_switched_to_string() {
        let task = Scenario08FixMissingLifetime;
        let files = vec![(
            "src/lib.rs",
            "pub struct TextCursor {\n\
             \x20   source: String,\n\
             \x20   position: usize,\n\
             }\n\
             \n\
             impl TextCursor {\n\
             \x20   pub fn new(source: &str) -> TextCursor {\n\
             \x20       TextCursor { source: source.to_string(), position: 0 }\n\
             \x20   }\n\
             \x20   pub fn peek(&self) -> Option<char> { self.source.chars().nth(self.position) }\n\
             \x20   pub fn advance(&mut self) {}\n\
             \x20   pub fn remaining(&self) -> &str { &self.source[self.position..] }\n\
             }\n",
        )];
        assert_eq!(task.verify(&files).verdict, TaskVerdict::Fail);
    }

    // ── Scenario 09 ─────────────────────────────────────────────────────

    #[test]
    fn scenario_09_passes_with_dereferenced_copy() {
        let task = Scenario09FixBorrowConflict;
        let files = vec![(
            "src/lib.rs",
            "use std::collections::HashMap;\n\
             \n\
             pub struct Scoreboard {\n\
             \x20   scores: HashMap<String, i64>,\n\
             }\n\
             \n\
             impl Scoreboard {\n\
             \x20   pub fn new() -> Scoreboard {\n\
             \x20       Scoreboard { scores: HashMap::new() }\n\
             \x20   }\n\
             \x20   pub fn add_score(&mut self, name: &str, points: i64) {\n\
             \x20       let current = *self.scores.get(name).unwrap_or(&0);\n\
             \x20       self.scores.insert(name.to_string(), current + points);\n\
             \x20   }\n\
             \x20   pub fn get_score(&self, name: &str) -> i64 {\n\
             \x20       *self.scores.get(name).unwrap_or(&0)\n\
             \x20   }\n\
             \x20   pub fn penalize(&mut self, name: &str, penalty: i64) {\n\
             \x20       let current = *self.scores.get(name).unwrap_or(&0);\n\
             \x20       self.scores.insert(name.to_string(), current - penalty);\n\
             \x20   }\n\
             }\n",
        )];
        assert_eq!(task.verify(&files).verdict, TaskVerdict::Pass);
    }

    #[test]
    fn scenario_09_passes_with_entry_api() {
        let task = Scenario09FixBorrowConflict;
        let files = vec![(
            "src/lib.rs",
            "use std::collections::HashMap;\n\
             \n\
             pub struct Scoreboard {\n\
             \x20   scores: HashMap<String, i64>,\n\
             }\n\
             \n\
             impl Scoreboard {\n\
             \x20   pub fn new() -> Scoreboard {\n\
             \x20       Scoreboard { scores: HashMap::new() }\n\
             \x20   }\n\
             \x20   pub fn add_score(&mut self, name: &str, points: i64) {\n\
             \x20       *self.scores.entry(name.to_string()).or_insert(0) += points;\n\
             \x20   }\n\
             \x20   pub fn get_score(&self, name: &str) -> i64 {\n\
             \x20       *self.scores.get(name).unwrap_or(&0)\n\
             \x20   }\n\
             \x20   pub fn penalize(&mut self, name: &str, penalty: i64) {\n\
             \x20       *self.scores.entry(name.to_string()).or_insert(0) -= penalty;\n\
             \x20   }\n\
             }\n",
        )];
        assert_eq!(task.verify(&files).verdict, TaskVerdict::Pass);
    }

    #[test]
    fn scenario_09_fails_with_original_borrow_conflict() {
        let task = Scenario09FixBorrowConflict;
        let files = task.initial_files();
        assert_eq!(task.verify(&files).verdict, TaskVerdict::Fail);
    }

    // ── Scenario 10 ─────────────────────────────────────────────────────

    #[test]
    fn scenario_10_passes_with_inline_bounds() {
        let task = Scenario10FixTraitBound;
        let files = vec![(
            "src/lib.rs",
            "/// A trait for types that can produce a checksum.\n\
             pub trait Checksum {\n\
             \x20   /// Return a simple checksum value.\n\
             \x20   fn checksum(&self) -> u64;\n\
             }\n\
             \n\
             impl Checksum for String {\n\
             \x20   fn checksum(&self) -> u64 {\n\
             \x20       self.bytes().fold(0u64, |acc, b| acc.wrapping_add(b as u64))\n\
             \x20   }\n\
             }\n\
             \n\
             impl Checksum for Vec<u8> {\n\
             \x20   fn checksum(&self) -> u64 {\n\
             \x20       self.iter().fold(0u64, |acc, b| acc.wrapping_add(*b as u64))\n\
             \x20   }\n\
             }\n\
             \n\
             /// Verify that a value's checksum matches the expected value.\n\
             pub fn verify<T: Checksum>(data: &T, expected: u64) -> bool {\n\
             \x20   data.checksum() == expected\n\
             }\n\
             \n\
             /// A container that holds a checksummable value.\n\
             pub struct Container<T: Checksum> {\n\
             \x20   pub value: T,\n\
             }\n\
             \n\
             impl<T: Checksum> Container<T> {\n\
             \x20   /// Create a new container.\n\
             \x20   pub fn new(value: T) -> Container<T> {\n\
             \x20       Container { value }\n\
             \x20   }\n\
             \n\
             \x20   /// Check if the stored value's checksum matches the expected value.\n\
             \x20   pub fn validate(&self, expected: u64) -> bool {\n\
             \x20       self.value.checksum() == expected\n\
             \x20   }\n\
             }\n",
        )];
        assert_eq!(task.verify(&files).verdict, TaskVerdict::Pass);
    }

    #[test]
    fn scenario_10_passes_with_where_clause() {
        let task = Scenario10FixTraitBound;
        let files = vec![(
            "src/lib.rs",
            "/// A trait for types that can produce a checksum.\n\
             pub trait Checksum {\n\
             \x20   /// Return a simple checksum value.\n\
             \x20   fn checksum(&self) -> u64;\n\
             }\n\
             \n\
             impl Checksum for String {\n\
             \x20   fn checksum(&self) -> u64 {\n\
             \x20       self.bytes().fold(0u64, |acc, b| acc.wrapping_add(b as u64))\n\
             \x20   }\n\
             }\n\
             \n\
             impl Checksum for Vec<u8> {\n\
             \x20   fn checksum(&self) -> u64 {\n\
             \x20       self.iter().fold(0u64, |acc, b| acc.wrapping_add(*b as u64))\n\
             \x20   }\n\
             }\n\
             \n\
             /// Verify that a value's checksum matches the expected value.\n\
             pub fn verify<T>(data: &T, expected: u64) -> bool\n\
             where T: Checksum\n\
             {\n\
             \x20   data.checksum() == expected\n\
             }\n\
             \n\
             /// A container that holds a checksummable value.\n\
             pub struct Container<T> where T: Checksum {\n\
             \x20   pub value: T,\n\
             }\n\
             \n\
             impl<T> Container<T> where T: Checksum {\n\
             \x20   /// Create a new container.\n\
             \x20   pub fn new(value: T) -> Container<T> {\n\
             \x20       Container { value }\n\
             \x20   }\n\
             \n\
             \x20   /// Check if the stored value's checksum matches the expected value.\n\
             \x20   pub fn validate(&self, expected: u64) -> bool {\n\
             \x20       self.value.checksum() == expected\n\
             \x20   }\n\
             }\n",
        )];
        assert_eq!(task.verify(&files).verdict, TaskVerdict::Pass);
    }

    #[test]
    fn scenario_10_fails_with_original_code() {
        let task = Scenario10FixTraitBound;
        let files = task.initial_files();
        assert_eq!(task.verify(&files).verdict, TaskVerdict::Fail);
    }
}
