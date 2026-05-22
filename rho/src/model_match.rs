//! Fuzzy matching for model identifiers.
//!
//! Provides string similarity scoring so that when a user specifies a model
//! that doesn't exist on any provider, we can suggest likely alternatives.

/// Compute a normalized Levenshtein similarity between two strings.
///
/// Returns a value in `[0.0, 1.0]` where `1.0` means identical. The similarity
/// is computed as `1 - (edit_distance / max_len)` so that short mismatches on
/// short strings and long mismatches on long strings are scored comparably.
fn normalized_levenshtein(a: &str, b: &str) -> f64 {
    let a_lower = a.to_lowercase();
    let b_lower = b.to_lowercase();

    let max_len = a_lower.len().max(b_lower.len());
    if max_len == 0 {
        return 1.0;
    }

    let dist = u32::try_from(levenshtein(&a_lower, &b_lower)).unwrap_or(u32::MAX);
    let max = u32::try_from(max_len).unwrap_or(u32::MAX);
    1.0 - (f64::from(dist) / f64::from(max))
}

/// Standard Levenshtein edit distance between two strings.
///
/// Uses the classic Wagner–Fischer dynamic programming algorithm.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let m = a.len();
    let n = b.len();

    // Single-row optimisation: only keep the previous row.
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];

    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[n]
}

/// A candidate model match with a similarity score.
#[derive(Debug)]
pub struct ModelCandidate<'a> {
    /// The provider name that has this model.
    pub provider_name: &'a str,
    /// The model identifier.
    pub model_id: &'a str,
    /// Normalized similarity score in `[0.0, 1.0]`.
    pub score: f64,
}

/// Find the best fuzzy matches for a query across a list of available models.
///
/// Returns candidates sorted by similarity score descending (best first).
/// Candidates must meet the minimum `threshold` score to be included.
///
/// # Arguments
///
/// * `query` — the model identifier the user specified
/// * `available` — `(provider_name, model_id)` pairs from all providers
/// * `threshold` — minimum similarity to include (typically `0.5` to `0.6`)
pub fn fuzzy_match<'a>(
    query: &str,
    available: &'a [(&'a str, String)],
    threshold: f64,
) -> Vec<ModelCandidate<'a>> {
    let mut candidates: Vec<ModelCandidate<'a>> = available
        .iter()
        .filter_map(|(provider_name, model_id)| {
            let score = normalized_levenshtein(query, model_id);
            if score >= threshold {
                Some(ModelCandidate {
                    provider_name,
                    model_id,
                    score,
                })
            } else {
                None
            }
        })
        .collect();

    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    candidates
}

/// Find an exact match for a model query across available models.
///
/// Returns the first exact match (case-sensitive). If no exact match is
/// found, returns `None`.
pub fn find_exact<'a>(
    query: &str,
    available: &'a [(&'a str, String)],
) -> Option<(&'a str, &'a str)> {
    available
        .iter()
        .find(|(_, model_id)| model_id == query)
        .map(|(provider_name, model_id)| (*provider_name, model_id.as_str()))
}

/// Format a list of fuzzy suggestions as a human-readable string.
///
/// Returns a string like:
///
/// ```text
/// qwen3-8b (local)
/// qwen3-32b (local)
/// ```
pub fn format_suggestions(candidates: &[ModelCandidate], max: usize) -> String {
    candidates
        .iter()
        .take(max)
        .map(|c| format!("  {} (provider: {})", c.model_id, c.provider_name))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levenshtein_identical() {
        assert_eq!(levenshtein("hello", "hello"), 0);
    }

    #[test]
    fn levenshtein_empty() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("", "abc"), 3);
    }

    #[test]
    fn levenshtein_basic() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("flaw", "lawn"), 2);
        assert_eq!(levenshtein("a", "b"), 1);
    }

    #[test]
    fn normalized_levenshtein_identical() {
        assert!((normalized_levenshtein("hello", "hello") - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn normalized_levenshtein_empty() {
        assert!((normalized_levenshtein("", "") - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn normalized_levenshtein_one_edit_short() {
        let score = normalized_levenshtein("abc", "adc");
        // 1 edit out of max_len 3 → 1 - 1/3 ≈ 0.667
        assert!((score - (2.0 / 3.0)).abs() < f64::EPSILON);
    }

    #[test]
    fn normalized_levenshtein_case_insensitive() {
        let score = normalized_levenshtein("ABC", "abc");
        assert!((score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn fuzzy_match_exact_hit() {
        let available: Vec<(&str, String)> = vec![
            ("local", "qwen3-8b".to_owned()),
            ("local", "llama3-70b".to_owned()),
        ];
        let matches = fuzzy_match("qwen3-8b", &available, 0.5);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].model_id, "qwen3-8b");
        assert!((matches[0].score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn fuzzy_match_near_miss() {
        let available: Vec<(&str, String)> = vec![
            ("local", "qwen3-8b".to_owned()),
            ("local", "llama3-70b".to_owned()),
        ];
        let matches = fuzzy_match("qwen3-8", &available, 0.5);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].model_id, "qwen3-8b");
        assert!(matches[0].score > 0.8);
    }

    #[test]
    fn fuzzy_match_no_results_below_threshold() {
        let available: Vec<(&str, String)> = vec![
            ("local", "qwen3-8b".to_owned()),
            ("local", "llama3-70b".to_owned()),
        ];
        let matches = fuzzy_match("gpt-4o", &available, 0.6);
        assert!(matches.is_empty());
    }

    #[test]
    fn fuzzy_match_sorted_by_score() {
        let available: Vec<(&str, String)> = vec![
            ("local", "qwen3-32b".to_owned()),
            ("local", "qwen3-8b".to_owned()),
        ];
        let matches = fuzzy_match("qwen3-8b", &available, 0.5);
        assert_eq!(matches.len(), 2);
        // qwen3-8b is exact match, should be first.
        assert_eq!(matches[0].model_id, "qwen3-8b");
        // qwen3-32b is less similar, should be second.
        assert_eq!(matches[1].model_id, "qwen3-32b");
    }

    #[test]
    fn find_exact_hit() {
        let available: Vec<(&str, String)> = vec![
            ("local", "qwen3-8b".to_owned()),
            ("remote", "gpt-4o".to_owned()),
        ];
        assert_eq!(
            find_exact("qwen3-8b", &available),
            Some(("local", "qwen3-8b"))
        );
    }

    #[test]
    fn find_exact_miss() {
        let available: Vec<(&str, String)> = vec![("local", "qwen3-8b".to_owned())];
        assert_eq!(find_exact("qwen3-8", &available), None);
    }

    #[test]
    fn find_exact_case_sensitive() {
        let available: Vec<(&str, String)> = vec![("local", "Qwen3-8B".to_owned())];
        assert_eq!(find_exact("qwen3-8b", &available), None);
    }

    #[test]
    fn format_suggestions_truncates() {
        let candidates = vec![
            ModelCandidate {
                provider_name: "local",
                model_id: "qwen3-8b",
                score: 1.0,
            },
            ModelCandidate {
                provider_name: "local",
                model_id: "qwen3-32b",
                score: 0.7,
            },
            ModelCandidate {
                provider_name: "remote",
                model_id: "gpt-4o",
                score: 0.3,
            },
        ];
        let formatted = format_suggestions(&candidates, 2);
        assert_eq!(
            formatted,
            "  qwen3-8b (provider: local)\n  qwen3-32b (provider: local)"
        );
    }
}
