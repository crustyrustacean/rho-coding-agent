//! Rustdoc lookup tool: resolve Rust stdlib queries to local rustdoc HTML.
//!
//! [`RustdocTool`] resolves type names, fully-qualified paths, method queries,
//! and trait names to locally installed rustdoc HTML files shipped by `rustup`.
//! It extracts plain-text documentation and returns it to the model without
//! network access.
//!
//! # Design notes (Phase 3.5)
//!
//! - Doc root is discovered once via `rustup doc --path` using
//!   `std::process::Command` (sync, since `ShellExecutor::execute` is async).
//! - No `ShellExecutor` field is needed: the tool only reads files at runtime.
//! - V1 uses a simple state machine for HTML stripping — zero new crate deps.

use async_trait::async_trait;
use rho_core::tool::{CancellationToken, Tool, ToolOutcome, ToolResult};
use rho_core::{Result, ToolName, ToolRisk};
use std::path::{Path, PathBuf};

// Maximum output size in bytes. Reserved for future truncation use.
#[allow(dead_code, clippy::missing_docs_in_private_items)]
const MAX_OUTPUT_BYTES: usize = 16 * 1024;

// ── ItemKind ──────────────────────────────────────────────────────────────────

/// The kind of rustdoc item, determines the URL prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
#[allow(clippy::missing_docs_in_private_items)]
enum ItemKind {
    Struct,
    Enum,
    Trait,
    Function,
    Macro,
    Primitive,
    Constant,
}

impl ItemKind {
    /// URL prefix used in rustdoc paths (e.g. `"struct"`, `"enum"`).
    fn prefix(self) -> &'static str {
        match self {
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Function => "fn",
            Self::Macro => "macro",
            Self::Primitive => "primitive",
            Self::Constant => "constant",
        }
    }
}

// ── KnownItem ─────────────────────────────────────────────────────────────────

/// A known stdlib item with its crate, module path, and kind.
struct KnownItem {
    /// Crate name (e.g. `"std"`, `"core"`, `"alloc"`).
    krate: &'static str,
    /// Module path relative to the crate root.
    module: &'static str,
    /// Item kind (struct, enum, trait, etc.).
    kind: ItemKind,
}

// ── RustdocTool ───────────────────────────────────────────────────────────────

/// Look up Rust standard library documentation from locally installed rustdoc HTML.
///
/// Resolves type names, fully-qualified paths, method queries, and trait names
/// to the local rustdoc HTML shipped by `rustup`, extracts plain-text
/// documentation, and returns it to the model.
///
/// # Construction
///
/// [`RustdocTool::new`] discovers the rustdoc root by running `rustup doc --path`.
/// It panics if `rustup` is not available. Use [`RustdocTool::with_doc_root`] in
/// tests or when the doc root is known ahead of time.
///
/// # Parameters
///
/// - `query` (required): Item to look up — bare type name (`Vec`), fully-qualified
///   path (`std::collections::HashMap`), or method query (`Option::map`).
/// - `section` (optional, default `"all"`): `all`, `methods`, `traits`,
///   `examples`, `signature`.
pub struct RustdocTool {
    /// Root directory of the locally installed rustdoc HTML.
    doc_root: PathBuf,
}

impl Default for RustdocTool {
    fn default() -> Self {
        Self::new()
    }
}

