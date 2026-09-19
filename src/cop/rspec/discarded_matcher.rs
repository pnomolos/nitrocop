use ruby_prism::Visit;

use crate::cop::shared::util::RSPEC_DEFAULT_INCLUDE;
use crate::cop::{Cop, CopConfig};
use crate::diagnostic::{Diagnostic, Severity};
use crate::parse::source::SourceFile;

/// Matcher methods whose return value is meaningless when discarded.
const MATCHER_METHODS: &[&[u8]] = &[
    b"change",
    b"have_received",
    b"output",
    b"receive",
    b"receive_messages",
    b"receive_message_chain",
];

/// `RSpec/Language: Examples` — Regular, Focused, Skipped and Pending.
const EXAMPLE_METHODS: &[&[u8]] = &[
    b"it",
    b"specify",
    b"example",
    b"scenario",
    b"its",
    b"fit",
    b"fspecify",
    b"fexample",
    b"fscenario",
    b"focus",
    b"xit",
    b"xspecify",
    b"xexample",
    b"xscenario",
    b"skip",
    b"pending",
];

const EXPECTATION_METHODS: &[&[u8]] = &[b"to", b"to_not", b"not_to"];

/// Checks for RSpec matchers (`change`, `receive`, `output`, …) evaluated in
/// void context — typically a missing `.and` between compound matchers.
///
/// Upstream fires when all of the following hold:
/// 1. the receiver-chain base of the statement is a receiverless call to a
///    matcher method (`MATCHER_METHODS` plus `CustomMatcherMethods`),
/// 2. the statement sits inside an example block (`it`/`specify`/…),
/// 3. that example also contains some `to`/`to_not`/`not_to` call whose
///    arguments contain a matcher call (so bare helpers like `output.rewind`
///    in a spec that never uses a matcher are left alone), and
/// 4. the statement is in void context: upstream walks `void_value?` up through
///    `:begin`, `:case` and `:when` parents until it reaches a `:block` that is
///    an example.
///
/// This implementation inverts (1) and (4): instead of walking up from each
/// matcher call, it walks *down* from every void statement of an example body,
/// following `receiver()` links to the base call. That is equivalent, because
/// upstream's `find_outermost_chain` only follows receiver links and a void
/// statement is by construction the maximal node of its receiver chain.
///
/// Prism-vs-Parser quirks:
/// - parser's `on_send` + `on_block` split does not exist here: `change { … }`
///   is a single `CallNode` carrying a `BlockNode`, so both upstream entry
///   points collapse into one receiver-chain walk.
/// - Upstream's `:begin`/`:case`/`:when` void recursion maps to three different
///   Prism shapes. parser's `:begin` covers both statement sequences (Prism
///   `StatementsNode`) and parenthesised groups (Prism `ParenthesesNode`);
///   parser's `when`/`case`-`else` bodies are direct children, while Prism
///   interposes a `StatementsNode` (and an `ElseNode` for `case`).
/// - A block body containing `rescue`/`ensure` is a Prism `BeginNode`, not a
///   `StatementsNode`. Upstream also skips those (the parent of such statements
///   is a `:rescue`/`:ensure` node, which `void_value?` does not recurse into),
///   so only `StatementsNode` bodies are walked.
/// - `example?` is `(block (send nil? …) …)` in parser, which excludes
///   `numblock`/`itblock`. Prism models those as a `BlockNode` with
///   `NumberedParametersNode`/`ItParametersNode` parameters, so those are
///   rejected explicitly.
pub struct DiscardedMatcher;

impl DiscardedMatcher {
    fn is_matcher_name(name: &[u8], custom: &[String]) -> bool {
        MATCHER_METHODS.contains(&name) || custom.iter().any(|c| c.as_bytes() == name)
    }

    /// The `BlockNode` of `node` when `node` matches parser's
    /// `(block (send nil? #Examples.all ...) ...)`.
    fn example_block<'pr>(node: &ruby_prism::CallNode<'pr>) -> Option<ruby_prism::BlockNode<'pr>> {
        if node.receiver().is_some() {
            return None;
        }
        if !EXAMPLE_METHODS.contains(&node.name().as_slice()) {
            return None;
        }
        let block = node.block()?.as_block_node()?;
        if let Some(params) = block.parameters() {
            // parser models these as `numblock`/`itblock`, which `example?` does
            // not match.
            if params.as_numbered_parameters_node().is_some()
                || params.as_it_parameters_node().is_some()
            {
                return None;
            }
        }
        Some(block)
    }

    /// Mirrors `matcher_call?` applied to the base of a receiver chain.
    fn chain_base_matcher_name<'pr>(
        node: &ruby_prism::Node<'pr>,
        custom: &[String],
    ) -> Option<Vec<u8>> {
        let mut current = node.as_call_node()?;
        while let Some(receiver) = current.receiver() {
            current = receiver.as_call_node()?;
        }
        let name = current.name();
        let name = name.as_slice();
        if Self::is_matcher_name(name, custom) {
            Some(name.to_vec())
        } else {
            None
        }
    }
}

