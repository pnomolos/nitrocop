use crate::cop::shared::method_dispatch_predicates::is_safe_navigation;
use crate::cop::shared::node_type::CALL_NODE;
use crate::cop::{Cop, CopConfig};
use crate::diagnostic::{Diagnostic, Severity};
use crate::parse::source::SourceFile;

const SELECT_METHODS: &[&[u8]] = &[b"select", b"filter", b"find_all"];
const FIND_METHODS: &[&[u8]] = &[b"find", b"detect"];

/// Flags `select`/`filter`/`find_all`/`reject`/`find`/`detect` blocks whose
/// sole body is a range check on the block parameter, and suggests
/// `grep`/`grep_v`.
///
/// Recognised range checks (optionally wrapped in `!`, with or without
/// parentheses around the inner call):
/// - `x.between?(min, max)` → `min..max`
/// - `(min..max).cover?(x)` / `(min..max).include?(x)` → `min..max`
///
/// The block parameter may be an explicit single required parameter, `_1`, or
/// `it`. The receiver must not look like a Hash (`{}` literal, `Hash.new`,
/// `Hash[]`, any `to_h`/`to_hash` call) or be `ENV`/`::ENV`, because `grep` is
/// not equivalent to `select` for hashes.
///
/// Autocorrect is unsafe (`SafeAutoCorrect: false` upstream): static analysis
/// cannot prove the receiver is really an Array.
///
/// Prism-vs-Parser quirks:
/// - Upstream needs 18 node-pattern alternatives because parser has three
///   distinct block node types (`block`, `numblock`, `itblock`) and wraps
///   parenthesised sub-expressions in `begin`. Prism has a single `BlockNode`
///   whose `parameters()` is `BlockParametersNode`, `NumberedParametersNode` or
///   `ItParametersNode`, so the three shapes collapse into one `BlockArg` enum.
/// - `!expr` is a `CallNode` named `!` in Prism, same as parser's
///   `(send _ :!)`; parser's `(begin …)` wrapper is Prism's `ParenthesesNode`,
///   which additionally interposes a `StatementsNode`.
/// - Prism uses one `CallNode` for both `.` and `&.` dispatch, where parser has
///   separate `send`/`csend` types. Upstream's range-check patterns spell `send`,
///   so `x&.between?(1, 10)` and `(1..10)&.cover?(x)` must be rejected
///   explicitly — the block *receiver* may still be safe-navigated
///   (`array&.select { … }`), because the cop aliases `on_csend` to `on_send`.
/// - Upstream's `node.loc.selector` (correction start) is Prism's
///   `message_loc()`, and `block_node.loc.end` is the `BlockNode`'s
///   `closing_loc()`.
/// - `block_node.body&.begin_type?` (bail on multi-statement blocks) maps to
///   "the `StatementsNode` body has more than one child"; a block body carrying
///   `rescue`/`ensure` is a `BeginNode` and is rejected outright, matching
///   upstream, whose pattern requires the body to be the range-check send.
pub struct SelectByRange;

enum BlockArg<'a> {
    Named(&'a [u8]),
    Numbered,
    It,
}

/// A matched range check: the (possibly negated) body call plus the range
/// source text the correction should use.
struct RangeCheck {
    negated: bool,
    range_literal: String,
}

impl SelectByRange {
    fn block_arg<'pr>(block: &'pr ruby_prism::BlockNode<'pr>) -> Option<BlockArg<'pr>> {
        let params = block.parameters()?;

        if let Some(numbered) = params.as_numbered_parameters_node() {
            return (numbered.maximum() == 1).then_some(BlockArg::Numbered);
        }
        if params.as_it_parameters_node().is_some() {
            return Some(BlockArg::It);
        }

        // parser's `(args (arg $_))`: exactly one plain required parameter.
        let inner = params.as_block_parameters_node()?.parameters()?;
        let requireds: Vec<_> = inner.requireds().iter().collect();
        if requireds.len() != 1
            || !inner.optionals().is_empty()
            || inner.rest().is_some()
            || !inner.posts().is_empty()
            || !inner.keywords().is_empty()
            || inner.keyword_rest().is_some()
            || inner.block().is_some()
        {
            return None;
        }
        let required = requireds[0].as_required_parameter_node()?;
        Some(BlockArg::Named(required.name().as_slice()))
    }