impl RustdocTool {
    /// Create a new `RustdocTool` by discovering the rustdoc root via `rustup doc --path`.
    ///
    /// # Panics
    ///
    /// Panics if `rustup` is not found on `$PATH` or if the reported doc root
    /// does not exist on disk.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let output = std::process::Command::new("rustup")
            .args(["doc", "--path"])
            .output()
            .expect("rustup not found on $PATH — cannot discover rustdoc root");

        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        // `rustup doc --path` prints the path to index.html; we want the directory.
        let doc_root = PathBuf::from(path)
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf();
        assert!(
            doc_root.exists(),
            "rustdoc root does not exist: {}",
            doc_root.display()
        );
        Self { doc_root }
    }

    /// Create a `RustdocTool` with an explicit doc root (for testing).
    pub fn with_doc_root(doc_root: PathBuf) -> Self {
        Self { doc_root }
    }

    /// Resolve a query string to a relative HTML path under the doc root.
    ///
    /// Returns `None` if the query cannot be resolved.
    pub(crate) fn resolve_query(&self, query: &str) -> Option<PathBuf> {
        // Handle method queries: "Type::method" — resolve the type first.
        if let Some(colon_idx) = query.find("::") {
            // Check if this is a method query (exactly one "::" and the right
            // side looks like a method name, not a module path).
            let left = &query[..colon_idx];
            let right = &query[colon_idx + 2..];

            // If right contains "::", it's a fully-qualified path, not a method query.
            if !right.contains(':') {
                // It could be a bare type::method OR the start of a qualified path
                // with a crate prefix. Check if left is a known crate prefix.
                if !is_crate_prefix(left) {
                    // Method query: resolve the type, then return its page.
                    // The caller (execute) will extract the method from the HTML.
                    return self.resolve_bare_name(left);
                }
            }

            // Fully-qualified path: "crate::module::...::Item"
            return self.resolve_qualified_path(query);
        }

        // Bare name: "Vec", "Option", "str", etc.
        self.resolve_bare_name(query)
    }

    /// Resolve a fully-qualified path like `std::collections::HashMap`.
    fn resolve_qualified_path(&self, query: &str) -> Option<PathBuf> {
        let segments: Vec<&str> = query.split("::").collect();
        if segments.len() < 2 {
            return None;
        }

        let krate = segments[0];
        if !is_crate_prefix(krate) {
            return None;
        }

        let item_name = segments[segments.len() - 1];
        let module_segments = &segments[1..segments.len() - 1];

        // Try each ItemKind until we find a file that exists.
        for kind in [
            ItemKind::Struct,
            ItemKind::Enum,
            ItemKind::Trait,
            ItemKind::Function,
            ItemKind::Macro,
            ItemKind::Constant,
        ] {
            let mut path = PathBuf::from(krate);
            for seg in module_segments {
                path.push(seg);
            }
            path.push(format!("{}.{}.html", kind.prefix(), item_name));

            if self.doc_root.join(&path).exists() {
                return Some(path);
            }
        }

        None
    }

    /// Resolve a bare name like `Vec`, `Option`, `str`, `Display`.
    fn resolve_bare_name(&self, name: &str) -> Option<PathBuf> {
        // Check primitives first (single-lowercase names like str, char, bool).
        if let Some(path) = try_primitive(name)
            && self.doc_root.join(&path).exists()
        {
            return Some(path);
        }

        // Check the hard-coded lookup table.
        if let Some(item) = lookup_known_item(name) {
            let path = format!(
                "{}/{}/{}.{}.html",
                item.krate,
                item.module,
                item.kind.prefix(),
                name
            );
            if self.doc_root.join(&path).exists() {
                return Some(PathBuf::from(path));
            }
        }

        // Check macros (name ending with '!').
        let macro_name = name.trim_end_matches('!');
        if name.ends_with('!') {
            let path = format!("std/macro.{macro_name}.html");
            if self.doc_root.join(&path).exists() {
                return Some(PathBuf::from(path));
            }
        }

        None
    }

    /// Read an HTML file under the doc root and extract plain-text documentation.
    fn extract_docs(&self, html_path: &Path, section: &str) -> Result<String> {
        let full_path = self.doc_root.join(html_path);
        let html = std::fs::read_to_string(&full_path)
            .map_err(|e| anyhow::anyhow!("rustdoc: cannot read `{}`: {e}", full_path.display()))?;

        let main_content = extract_main_content(&html);
        let plain = strip_html_tags(&main_content);
        let decoded = decode_html_entities(&plain);
        let collapsed = collapse_whitespace(&decoded);

        let filtered = apply_section_filter(&collapsed, section);

        Ok(format!(
            "<stdlib reference query=\"{}\">\n{}\n</stdlib reference>",
            html_path.display(),
            filtered.trim()
        ))
    }
}

// ── Tool trait impl ───────────────────────────────────────────────────────────

#[async_trait]
impl Tool for RustdocTool {
    fn name(&self) -> ToolName {
        ToolName::from("rustdoc_lookup")
    }

    fn description(&self) -> &str {
        "Look up Rust standard library documentation from locally installed \
         rustdoc HTML. Pass a type name (Vec), fully-qualified path \
         (std::collections::HashMap), or method query (Option::map). \
         Returns documentation text without network access. \
         Use this to verify method signatures, trait implementations, \
         and code examples for the installed Rust toolchain version."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Item to look up: type name (Vec), fully-qualified path (std::collections::HashMap), method query (Option::map), trait name (Display), or primitive (str)."
                },
                "section": {
                    "type": "string",
                    "enum": ["all", "methods", "traits", "examples", "signature"],
                    "description": "Optional: which section of the documentation to return. Default is 'all'."
                }
            },
            "required": ["query"]
        })
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Read
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome> {
        if cancel.is_cancelled() {
            return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
        }

        let query = arguments
            .get("query")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing required field: query"))?;

        let section = arguments
            .get("section")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("all");

        // Validate section parameter.
        if !["all", "methods", "traits", "examples", "signature"].contains(&section) {
            return Ok(ToolOutcome::Immediate(ToolResult::error(format!(
                "invalid section '{section}': must be one of all, methods, traits, examples, signature"
            ))));
        }

        let Some(html_path) = self.resolve_query(query) else {
            return Ok(ToolOutcome::Immediate(ToolResult::error(format!(
                "could not resolve '{query}' to any rustdoc page"
            ))));
        };

        match self.extract_docs(&html_path, section) {
            Ok(output) => Ok(ToolOutcome::Immediate(ToolResult::success(output))),
            Err(e) => Ok(ToolOutcome::Immediate(ToolResult::error(format!(
                "rustdoc lookup failed for '{query}': {e}"
            )))),
        }
    }
}

