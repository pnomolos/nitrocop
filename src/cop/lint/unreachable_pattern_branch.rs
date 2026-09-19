use ruby_prism::Visit;

use crate::cop::{Cop, CopConfig};
use crate::diagnostic::{Diagnostic, Severity};
use crate::parse::source::SourceFile;

const MSG: &str = "Unreachable `in` pattern branch detected.";
const MSG_ELSE: &str = "Unreachable `else` branch detected.";

/// Checks for unreachable `in` pattern branches in `case...in` statements.
///
/// An `in` branch is unreachable when an earlier branch uses an *unguarded*
/// catch-all pattern. A catch-all pattern is a bare variable capture (`in x`),
/// an underscore (`in _`), a pattern alias whose left side is a catch-all
/// (`in _ => y`), a parenthesized catch-all (`in (x)`), or an alternation with
/// at least one catch-all alternative (`in _ | Integer`). Once such a branch is
/// seen, every following `in` branch is reported, and the `else` clause (if any)
/// is reported with a distinct message.
///
/// A catch-all with a guard (`in x if cond`) does *not* make later branches
/// unreachable, because the guard may fail.
///
/// Prism-vs-Parser quirks:
/// - Guards are not a separate `InNode` field in Prism the way parser exposes
///   `in_pattern.children[1]`. `in x if cond` parses as `InNode { pattern:
///   IfNode { statements: [LocalVariableTargetNode] } }` (`UnlessNode` for
///   `unless`). Detecting a guard therefore means checking whether the pattern
///   itself is an `IfNode`/`UnlessNode`, and such a pattern is never a catch-all.
/// - parser's `:begin` case (parenthesized pattern) is Prism's `ParenthesesNode`;
///   `in (_ | Integer) => y` elides the parentheses entirely, so both shapes are
///   unwrapped.
/// - parser's `match_as` is Prism's `CapturePatternNode`, whose `value()` is the
///   pattern and `target()` the captured variable (parser's `children[0]` /
///   `children[1]`). parser's n-ary `match_alt` is Prism's binary
///   `AlternationPatternNode`, so the check recurses into `left()` and `right()`.
pub struct UnreachablePatternBranch;

impl UnreachablePatternBranch {
    /// Mirrors upstream `catch_all_pattern?`.
    fn is_catch_all(pattern: &ruby_prism::Node<'_>) -> bool {
        if pattern.as_local_variable_target_node().is_some() {
            return true;
        }
        if let Some(capture) = pattern.as_capture_pattern_node() {
            return Self::is_catch_all(&capture.value());
        }
        if let Some(parens) = pattern.as_parentheses_node() {
            return match parens.body() {
                Some(body) => match body.as_statements_node() {
                    Some(stmts) => stmts
                        .body()
                        .iter()
                        .next()
                        .is_some_and(|inner| Self::is_catch_all(&inner)),
                    None => Self::is_catch_all(&body),
                },
                None => false,
            };
        }
        if let Some(alt) = pattern.as_alternation_pattern_node() {
            return Self::is_catch_all(&alt.left()) || Self::is_catch_all(&alt.right());
        }
        false
    }

    /// `in x if cond` / `in x unless cond` — Prism wraps the pattern in an
    /// `IfNode`/`UnlessNode` instead of exposing a separate guard child.
    fn has_guard(pattern: &ruby_prism::Node<'_>) -> bool {
        pattern.as_if_node().is_some() || pattern.as_unless_node().is_some()
    }
}

impl Cop for UnreachablePatternBranch {
    fn name(&self) -> &'static str {
        "Lint/UnreachablePatternBranch"
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
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
        // Upstream: `minimum_target_ruby_version 2.7` (`case/in` is a syntax
        // error before 2.7, so this only matters for explicit config).
        let target_ruby = config
            .options
            .get("TargetRubyVersion")
            .and_then(|v| v.as_f64().or_else(|| v.as_u64().map(|u| u as f64)))
            .unwrap_or(2.7);
        if target_ruby < 2.7 {
            return;
        }

        let mut visitor = PatternVisitor {
            cop: self,
            source,
            diagnostics: Vec::new(),
        };
        visitor.visit(&parse_result.node());
        diagnostics.extend(visitor.diagnostics);
    }
}

struct PatternVisitor<'a, 'src> {
    cop: &'a UnreachablePatternBranch,
    source: &'src SourceFile,
    diagnostics: Vec<Diagnostic>,
}

impl<'pr> Visit<'pr> for PatternVisitor<'_, '_> {
    fn visit_case_match_node(&mut self, node: &ruby_prism::CaseMatchNode<'pr>) {
        let mut catch_all_found = false;

        for clause in node.conditions().iter() {
            let Some(in_node) = clause.as_in_node() else {
                continue;
            };

            if catch_all_found {
                let loc = in_node.location();
                let (line, column) = self.source.offset_to_line_col(loc.start_offset());
                self.diagnostics.push(self.cop.diagnostic(
                    self.source,
                    line,
                    column,
                    MSG.to_string(),
                ));
                continue;
            }

            let pattern = in_node.pattern();
            if !UnreachablePatternBranch::has_guard(&pattern)
                && UnreachablePatternBranch::is_catch_all(&pattern)
            {
                catch_all_found = true;
            }
        }

        if catch_all_found {
            if let Some(else_clause) = node.else_clause() {
                let loc = else_clause.else_keyword_loc();
                let (line, column) = self.source.offset_to_line_col(loc.start_offset());
                self.diagnostics.push(self.cop.diagnostic(
                    self.source,
                    line,
                    column,
                    MSG_ELSE.to_string(),
                ));
            }
        }

        ruby_prism::visit_case_match_node(self, node);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    crate::cop_fixture_tests!(
        UnreachablePatternBranch,
        "cops/lint/unreachable_pattern_branch"
    );
}
