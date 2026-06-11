//! Grammar selection via the [`Language`] enum.
//!
//! Each variant corresponds to a Cargo feature that compiles a tree-sitter
//! grammar into the binary. Calling [`Language::tree_sitter_language`] returns
//! the underlying `tree_sitter::Language` needed to configure a parser, or an
//! error if the grammar is not available.

use crate::error::HighlightError;

/// A programming language supported by `rho-highlight`.
///
/// Variants gated behind optional features return
/// [`HighlightError::GrammarNotAvailable`] when their feature is not enabled.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Language {
    /// Rust — default feature `rust`.
    Rust,
    /// PowerShell — planned.
    PowerShell,
    /// TOML — planned.
    Toml,
    /// JSON — planned.
    Json,
    /// Markdown — planned.
    Markdown,
}

impl Language {
    /// Return the human-readable name of this language.
    pub fn name(self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::PowerShell => "PowerShell",
            Self::Toml => "TOML",
            Self::Json => "JSON",
            Self::Markdown => "Markdown",
        }
    }

    /// Return the `tree_sitter::Language` for this grammar.
    ///
    /// # Errors
    ///
    /// Returns [`HighlightError::GrammarNotAvailable`] when the corresponding
    /// Cargo feature is not enabled or the grammar has not yet been added.
    pub fn tree_sitter_language(self) -> Result<tree_sitter::Language, HighlightError> {
        match self {
            #[cfg(feature = "rust")]
            Self::Rust => Ok(tree_sitter_rust::LANGUAGE.into()),

            _ => Err(HighlightError::GrammarNotAvailable(self.name().to_owned())),
        }
    }
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_language_available_with_feature() {
        // The `rust` feature is on by default; this must succeed.
        let result = Language::Rust.tree_sitter_language();
        assert!(
            result.is_ok(),
            "Rust grammar should be available with the default `rust` feature"
        );
    }

    #[test]
    fn unsupported_languages_return_error() {
        for lang in [
            Language::PowerShell,
            Language::Toml,
            Language::Json,
            Language::Markdown,
        ] {
            let result = lang.tree_sitter_language();
            assert!(
                result.is_err(),
                "{lang} grammar should not be available without its feature"
            );
            assert!(
                matches!(result.unwrap_err(), HighlightError::GrammarNotAvailable(_)),
                "error should be GrammarNotAvailable for {lang}"
            );
        }
    }

    #[test]
    fn language_names_are_stable() {
        assert_eq!(Language::Rust.name(), "Rust");
        assert_eq!(Language::PowerShell.name(), "PowerShell");
        assert_eq!(Language::Toml.name(), "TOML");
        assert_eq!(Language::Json.name(), "JSON");
        assert_eq!(Language::Markdown.name(), "Markdown");
    }

    #[test]
    fn language_display_matches_name() {
        assert_eq!(Language::Rust.to_string(), Language::Rust.name());
    }

    #[test]
    fn language_eq_and_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(Language::Rust);
        set.insert(Language::Rust);
        assert_eq!(set.len(), 1, "duplicate insertions should collapse");
        set.insert(Language::Toml);
        assert_eq!(set.len(), 2);
    }
}