// ── Query resolution helpers ──────────────────────────────────────────────────

/// Check whether a string is a known Rust crate prefix for stdlib docs.
fn is_crate_prefix(s: &str) -> bool {
    matches!(s, "std" | "core" | "alloc")
}

/// Try to resolve a name as a primitive type.
fn try_primitive(name: &str) -> Option<PathBuf> {
    static PRIMITIVES: &[&str] = &[
        "str",
        "char",
        "bool",
        "u8",
        "u16",
        "u32",
        "u64",
        "u128",
        "usize",
        "i8",
        "i16",
        "i32",
        "i64",
        "i128",
        "isize",
        "f32",
        "f64",
        "array",
        "slice",
        "tuple",
        "unit",
        "reference",
        "fn",
        "pointer",
        "never",
    ];

    if PRIMITIVES.contains(&name) {
        Some(PathBuf::from(format!("std/primitive.{name}.html")))
    } else {
        None
    }
}

/// Look up a known stdlib item by name.
///
/// Returns the [`KnownItem`] if `name` is in the hard-coded table.
/// This avoids filesystem walks for the most frequent lookups. Items not in
/// this table fall through to sidebar search (future work).
#[allow(clippy::too_many_lines)]
fn lookup_known_item(name: &str) -> Option<KnownItem> {
    /// (`item_name`, crate, module, kind)
    static TABLE: &[(&str, &str, &str, ItemKind)] = &[
        // Structs (std)
        ("Vec", "std", "vec", ItemKind::Struct),
        ("HashMap", "std", "collections", ItemKind::Struct),
        ("HashSet", "std", "collections", ItemKind::Struct),
        ("Mutex", "std", "sync", ItemKind::Struct),
        ("RwLock", "std", "sync", ItemKind::Struct),
        ("Condvar", "std", "sync", ItemKind::Struct),
        ("Once", "std", "sync", ItemKind::Struct),
        ("Barrier", "std", "sync", ItemKind::Struct),
        ("OnceLock", "std", "sync", ItemKind::Struct),
        ("LazyLock", "std", "sync", ItemKind::Struct),
        ("JoinHandle", "std", "thread", ItemKind::Struct),
        ("Instant", "std", "time", ItemKind::Struct),
        ("SystemTime", "std", "time", ItemKind::Struct),
        ("Path", "std", "path", ItemKind::Struct),
        ("PathBuf", "std", "path", ItemKind::Struct),
        ("OsStr", "std", "ffi", ItemKind::Struct),
        ("OsString", "std", "ffi", ItemKind::Struct),
        ("IoSlice", "std", "io", ItemKind::Struct),
        ("IoSliceMut", "std", "io", ItemKind::Struct),
        ("Cursor", "std", "io", ItemKind::Struct),
        ("Args", "std", "env", ItemKind::Struct),
        ("File", "std", "fs", ItemKind::Struct),
        ("Metadata", "std", "fs", ItemKind::Struct),
        ("OpenOptions", "std", "fs", ItemKind::Struct),
        ("Permissions", "std", "fs", ItemKind::Struct),
        ("Sink", "std", "io", ItemKind::Struct),
        ("Empty", "std", "io", ItemKind::Struct),
        ("Repeat", "std", "io", ItemKind::Struct),
        ("Stdin", "std", "io", ItemKind::Struct),
        ("Stdout", "std", "io", ItemKind::Struct),
        ("Stderr", "std", "io", ItemKind::Struct),
        // Structs (alloc)
        ("String", "alloc", "string", ItemKind::Struct),
        ("Box", "alloc", "boxed", ItemKind::Struct),
        ("Rc", "alloc", "rc", ItemKind::Struct),
        ("Arc", "alloc", "sync", ItemKind::Struct),
        ("Cow", "alloc", "borrow", ItemKind::Struct),
        ("VecDeque", "alloc", "collections", ItemKind::Struct),
        ("BTreeMap", "alloc", "collections", ItemKind::Struct),
        ("BTreeSet", "alloc", "collections", ItemKind::Struct),
        ("BinaryHeap", "alloc", "collections", ItemKind::Struct),
        ("LinkedList", "alloc", "collections", ItemKind::Struct),
        ("CString", "alloc", "ffi", ItemKind::Struct),
        // Structs (core)
        ("Cell", "core", "cell", ItemKind::Struct),
        ("RefCell", "core", "cell", ItemKind::Struct),
        ("UnsafeCell", "core", "cell", ItemKind::Struct),
        ("Pin", "core", "pin", ItemKind::Struct),
        ("PhantomData", "core", "marker", ItemKind::Struct),
        ("ManuallyDrop", "core", "mem", ItemKind::Struct),
        ("NonNull", "core", "ptr", ItemKind::Struct),
        ("Duration", "core", "time", ItemKind::Struct),
        ("Span", "core", "panic", ItemKind::Struct),
        ("AssertUnwindSafe", "core", "panic", ItemKind::Struct),
        // Enums
        ("Option", "core", "option", ItemKind::Enum),
        ("Result", "core", "result", ItemKind::Enum),
        ("VarError", "std", "env", ItemKind::Enum),
        ("IpAddr", "std", "net", ItemKind::Enum),
        ("SocketAddr", "std", "net", ItemKind::Enum),
        // Structs in net
        ("Ipv4Addr", "std", "net", ItemKind::Struct),
        ("Ipv6Addr", "std", "net", ItemKind::Struct),
        ("TcpListener", "std", "net", ItemKind::Struct),
        ("TcpStream", "std", "net", ItemKind::Struct),
        ("UdpSocket", "std", "net", ItemKind::Struct),
        // Traits
        ("Display", "std", "fmt", ItemKind::Trait),
        ("Debug", "std", "fmt", ItemKind::Trait),
        ("FromStr", "core", "str", ItemKind::Trait),
        ("Iterator", "core", "iter", ItemKind::Trait),
        ("IntoIterator", "core", "iter", ItemKind::Trait),
        ("Clone", "core", "clone", ItemKind::Trait),
        ("Copy", "core", "marker", ItemKind::Trait),
        ("Send", "core", "marker", ItemKind::Trait),
        ("Sync", "core", "marker", ItemKind::Trait),
        ("Sized", "core", "marker", ItemKind::Trait),
        ("Unpin", "core", "marker", ItemKind::Trait),
        ("From", "core", "convert", ItemKind::Trait),
        ("Into", "core", "convert", ItemKind::Trait),
        ("TryFrom", "core", "convert", ItemKind::Trait),
        ("TryInto", "core", "convert", ItemKind::Trait),
        ("AsRef", "core", "convert", ItemKind::Trait),
        ("AsMut", "core", "convert", ItemKind::Trait),
        ("Error", "core", "error", ItemKind::Trait),
        ("Read", "std", "io", ItemKind::Trait),
        ("Write", "std", "io", ItemKind::Trait),
        ("BufRead", "std", "io", ItemKind::Trait),
        ("Seek", "std", "io", ItemKind::Trait),
        ("Hash", "core", "hash", ItemKind::Trait),
        ("Ord", "core", "cmp", ItemKind::Trait),
        ("PartialOrd", "core", "cmp", ItemKind::Trait),
        ("Eq", "core", "cmp", ItemKind::Trait),
        ("PartialEq", "core", "cmp", ItemKind::Trait),
        ("Fn", "core", "ops", ItemKind::Trait),
        ("FnMut", "core", "ops", ItemKind::Trait),
        ("FnOnce", "core", "ops", ItemKind::Trait),
        ("Drop", "core", "ops", ItemKind::Trait),
        ("Deref", "core", "ops", ItemKind::Trait),
        ("DerefMut", "core", "ops", ItemKind::Trait),
        ("Default", "core", "default", ItemKind::Trait),
        ("ToOwned", "alloc", "borrow", ItemKind::Trait),
        ("Borrow", "core", "borrow", ItemKind::Trait),
        ("BorrowMut", "core", "borrow", ItemKind::Trait),
        ("Future", "core", "future", ItemKind::Trait),
        ("Stream", "core", "stream", ItemKind::Trait),
    ];

    TABLE
        .iter()
        .find(|(n, _, _, _)| *n == name)
        .map(|&(_, krate, module, kind)| KnownItem {
            krate,
            module,
            kind,
        })
}

