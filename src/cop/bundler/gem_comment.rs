use std::collections::HashSet;

use ruby_prism::Visit;

use crate::cop::{Cop, CopConfig};
use crate::diagnostic::Diagnostic;
use crate::parse::source::SourceFile;

use super::extract_gem_name;

pub struct GemComment;

/// ## Corpus investigation (2026-03-03)
///
/// ### FP=10 — FIXED (commit 0a40768)
///
/// All 10 FPs were from `extract_gem_name` (in `mod.rs`) matching lines where the gem
/// "name" was a variable, interpolation, or method call. The function found the first
/// quoted string anywhere on the line, which picked up argument values rather than gem
/// names. Fix: `extract_gem_name` now requires the first non-whitespace character after
/// `gem ` to be a quote, and rejects names containing `#{` (interpolation).
///
/// ### FN=16 — FIXED (AST-based modifier detection + percent-string fix)
///
/// All 16 FNs were gem declarations inside modifier `if`/`unless` with a preceding
/// comment. RuboCop's `ast_with_comments` associates the preceding comment with the
/// outermost AST node (the IfNode), not the inner gem SendNode. So `commented?(gem_node)`
/// returns false and the offense is reported.
///
/// Fix: uses a hybrid approach. The gem detection and comment checking use the proven
/// line-based logic (handles all edge cases correctly). An AST visitor collects the set
/// of 1-based line numbers where gem CallNodes are inside modifier if/unless (detected
/// via `end_keyword_loc().is_none()`). For those lines, preceding-line comments are not
/// counted as gem documentation — only inline comments on the same line count.
///
/// One additional FN (asciidoctor-pdf) was caused by `has_inline_comment()` not handling
/// `%(...)` percent-string literals. The `#` inside `%(~> #{...})` was falsely detected
/// as an inline comment, making the gem appear "commented". Fix: `has_inline_comment()`
/// now tracks `%(...)`/`%w(...)`/etc. paren-depth and skips `#` inside percent-strings.
///
/// ### Extended corpus FP=5 — FIXED (multi-line gem continuation comments)
///
/// 5 FPs from `dradis__dradis-legacy` (3) and `hummingbird-me__kitsu-server` (2).
/// The kitsu-server FPs were multi-line gem declarations (`gem 'name', github: ...,\n
///   branch: '...' # comment`). Comments on continuation lines were not detected.
/// Fix: `has_continuation_comment()` checks subsequent lines of multi-line gem calls
/// (lines ending with `,` or `\`) for inline comments.
///
/// ### Extended corpus FN=28 — FIXED (when/then/else gem calls)
///
/// All 28 FNs were gem calls on `when ... then` or `else` lines inside `case` statements.
/// `extract_gem_name()` requires lines to start with `gem`, missing these patterns.
/// Fix: `extract_inline_gem()` finds `gem 'name'` patterns anywhere on a line when
/// preceded by whitespace, catching `when 'foo' then gem 'bar'` and `else gem 'baz'`.
///
/// ### Extended corpus FP=3, FN=1 — FIXED (2026-03-20)
///
/// **FP=3**: Gems inside `group :dev do...end unless ENV['X']` — the modifier
/// `unless` wraps the entire block call, but the `ModifierGemVisitor` propagated
/// `in_modifier_conditional` into the block body. Gems inside the block body are
/// not "directly" in the modifier conditional. Fix: reset `in_modifier_conditional`
/// to false when entering a CallNode with a block child.
///
/// **FN=1**: `if ... then gem 'sinatra'` (rkh/big_band) — the gem appears after
/// `then` on the same line as a preceding-line comment. The comment is for the
/// `if` statement, not the gem. But nitrocop's line-based logic counted it as
/// gem documentation. Fix: skip preceding-line comment check for inline gems
/// (gems extracted by `extract_inline_gem`, not at column 0).
impl Cop for GemComment {
    fn name(&self) -> &'static str {
        "Bundler/GemComment"
    }

    fn default_enabled(&self) -> bool {
        false
    }

    fn default_include(&self) -> &'static [&'static str] {
        &["**/*.gemfile", "**/Gemfile", "**/gems.rb"]
    }

    fn check_source(
        &self,
        source: &SourceFile,
        parse_result: &ruby_prism::ParseResult<'_>,
        _code_map: &crate::parse::codemap::CodeMap,
        config: &CopConfig,
        diagnostics: &mut Vec<Diagnostic>,
        _corrections: Option<&mut Vec<crate::correction::Correction>>,
    ) {
        // Renamed IgnoredGems -> AllowedGems in rubocop 1.9x (config/obsoletion.yml
        // warns on the old name but does not translate its value; matches real RuboCop).
        let ignored_gems = config.get_string_array("AllowedGems").unwrap_or_default();
        let only_for = config.get_string_array("OnlyFor").unwrap_or_default();
        let check_version_specifiers = only_for.iter().any(|s| s == "version_specifiers");

        // Use AST visitor to find gem lines inside modifier if/unless
        let mut visitor = ModifierGemVisitor {
            source,
            modifier_gem_lines: HashSet::new(),
            in_modifier_conditional: false,
        };
        visitor.visit(&parse_result.node());
        let modifier_gem_lines = visitor.modifier_gem_lines;

        // Line-based gem detection and comment checking (proven approach)
        let lines: Vec<&[u8]> = source.lines().collect();
        let mut in_block_comment = false;

        for (i, line) in lines.iter().enumerate() {
            let line_str = std::str::from_utf8(line).unwrap_or("");
            let trimmed = line_str.trim_start();

            if in_block_comment {
                if trimmed.starts_with("=end") {
                    in_block_comment = false;
                }
                continue;
            }
            if trimmed.starts_with("=begin") {
                if !trimmed.contains("=end") {
                    in_block_comment = true;
                }
                continue;
            }

            // Try standard extraction first (line starts with `gem`)
            // then try extracting from `when ... then gem ...` / `else gem ...` patterns
            let (gem_name, gem_col, is_inline) = if let Some(name) = extract_gem_name(line_str) {
                (name, 0usize, false)
            } else if let Some((name, col)) = extract_inline_gem(trimmed) {
                (
                    name,
                    line_str.len() - line_str.trim_start().len() + col,
                    true,
                )
            } else {
                continue;
            };

            {
                // Skip ignored gems
                if ignored_gems.iter().any(|g| g == gem_name) {
                    continue;
                }

                // When OnlyFor includes "version_specifiers", only flag gems with version constraints
                if check_version_specifiers && !has_version_specifier(line_str) {
                    continue;
                }

                let line_num = i + 1; // 1-based

                // Check if this gem line is inside a modifier if/unless
                let is_modifier = modifier_gem_lines.contains(&line_num);

                // Check if the preceding line is a comment, or this line has an inline comment.
                // For inline gems (e.g., `if ... then gem 'x'`), the preceding-line
                // comment is for the enclosing statement, not the gem — skip it.
                let has_comment = has_inline_comment(line_str)
                    || has_continuation_comment(&lines, i)
                    || (!is_modifier
                        && !is_inline
                        && i > 0
                        && std::str::from_utf8(lines[i - 1])
                            .unwrap_or("")
                            .trim()
                            .starts_with('#')
                        && !is_magic_comment(
                            std::str::from_utf8(lines[i - 1]).unwrap_or("").trim(),
                        ));

                if !has_comment {
                    diagnostics.push(self.diagnostic(
                        source,
                        line_num,
                        gem_col,
                        "Missing gem description comment.".to_string(),
                    ));
                }
            }
        }
    }
}

