//! Extension discovery — scan directories for TypeScript extension files.
//!
//! Finds extensions in two locations:
//! - **User-level:** `~/.rho/extensions/`
//! - **Project-local:** `<project-root>/.rho/extensions/`
//!
//! An extension is either:
//! - A single `*.ts` file (e.g. `~/.rho/extensions/crates_search.ts`)
//! - A directory with a `mod.ts` entry point (e.g. `~/.rho/extensions/rust_docs/mod.ts`)
//!
//! Project-local extensions take precedence over user-level ones with the same
//! name (name derived from filename or directory name).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rho_core::config::ExtensionConfig;

/// A discovered extension on disk.
#[derive(Debug, Clone)]
pub struct DiscoveredExtension {
    /// Extension name derived from the filename or directory name.
    ///
    /// - `crates_search.ts` → `"crates_search"`
    /// - `rust_docs/mod.ts` → `"rust_docs"`
    pub name: String,
    /// Path to the entry point TypeScript file.
    pub entry_path: PathBuf,
    /// Root directory for the extension (for import sandboxing).
    ///
    /// - Single-file: parent directory of the `.ts` file
    /// - Multi-file: the directory containing `mod.ts`
    pub root_dir: PathBuf,
}

/// Scan the given extension directories for TypeScript extensions.
///
/// Directories that don't exist are silently skipped. Returns discovered
/// extensions in the order they were found.
///
/// # Errors
///
/// Returns an error only if a directory exists but cannot be read (I/O error).
pub fn discover(dirs: &[PathBuf]) -> Result<Vec<DiscoveredExtension>, std::io::Error> {
    let mut extensions = Vec::new();
    for dir in dirs {
        if dir.is_dir() {
            scan_directory(dir, &mut extensions)?;
        }
    }
    Ok(extensions)
}

/// Deduplicate discovered extensions by name, keeping the last occurrence.
///
/// When scanning multiple directories (user-level then project-local),
/// project-local extensions should override user-level ones. Since `discover`
/// returns extensions in scan order, keeping the last occurrence of each name
/// gives the correct precedence.
pub fn deduplicate(extensions: Vec<DiscoveredExtension>) -> Vec<DiscoveredExtension> {
    let mut seen: HashMap<String, DiscoveredExtension> = HashMap::new();
    for ext in extensions {
        seen.insert(ext.name.clone(), ext);
    }
    seen.into_values().collect()
}

/// Derive an extension name from a path.
///
/// - For a file (`crates_search.ts`): returns `"crates_search"` (stem)
/// - For a directory (`rust_docs`): returns `"rust_docs"` (dir name)
pub fn name_from_path(path: &Path) -> String {
    if path.is_file() || path.extension().is_some() {
        // It's a file — use the stem (filename without extension)
        path.file_stem().map_or_else(
            || path.to_string_lossy().to_string(),
            |s| s.to_string_lossy().to_string(),
        )
    } else {
        // It's a directory — use the directory name
        path.file_name().map_or_else(
            || path.to_string_lossy().to_string(),
            |s| s.to_string_lossy().to_string(),
        )
    }
}

/// Scan a single extension directory for `.ts` files and `*/mod.ts` directories.
///
/// Found extensions are appended to `out`.
fn scan_directory(dir: &Path, out: &mut Vec<DiscoveredExtension>) -> Result<(), std::io::Error> {
    let entries = std::fs::read_dir(dir)?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();

        // Skip hidden files/dirs (e.g. .gitkeep)
        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy();
        if name_str.starts_with('.') {
            continue;
        }

        if path.is_file() {
            // Single-file extension: *.ts in the extensions directory
            if path.extension().is_some_and(|ext| ext == "ts") {
                let name = name_from_path(&path);
                out.push(DiscoveredExtension {
                    name,
                    entry_path: path.clone(),
                    root_dir: dir.to_path_buf(),
                });
            }
        } else if path.is_dir() {
            // Multi-file extension: directory with a mod.ts
            let mod_path = path.join("mod.ts");
            if mod_path.is_file() {
                let name = name_from_path(&path);
                out.push(DiscoveredExtension {
                    name,
                    entry_path: mod_path,
                    root_dir: path,
                });
            }
        }
    }

    Ok(())
}