// ── HTML extraction ───────────────────────────────────────────────────────────

/// Extract the content of the `<main>` or `<section id="main-content">` element.
fn extract_main_content(html: &str) -> String {
    // Try <section id="main-content"> first (modern rustdoc).
    if let Some(content) = extract_tag_content(html, "section", Some("main-content")) {
        return content;
    }
    // Fallback: <main>.
    if let Some(content) = extract_tag_content(html, "main", None) {
        return content;
    }
    // Last resort: return the whole HTML.
    html.to_string()
}

/// Extract the inner content of a specific HTML tag, optionally matching an id attribute.
fn extract_tag_content(html: &str, tag: &str, id: Option<&str>) -> Option<String> {
    let open_pattern = if let Some(id_val) = id {
        format!(r#"id="{id_val}""#)
    } else {
        format!("<{tag}")
    };

    let start = html.find(&open_pattern)?;
    // Walk back to find the '<' of this tag.
    let _tag_start = html[..start].rfind('<')?;
    let after_open = html[start..].find('>')?;
    let content_start = start + after_open + 1;

    // Find the matching closing tag, tracking nesting depth.
    let close_tag = format!("</{tag}>");
    let mut depth: i64 = 1;
    let mut pos = content_start;
    let open_tag_with_bracket = format!("<{tag}");

    while depth > 0 && pos < html.len() {
        if let Some(next_close) = html[pos..].find(&close_tag) {
            // Check for nested opening tags before this close.
            let search_region = &html[pos..pos + next_close];
            let nested_count = search_region.matches(&open_tag_with_bracket).count();
            depth += i64::from(u32::try_from(nested_count).unwrap_or(u32::MAX));
            depth -= 1;

            if depth == 0 {
                return Some(html[content_start..pos + next_close].to_string());
            }
            pos += next_close + close_tag.len();
        } else {
            break;
        }
    }

    // If we couldn't find a proper closing, return from content_start to end.
    Some(html[content_start..].to_string())
}

/// Strip all HTML tags from text, leaving only the text content.
fn strip_html_tags(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut in_script = false;

    let mut chars = html.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        if ch == '<' {
            // Check if this is a script or style tag.
            let rest = &html[i..];
            if rest.starts_with("<script") || rest.starts_with("<style") {
                in_script = true;
            }
            if rest.starts_with("</script") || rest.starts_with("</style") {
                in_script = false;
                // Skip past the closing '>'.
                for (_, c) in chars.by_ref() {
                    if c == '>' {
                        break;
                    }
                }
                continue;
            }
            if !in_script {
                in_tag = true;
                // Emit a space to separate text that was separated by tags.
                result.push(' ');
            }
        } else if ch == '>' && in_tag {
            in_tag = false;
        } else if !in_tag && !in_script {
            result.push(ch);
        }
    }

    result
}