    fn is_block_arg(node: &ruby_prism::Node<'_>, arg: &BlockArg<'_>) -> bool {
        match arg {
            BlockArg::Named(name) => node
                .as_local_variable_read_node()
                .is_some_and(|lvar| lvar.name().as_slice() == *name),
            BlockArg::Numbered => node
                .as_local_variable_read_node()
                .is_some_and(|lvar| lvar.name().as_slice() == b"_1"),
            BlockArg::It => node.as_it_local_variable_read_node().is_some(),
        }
    }

    /// The sole expression of a block body, or `None` for empty/multi-statement
    /// bodies and bodies wrapped in `rescue`/`ensure` (a Prism `BeginNode`).
    fn sole_body_expression<'pr>(
        block: &'pr ruby_prism::BlockNode<'pr>,
    ) -> Option<ruby_prism::Node<'pr>> {
        let stmts = block.body()?.as_statements_node()?;
        let mut iter = stmts.body().iter();
        let first = iter.next()?;
        iter.next().is_none().then_some(first)
    }

    /// parser's `(begin X)` unwrapping — a single-statement `ParenthesesNode`.
    fn unwrap_parens<'pr>(node: &ruby_prism::Node<'pr>) -> Option<ruby_prism::Node<'pr>> {
        let stmts = node.as_parentheses_node()?.body()?.as_statements_node()?;
        let mut iter = stmts.body().iter();
        let first = iter.next()?;
        iter.next().is_none().then_some(first)
    }

    /// `min..max` / `(min..max)` — parser's `{range (begin range)}`.
    fn range_source(node: &ruby_prism::Node<'_>, source: &SourceFile) -> Option<String> {
        if let Some(range) = node.as_range_node() {
            return Some(Self::node_source(&range.as_node(), source));
        }
        let inner = Self::unwrap_parens(node)?;
        let range = inner.as_range_node()?;
        Some(Self::node_source(&range.as_node(), source))
    }

    fn node_source(node: &ruby_prism::Node<'_>, source: &SourceFile) -> String {
        let loc = node.location();
        String::from_utf8_lossy(&source.as_bytes()[loc.start_offset()..loc.end_offset()])
            .into_owned()
    }

    /// Upstream's `range_check?` + `calls_lvar_in_range_check?` + `find_range`.
    fn match_range_check(
        body: &ruby_prism::Node<'_>,
        arg: &BlockArg<'_>,
        source: &SourceFile,
    ) -> Option<RangeCheck> {
        let call = body.as_call_node()?;

        let (negated, inner) = if call.name().as_slice() == b"!" {
            let receiver = call.receiver()?;
            let unwrapped = Self::unwrap_parens(&receiver).unwrap_or(receiver);
            (true, unwrapped.as_call_node()?)
        } else {
            (false, call)
        };

        // parser's patterns use `send`, not `call`, for the range check itself,
        // so a safe-navigated `x&.between?(…)` / `(1..10)&.cover?(x)` (a `csend`)
        // never matches upstream.
        if is_safe_navigation(&inner) {
            return None;
        }

        let args = inner.arguments()?;
        let arg_list: Vec<_> = args.arguments().iter().collect();
        let receiver = inner.receiver()?;

        let range_literal = match inner.name().as_slice() {
            b"between?" => {
                if arg_list.len() != 2 || !Self::is_block_arg(&receiver, arg) {
                    return None;
                }
                format!(
                    "{}..{}",
                    Self::node_source(&arg_list[0], source),
                    Self::node_source(&arg_list[1], source)
                )
            }
            b"cover?" | b"include?" => {
                if arg_list.len() != 1 || !Self::is_block_arg(&arg_list[0], arg) {
                    return None;
                }
                Self::range_source(&receiver, source)?
            }
            _ => return None,
        };

        Some(RangeCheck {
            negated,
            range_literal,
        })
    }

    /// Upstream's `receiver_allowed?`.
    fn receiver_allowed(node: &ruby_prism::Node<'_>) -> bool {
        if node.as_hash_node().is_some() || node.as_keyword_hash_node().is_some() {
            return true;
        }
        if let Some(constant) = node.as_constant_read_node() {
            // `(const {nil? cbase} :ENV)` — a namespaced `Foo::ENV` does not count.
            return constant.name().as_slice() == b"ENV";
        }
        if let Some(path) = node.as_constant_path_node() {
            return path.parent().is_none() && path.name().is_some_and(|n| n.as_slice() == b"ENV");
        }
        if let Some(call) = node.as_call_node() {
            let name = call.name();
            let name = name.as_slice();
            if matches!(name, b"to_h" | b"to_hash") {
                return true;
            }
            // `(call (const _ :Hash) {:new :[]} ...)`, with or without a block.
            if matches!(name, b"new" | b"[]") {
                if let Some(receiver) = call.receiver() {
                    if receiver
                        .as_constant_read_node()
                        .is_some_and(|c| c.name().as_slice() == b"Hash")
                    {
                        return true;
                    }
                    if receiver
                        .as_constant_path_node()
                        .and_then(|p| p.name())
                        .is_some_and(|n| n.as_slice() == b"Hash")
                    {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Upstream's `replacement`. Returns the message fragment.
    fn replacement(method: &[u8], negated: bool) -> &'static str {
        if SELECT_METHODS.contains(&method) {
            if negated { "grep_v" } else { "grep" }
        } else if FIND_METHODS.contains(&method) {
            if negated {
                "grep_v(...).first"
            } else {
                "grep(...).first"
            }
        } else if negated {
            "grep"
        } else {
            "grep_v"
        }
    }
}

impl Cop for SelectByRange {
    fn name(&self) -> &'static str {
        "Style/SelectByRange"
    }

    fn default_severity(&self) -> Severity {
        Severity::Convention
    }

    fn supports_autocorrect(&self) -> bool {
        true
    }

    fn safe_autocorrect(&self) -> bool {
        false
    }

    fn interested_node_types(&self) -> &'static [u8] {
        &[CALL_NODE]
    }

    fn check_node(
        &self,
        source: &SourceFile,
        node: &ruby_prism::Node<'_>,
        _parse_result: &ruby_prism::ParseResult<'_>,
        _config: &CopConfig,
        diagnostics: &mut Vec<Diagnostic>,
        corrections: Option<&mut Vec<crate::correction::Correction>>,
    ) {
        let Some(call) = node.as_call_node() else {
            return;
        };
        let method_name = call.name();
        let method = method_name.as_slice();
        if !SELECT_METHODS.contains(&method)
            && !FIND_METHODS.contains(&method)
            && method != b"reject"
        {
            return;
        }

        let Some(block) = call.block().and_then(|b| b.as_block_node()) else {
            return;
        };
        if let Some(receiver) = call.receiver() {
            if Self::receiver_allowed(&receiver) {
                return;
            }
        }

        let Some(arg) = Self::block_arg(&block) else {
            return;
        };
        let Some(body) = Self::sole_body_expression(&block) else {
            return;
        };
        let Some(check) = Self::match_range_check(&body, &arg, source) else {
            return;
        };

        let replacement = Self::replacement(method, check.negated);
        let loc = node.location();
        let (line, column) = source.offset_to_line_col(loc.start_offset());
        diagnostics.push(self.diagnostic(
            source,
            line,
            column,
            format!(
                "Prefer `{replacement}` to `{}` with a range check.",
                String::from_utf8_lossy(method)
            ),
        ));

        if let Some(corrections) = corrections {
            let Some(message_loc) = call.message_loc() else {
                return;
            };
            let closing_loc = block.closing_loc();
            let grep = if replacement.contains("grep_v") {
                "grep_v"
            } else {
                "grep"
            };
            let suffix = if replacement.contains(".first") {
                ".first"
            } else {
                ""
            };
            corrections.push(crate::correction::Correction {
                start: message_loc.start_offset(),
                end: closing_loc.end_offset(),
                replacement: format!("{grep}({}){suffix}", check.range_literal),
                cop_name: self.name(),
                cop_index: 0,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    crate::cop_fixture_tests!(SelectByRange, "cops/style/select_by_range");
    crate::cop_autocorrect_fixture_tests!(SelectByRange, "cops/style/select_by_range");
}
