//! Result persistence — save benchmark results to disk as JSON.

use std::path::Path;

use anyhow::{Context, Result};
use rho_eval::EvalRun;

/// Save benchmark results to the given directory.
///
/// Creates the directory if it doesn't exist. Writes:
/// - `latest.json` — the most recent run results (overwritten each time)
/// - `<timestamp>.json` — a timestamped archive
pub fn save_results(runs: &[EvalRun], results_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(results_dir)
        .with_context(|| format!("cannot create results directory {}", results_dir.display()))?;

    let json =
        serde_json::to_string_pretty(runs).context("failed to serialize benchmark results")?;

    // Always write `latest.json` for easy access.
    let latest_path = results_dir.join("latest.json");
    std::fs::write(&latest_path, &json)
        .with_context(|| format!("cannot write {}", latest_path.display()))?;

    // Write a timestamped file for history.
    let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let archive_path = results_dir.join(format!("{timestamp}.json"));
    std::fs::write(&archive_path, &json)
        .with_context(|| format!("cannot write {}", archive_path.display()))?;

    eprintln!("results saved to: {}", latest_path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_results_creates_files() {
        let dir = std::env::temp_dir().join("rho-bench-test-save");
        let _ = std::fs::remove_dir_all(&dir);

        let run = EvalRun::new("base", "composed").with_model("test-model");
        save_results(&[run], &dir).unwrap();

        assert!(dir.join("latest.json").exists());
        // There should be at least one timestamped file.
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .map(|e| e.path())
            .collect();
        assert!(entries.len() >= 2);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