/// AST visitor that collects 1-based line numbers of gem CallNodes
/// that are directly inside a modifier if/unless.
struct ModifierGemVisitor<'a> {
    source: &'a SourceFile,
    modifier_gem_lines: HashSet<usize>,
    in_modifier_conditional: bool,
}

impl<'pr> Visit<'pr> for ModifierGemVisitor<'_> {
    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
        if self.in_modifier_conditional
            && node.receiver().is_none()
            && node.name().as_slice() == b"gem"
        {
            let loc = node.location();
            let (line, _) = self.source.offset_to_line_col(loc.start_offset());
            self.modifier_gem_lines.insert(line);
        }
        // Don't propagate in_modifier_conditional into block bodies.
        // `group :dev do...end unless cond` — the modifier wraps the block call,
        // but gems inside the block body are not "directly" in the modifier.
        if node.block().is_some() {
            let prev = self.in_modifier_conditional;
            self.in_modifier_conditional = false;
            ruby_prism::visit_call_node(self, node);
            self.in_modifier_conditional = prev;
        } else {
            ruby_prism::visit_call_node(self, node);
        }
    }

    fn visit_if_node(&mut self, node: &ruby_prism::IfNode<'pr>) {
        // Modifier if: no end keyword and has if keyword (excludes ternary)
        let is_modifier = node.end_keyword_loc().is_none() && node.if_keyword_loc().is_some();
        if is_modifier {
            let prev = self.in_modifier_conditional;
            self.in_modifier_conditional = true;
            ruby_prism::visit_if_node(self, node);
            self.in_modifier_conditional = prev;
        } else {
            ruby_prism::visit_if_node(self, node);
        }
    }

    fn visit_unless_node(&mut self, node: &ruby_prism::UnlessNode<'pr>) {
        let is_modifier = node.end_keyword_loc().is_none();
        if is_modifier {
            let prev = self.in_modifier_conditional;
            self.in_modifier_conditional = true;
            ruby_prism::visit_unless_node(self, node);
            self.in_modifier_conditional = prev;
        } else {
            ruby_prism::visit_unless_node(self, node);
        }
    }
}

