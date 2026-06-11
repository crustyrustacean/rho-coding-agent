//! Hashline computation and editing module.
//!
//! Provides hash computation for file lines using a custom 4-character hash
//! from a 16-letter alphabet (65,536 possible values), plus hashline-anchored
//! editing types, fuzzy validation, and diff formatting.
//!
//! # Hash Algorithm
//!
//! - Lines with alphanumeric characters: Hash based on line content
//! - Lines without alphanumerics: Hash based on line number
//!
//! The 32-bit seed is folded into 4 indices of 4 bits each, selecting from
//! a 16-character alphabet to produce a 4-character hash.
//!
//! # Collision properties
//!
//! With 65,536 possible values, the birthday-problem threshold (50% collision
//! probability) is ~302 lines. For files under ~250 lines (the vast majority
//! of edits), collision probability is under 40%.

/// Custom alphabet for hash characters (excludes hex, vowels, ambiguous letters).
const ALPHABET: &[u8] = b"ZPMQVRWSNKTXJBYH";

/// Number of hash characters produced by [`compute_line_hash`].
pub const HASH_LEN: usize = 4;

/// Compute a 4-character hash for a line.
///
/// # Panics
///
/// Never panics. Falls back to line number on non-alphanumeric lines.
pub fn compute_line_hash(line: &str, line_num: usize) -> String {
    let seed = if line.chars().any(char::is_alphanumeric) {
        line.bytes().fold(0u32, |acc, b| {
            acc.wrapping_mul(31).wrapping_add(u32::from(b))
        })
    } else {
        u32::try_from(line_num).unwrap_or(u32::MAX)
    };

    let idx1 = (seed & 0x0F) as usize;
    let idx2 = ((seed >> 4) & 0x0F) as usize;
    let idx3 = ((seed >> 8) & 0x0F) as usize;
    let idx4 = ((seed >> 12) & 0x0F) as usize;

    let bytes = [
        ALPHABET[idx1],
        ALPHABET[idx2],
        ALPHABET[idx3],
        ALPHABET[idx4],
    ];
    String::from_utf8(bytes.to_vec()).expect("ALPHABET contains valid UTF-8")
}

// ── Hashline Edit Types ─────────────────────────────────────────────────────

/// Hashline edit operation type.
#[derive(Debug, Clone, PartialEq)]
pub enum HashlineOp {
    Replace,
    Append,
    Prepend,
    Delete,
}

/// Parsed hashline anchor: line number and hash.
pub struct HashlineAnchor {
    pub line_num: usize,
    pub hash: String,
}

impl HashlineAnchor {
    pub fn parse(anchor: &str) -> Option<Self> {
        let (line_num_str, hash) = anchor.split_once('#')?;
        let line_num = line_num_str.parse::<usize>().ok()?;
        if hash.is_empty() {
            return None;
        }
        Some(HashlineAnchor {
            line_num,
            hash: hash.to_string(),
        })
    }
}

/// Hashline edit with operation type and parameters.
pub struct HashlineEdit {
    pub op: HashlineOp,
    pub pos: HashlineAnchor,
    pub end: Option<HashlineAnchor>,
    pub lines: Vec<String>,
}

/// Result of fuzzy anchor validation.
pub struct AnchorResolution {
    pub resolved_line: usize,
    pub exact_match: bool,
    pub relaxation_note: Option<String>,
}

// ── Helpers ─────────────────────────────────────────────────────────────────

pub fn truncate_for_error(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        text.to_owned()
    } else {
        let truncated: String = text.chars().take(max_len).collect();
        format!("{truncated}…")
    }
}

pub fn detect_regex_patterns(text: &str) -> Option<String> {
    const REGEX_TOKENS: &[&str] = &[
        r"\s*", r"\s+", r"\d+", r"\d*", r"\w+", r"\w*", r"\n", r"\t", r"\r", r".+", r".*",
    ];

    let mut found: Vec<&str> = Vec::new();
    for token in REGEX_TOKENS {
        if text.contains(token) && !found.contains(token) {
            found.push(token);
        }
    }

    if found.is_empty() {
        None
    } else {
        Some(found.join(", "))
    }
}