/// Decode common HTML entities in text.
/// Decode common HTML entities in text.
fn decode_html_entities(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'&' {
            // Try to extract an entity up to ';'.
            let amp_pos = i;
            i += 1;
            let entity_start = i;
            while i < bytes.len() && bytes[i] != b';' && bytes[i] != b'<' && bytes[i] != b'&' {
                i += 1;
            }

            if i < bytes.len() && bytes[i] == b';' {
                let entity = &text[entity_start..i];
                i += 1; // consume ';'

                let decoded = match entity {
                    "amp" => "&",
                    "lt" => "<",
                    "gt" => ">",
                    "quot" => "\"",
                    "apos" | "#39" | "#x27" => "'",
                    "nbsp" => " ",
                    "mdash" => "\u{2014}",
                    "ndash" => "\u{2013}",
                    "hellip" => "\u{2026}",
                    _ => {
                        // Numeric entities: &#NNN; or &#xHHH;
                        if let Some(digits) = entity.strip_prefix('#') {
                            if let Some(hex) = digits.strip_prefix('x')
                                && let Ok(cp) = u32::from_str_radix(hex, 16)
                                && let Some(c) = char::from_u32(cp)
                            {
                                result.push(c);
                                continue;
                            } else if let Ok(cp) = digits.parse::<u32>()
                                && let Some(c) = char::from_u32(cp)
                            {
                                result.push(c);
                                continue;
                            }
                        }
                        // Unknown entity: preserve as-is.
                        result.push_str(&text[amp_pos..i]);
                        continue;
                    }
                };
                result.push_str(decoded);
            } else {
                // No semicolon found: not an entity, emit the '&'.
                result.push('&');
                i = entity_start;
            }
        } else {
            result.push(bytes[i] as char);
            i += 1;
        }
    }

    result
}

/// Collapse runs of whitespace (spaces, newlines, tabs) into single spaces/newlines.
fn collapse_whitespace(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut prev_was_space = false;
    let mut prev_was_newline = false;

    for ch in text.chars() {
        match ch {
            '\n' => {
                if !prev_was_newline {
                    result.push('\n');
                }
                prev_was_newline = true;
                prev_was_space = false;
            }
            ' ' | '\t' | '\r' => {
                if !prev_was_space && !prev_was_newline {
                    result.push(' ');
                }
                prev_was_space = true;
            }
            _ => {
                result.push(ch);
                prev_was_space = false;
                prev_was_newline = false;
            }
        }
    }

    result
}

// ── Section filtering ─────────────────────────────────────────────────────────

/// Apply a section filter to already-stripped plain text.
///
/// The text has been stripped of HTML tags, so section detection uses plain-text
/// heading patterns that rustdoc produces (e.g., "§Methods", "§Trait Implementations").
fn apply_section_filter(text: &str, section: &str) -> String {
    match section {
        "signature" => extract_signature(text),
        "methods" => extract_section_by_heading(text, &["Methods", "Implementations"]),
        "traits" => extract_section_by_heading(text, &["Trait Implementations"]),
        "examples" => extract_code_examples(text),
        _ => text.to_string(),
    }
}

/// Extract the type declaration / first paragraph (signature).
fn extract_signature(text: &str) -> String {
    // The signature is typically the first non-empty content block,
    // ending at the first blank line or "§" heading.
    let lines: Vec<&str> = text.lines().collect();
    let mut result = Vec::new();

    for line in &lines {
        let trimmed = line.trim();
        if trimmed.starts_with("§") {
            break;
        }
        if trimmed.is_empty() && !result.is_empty() {
            break;
        }
        if !trimmed.is_empty() {
            result.push(*line);
        }
    }

    result.join("\n").trim().to_string()
}