/// Extract a gem name from a line where `gem` is not at the start, e.g.:
/// `when "mysql" then gem "mysql2", "~>0.2.0"`
/// `else gem 'mongoid', '~> 7.0'`
/// Returns (gem_name, column_offset_within_trimmed) if found.
fn extract_inline_gem(trimmed: &str) -> Option<(&str, usize)> {
    // Look for ` gem '` or ` gem "` or ` gem(` patterns after when/then/else
    let mut search_from = 0;
    while search_from < trimmed.len() {
        let remaining = &trimmed[search_from..];
        // Find next occurrence of "gem " or "gem("
        let gem_pos = remaining.find("gem ").or_else(|| remaining.find("gem("));
        let rel_pos = gem_pos?;
        let abs_pos = search_from + rel_pos;

        // Ensure `gem` is preceded by whitespace (not part of another word)
        if abs_pos > 0 && !trimmed.as_bytes()[abs_pos - 1].is_ascii_whitespace() {
            search_from = abs_pos + 3;
            continue;
        }

        // Try to extract the gem name from this position
        let gem_call = &trimmed[abs_pos..];
        if let Some(name) = extract_gem_name(gem_call) {
            return Some((name, abs_pos));
        }

        search_from = abs_pos + 3;
    }
    None
}

/// Check if a multi-line gem declaration has a comment on any continuation line.
/// A continuation line follows a line ending with `,` or `\` and is more indented.
fn has_continuation_comment(lines: &[&[u8]], gem_line_idx: usize) -> bool {
    let gem_line = std::str::from_utf8(lines[gem_line_idx]).unwrap_or("");
    let gem_trimmed = gem_line.trim_end();

    // Only check continuation if the gem line ends with a comma or backslash
    if !gem_trimmed.ends_with(',') && !gem_trimmed.ends_with('\\') {
        return false;
    }

    // Check subsequent lines that are continuations
    for line_bytes in &lines[(gem_line_idx + 1)..] {
        let cont_line = std::str::from_utf8(line_bytes).unwrap_or("");
        let cont_trimmed = cont_line.trim();
        if cont_trimmed.is_empty() {
            break;
        }
        // A continuation line should not start with `gem` (that's a new gem declaration)
        if cont_trimmed.starts_with("gem ") || cont_trimmed.starts_with("gem(") {
            break;
        }
        // Check for inline comment on the continuation line
        if has_inline_comment(cont_line) {
            return true;
        }
        // If this continuation line doesn't end with comma/backslash, it's the last one
        let ct = cont_line.trim_end();
        if !ct.ends_with(',') && !ct.ends_with('\\') {
            break;
        }
    }
    false
}