/// Return `true` if the line has enough distinct content to be a reliable anchor.
fn is_high_information_line(line: &str) -> bool {
    let stripped = line.trim();
    if stripped.len() < 4 {
        return false;
    }
    stripped.chars().any(char::is_alphanumeric)
}

// ── Public Functions ───────────────────────────────────────────────────────

/// Parse raw JSON edit values into [`HashlineEdit`] structs.
///
/// # Errors
///
/// Returns an error string if any edit is missing required fields or has an unknown `op`.
pub fn parse_hashline_edits(
    edits_arg: &[serde_json::Value],
) -> std::result::Result<Vec<HashlineEdit>, String> {
    let mut hashline_edits = Vec::new();
    for (i, edit_val) in edits_arg.iter().enumerate() {
        let op_str = edit_val["op"]
            .as_str()
            .ok_or_else(|| format!("edit_file: edit {i} missing 'op'"))?;
        let op = match op_str {
            "replace" => HashlineOp::Replace,
            "append" => HashlineOp::Append,
            "prepend" => HashlineOp::Prepend,
            "delete" => HashlineOp::Delete,
            other => return Err(format!("edit_file: invalid op '{other}'")),
        };
        let pos_str = edit_val["pos"]
            .as_str()
            .ok_or_else(|| format!("edit_file: edit {i} missing 'pos'"))?;
        let pos = HashlineAnchor::parse(pos_str)
            .ok_or_else(|| format!("edit_file: edit {i} invalid anchor '{pos_str}'"))?;
        let end = edit_val["end"].as_str().and_then(HashlineAnchor::parse);
        let edit_lines = if op == HashlineOp::Delete {
            Vec::new()
        } else {
            edit_val["lines"]
                .as_array()
                .ok_or_else(|| format!("edit_file: edit {i} missing 'lines'"))?
                .iter()
                .map(|v| v.as_str().unwrap_or_default().to_string())
                .collect()
        };
        hashline_edits.push(HashlineEdit {
            op,
            pos,
            end,
            lines: edit_lines,
        });
    }
    Ok(hashline_edits)
}