// ── Config-based filtering ────────────────────────────────────────────────────

/// Filter discovered extensions by config.
///
/// Uses [`ExtensionConfig::is_enabled`] to decide which extensions to keep.
/// Extensions that are disabled are dropped silently.
///
/// This should be called after `discover` and `deduplicate`.
///
/// # Example
///
/// ```ignore
/// let dirs = vec![user_extensions_dir(), project_extensions_dir(project_root)];
/// let discovered = discover(&dirs)?;
/// let deduped = deduplicate(discovered);
/// let filtered = filter_by_config(&deduped, &config.extensions);
/// ```
pub fn filter_by_config(
    extensions: &[DiscoveredExtension],
    config: &ExtensionConfig,
) -> Vec<DiscoveredExtension> {
    extensions
        .iter()
        .filter(|ext| config.is_enabled(&ext.name))
        .cloned()
        .collect()
}

/// Resolve the effective permissions for each discovered extension.
///
/// Returns an iterator of `(DiscoveredExtension, ExtensionPermissions)` pairs,
/// one per extension in the input.
///
/// This should be called after `filter_by_config`.
pub fn resolve_permissions<'a>(
    extensions: &'a [DiscoveredExtension],
    config: &ExtensionConfig,
) -> Vec<(DiscoveredExtension, rho_core::config::ExtensionPermissions)> {
    extensions
        .iter()
        .map(|ext| (ext.clone(), config.permissions_for(&ext.name)))
        .collect()
}

// ── Standard extension directories ────────────────────────────────────────────

/// Return the standard user-level extension directory (`~/.rho/extensions/`).
///
/// Does not check if the directory exists.
pub fn user_extensions_dir() -> PathBuf {
    dirs_home().join(".rho").join("extensions")
}

/// Return the project-local extension directory (`<project>/.rho/extensions/`).
///
/// Does not check if the directory exists.
pub fn project_extensions_dir(project_root: &Path) -> PathBuf {
    project_root.join(".rho").join("extensions")
}

