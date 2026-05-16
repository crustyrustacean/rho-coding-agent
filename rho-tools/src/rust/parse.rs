//! NDJSON parsing for cargo `--message-format=json` output.

use std::path::Path;

use rho_core::Diagnostic;
use serde_json;

use super::convert::convert_diagnostic;
use super::types::CargoMessage;

/// Parse cargo `--message-format=json` NDJSON output into structured diagnostics.
///
/// Filters to only `compiler-message` lines whose target source path lies
/// within `workspace_root`. This removes dependency noise — diagnostics from
/// crates in `~/.cargo/registry` or vendored paths outside the workspace.
///
/// Lines that fail to parse as JSON are silently skipped (cargo sometimes
/// emits non-JSON progress lines to stdout).
pub fn parse_cargo_diagnostics(ndjson: &str, workspace_root: &Path) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    for line in ndjson.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let Ok(msg) = serde_json::from_str::<CargoMessage>(line) else {
            continue;
        };

        if msg.reason != "compiler-message" {
            continue;
        }

        let Some(raw_diag) = &msg.message else {
            continue;
        };

        // Filter: skip diagnostics from outside the workspace.
        if let Some(target) = &msg.target
            && !target.src_path.is_empty()
            && !path_is_within(workspace_root, &target.src_path)
        {
            continue;
        }

        // Skip summary diagnostics with no actionable content.
        if raw_diag.level == "failure-note"
            || (raw_diag.code.is_none()
                && raw_diag.spans.is_empty()
                && raw_diag.message.starts_with("aborting due to"))
        {
            continue;
        }

        diagnostics.push(convert_diagnostic(raw_diag));
    }

    diagnostics
}

/// Check if `candidate` path is within `root`.
///
/// Uses simple string prefix matching after normalizing separators.
/// This avoids canonicalization (which requires the path to exist on disk).
fn path_is_within(root: &Path, candidate: &str) -> bool {
    let root_str = root.to_string_lossy().replace('\\', "/");
    let candidate_normalized = candidate.replace('\\', "/");
    candidate_normalized.starts_with(&root_str)
}
