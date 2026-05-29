//! TypeScript → JavaScript transpilation via `deno_ast`.

use deno_ast::{EmitOptions, MediaType, ParseParams, TranspileModuleOptions, TranspileOptions};
use url::Url;

/// Transpile a TypeScript source string to JavaScript.
///
/// Uses `deno_ast` with default transpilation options. No type checking is
/// performed — only syntax stripping and ES feature down-leveling.
///
/// # Errors
///
/// Returns an error if the source cannot be parsed or transpiled.
pub fn transpile(specifier: &Url, source: &str) -> Result<String, String> {
    let parsed = deno_ast::parse_module(ParseParams {
        specifier: specifier.clone(),
        text: source.into(),
        media_type: MediaType::TypeScript,
        capture_tokens: false,
        scope_analysis: false,
        maybe_syntax: None,
    })
    .map_err(|e| format!("{e}"))?;

    let transpiled = parsed
        .transpile(
            &TranspileOptions::default(),
            &TranspileModuleOptions::default(),
            &EmitOptions::default(),
        )
        .map_err(|e| format!("{e}"))?;

    Ok(transpiled.into_source().text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transpile_strips_types() {
        let ts = r"export async function greet(name: string): Promise<string> { return `Hello, ${name}!`; }";
        let js = transpile(&Url::parse("file:///test.ts").unwrap(), ts).unwrap();
        // TypeScript annotations should be gone
        assert!(!js.contains(": string"));
        assert!(js.contains("Hello"));
    }

    #[test]
    fn transpile_rejects_invalid_syntax() {
        let bad = r"export function {{{(";
        let result = transpile(&Url::parse("file:///bad.ts").unwrap(), bad);
        assert!(result.is_err());
    }

    #[test]
    fn transpile_preserves_async() {
        let ts = r#"export async function work(): Promise<string> { return "done"; }"#;
        let js = transpile(&Url::parse("file:///test.ts").unwrap(), ts).unwrap();
        assert!(js.contains("async"));
    }
}