/// Best-effort home directory.
fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_or_else(|_| PathBuf::from("."), PathBuf::from)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a temp dir with the following structure:
    ///
    /// ```text
    /// <dir>/
    /// ├── hello.ts
    /// ├── ignore.txt
    /// ├── .hidden.ts
    /// ├── multi_file/
    /// │   ├── mod.ts
    /// │   └── helper.ts
    /// ├── no_mod/
    /// │   └── other.ts        <- no mod.ts, should be skipped
    /// └── another.ts
    /// ```
    fn setup_test_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();

        // Single-file extensions
        std::fs::write(dir.path().join("hello.ts"), "// hello").unwrap();
        std::fs::write(dir.path().join("another.ts"), "// another").unwrap();

        // Non-TS file — should be ignored
        std::fs::write(dir.path().join("ignore.txt"), "not an extension").unwrap();

        // Hidden file — should be ignored
        std::fs::write(dir.path().join(".hidden.ts"), "// hidden").unwrap();

        // Multi-file extension with mod.ts
        let multi = dir.path().join("multi_file");
        std::fs::create_dir_all(&multi).unwrap();
        std::fs::write(multi.join("mod.ts"), "// mod entry").unwrap();
        std::fs::write(multi.join("helper.ts"), "// helper").unwrap();

        // Directory without mod.ts — should be skipped
        let no_mod = dir.path().join("no_mod");
        std::fs::create_dir_all(&no_mod).unwrap();
        std::fs::write(no_mod.join("other.ts"), "// no mod").unwrap();

        dir
    }

    #[test]
    fn discovers_single_file_extensions() {
        let dir = setup_test_dir();
        let exts = discover(&[dir.path().to_path_buf()]).unwrap();

        let names: Vec<&str> = exts.iter().map(|e| e.name.as_str()).collect();
        assert!(
            names.contains(&"hello"),
            "should find hello.ts, got: {names:?}"
        );
        assert!(
            names.contains(&"another"),
            "should find another.ts, got: {names:?}"
        );
    }

    #[test]
    fn discovers_multi_file_extensions() {
        let dir = setup_test_dir();
        let exts = discover(&[dir.path().to_path_buf()]).unwrap();

        let multi = exts.iter().find(|e| e.name == "multi_file");
        assert!(multi.is_some(), "should find multi_file extension");

        let multi = multi.unwrap();
        assert!(multi.entry_path.ends_with("multi_file/mod.ts"));
        assert!(multi.root_dir.ends_with("multi_file"));
    }

    #[test]
    fn ignores_non_ts_files() {
        let dir = setup_test_dir();
        let exts = discover(&[dir.path().to_path_buf()]).unwrap();

        let names: Vec<&str> = exts.iter().map(|e| e.name.as_str()).collect();
        assert!(
            !names.contains(&"ignore"),
            "should not find .txt files, got: {names:?}"
        );
    }

    #[test]
    fn ignores_hidden_files() {
        let dir = setup_test_dir();
        let exts = discover(&[dir.path().to_path_buf()]).unwrap();

        let names: Vec<&str> = exts.iter().map(|e| e.name.as_str()).collect();
        assert!(
            !names.contains(&"hidden"),
            "should not find hidden files, got: {names:?}"
        );
    }

    #[test]
    fn skips_directories_without_mod_ts() {
        let dir = setup_test_dir();
        let exts = discover(&[dir.path().to_path_buf()]).unwrap();

        let names: Vec<&str> = exts.iter().map(|e| e.name.as_str()).collect();
        assert!(
            !names.contains(&"no_mod"),
            "should skip dirs without mod.ts, got: {names:?}"
        );
    }

    #[test]
    fn single_file_root_is_parent_directory() {
        let dir = setup_test_dir();
        let exts = discover(&[dir.path().to_path_buf()]).unwrap();

        let hello = exts.iter().find(|e| e.name == "hello").unwrap();
        assert_eq!(hello.root_dir, dir.path());
        assert_eq!(hello.entry_path, dir.path().join("hello.ts"));
    }

    #[test]
    fn multi_file_root_is_extension_directory() {
        let dir = setup_test_dir();
        let exts = discover(&[dir.path().to_path_buf()]).unwrap();

        let multi = exts.iter().find(|e| e.name == "multi_file").unwrap();
        assert_eq!(multi.root_dir, dir.path().join("multi_file"));
        assert_eq!(multi.entry_path, dir.path().join("multi_file/mod.ts"));
    }

    #[test]
    fn skips_nonexistent_directories() {
        let exts = discover(&[PathBuf::from("/no/such/directory")]).unwrap();
        assert!(exts.is_empty());
    }

    #[test]
    fn empty_dirs_ok() {
        let dir = tempfile::tempdir().unwrap();
        let exts = discover(&[dir.path().to_path_buf()]).unwrap();
        assert!(exts.is_empty());
    }

    #[test]
    fn deduplicate_keeps_last() {
        let dir1 = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();

        std::fs::write(dir1.path().join("search.ts"), "// v1").unwrap();
        std::fs::write(dir2.path().join("search.ts"), "// v2").unwrap();

        let exts = discover(&[dir1.path().to_path_buf(), dir2.path().to_path_buf()]).unwrap();

        let deduped = deduplicate(exts);
        assert_eq!(deduped.len(), 1);
        // Should keep the last one (dir2)
        let content = std::fs::read_to_string(&deduped[0].entry_path).unwrap();
        assert_eq!(content, "// v2");
    }

    #[test]
    fn deduplicate_preserves_unique_names() {
        let dir = setup_test_dir();
        let exts = discover(&[dir.path().to_path_buf()]).unwrap();
        let deduped = deduplicate(exts);

        let names: Vec<&str> = deduped.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"hello"));
        assert!(names.contains(&"another"));
        assert!(names.contains(&"multi_file"));
    }

    #[test]
    fn name_from_file_path() {
        let path = PathBuf::from("/extensions/crates_search.ts");
        assert_eq!(name_from_path(&path), "crates_search");
    }

    #[test]
    fn name_from_directory() {
        let path = PathBuf::from("/extensions/rust_docs");
        assert_eq!(name_from_path(&path), "rust_docs");
    }

    #[test]
    fn user_extensions_dir_path() {
        let dir = user_extensions_dir();
        assert!(dir.to_string_lossy().contains(".rho"));
        assert!(dir.to_string_lossy().contains("extensions"));
    }

    #[test]
    fn project_extensions_dir_path() {
        let dir = project_extensions_dir(Path::new("/my/project"));
        assert_eq!(dir, PathBuf::from("/my/project/.rho/extensions"));
    }

    // ── Config filtering tests ────────────────────────────────────────────

    fn make_ext(name: &str) -> DiscoveredExtension {
        DiscoveredExtension {
            name: name.to_string(),
            entry_path: PathBuf::from(format!("/{name}.ts")),
            root_dir: PathBuf::from("/extensions"),
        }
    }

    #[test]
    fn filter_allows_all_when_no_config() {
        let config = ExtensionConfig::default();
        let exts = vec![make_ext("a"), make_ext("b"), make_ext("c")];
        let filtered = filter_by_config(&exts, &config);
        assert_eq!(filtered.len(), 3);
    }

    #[test]
    fn filter_respects_enabled_list() {
        let config = ExtensionConfig {
            enabled: vec!["a".into(), "c".into()],
            ..Default::default()
        };
        let exts = vec![make_ext("a"), make_ext("b"), make_ext("c")];
        let filtered = filter_by_config(&exts, &config);
        let names: Vec<&str> = filtered.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a", "c"]);
    }

    #[test]
    fn filter_respects_disabled_list() {
        let config = ExtensionConfig {
            disabled: vec!["b".into()],
            ..Default::default()
        };
        let exts = vec![make_ext("a"), make_ext("b"), make_ext("c")];
        let filtered = filter_by_config(&exts, &config);
        let names: Vec<&str> = filtered.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a", "c"]);
    }

    #[test]
    fn filter_disabled_overrides_enabled() {
        let config = ExtensionConfig {
            enabled: vec!["a".into(), "b".into()],
            disabled: vec!["b".into()],
            ..Default::default()
        };
        let exts = vec![make_ext("a"), make_ext("b")];
        let filtered = filter_by_config(&exts, &config);
        let names: Vec<&str> = filtered.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a"]);
    }

    #[test]
    fn filter_empty_exts() {
        let config = ExtensionConfig {
            enabled: vec!["a".into()],
            ..Default::default()
        };
        let filtered = filter_by_config(&[], &config);
        assert!(filtered.is_empty());
    }

    #[test]
    fn resolve_permissions_applies_defaults() {
        let config = ExtensionConfig {
            defaults: rho_core::config::ExtensionPermissions {
                network: Some(true),
                max_memory_mb: Some(64),
                ..Default::default()
            },
            ..Default::default()
        };
        let exts = vec![make_ext("a"), make_ext("b")];
        let resolved = resolve_permissions(&exts, &config);

        assert_eq!(resolved.len(), 2);
        for (_, perms) in &resolved {
            assert_eq!(perms.network, Some(true));
            assert_eq!(perms.max_memory_mb, Some(64));
        }
    }

    #[test]
    fn resolve_permissions_per_extension_override() {
        let config = ExtensionConfig {
            defaults: rho_core::config::ExtensionPermissions {
                network: Some(true),
                max_memory_mb: Some(64),
                ..Default::default()
            },
            per_extension: {
                let mut map = std::collections::HashMap::new();
                map.insert(
                    "a".into(),
                    rho_core::config::ExtensionPermissions {
                        max_memory_mb: Some(256),
                        ..Default::default()
                    },
                );
                map
            },
            ..Default::default()
        };
        let exts = vec![make_ext("a"), make_ext("b")];
        let resolved = resolve_permissions(&exts, &config);

        // "a" gets per-extension override
        let perms_a = resolved.iter().find(|(e, _)| e.name == "a").unwrap().1.clone();
        assert_eq!(perms_a.network, Some(true)); // inherited from defaults
        assert_eq!(perms_a.max_memory_mb, Some(256)); // overridden

        // "b" gets only defaults
        let perms_b = resolved.iter().find(|(e, _)| e.name == "b").unwrap().1.clone();
        assert_eq!(perms_b.network, Some(true));
        assert_eq!(perms_b.max_memory_mb, Some(64));
    }
}