/// Check if a gem declaration line has a version specifier.
/// Version specifiers look like: '~> 1.0', '>= 2.0', '1.0', etc.
fn has_version_specifier(line: &str) -> bool {
    let trimmed = line.trim();
    // After `gem 'name'`, look for version-like arguments
    // Find the closing quote of the gem name
    let first_quote = match trimmed.find(['\'', '"']) {
        Some(idx) => idx,
        None => return false,
    };
    let quote_char = trimmed.as_bytes()[first_quote];
    let after_name_start = first_quote + 1;
    let name_end = match trimmed[after_name_start..].find(|c: char| c as u8 == quote_char) {
        Some(idx) => after_name_start + idx + 1,
        None => return false,
    };

    let rest = &trimmed[name_end..];
    // Look for version string patterns after a comma
    // A version string starts with optional operator (>=, ~>, <=, >, <, =, !=) then digits
    if let Some(comma_idx) = rest.find(',') {
        let after_comma = rest[comma_idx + 1..].trim();
        // Check if next argument is a quoted version string
        if after_comma.starts_with('\'') || after_comma.starts_with('"') {
            let q = after_comma.as_bytes()[0];
            if let Some(end) = after_comma[1..].find(|c: char| c as u8 == q) {
                let val = &after_comma[1..1 + end];
                if is_version_string(val) {
                    return true;
                }
            }
        }
    }
    false
}

/// Check if a string looks like a version specifier.
/// Examples: "1.0", "~> 1.0", ">= 2.0", "< 3.0"
fn is_version_string(s: &str) -> bool {
    let s = s.trim();
    let s = s
        .trim_start_matches("~>")
        .trim_start_matches(">=")
        .trim_start_matches("<=")
        .trim_start_matches("!=")
        .trim_start_matches('>')
        .trim_start_matches('<')
        .trim_start_matches('=')
        .trim();
    // Should start with a digit
    s.starts_with(|c: char| c.is_ascii_digit())
}

/// Check if the line has an inline comment (# after the gem declaration).
fn has_inline_comment(line: &str) -> bool {
    // Heuristic: look for # that's not inside quotes or percent-string literals.
    let mut in_single = false;
    let mut in_double = false;
    let mut paren_depth: usize = 0;
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if paren_depth > 0 {
            match b {
                b'(' => paren_depth += 1,
                b')' => paren_depth -= 1,
                _ => {}
            }
            i += 1;
            continue;
        }
        match b {
            b'%' if !in_single && !in_double => {
                // Check for percent-string: %(...) or %X(...) where X is a letter
                if bytes.get(i + 1) == Some(&b'(') {
                    paren_depth = 1;
                    i += 2; // skip %(
                    continue;
                }
                if bytes.get(i + 1).is_some_and(|c| c.is_ascii_alphabetic())
                    && bytes.get(i + 2) == Some(&b'(')
                {
                    paren_depth = 1;
                    i += 3; // skip %X(
                    continue;
                }
            }
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'#' if !in_single && !in_double => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

fn is_magic_comment(line: &str) -> bool {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('#') {
        return false;
    }

    let body = trimmed.trim_start_matches('#').trim_start();
    body.starts_with("frozen_string_literal:")
        || body.starts_with("encoding:")
        || body.starts_with("coded by:")
        || body.starts_with("-*-")
}

#[cfg(test)]
mod tests {
    use super::*;
    crate::cop_fixture_tests!(GemComment, "cops/bundler/gem_comment");
}
