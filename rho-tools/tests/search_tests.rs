//! Tests for the Phase 0 read-tier tools: `search_files` and `find_files`.

use rho_core::SandboxRoot;
use rho_core::tool::{CancellationToken, Tool, ToolOutcome, ToolRisk};
use rho_test_helpers::tempdir_with_sandbox;
use rho_tools::search::{FindFiles, SearchFiles};
use serde_json::json;

fn immediate_output(outcome: &ToolOutcome) -> String {
    match outcome {
        ToolOutcome::Immediate(result) => result.output.clone(),
        ToolOutcome::Streamed(_) => panic!("unexpected streamed output"),
    }
}

fn immediate_is_error(outcome: &ToolOutcome) -> bool {
    matches!(outcome, ToolOutcome::Immediate(r) if r.is_error)
}

/// Create a sandboxed fixture tree for search tests:
///
/// ```text
/// src/main.rs          (needle in src)
/// src/lib.rs           (needle in src, twice)
/// docs/readme.md       (needle elsewhere)
/// hidden.rs            (hidden file, must be skipped)
/// ignored_dir/file.rs  (excluded via .gitignore, must be skipped)
/// binary.bin           (NUL bytes, must be skipped)
/// ```
fn fixture() -> (tempfile::TempDir, SandboxRoot) {
    let (dir, sandbox) = tempdir_with_sandbox();
    let root = sandbox.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::create_dir_all(root.join("ignored_dir")).unwrap();

    std::fs::write(
        root.join("src/main.rs"),
        "fn main() {\n    let needle = 1;\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        "// needle one\npub fn x() {}\n// needle two\n",
    )
    .unwrap();
    std::fs::write(root.join("docs/readme.md"), "docs mention needle once\n").unwrap();
    std::fs::write(root.join(".hidden.rs"), "needle in hidden file\n").unwrap();
    std::fs::write(root.join("ignored_dir/file.rs"), "needle in ignored dir\n").unwrap();
    std::fs::write(root.join("binary.bin"), "needle\0with nul\0bytes\n").unwrap();
    // A .gitignore excluding ignored_dir/. Note: `ignore` only applies
    // .gitignore when a git repo exists OR require_git(false) — the tool
    // sets require_git(false) precisely so this works without git init.
    std::fs::write(root.join(".gitignore"), "ignored_dir/\n").unwrap();

    (dir, sandbox)
}

// ── search_files ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn search_files_finds_matches_with_path_and_line() {
    let (_dir, sandbox) = fixture();
    let tool = SearchFiles::new(sandbox.clone());
    let out = tool
        .execute(json!({"pattern": "needle"}), CancellationToken::new())
        .await
        .unwrap();
    let text = immediate_output(&out);
    assert!(!immediate_is_error(&out));
    assert!(text.contains("src/main.rs:2:"), "got:\n{text}");
    assert!(text.contains("src/lib.rs:1:"), "got:\n{text}");
    assert!(text.contains("src/lib.rs:3:"), "got:\n{text}");
    assert!(text.contains("docs/readme.md:1:"), "got:\n{text}");
}

#[tokio::test]
async fn search_files_skips_hidden_gitignored_and_binary() {
    let (_dir, sandbox) = fixture();
    let tool = SearchFiles::new(sandbox.clone());
    let out = tool
        .execute(json!({"pattern": "needle"}), CancellationToken::new())
        .await
        .unwrap();
    let text = immediate_output(&out);
    assert!(
        !text.contains(".hidden.rs"),
        "hidden file searched:\n{text}"
    );
    assert!(
        !text.contains("ignored_dir"),
        "gitignored dir searched:\n{text}"
    );
    assert!(!text.contains("binary.bin"), "binary searched:\n{text}");
}

