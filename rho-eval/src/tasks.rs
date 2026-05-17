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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_tasks_returns_six_tasks() {
        let tasks = all_tasks();
        assert_eq!(tasks.len(), 6);
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
}