/// Validate hashline anchors against actual file content with fuzzy fallback.
///
/// # Errors
///
/// Returns an error string if any anchor line is out of range, or all fuzzy
/// relaxation attempts fail.
#[allow(clippy::too_many_lines)]
pub fn validate_hashline_edits_fuzzy(
    lines: &[&str],
    hashline_edits: &[HashlineEdit],
) -> std::result::Result<Vec<AnchorResolution>, String> {
    for edit in hashline_edits {
        if edit.pos.line_num == 0 || edit.pos.line_num > lines.len() {
            return Err(format!(
                "edit_file: anchor line {} is out of range (file has {} lines)",
                edit.pos.line_num,
                lines.len()
            ));
        }
        if let Some(ref end) = edit.end
            && (end.line_num == 0 || end.line_num > lines.len())
        {
            return Err(format!(
                "edit_file: end anchor line {} is out of range (file has {} lines)",
                end.line_num,
                lines.len()
            ));
        }
    }

    let mut resolutions = Vec::new();
    for edit in hashline_edits {
        let target_idx = edit.pos.line_num - 1;
        let line_content = lines[target_idx];
        let current_hash = compute_line_hash(line_content, edit.pos.line_num);

        if current_hash == edit.pos.hash {
            resolutions.push(AnchorResolution {
                resolved_line: target_idx,
                exact_match: true,
                relaxation_note: None,
            });
            continue;
        }

        if is_high_information_line(line_content) {
            resolutions.push(AnchorResolution {
                resolved_line: target_idx,
                exact_match: false,
                relaxation_note: Some(format!(
                    "anchor {}#{} relaxed to {}#{} (hash stale, line number valid)",
                    edit.pos.line_num, edit.pos.hash, edit.pos.line_num, current_hash
                )),
            });
            continue;
        }

        let search_radius = 5usize;
        let mut best_match: Option<(usize, String)> = None;
        for offset in 1..=search_radius {
            if let Some(candidate_idx) = target_idx.checked_add(offset).filter(|&i| i < lines.len())
            {
                let candidate_line = lines[candidate_idx];
                let candidate_hash = compute_line_hash(candidate_line, candidate_idx + 1);
                if candidate_hash == edit.pos.hash && is_high_information_line(candidate_line) {
                    best_match = Some((
                        candidate_idx,
                        format!(
                            "anchor {}#{} resolved to {}#{} (neighborhood search, +{offset})",
                            edit.pos.line_num,
                            edit.pos.hash,
                            candidate_idx + 1,
                            candidate_hash
                        ),
                    ));
                    break;
                }
            }
            if let Some(candidate_idx) = target_idx.checked_sub(offset) {
                let candidate_line = lines[candidate_idx];
                let candidate_hash = compute_line_hash(candidate_line, candidate_idx + 1);
                if candidate_hash == edit.pos.hash && is_high_information_line(candidate_line) {
                    best_match = Some((
                        candidate_idx,
                        format!(
                            "anchor {}#{} resolved to {}#{} (neighborhood search, -{offset})",
                            edit.pos.line_num,
                            edit.pos.hash,
                            candidate_idx + 1,
                            candidate_hash
                        ),
                    ));
                    break;
                }
            }
        }

        if let Some((resolved_idx, note)) = best_match {
            resolutions.push(AnchorResolution {
                resolved_line: resolved_idx,
                exact_match: false,
                relaxation_note: Some(note),
            });
            continue;
        }

        let context_start = edit.pos.line_num.saturating_sub(3);
        let context_end = (edit.pos.line_num + 3).min(lines.len());
        let mut context_lines = Vec::new();
        let width = lines.len().to_string().len();
        for (i, line) in lines.iter().enumerate() {
            let line_num = i + 1;
            if line_num >= context_start && line_num <= context_end {
                let hash = compute_line_hash(line, line_num);
                context_lines.push(format!("{line_num:>width$}#{hash}:{line}"));
            }
        }
        return Err(format!(
            "edit_file: hash mismatch at anchor {}#{} and no similar content found nearby\n\
             Expected line:  {}#{}:{}\n\
             Actual line:    {}#{}:{}\n\
             \n\
             Fresh hashes around mismatch:\n\
             {}\n\
             \n\
             Use updated anchor {}#{} to retry.",
            edit.pos.line_num,
            edit.pos.hash,
            edit.pos.line_num,
            edit.pos.hash,
            line_content,
            edit.pos.line_num,
            current_hash,
            line_content,
            context_lines.join("\n                     "),
            edit.pos.line_num,
            current_hash
        ));
    }
    Ok(resolutions)
}

