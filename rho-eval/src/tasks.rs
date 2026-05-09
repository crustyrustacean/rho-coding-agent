//! Built-in eval task definitions.
//!
//! Each task represents a canonical coding scenario with a known correct outcome.

use crate::task::{EvalTask, TaskOutcome, TaskVerdict};

/// Returns all built-in eval tasks.
pub fn all_tasks() -> Vec<Box<dyn EvalTask>> {
    vec![
        Box::new(FixE0308TypeMismatch),
        Box::new(FixE0425UnresolvedName),
        Box::new(FixUnusedImport),
        Box::new(AddMissingDerive),
        Box::new(FixMutableBorrow),
    ]
}

// ── Task 1: Fix E0308 type mismatch ──────────────────────────────────────────

/// The function returns an integer but is declared to return `String`.
struct FixE0308TypeMismatch;

impl EvalTask for FixE0308TypeMismatch {
    fn id(&self) -> &str {
        "fix_e0308_type_mismatch"
    }

    fn name(&self) -> &str {
        "Fix E0308 type mismatch"
    }

    fn description(&self) -> &str {
        "The function `greet` is declared to return String but returns an integer. Fix it."
    }

    fn initial_files(&self) -> Vec<(&str, &str)> {
        vec![("src/lib.rs", "pub fn greet() -> String {\n    42\n}\n")]
    }

    fn user_prompt(&self) -> &str {
        "Fix the compilation error in src/lib.rs"
    }

    fn verify(&self, files: &[(&str, &str)]) -> TaskOutcome {
        let lib = files.iter().find(|(p, _)| *p == "src/lib.rs");
        match lib {
            Some((_, content)) => {
                // The fix should make the function return a String.
                let has_string_return = content.contains("String::from")
                    || content.contains("to_string()")
                    || content.contains("format!")
                    || content.contains(".to_owned()");
                if has_string_return && !content.contains("42") {
                    TaskOutcome::new(
                        self.id(),
                        self.name(),
                        TaskVerdict::Pass,
                        "function now returns a String",
                    )
                } else {
                    TaskOutcome::new(
                        self.id(),
                        self.name(),
                        TaskVerdict::Fail,
                        format!("expected String return, got: {content}"),
                    )
                }
            }
            None => TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Error,
                "src/lib.rs not found in output",
            ),
        }
    }
}

// ── Task 2: Fix E0425 unresolved name ────────────────────────────────────────

/// The function calls an undefined function.
struct FixE0425UnresolvedName;

impl EvalTask for FixE0425UnresolvedName {
    fn id(&self) -> &str {
        "fix_e0425_unresolved_name"
    }

    fn name(&self) -> &str {
        "Fix E0425 unresolved name"
    }

    fn description(&self) -> &str {
        "The function `compute` calls `add_one` which doesn't exist. Define it."
    }

    fn initial_files(&self) -> Vec<(&str, &str)> {
        vec![(
            "src/lib.rs",
            "pub fn compute(x: i32) -> i32 {\n    add_one(x)\n}\n",
        )]
    }

    fn user_prompt(&self) -> &str {
        "Fix the compilation error in src/lib.rs by defining the missing function."
    }

    fn verify(&self, files: &[(&str, &str)]) -> TaskOutcome {
        let lib = files.iter().find(|(p, _)| *p == "src/lib.rs");
        match lib {
            Some((_, content)) => {
                if content.contains("fn add_one") {
                    TaskOutcome::new(
                        self.id(),
                        self.name(),
                        TaskVerdict::Pass,
                        "add_one function defined",
                    )
                } else {
                    TaskOutcome::new(
                        self.id(),
                        self.name(),
                        TaskVerdict::Fail,
                        "add_one function not found",
                    )
                }
            }
            None => TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Error,
                "src/lib.rs not found",
            ),
        }
    }
}

// ── Task 3: Fix unused import ────────────────────────────────────────────────

/// The file has an unused import that should be removed.
struct FixUnusedImport;

impl EvalTask for FixUnusedImport {
    fn id(&self) -> &str {
        "fix_unused_import"
    }

    fn name(&self) -> &str {
        "Fix unused import warning"
    }

    fn description(&self) -> &str {
        "Remove the unused import `use std::io;` from src/lib.rs"
    }

    fn initial_files(&self) -> Vec<(&str, &str)> {
        vec![(
            "src/lib.rs",
            "use std::io;\n\npub fn hello() -> &'static str {\n    \"hello\"\n}\n",
        )]
    }

    fn user_prompt(&self) -> &str {
        "Fix the unused import warning in src/lib.rs"
    }

    fn verify(&self, files: &[(&str, &str)]) -> TaskOutcome {
        let lib = files.iter().find(|(p, _)| *p == "src/lib.rs");
        match lib {
            Some((_, content)) => {
                if content.contains("use std::io") {
                    TaskOutcome::new(
                        self.id(),
                        self.name(),
                        TaskVerdict::Fail,
                        "unused import still present",
                    )
                } else if content.contains("fn hello") {
                    TaskOutcome::new(
                        self.id(),
                        self.name(),
                        TaskVerdict::Pass,
                        "unused import removed",
                    )
                } else {
                    TaskOutcome::new(
                        self.id(),
                        self.name(),
                        TaskVerdict::Fail,
                        "function hello not found after edit",
                    )
                }
            }
            None => TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Error,
                "src/lib.rs not found",
            ),
        }
    }
}

// ── Task 4: Add missing derive ───────────────────────────────────────────────

/// A struct needs `#[derive(Debug)]` to satisfy a trait bound.
struct AddMissingDerive;

impl EvalTask for AddMissingDerive {
    fn id(&self) -> &str {
        "add_missing_derive_debug"
    }