/// Extract sections matching any of the given heading names.
fn extract_section_by_heading(text: &str, headings: &[&str]) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut result = Vec::new();
    let mut in_section = false;
    let mut found_any = false;

    for line in &lines {
        let trimmed = line.trim();

        // Check if this is a heading that matches one of our targets.
        if trimmed.starts_with("§") {
            let heading_text = trimmed.trim_start_matches('§').trim();
            if headings.iter().any(|h| heading_text.starts_with(h)) {
                in_section = true;
                found_any = true;
                continue;
            }
            // Another heading — end of our section.
            if in_section {
                in_section = false;
            }
        }

        if in_section && !trimmed.is_empty() {
            result.push(*line);
        }
    }

    if found_any {
        result.join("\n").trim().to_string()
    } else {
        "No matching section found.".to_string()
    }
}

/// Extract code examples from the text.
///
/// Code examples appear as indented blocks in the stripped text (they were
/// inside `<pre><code>` tags in the HTML).
fn extract_code_examples(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut result = Vec::new();
    let mut in_code_block = false;
    let mut code_buffer: Vec<&str> = Vec::new();

    for line in &lines {
        let trimmed = line.trim();

        // Detect code block boundaries: lines that start with significant whitespace
        // and look like code patterns (Rust keywords, braces, etc.).
        let is_code_line = !trimmed.is_empty()
            && (line.starts_with("    ") || line.starts_with('\t'))
            && !trimmed.starts_with("§");

        if is_code_line {
            if !in_code_block {
                in_code_block = true;
                code_buffer.clear();
            }
            code_buffer.push(trimmed);
        } else if in_code_block {
            in_code_block = false;
            if !code_buffer.is_empty() {
                result.push(code_buffer.join("\n"));
                result.push(String::new()); // blank line separator
                code_buffer.clear();
            }
        }
    }

    // Flush remaining buffer.
    if !code_buffer.is_empty() {
        result.push(code_buffer.join("\n"));
    }

    let output = result.join("\n").trim().to_string();
    if output.is_empty() {
        "No code examples found.".to_string()
    } else {
        output
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Create a temporary rustdoc structure for testing `resolve_query`.
    fn setup_fake_docroot() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("create tempdir");

        // std/vec/struct.Vec.html
        let vec_dir = dir.path().join("std/vec");
        fs::create_dir_all(&vec_dir).unwrap();
        fs::write(vec_dir.join("struct.Vec.html"), fake_html("Vec")).unwrap();

        // core/option/enum.Option.html
        let opt_dir = dir.path().join("core/option");
        fs::create_dir_all(&opt_dir).unwrap();
        fs::write(opt_dir.join("enum.Option.html"), fake_html("Option")).unwrap();

        // std/collections/struct.HashMap.html
        let map_dir = dir.path().join("std/collections");
        fs::create_dir_all(&map_dir).unwrap();
        fs::write(map_dir.join("struct.HashMap.html"), fake_html("HashMap")).unwrap();

        // std/fmt/trait.Display.html
        let fmt_dir = dir.path().join("std/fmt");
        fs::create_dir_all(&fmt_dir).unwrap();
        fs::write(fmt_dir.join("trait.Display.html"), fake_html("Display")).unwrap();

        // std/primitive.str.html
        let prim_dir = dir.path().join("std");
        fs::create_dir_all(&prim_dir).unwrap();
        fs::write(prim_dir.join("primitive.str.html"), fake_html("str")).unwrap();

        // std/primitive.u8.html
        fs::write(prim_dir.join("primitive.u8.html"), fake_html("u8")).unwrap();

        // std/macro.vec.html
        fs::write(dir.path().join("std/macro.vec.html"), fake_html("vec!")).unwrap();

        dir
    }

    fn fake_html(name: &str) -> String {
        format!(
            "<html><head><title>{name}</title></head>\
             <body>\
             <nav>Sidebar</nav>\
             <section id=\"main-content\">\
             <h1>{name}</h1>\
             <p>Description of {name}.</p>\
             <h2 id=\"examples\">§Examples</h2>\
             <pre><code>let x = 1;</code></pre>\
             <h2 id=\"methods\">§Methods</h2>\
             <p>fn foo(&amp;self) -&gt; bool</p>\
             <h2 id=\"trait-implementations\">§Trait Implementations</h2>\
             <p>impl Display for {name}</p>\
             </section>\
             <footer>Footer</footer>\
             </body></html>"
        )
    }

    // -- Query resolution tests --

    #[test]
    fn resolve_vec() {
        let dir = setup_fake_docroot();
        let tool = RustdocTool::with_doc_root(dir.path().to_path_buf());
        let result = tool.resolve_query("Vec").unwrap();
        assert_eq!(result, PathBuf::from("std/vec/struct.Vec.html"));
    }

    #[test]
    fn resolve_option() {
        let dir = setup_fake_docroot();
        let tool = RustdocTool::with_doc_root(dir.path().to_path_buf());
        let result = tool.resolve_query("Option").unwrap();
        assert_eq!(result, PathBuf::from("core/option/enum.Option.html"));
    }

    #[test]
    fn resolve_hashmap_qualified() {
        let dir = setup_fake_docroot();
        let tool = RustdocTool::with_doc_root(dir.path().to_path_buf());
        let result = tool.resolve_query("std::collections::HashMap").unwrap();
        assert_eq!(result, PathBuf::from("std/collections/struct.HashMap.html"));
    }

    #[test]
    fn resolve_display_trait() {
        let dir = setup_fake_docroot();
        let tool = RustdocTool::with_doc_root(dir.path().to_path_buf());
        let result = tool.resolve_query("Display").unwrap();
        assert_eq!(result, PathBuf::from("std/fmt/trait.Display.html"));
    }

    #[test]
    fn resolve_str_primitive() {
        let dir = setup_fake_docroot();
        let tool = RustdocTool::with_doc_root(dir.path().to_path_buf());
        let result = tool.resolve_query("str").unwrap();
        assert_eq!(result, PathBuf::from("std/primitive.str.html"));
    }

    #[test]
    fn resolve_u8_primitive() {
        let dir = setup_fake_docroot();
        let tool = RustdocTool::with_doc_root(dir.path().to_path_buf());
        let result = tool.resolve_query("u8").unwrap();
        assert_eq!(result, PathBuf::from("std/primitive.u8.html"));
    }

    #[test]
    fn resolve_unknown_returns_none() {
        let dir = setup_fake_docroot();
        let tool = RustdocTool::with_doc_root(dir.path().to_path_buf());
        assert_eq!(tool.resolve_query("NonExistentType123"), None);
    }

    #[test]
    fn resolve_method_query() {
        let dir = setup_fake_docroot();
        let tool = RustdocTool::with_doc_root(dir.path().to_path_buf());
        // "Option::map" should resolve to the Option page.
        let result = tool.resolve_query("Option::map").unwrap();
        assert_eq!(result, PathBuf::from("core/option/enum.Option.html"));
    }

    #[test]
    fn resolve_macro() {
        let dir = setup_fake_docroot();
        let tool = RustdocTool::with_doc_root(dir.path().to_path_buf());
        let result = tool.resolve_query("vec!").unwrap();
        assert_eq!(result, PathBuf::from("std/macro.vec.html"));
    }

    #[test]
    fn resolve_macro_without_bang() {
        let dir = setup_fake_docroot();
        let tool = RustdocTool::with_doc_root(dir.path().to_path_buf());
        // "vec" (no !) should not match the macro (no struct either).
        // It won't match because there's no struct.vec.html.
        // But we do have a macro.vec.html — bare names don't try macros without '!'
        // unless they're in the known_items table.
        assert!(tool.resolve_query("vec").is_none());
    }

    // -- HTML stripping tests --

    #[test]
    fn strip_simple_tags() {
        let html = "<p>Hello <em>world</em></p>";
        let stripped = strip_html_tags(html);
        assert_eq!(stripped.trim(), "Hello  world");
    }

    #[test]
    fn strip_nested_tags() {
        let html = "<div><p>Text <a href=\"#\">link</a></p></div>";
        let stripped = strip_html_tags(html);
        assert!(stripped.contains("Text"));
        assert!(stripped.contains("link"));
        assert!(!stripped.contains('<'));
    }

    #[test]
    fn strip_preserves_text_outside_tags() {
        let html = "before <b>bold</b> after";
        let stripped = strip_html_tags(html);
        assert!(stripped.contains("before"));
        assert!(stripped.contains("bold"));
        assert!(stripped.contains("after"));
    }

    #[test]
    fn strip_skips_script_tags() {
        let html = "text<script>var x = 1;</script>more text";
        let stripped = strip_html_tags(html);
        assert!(stripped.contains("text"));
        assert!(stripped.contains("more text"));
        assert!(!stripped.contains("var x"));
    }

    // -- HTML entity decoding tests --

    #[test]
    fn decode_common_entities() {
        let decoded = decode_html_entities("&amp; &lt; &gt;");
        assert_eq!(decoded, "& < >");
    }

    #[test]
    fn decode_quot_entity() {
        let decoded = decode_html_entities("&quot;hello&quot;");
        assert_eq!(decoded, "\"hello\"");
    }

    #[test]
    fn decode_numeric_entity() {
        let decoded = decode_html_entities("&#65;");
        assert_eq!(decoded, "A");
    }

    #[test]
    fn decode_hex_entity() {
        let decoded = decode_html_entities("&#x41;");
        assert_eq!(decoded, "A");
    }

    #[test]
    fn decode_unknown_entity_preserved() {
        let decoded = decode_html_entities("&foo;");
        assert_eq!(decoded, "&foo;");
    }

    #[test]
    fn decode_lone_ampersand() {
        let decoded = decode_html_entities("a & b");
        assert_eq!(decoded, "a & b");
    }

    // -- Whitespace collapse tests --

    #[test]
    fn collapse_multiple_spaces() {
        let collapsed = collapse_whitespace("hello   world");
        assert_eq!(collapsed, "hello world");
    }

    #[test]
    fn collapse_multiple_newlines() {
        let collapsed = collapse_whitespace("hello\n\n\nworld");
        assert_eq!(collapsed, "hello\nworld");
    }

    #[test]
    fn collapse_tabs_and_spaces() {
        let collapsed = collapse_whitespace("hello \t world");
        assert_eq!(collapsed, "hello world");
    }

    // -- Main content extraction tests --

    #[test]
    fn extract_main_content_finds_section() {
        let html = "<nav>sidebar</nav><section id=\"main-content\"><p>Main text</p></section><footer>foot</footer>";
        let content = extract_main_content(html);
        assert!(content.contains("Main text"));
        assert!(!content.contains("sidebar"));
        assert!(!content.contains("foot"));
    }

    #[test]
    fn extract_main_content_falls_back_to_main_tag() {
        let html = "<nav>sidebar</nav><main><p>Main text</p></main><footer>foot</footer>";
        let content = extract_main_content(html);
        assert!(content.contains("Main text"));
    }

    // -- Section filter tests --

    #[test]
    fn section_all_returns_everything() {
        let text = "Signature\n§Examples\nexample code\n§Methods\nmethod stuff";
        let result = apply_section_filter(text, "all");
        assert_eq!(result, text);
    }

    #[test]
    fn section_methods_extracts_methods() {
        let text = "Signature\n§Examples\nexample code\n§Methods\nfn foo()\n§Trait Implementations\nimpl Display";
        let result = apply_section_filter(text, "methods");
        assert!(result.contains("fn foo()"));
        assert!(!result.contains("example code"));
        assert!(!result.contains("impl Display"));
    }

    #[test]
    fn section_traits_extracts_trait_impls() {
        let text =
            "Signature\n§Methods\nfn foo()\n§Trait Implementations\nimpl Display\nimpl Debug";
        let result = apply_section_filter(text, "traits");
        assert!(result.contains("impl Display"));
        assert!(result.contains("impl Debug"));
        assert!(!result.contains("fn foo()"));
    }

    #[test]
    fn section_examples_extracts_code() {
        let text = "Description\n    let x = 1;\n    let y = 2;\nMore text\n    let z = 3;";
        let result = apply_section_filter(text, "examples");
        assert!(result.contains("let x = 1;"));
        assert!(result.contains("let z = 3;"));
        assert!(!result.contains("More text"));
    }

    #[test]
    fn section_signature_extracts_first_block() {
        let text = "pub struct Vec<T>\n\n§Examples\nexample";
        let result = apply_section_filter(text, "signature");
        assert_eq!(result, "pub struct Vec<T>");
    }

    #[test]
    fn section_no_match_returns_not_found() {
        let text = "Signature only, no headings";
        let result = apply_section_filter(text, "methods");
        assert!(result.contains("No matching section found"));
    }

    // -- End-to-end extract_docs test --

    #[test]
    fn extract_docs_wraps_in_framing() {
        let dir = setup_fake_docroot();
        let tool = RustdocTool::with_doc_root(dir.path().to_path_buf());
        let html_path = PathBuf::from("std/vec/struct.Vec.html");
        let result = tool.extract_docs(&html_path, "all").unwrap();
        assert!(result.starts_with("<stdlib reference query=\""));
        assert!(result.contains("Vec"));
        assert!(result.ends_with("</stdlib reference>"));
    }

    #[test]
    fn extract_docs_missing_file_returns_error() {
        let dir = setup_fake_docroot();
        let tool = RustdocTool::with_doc_root(dir.path().to_path_buf());
        let html_path = PathBuf::from("nonexistent.html");
        assert!(tool.extract_docs(&html_path, "all").is_err());
    }

    // -- is_crate_prefix tests --

    #[test]
    fn crate_prefix_recognized() {
        assert!(is_crate_prefix("std"));
        assert!(is_crate_prefix("core"));
        assert!(is_crate_prefix("alloc"));
    }

    #[test]
    fn crate_prefix_rejects_unknown() {
        assert!(!is_crate_prefix("serde"));
        assert!(!is_crate_prefix("Vec"));
    }

    // -- try_primitive tests --

    #[test]
    fn primitive_str() {
        assert_eq!(
            try_primitive("str"),
            Some(PathBuf::from("std/primitive.str.html"))
        );
    }

    #[test]
    fn primitive_not_found() {
        assert_eq!(try_primitive("Vec"), None);
    }
}