/// Apply hashline edits to file content, returning the modified string and any relaxation notes.
///
/// # Errors
///
/// Returns an error if the file is empty, parsing fails, or anchor resolution fails.
pub fn apply_hashline_to_content(
    content: &str,
    edits_arg: &[serde_json::Value],
) -> std::result::Result<(String, Vec<String>), String> {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return Err("edit_file: cannot edit empty file".to_string());
    }

    let hashline_edits = parse_hashline_edits(edits_arg)?;
    let resolutions = validate_hashline_edits_fuzzy(&lines, &hashline_edits)?;
    let relaxation_notes: Vec<String> = resolutions
        .iter()
        .filter_map(|r| r.relaxation_note.clone())
        .collect();

    let mut apply_order: Vec<usize> = (0..hashline_edits.len()).collect();
    apply_order.sort_by(|&a, &b| {
        resolutions[b]
            .resolved_line
            .cmp(&resolutions[a].resolved_line)
    });

    let mut modified: Vec<String> = lines.iter().map(std::string::ToString::to_string).collect();
    for edit_idx in apply_order {
        let edit = &hashline_edits[edit_idx];
        let idx = resolutions[edit_idx].resolved_line;
        match edit.op {
            HashlineOp::Replace => {
                if let Some(ref end) = edit.end {
                    let original_span = end.line_num - edit.pos.line_num;
                    let end_idx = idx + original_span;
                    if end_idx >= modified.len() {
                        return Err("edit_file: range end would exceed file length".to_string());
                    }
                    modified.splice(idx..=end_idx, edit.lines.clone());
                } else {
                    modified[idx] = edit.lines.join("\n");
                }
            }
            HashlineOp::Append => {
                let insert_pos = idx + 1;
                if insert_pos < modified.len() {
                    modified.splice(insert_pos..insert_pos, edit.lines.clone());
                } else {
                    modified.extend(edit.lines.clone());
                }
            }
            HashlineOp::Prepend => {
                modified.splice(idx..idx, edit.lines.clone());
            }
            HashlineOp::Delete => {
                if let Some(ref end) = edit.end {
                    let original_span = end.line_num - edit.pos.line_num;
                    let end_idx = idx + original_span;
                    if end_idx >= modified.len() {
                        return Err("edit_file: range end would exceed file length".to_string());
                    }
                    modified.drain(idx..=end_idx);
                } else {
                    modified.remove(idx);
                }
            }
        }
    }
    Ok((modified.join("\n"), relaxation_notes))
}

pub fn format_hashline_diff(old: &str, new: &str) -> String {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let width = new_lines.len().to_string().len().max(1);

    let mut changed_indices: Vec<usize> = Vec::new();
    let max_cmp = new_lines.len().min(old_lines.len());
    for i in 0..max_cmp {
        if old_lines[i] != new_lines[i] {
            changed_indices.push(i);
        }
    }
    for i in old_lines.len()..new_lines.len() {
        changed_indices.push(i);
    }
    if old_lines.len() > new_lines.len() && !new_lines.is_empty() {
        let last = new_lines.len() - 1;
        if changed_indices.last() != Some(&last) {
            changed_indices.push(last);
        }
    }

    if changed_indices.is_empty() {
        return String::new();
    }

    let ctx = 3;
    let mut regions: Vec<(usize, usize)> = Vec::new();
    for &idx in &changed_indices {
        let start = idx.saturating_sub(ctx);
        let end = (idx + ctx).min(new_lines.len().saturating_sub(1));
        if let Some(last) = regions.last_mut()
            && start <= last.1 + 1
        {
            last.1 = last.1.max(end);
        } else {
            regions.push((start, end));
        }
    }

    let mut diff = String::new();
    for (ri, &(start, end)) in regions.iter().enumerate() {
        if ri > 0 {
            diff.push_str("  ...\n");
        }
        for i in start..=end {
            if i >= new_lines.len() {
                break;
            }
            let line_num = i + 1;
            let hash = compute_line_hash(new_lines[i], line_num);
            let line_content = new_lines[i];
            let is_changed = changed_indices.binary_search(&i).is_ok();
            if is_changed && i < old_lines.len() {
                let old_content = old_lines[i];
                let old_hash = compute_line_hash(old_content, line_num);
                let _ = std::fmt::write(
                    &mut diff,
                    format_args!("- {line_num:>width$}#{old_hash}:{old_content}\n"),
                );
                let _ = std::fmt::write(
                    &mut diff,
                    format_args!("+ {line_num:>width$}#{hash}:{line_content}\n"),
                );
            } else if is_changed {
                let _ = std::fmt::write(
                    &mut diff,
                    format_args!("+ {line_num:>width$}#{hash}:{line_content}\n"),
                );
            } else {
                let _ = std::fmt::write(
                    &mut diff,
                    format_args!("  {line_num:>width$}#{hash}:{line_content}\n"),
                );
            }
        }
    }
    diff
}