    fn name(&self) -> &str {
        "Add missing #[derive(Debug)]"
    }

    fn description(&self) -> &str {
        "The struct Foo is used in a context requiring Debug. Add the derive."
    }

    fn initial_files(&self) -> Vec<(&str, &str)> {
        vec![(
            "src/lib.rs",
            "pub struct Foo {\n    pub x: i32,\n}\n\npub fn show(f: &Foo) -> String {\n    format!(\"{:?}\", f)\n}\n",
        )]
    }

    fn user_prompt(&self) -> &str {
        "Fix the compilation error by adding the necessary derive macro."
    }

    fn verify(&self, files: &[(&str, &str)]) -> TaskOutcome {
        let lib = files.iter().find(|(p, _)| *p == "src/lib.rs");
        match lib {
            Some((_, content)) => {
                if content.contains("derive") && content.contains("Debug") {
                    TaskOutcome::new(
                        self.id(),
                        self.name(),
                        TaskVerdict::Pass,
                        "#[derive(Debug)] added",
                    )
                } else {
                    TaskOutcome::new(
                        self.id(),
                        self.name(),
                        TaskVerdict::Fail,
                        "derive(Debug) not found",
                    )
                }
            }
            None => TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Error,
                "src/lib.rs not found",
            ),
        }
    }
}

// ── Task 5: Fix mutable borrow error ────────────────────────────────────────

/// A variable needs to be declared `mut` to allow mutation.
struct FixMutableBorrow;

impl EvalTask for FixMutableBorrow {
    fn id(&self) -> &str {
        "fix_mutable_borrow"
    }

    fn name(&self) -> &str {
        "Fix mutable borrow error"
    }

    fn description(&self) -> &str {
        "The variable `items` needs to be mutable to call `push`."
    }

    fn initial_files(&self) -> Vec<(&str, &str)> {
        vec![(
            "src/lib.rs",
            "pub fn build_list() -> Vec<i32> {\n    let items = Vec::new();\n    items.push(1);\n    items.push(2);\n    items\n}\n",
        )]
    }

    fn user_prompt(&self) -> &str {
        "Fix the compilation error in src/lib.rs"
    }

    fn verify(&self, files: &[(&str, &str)]) -> TaskOutcome {
        let lib = files.iter().find(|(p, _)| *p == "src/lib.rs");
        match lib {
            Some((_, content)) => {
                if content.contains("let mut items") || content.contains("let mut items") {
                    TaskOutcome::new(
                        self.id(),
                        self.name(),
                        TaskVerdict::Pass,
                        "variable declared as mutable",
                    )
                } else {
                    TaskOutcome::new(
                        self.id(),
                        self.name(),
                        TaskVerdict::Fail,
                        "let mut not found",
                    )
                }
            }
            None => TaskOutcome::new(
                self.id(),
                self.name(),
                TaskVerdict::Error,
                "src/lib.rs not found",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_tasks_returns_five_tasks() {
        let tasks = all_tasks();
        assert_eq!(tasks.len(), 5);
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

    #[test]
    fn fix_e0308_passes_with_string_return() {
        let task = FixE0308TypeMismatch;
        let files = vec![(
            "src/lib.rs",
            "pub fn greet() -> String {\n    String::from(\"hello\")\n}\n",
        )];
        let outcome = task.verify(&files);
        assert_eq!(outcome.verdict, TaskVerdict::Pass);
    }

    #[test]
    fn fix_e0308_fails_with_integer_return() {
        let task = FixE0308TypeMismatch;
        let files = vec![("src/lib.rs", "pub fn greet() -> String {\n    42\n}\n")];
        let outcome = task.verify(&files);
        assert_eq!(outcome.verdict, TaskVerdict::Fail);
    }

    #[test]
    fn fix_e0425_passes_when_function_defined() {
        let task = FixE0425UnresolvedName;
        let files = vec![(
            "src/lib.rs",
            "fn add_one(x: i32) -> i32 { x + 1 }\npub fn compute(x: i32) -> i32 {\n    add_one(x)\n}\n",
        )];
        let outcome = task.verify(&files);
        assert_eq!(outcome.verdict, TaskVerdict::Pass);
    }

    #[test]
    fn fix_unused_import_passes_when_removed() {
        let task = FixUnusedImport;
        let files = vec![(
            "src/lib.rs",
            "pub fn hello() -> &'static str {\n    \"hello\"\n}\n",
        )];
        let outcome = task.verify(&files);
        assert_eq!(outcome.verdict, TaskVerdict::Pass);
    }

    #[test]
    fn add_derive_passes_when_debug_added() {
        let task = AddMissingDerive;
        let files = vec![(
            "src/lib.rs",
            "#[derive(Debug)]\npub struct Foo {\n    pub x: i32,\n}\n\npub fn show(f: &Foo) -> String {\n    format!(\"{:?}\", f)\n}\n",
        )];
        let outcome = task.verify(&files);
        assert_eq!(outcome.verdict, TaskVerdict::Pass);
    }

    #[test]
    fn fix_mutable_borrow_passes_when_mut_added() {
        let task = FixMutableBorrow;
        let files = vec![(
            "src/lib.rs",
            "pub fn build_list() -> Vec<i32> {\n    let mut items = Vec::new();\n    items.push(1);\n    items.push(2);\n    items\n}\n",
        )];
        let outcome = task.verify(&files);
        assert_eq!(outcome.verdict, TaskVerdict::Pass);
    }

    #[test]
    fn missing_file_returns_error_verdict() {
        let task = FixE0308TypeMismatch;
        let files: Vec<(&str, &str)> = vec![];
        let outcome = task.verify(&files);
        assert_eq!(outcome.verdict, TaskVerdict::Error);
    }
}