#[tokio::test]
async fn search_files_regex_patterns_work() {
    let (_dir, sandbox) = fixture();
    let tool = SearchFiles::new(sandbox.clone());
    // Alternation + anchors.
    let out = tool
        .execute(
            json!({"pattern": "^// needle (one|two)$"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let text = immediate_output(&out);
    assert!(text.contains("src/lib.rs:1:"), "got:\n{text}");
    assert!(text.contains("src/lib.rs:3:"), "got:\n{text}");
    assert!(!text.contains("main.rs"), "got:\n{text}");
}

#[tokio::test]
async fn search_files_case_sensitivity_default_and_flag() {
    let (_dir, sandbox) = fixture();
    let tool = SearchFiles::new(sandbox.clone());
    let root = sandbox.path();
    std::fs::write(root.join("case.txt"), "Mixed Case LINE here\n").unwrap();

    // Default: case-sensitive — 'mixed' does not match 'Mixed'.
    let out = tool
        .execute(
            json!({"pattern": "mixed case line"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(immediate_output(&out).contains("No matches"));

    // ignore_case=true matches.
    let out = tool
        .execute(
            json!({"pattern": "mixed case line", "ignore_case": true}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(
        immediate_output(&out).contains("case.txt:1:"),
        "got:\n{}",
        immediate_output(&out)
    );
}

#[tokio::test]
async fn search_files_glob_filter() {
    let (_dir, sandbox) = fixture();
    let tool = SearchFiles::new(sandbox.clone());
    let out = tool
        .execute(
            json!({"pattern": "needle", "glob": "*.rs"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let text = immediate_output(&out);
    assert!(text.contains("src/main.rs:2:"), "got:\n{text}");
    assert!(!text.contains("readme.md"), "glob not applied:\n{text}");
}

#[tokio::test]
async fn search_files_path_scoping() {
    let (_dir, sandbox) = fixture();
    let tool = SearchFiles::new(sandbox.clone());
    let out = tool
        .execute(
            json!({"pattern": "needle", "path": "src"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let text = immediate_output(&out);
    assert!(
        text.contains("src/main.rs:2:") || text.contains("main.rs:2:"),
        "got:\n{text}"
    );
    assert!(!text.contains("readme.md"), "scope not applied:\n{text}");
}

#[tokio::test]
async fn search_files_zero_matches_is_clean_success() {
    let (_dir, sandbox) = fixture();
    let tool = SearchFiles::new(sandbox.clone());
    let out = tool
        .execute(
            json!({"pattern": "definitely_not_present"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(!immediate_is_error(&out));
    assert!(immediate_output(&out).contains("No matches"));
}

#[tokio::test]
async fn search_files_invalid_regex_is_error_with_hint() {
    let (_dir, sandbox) = fixture();
    let tool = SearchFiles::new(sandbox.clone());
    // Lookbehind is unsupported by the regex engine — the error must be a
    // clean tool error, not a panic, and must hint at the limitation.
    let out = tool
        .execute(json!({"pattern": "(?<=foo)bar"}), CancellationToken::new())
        .await
        .unwrap();
    assert!(immediate_is_error(&out));
    let text = immediate_output(&out);
    assert!(text.contains("invalid regex"), "got:\n{text}");
    assert!(text.contains("look-around"), "hint missing:\n{text}");
}

#[tokio::test]
async fn search_files_truncates_at_max_results_with_notice() {
    let (_dir, sandbox) = fixture();
    let tool = SearchFiles::new(sandbox.clone());
    // '.' matches every line in the fixture — set max_results to 3.
    let out = tool
        .execute(
            json!({"pattern": ".", "max_results": 3}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let text = immediate_output(&out);
    let match_lines: Vec<&str> = text.lines().filter(|l| !l.starts_with('…')).collect();
    assert!(match_lines.len() <= 3, "cap exceeded:\n{text}");
    assert!(text.contains("truncated"), "notice missing:\n{text}");
}

#[tokio::test]
async fn search_files_binary_line_content_not_leaked() {
    // A file whose only "lines" are enormous must not flood the output.
    let (dir, sandbox) = tempdir_with_sandbox();
    let giant = "x".repeat(10_000);
    std::fs::write(dir.path().join("giant.txt"), format!("needle {giant}\n")).unwrap();
    let tool = SearchFiles::new(sandbox.clone());
    let out = tool
        .execute(json!({"pattern": "needle"}), CancellationToken::new())
        .await
        .unwrap();
    let text = immediate_output(&out);
    let matched_line = text.lines().next().unwrap_or_default();
    assert!(
        matched_line.chars().count() < 300,
        "line not capped:\n{matched_line}"
    );
}

#[tokio::test]
async fn search_files_rejects_path_outside_sandbox() {
    let (_dir, sandbox) = fixture();
    let tool = SearchFiles::new(sandbox.clone());
    let out = tool
        .execute(
            json!({"pattern": "needle", "path": "../"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(immediate_is_error(&out));
    assert!(
        immediate_output(&out).contains("outside the project sandbox"),
        "got:\n{}",
        immediate_output(&out)
    );
}

#[tokio::test]
async fn search_files_cancelled_token_returns_cancelled() {
    let (_dir, sandbox) = fixture();
    let tool = SearchFiles::new(sandbox.clone());
    let cancel = CancellationToken::new();
    cancel.cancel();
    let out = tool
        .execute(json!({"pattern": "needle"}), cancel)
        .await
        .unwrap();
    assert!(immediate_output(&out).contains("cancelled"));
}

#[tokio::test]
async fn search_files_missing_pattern_is_error() {
    let (_dir, sandbox) = fixture();
    let tool = SearchFiles::new(sandbox.clone());
    let result = tool.execute(json!({}), CancellationToken::new()).await;
    assert!(result.is_err(), "missing pattern must be a hard error");
}

#[test]
fn search_files_metadata() {
    let (_dir, sandbox) = fixture();
    let tool = SearchFiles::new(sandbox.clone());
    assert_eq!(tool.name().to_string(), "search_files");
    assert!(matches!(tool.risk(), ToolRisk::Read));
    let schema = tool.parameters_schema();
    assert!(
        schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("pattern"))
    );
}

// ── find_files ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn find_files_glob_matches_nested_files() {
    let (_dir, sandbox) = fixture();
    let tool = FindFiles::new(sandbox.clone());
    let out = tool
        .execute(json!({"pattern": "*.rs"}), CancellationToken::new())
        .await
        .unwrap();
    let text = immediate_output(&out);
    assert!(text.contains("src/main.rs"), "got:\n{text}");
    assert!(text.contains("src/lib.rs"), "got:\n{text}");
    // Hidden and gitignored files must not appear.
    assert!(!text.contains(".hidden.rs"), "hidden leaked:\n{text}");
    assert!(!text.contains("ignored_dir"), "gitignored leaked:\n{text}");
}

#[tokio::test]
async fn find_files_exact_name() {
    let (_dir, sandbox) = fixture();
    let tool = FindFiles::new(sandbox.clone());
    let out = tool
        .execute(json!({"pattern": ".gitignore"}), CancellationToken::new())
        .await
        .unwrap();
    let text = immediate_output(&out);
    // .gitignore itself is a hidden file and is skipped by the walker — this
    // asserts the documented behavior (hidden skipped) rather than fighting it.
    assert!(text.contains("No files matching"), "got:\n{text}");
}

#[tokio::test]
async fn find_files_substring_pattern_in_name_only() {
    let (_dir, sandbox) = fixture();
    let tool = FindFiles::new(sandbox.clone());
    let out = tool
        .execute(json!({"pattern": "*main*"}), CancellationToken::new())
        .await
        .unwrap();
    let text = immediate_output(&out);
    assert!(text.contains("main.rs"), "got:\n{text}");
    // A file whose *content* mentions 'main' but whose name doesn't must
    // not match (pattern applies to names only).
    assert!(
        !text.contains("lib.rs"),
        "name-only matching violated:\n{text}"
    );
}

#[tokio::test]
async fn find_files_path_scoping() {
    let (_dir, sandbox) = fixture();
    let tool = FindFiles::new(sandbox.clone());
    let out = tool
        .execute(
            json!({"pattern": "*.md", "path": "docs"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let text = immediate_output(&out);
    assert!(text.contains("readme.md"), "got:\n{text}");
    assert!(
        !text.lines().any(|l| std::path::Path::new(l)
            .extension()
            .is_some_and(|e| e == "rs")),
        "scope not applied:\n{text}"
    );
}

#[tokio::test]
async fn find_files_truncation_notice() {
    let (_dir, sandbox) = fixture();
    let tool = FindFiles::new(sandbox.clone());
    // Every file matches '*'; cap at 2 (only 6 files exist but the cap is
    // what we're testing, and .gitignore/binary.bin/hidden are excluded so
    // 4 candidates exist: main.rs, lib.rs, readme.md, and… binary.bin is not
    // hidden so it counts).
    let out = tool
        .execute(
            json!({"pattern": "*", "max_results": 2}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let text = immediate_output(&out);
    assert!(text.contains("truncated"), "notice missing:\n{text}");
    let listed: Vec<&str> = text.lines().filter(|l| !l.starts_with('…')).collect();
    assert_eq!(listed.len(), 2, "cap not honored:\n{text}");
}

#[tokio::test]
async fn find_files_rejects_path_outside_sandbox() {
    let (_dir, sandbox) = fixture();
    let tool = FindFiles::new(sandbox.clone());
    let out = tool
        .execute(
            json!({"pattern": "*.rs", "path": "/etc"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(immediate_is_error(&out));
}

#[tokio::test]
async fn find_files_cancelled_token_returns_error() {
    let (_dir, sandbox) = fixture();
    let tool = FindFiles::new(sandbox.clone());
    let cancel = CancellationToken::new();
    cancel.cancel();
    let out = tool
        .execute(json!({"pattern": "*.rs"}), cancel)
        .await
        .unwrap();
    assert!(immediate_is_error(&out));
    assert!(immediate_output(&out).contains("cancelled"));
}

#[test]
fn find_files_metadata() {
    let (_dir, sandbox) = fixture();
    let tool = FindFiles::new(sandbox.clone());
    assert_eq!(tool.name().to_string(), "find_files");
    assert!(matches!(tool.risk(), ToolRisk::Read));
    let schema = tool.parameters_schema();
    assert!(
        schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("pattern"))
    );
}

#[tokio::test]
async fn search_files_truncation_notice_fires_when_cap_hit_at_last_file() {
    // Regression: the walk used to `break` once the cap was reached, so if the
    // directory iteration order put the last matching file *before* others,
    // no overflow match was ever observed and the "truncated" notice never
    // fired — a filesystem-order-dependent flake (failed on CI, passed
    // locally). The fix keeps walking (discarding results past the cap) so
    // the notice depends on the data, not the readdir order. This test pins
    // the invariant: cap N, more-than-N total matches across files ⇒ notice
    // present regardless of which file fills the cap.
    let (dir, sandbox) = tempdir_with_sandbox();
    let root = sandbox.path();
    // Three files, each with exactly one match. Cap = 3 ⇒ whichever file is
    // visited last still has its match observed as overflow… unless total
    // matches == cap exactly. Use FOUR files with one match each, cap = 3:
    // any iteration order leaves ≥1 overflow match.
    for name in ["a.txt", "b.txt", "c.txt", "d.txt"] {
        std::fs::write(root.join(name), "needle\n").unwrap();
    }
    let tool = SearchFiles::new(sandbox.clone());
    let out = tool
        .execute(
            json!({"pattern": "needle", "max_results": 3}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let text = immediate_output(&out);
    let match_lines: Vec<&str> = text.lines().filter(|l| !l.starts_with('…')).collect();
    assert_eq!(match_lines.len(), 3, "cap exceeded:\n{text}");
    assert!(text.contains("truncated"), "notice missing:\n{text}");
    // And the exact-cap case must NOT show a notice (no overflow anywhere).
    let dir2 = tempfile::tempdir().unwrap();
    let sandbox2 = SandboxRoot::new(dunce::canonicalize(dir2.path()).unwrap()).unwrap();
    std::fs::create_dir_all(sandbox2.path()).unwrap();
    for name in ["x.txt", "y.txt"] {
        std::fs::write(sandbox2.path().join(name), "needle\n").unwrap();
    }
    let tool2 = SearchFiles::new(sandbox2.clone());
    let out2 = tool2
        .execute(
            json!({"pattern": "needle", "max_results": 2}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let text2 = immediate_output(&out2);
    assert!(
        !text2.contains("truncated"),
        "notice shown with no overflow:\n{text2}"
    );
    let _ = dir;
}