impl Cop for DiscardedMatcher {
    fn name(&self) -> &'static str {
        "RSpec/DiscardedMatcher"
    }

    fn default_severity(&self) -> Severity {
        Severity::Convention
    }

    fn default_include(&self) -> &'static [&'static str] {
        RSPEC_DEFAULT_INCLUDE
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
        let custom = config
            .get_string_array("CustomMatcherMethods")
            .unwrap_or_default();

        let mut visitor = ExampleVisitor {
            cop: self,
            source,
            custom,
            diagnostics: Vec::new(),
        };
        visitor.visit(&parse_result.node());
        diagnostics.extend(visitor.diagnostics);
    }
}

struct ExampleVisitor<'a, 'src> {
    cop: &'a DiscardedMatcher,
    source: &'src SourceFile,
    custom: Vec<String>,
    diagnostics: Vec<Diagnostic>,
}

impl ExampleVisitor<'_, '_> {
    /// Upstream `example_with_matcher_expectation?`.
    fn has_matcher_expectation(&self, block: &ruby_prism::BlockNode<'_>) -> bool {
        let mut finder = ExpectationFinder {
            custom: &self.custom,
            found: false,
        };
        if let Some(body) = block.body() {
            finder.visit(&body);
        }
        finder.found
    }

    /// Walk the void statements of an example body, mirroring upstream's
    /// `void_value?` recursion through `:begin`, `:case` and `:when`.
    fn check_void_node(&mut self, node: &ruby_prism::Node<'_>) {
        if let Some(case_node) = node.as_case_node() {
            if let Some(predicate) = case_node.predicate() {
                self.check_void_node(&predicate);
            }
            for condition in case_node.conditions().iter() {
                let Some(when_node) = condition.as_when_node() else {
                    continue;
                };
                for cond in when_node.conditions().iter() {
                    self.check_void_node(&cond);
                }
                if let Some(stmts) = when_node.statements() {
                    for stmt in stmts.body().iter() {
                        self.check_void_node(&stmt);
                    }
                }
            }
            if let Some(else_node) = case_node.else_clause() {
                if let Some(stmts) = else_node.statements() {
                    for stmt in stmts.body().iter() {
                        self.check_void_node(&stmt);
                    }
                }
            }
            return;
        }

        if let Some(parens) = node.as_parentheses_node() {
            if let Some(body) = parens.body() {
                if let Some(stmts) = body.as_statements_node() {
                    for stmt in stmts.body().iter() {
                        self.check_void_node(&stmt);
                    }
                }
            }
            return;
        }

        let Some(method) = DiscardedMatcher::chain_base_matcher_name(node, &self.custom) else {
            return;
        };

        let loc = node.location();
        let (line, column) = self.source.offset_to_line_col(loc.start_offset());
        self.diagnostics.push(self.cop.diagnostic(
            self.source,
            line,
            column,
            format!(
                "The result of `{}` is not used. Did you mean to chain it with `.and`?",
                String::from_utf8_lossy(&method)
            ),
        ));
    }
}

impl<'pr> Visit<'pr> for ExampleVisitor<'_, '_> {
    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
        if let Some(block) = DiscardedMatcher::example_block(node) {
            if self.has_matcher_expectation(&block) {
                if let Some(body) = block.body() {
                    if let Some(stmts) = body.as_statements_node() {
                        for stmt in stmts.body().iter() {
                            self.check_void_node(&stmt);
                        }
                    }
                }
            }
        }

        ruby_prism::visit_call_node(self, node);
    }
}

/// Finds `to`/`to_not`/`not_to` calls whose arguments contain a matcher call.
struct ExpectationFinder<'a> {
    custom: &'a [String],
    found: bool,
}

impl<'pr> Visit<'pr> for ExpectationFinder<'_> {
    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
        if !self.found && EXPECTATION_METHODS.contains(&node.name().as_slice()) {
            if let Some(args) = node.arguments() {
                for arg in args.arguments().iter() {
                    let mut finder = MatcherCallFinder {
                        custom: self.custom,
                        found: false,
                    };
                    finder.visit(&arg);
                    if finder.found {
                        self.found = true;
                        break;
                    }
                }
            }
        }

        ruby_prism::visit_call_node(self, node);
    }
}

/// Finds any receiverless call to a matcher method in a subtree.
struct MatcherCallFinder<'a> {
    custom: &'a [String],
    found: bool,
}

impl<'pr> Visit<'pr> for MatcherCallFinder<'_> {
    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
        if node.receiver().is_none()
            && DiscardedMatcher::is_matcher_name(node.name().as_slice(), self.custom)
        {
            self.found = true;
        }

        ruby_prism::visit_call_node(self, node);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    crate::cop_fixture_tests!(DiscardedMatcher, "cops/rspec/discarded_matcher");
    crate::cop_variant_fixture_tests!(
        DiscardedMatcher,
        "cops/rspec/discarded_matcher",
        custom_matcher_methods,
    );
}