/// Build a `<fresh-anchors>` block with newly-hashed lines around edit regions.
pub fn format_fresh_anchors(old_content: &str, new_content: &str) -> Option<String> {
    let new_lines: Vec<&str> = new_content.lines().collect();
    let old_lines: Vec<&str> = old_content.lines().collect();

    if new_lines.is_empty() {
        return None;
    }

    let mut changed_indices: Vec<usize> = Vec::new();
    let max_cmp = new_lines.len().min(old_lines.len());
    for i in 0..max_cmp {
        if old_lines[i] != new_lines[i] {
            changed_indices.push(i);
        }
    }
    for i in old_lines.len()..new_lines.len() {
        changed_indices.push(i);
    }
    if old_lines.len() > new_lines.len() && !new_lines.is_empty() {
        let last = new_lines.len() - 1;
        if changed_indices.last() != Some(&last) {
            changed_indices.push(last);
        }
    }

    if changed_indices.is_empty() {
        return None;
    }

    let first_change = changed_indices.first()?;
    let last_change = changed_indices.last()?;
    let anchor_radius = 5usize;
    let anchor_start = first_change.saturating_sub(anchor_radius);
    let anchor_end = (last_change + anchor_radius).min(new_lines.len() - 1);

    let width = new_lines.len().to_string().len();
    let mut fresh_anchors = String::new();
    for (i, line) in new_lines
        .iter()
        .enumerate()
        .skip(anchor_start)
        .take(anchor_end - anchor_start + 1)
    {
        let line_num = i + 1;
        let hash = compute_line_hash(line, line_num);
        let _ = std::fmt::write(
            &mut fresh_anchors,
            format_args!("  {line_num:>width$}#{hash}:{line}\n"),
        );
    }

    let mut output = String::new();
    output.push_str("\n<fresh-anchors>\n");
    output.push_str(&fresh_anchors);
    output.push_str("</fresh-anchors>");
    output.push_str("\nLines have fresh anchors. Use these for subsequent edits to this region.");
    Some(output)
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deterministic_output_for_same_input() {
        let line = "function hello() {";
        let hash1 = compute_line_hash(line, 1);
        let hash2 = compute_line_hash(line, 1);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_hash_length() {
        let line = "function hello() {";
        let hash = compute_line_hash(line, 1);
        assert_eq!(hash.len(), HASH_LEN);
    }

    #[test]
    fn test_empty_line() {
        let hash = compute_line_hash("", 1);
        assert_eq!(hash.len(), HASH_LEN);
    }

    #[test]
    fn truncate_short_text_unchanged() {
        assert_eq!(truncate_for_error("hello", 10), "hello");
    }

    #[test]
    fn truncate_long_text() {
        let long = "a".repeat(100);
        let result = truncate_for_error(&long, 10);
        assert_eq!(result, "aaaaaaaaaa…");
    }

    #[test]
    fn truncate_exact_length_unchanged() {
        assert_eq!(truncate_for_error("hello", 5), "hello");
    }

    #[test]
    fn truncate_empty() {
        assert_eq!(truncate_for_error("", 10), "");
    }

    #[test]
    fn detect_regex_finds_backslash_s_star() {
        let text = r"fn foo() {\s*bar()}";
        let result = detect_regex_patterns(text);
        assert!(result.is_some());
        assert!(result.unwrap().contains(r"\s*"));
    }

    #[test]
    fn detect_regex_clean_literal_text() {
        let text = "fn main() {\n    println!(\"hello\");\n}";
        assert!(detect_regex_patterns(text).is_none());
    }

    #[test]
    fn detect_regex_empty_text() {
        assert!(detect_regex_patterns("").is_none());
    }

    #[test]
    fn detect_regex_deduplicates_repeated_tokens() {
        let text = r"a\s*b\s*c\s*d";
        let result = detect_regex_patterns(text);
        assert!(result.is_some());
        let count = result.unwrap().matches(r"\s*").count();
        assert_eq!(count, 1);
    }
}
