use ruby_prism::Visit;

use crate::cop::shared::node_type::CALL_NODE;
use crate::cop::{Cop, CopConfig};
use crate::correction::Correction;
use crate::diagnostic::Diagnostic;
use crate::parse::source::SourceFile;

const COP_NAME: &str = "Style/ReduceToHash";

/// Flags `each_with_object({})`, `inject({})` and `reduce({})` blocks that build
/// a hash one `[]=` at a time, where `to_h { … }` says the same
/// (`Style/ReduceToHash`, new in RuboCop 1.85, `Enabled: pending`,
/// `Safe: false`, `minimum_target_ruby_version 2.6`).
///
/// The two accepted shapes are literal node patterns upstream, ported here as
/// explicit structure checks:
///
/// ```text
/// (block (call _ :each_with_object (hash)) (args (arg _elem) (arg _hash))
///        (send (lvar _hash) :[]= $_key $_value))
/// (block (call _ {:inject :reduce}) (hash)) (args (arg _hash) (arg _elem))
///        (begin (send (lvar _hash) :[]= $_key $_value) (lvar _hash)))
/// ```
///
/// plus the `numblock` variants with exactly two numbered parameters. Three
/// things then disqualify a match: the accumulator being *read* in the key or
/// the value (`hash[elem.id] = hash[elem.id].to_i + 1` is a fold, not a
/// mapping), and either expression containing a nested `each_with_object` /
/// `inject` / `reduce` that matches the same pattern.
///
/// `Safe: false` is a cop-level property (the receiver is not provably an
/// `Enumerable`, and `each_with_object` returns the accumulator itself while
/// `to_h` returns a fresh hash), so it lives in config rather than in
/// `safe_autocorrect()`.
///
/// ## RuboCop quirks replicated
///
/// - **The initial value must be an empty hash *literal*.** `(hash)` matches a
///   `hash` node with no children, so `each_with_object(Hash.new(0))` and
///   `each_with_object({ a: 1 })` are both left alone.
/// - **The body must be exactly `[]=`** with exactly two arguments, on a
///   receiver that is the accumulator parameter itself — `hash.merge!(…)` and
///   `other[elem] = true` do not match.
/// - **`inject`/`reduce` must return the accumulator** as the second and last
///   statement of a two-statement body; an extra `puts` or a different trailing
///   expression disqualifies it.
/// - **The nested case reports the inner call only.** `nested_match?` suppresses
///   the outer offense so the two corrections cannot clobber each other;
///   RuboCop's own spec gets the fully-rewritten outer form only because
///   `expect_correction` loops the cop, and `rubocop -A` loops the same way.
/// - **`_2` is rewritten to `_1` for `inject`/`reduce` numblocks only**, by
///   `String#gsub` on the extracted source — a plain textual substitution, not
///   an AST rename, so it is reproduced as a plain textual substitution here.
///
/// ## Prism-vs-Parser notes
///
/// - Parser's `numblock` is Prism's `BlockNode` with a `NumberedParametersNode`
///   whose `maximum` is 2; `_1`/`_2` themselves are ordinary
///   `LocalVariableReadNode`s, so the accumulator-reference scan needs no
///   special case for them.
/// - `hash[k] = v` is a `CallNode` named `[]=` with the receiver and two
///   arguments, matching Parser's `(send (lvar _hash) :[]= key value)` exactly.
/// - Subtree scans (`references_variable?`, `nested_match?`) override the typed
///   `visit_local_variable_read_node` / `visit_call_node` rather than the
///   `visit_*_node_enter` hooks, because Prism's generated visitors reach typed
///   fields (a `StatementsNode` child, for one) without going through the `Node`
///   dispatcher that fires those hooks.
pub struct ReduceToHash;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    EachWithObject,
    Inject,
}

impl Cop for ReduceToHash {
    fn name(&self) -> &'static str {
        COP_NAME
    }

    fn supports_autocorrect(&self) -> bool {
        true
    }

    fn interested_node_types(&self) -> &'static [u8] {
        &[CALL_NODE]
    }

    fn check_node(
        &self,
        source: &SourceFile,
        node: &ruby_prism::Node<'_>,
        _parse_result: &ruby_prism::ParseResult<'_>,
        config: &CopConfig,
        diagnostics: &mut Vec<Diagnostic>,
        corrections: Option<&mut Vec<Correction>>,
    ) {
        // `minimum_target_ruby_version 2.6` — `to_h` with a block is 2.6+.
        let ruby_version = config
            .options
            .get("TargetRubyVersion")
            .and_then(|v| v.as_f64().or_else(|| v.as_u64().map(|u| u as f64)))
            .unwrap_or(3.4);
        if ruby_version < 2.6 {
            return;
        }

        let Some(call) = node.as_call_node() else {
            return;
        };
        let Some(kind) = call_kind(call.name().as_slice()) else {
            return;
        };
        let Some(block) = call.block().and_then(|b| b.as_block_node()) else {
            return;
        };
        let Some(matched) = match_pattern(&call, &block, kind) else {
            return;
        };

        if references_variable(&matched.key, &matched.accumulator)
            || references_variable(&matched.value, &matched.accumulator)
        {
            return;
        }
        if nested_match(&matched.key) || nested_match(&matched.value) {
            return;
        }

        let Some(selector) = call.message_loc() else {
            return;
        };
        let (line, column) = source.offset_to_line_col(selector.start_offset());
        let mut diagnostic = self.diagnostic(
            source,
            line,
            column,
            format!(
                "Use `to_h {{ ... }}` instead of `{}`.",
                String::from_utf8_lossy(call.name().as_slice())
            ),
        );

        if let Some(sink) = corrections
            && let Some(replacement) = build_replacement(source, &call, &block, &matched, kind)
        {
            sink.push(Correction {
                start: selector.start_offset(),
                end: block.location().end_offset(),
                replacement,
                cop_name: COP_NAME,
                cop_index: 0,
            });
            diagnostic.corrected = true;
        }
        diagnostics.push(diagnostic);
    }
}

fn call_kind(name: &[u8]) -> Option<Kind> {
    match name {
        b"each_with_object" => Some(Kind::EachWithObject),
        b"inject" | b"reduce" => Some(Kind::Inject),
        _ => None,
    }
}

struct Matched<'pr> {
    key: ruby_prism::Node<'pr>,
    value: ruby_prism::Node<'pr>,
    accumulator: Vec<u8>,
    /// `NumberedParametersNode` block (Parser's `numblock`).
    numbered: bool,
    /// Source of the element parameter, for the rewritten block header.
    element_parameter: Option<(usize, usize)>,
}

fn match_pattern<'pr>(
    call: &ruby_prism::CallNode<'pr>,
    block: &ruby_prism::BlockNode<'pr>,
    kind: Kind,
) -> Option<Matched<'pr>> {
    // `(call _ <method> (hash))`: exactly one argument, an empty hash literal.
    let arguments = call.arguments()?;
    let mut arguments = arguments.arguments().iter();
    let initial = arguments.next()?;
    if arguments.next().is_some() {
        return None;
    }
    // Parser has one `hash` node for both `{}` and bare keyword arguments;
    // Prism splits them into `HashNode` and `KeywordHashNode`. Both are checked
    // so the port is faithful to `(hash)`, though only the braced literal can
    // ever be empty (`**{}` yields a splat element).
    let initial_elements = if let Some(hash) = initial.as_hash_node() {
        hash.elements().iter().count()
    } else if let Some(keywords) = initial.as_keyword_hash_node() {
        keywords.elements().iter().count()
    } else {
        return None;
    };
    if initial_elements > 0 {
        return None;
    }

    let parameters = block_parameters(block, kind)?;
    let accumulator = parameters.accumulator;
    let (assignment, returns_accumulator) = block_body(block, kind)?;
    if !returns_accumulator {
        return None;
    }

    // `(send (lvar _hash) :[]= $_key $_value)`
    let assignment = assignment.as_call_node()?;
    if assignment.name().as_slice() != b"[]="
        || assignment.is_safe_navigation()
        || assignment.block().is_some()
    {
        return None;
    }
    let receiver = assignment.receiver()?;
    if receiver.as_local_variable_read_node()?.name().as_slice() != accumulator {
        return None;
    }
    let mut assignment_arguments = assignment.arguments()?.arguments().iter();
    let key = assignment_arguments.next()?;
    let value = assignment_arguments.next()?;
    if assignment_arguments.next().is_some() {
        return None;
    }

    Some(Matched {
        key,
        value,
        accumulator,
        numbered: parameters.numbered,
        element_parameter: parameters.element,
    })
}

/// The accumulator's name, the element parameter's source range (absent for a
/// numblock) and whether the block used numbered parameters.
struct BlockParameters {
    accumulator: Vec<u8>,
    element: Option<(usize, usize)>,
    numbered: bool,
}

/// `(args (arg _a) (arg _b))` or two numbered parameters.
fn block_parameters(block: &ruby_prism::BlockNode<'_>, kind: Kind) -> Option<BlockParameters> {
    let parameters = block.parameters()?;

    if let Some(numbered) = parameters.as_numbered_parameters_node() {
        if numbered.maximum() != 2 {
            return None;
        }
        let accumulator: &[u8] = match kind {
            Kind::EachWithObject => b"_2",
            Kind::Inject => b"_1",
        };
        return Some(BlockParameters {
            accumulator: accumulator.to_vec(),
            element: None,
            numbered: true,
        });
    }

    let parameters = parameters.as_block_parameters_node()?;
    // `(args …)` carries no block-local declarations (`{ |a, b; c| }`).
    if parameters.locals().iter().count() > 0 {
        return None;
    }
    let inner = parameters.parameters()?;
    if inner.optionals().iter().count() > 0
        || inner.rest().is_some()
        || inner.posts().iter().count() > 0
        || inner.keywords().iter().count() > 0
        || inner.keyword_rest().is_some()
        || inner.block().is_some()
    {
        return None;
    }
    let mut requireds = inner.requireds().iter();
    // Destructured parameters (`|(k, v), h|`) are `MultiTargetNode`, not `arg`.
    let first = requireds.next()?.as_required_parameter_node()?;
    let second = requireds.next()?.as_required_parameter_node()?;
    if requireds.next().is_some() {
        return None;
    }

    let (accumulator, element) = match kind {
        Kind::EachWithObject => (second, first),
        Kind::Inject => (first, second),
    };
    let element_location = element.location();
    Some(BlockParameters {
        accumulator: accumulator.name().as_slice().to_vec(),
        element: Some((
            element_location.start_offset(),
            element_location.end_offset(),
        )),
        numbered: false,
    })
}

/// The `[]=` statement, plus whether the `inject`/`reduce` body ends by
/// returning the accumulator (always true for `each_with_object`).
fn block_body<'pr>(
    block: &ruby_prism::BlockNode<'pr>,
    kind: Kind,
) -> Option<(ruby_prism::Node<'pr>, bool)> {
    let statements = block.body()?.as_statements_node()?;
    let mut body = statements.body().iter();
    let first = body.next()?;
    match kind {
        Kind::EachWithObject => {
            if body.next().is_some() {
                return None;
            }
            Some((first, true))
        }
        Kind::Inject => {
            let last = body.next()?;
            if body.next().is_some() {
                return None;
            }
            // The trailing `(lvar _hash)` is checked against the accumulator by
            // the caller's receiver comparison; here it only has to be a plain
            // local variable read, which `returns_accumulator` re-checks below.
            let Some(read) = last.as_local_variable_read_node() else {
                return Some((first, false));
            };
            let returns = first
                .as_call_node()
                .and_then(|assignment| assignment.receiver())
                .and_then(|receiver| {
                    receiver
                        .as_local_variable_read_node()
                        .map(|r| r.name().as_slice().to_vec())
                })
                .is_some_and(|name| name == read.name().as_slice());
            Some((first, returns))
        }
    }
}

fn build_replacement(
    source: &SourceFile,
    call: &ruby_prism::CallNode<'_>,
    block: &ruby_prism::BlockNode<'_>,
    matched: &Matched<'_>,
    kind: Kind,
) -> Option<String> {
    let key = adjusted_source(source, &matched.key, matched.numbered, kind)?;
    let value = adjusted_source(source, &matched.value, matched.numbered, kind)?;
    let body = format!("[{key}, {value}]");

    let opening = block.opening_loc();
    let braces = source
        .try_byte_slice(opening.start_offset(), opening.end_offset())
        .is_some_and(|text| text == "{");

    let (_, indent_column) = source.offset_to_line_col(call.location().start_offset());
    let indent = " ".repeat(indent_column);

    if matched.numbered {
        return Some(if braces {
            format!("to_h {{ {body} }}")
        } else {
            format!("to_h do\n{indent}  {body}\n{indent}end")
        });
    }

    let (start, end) = matched.element_parameter?;
    let argument = source.try_byte_slice(start, end)?;
    Some(if braces {
        format!("to_h {{ |{argument}| {body} }}")
    } else {
        format!("to_h do |{argument}|\n{indent}  {body}\n{indent}end")
    })
}

/// For `inject`/`reduce` numblocks `_2` is the element and becomes `_1`. This is
/// RuboCop's `String#gsub`, textual and unscoped.
fn adjusted_source(
    source: &SourceFile,
    node: &ruby_prism::Node<'_>,
    numbered: bool,
    kind: Kind,
) -> Option<String> {
    let location = node.location();
    let text = source.try_byte_slice(location.start_offset(), location.end_offset())?;
    if numbered && kind == Kind::Inject {
        Some(text.replace("_2", "_1"))
    } else {
        Some(text.to_string())
    }
}

// ---------------------------------------------------------------------------
// Subtree scans
// ---------------------------------------------------------------------------

struct VariableFinder<'a> {
    name: &'a [u8],
    found: bool,
}

impl<'pr> Visit<'pr> for VariableFinder<'_> {
    fn visit_local_variable_read_node(&mut self, node: &ruby_prism::LocalVariableReadNode<'pr>) {
        if node.name().as_slice() == self.name {
            self.found = true;
        }
    }
}

fn references_variable(node: &ruby_prism::Node<'_>, name: &[u8]) -> bool {
    let mut finder = VariableFinder { name, found: false };
    finder.visit(node);
    finder.found
}

#[derive(Default)]
struct NestedFinder {
    found: bool,
}

impl<'pr> Visit<'pr> for NestedFinder {
    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
        if !self.found
            && let Some(kind) = call_kind(node.name().as_slice())
            && let Some(block) = node.block().and_then(|b| b.as_block_node())
            && match_pattern(node, &block, kind).is_some()
        {
            self.found = true;
        }
        ruby_prism::visit_call_node(self, node);
    }
}

fn nested_match(node: &ruby_prism::Node<'_>) -> bool {
    let mut finder = NestedFinder::default();
    finder.visit(node);
    finder.found
}

#[cfg(test)]
mod tests {
    use super::*;

    crate::cop_fixture_tests!(ReduceToHash, "cops/style/reduce_to_hash");
    crate::cop_autocorrect_fixture_tests!(ReduceToHash, "cops/style/reduce_to_hash");

    #[test]
    fn offense_nested_fixture() {
        crate::testutil::assert_cop_offenses_full(
            &ReduceToHash,
            include_bytes!("../../../tests/fixtures/cops/style/reduce_to_hash/offense_nested.rb"),
        );
    }

    fn corrected(input: &[u8]) -> String {
        let (_diags, corrections) = crate::testutil::run_cop_autocorrect(&ReduceToHash, input);
        let cs = crate::correction::CorrectionSet::from_vec(corrections);
        String::from_utf8(cs.apply(input)).unwrap()
    }

    /// One pass rewrites the inner call only; the outer becomes correctable on
    /// the next pass, which is how `rubocop -A` reaches the fully-nested form.
    #[test]
    fn nested_autocorrect_rewrites_the_inner_call_first() {
        let input = b"tables.each_with_object({}) { |table, h|\n  h[table.node] = table.columns.each_with_object({}) { |column, i| i[column.name] = column.alias }\n}\n";
        let first = corrected(input);
        assert_eq!(
            first,
            "tables.each_with_object({}) { |table, h|\n  h[table.node] = table.columns.to_h { |column| [column.name, column.alias] }\n}\n"
        );
        assert_eq!(
            corrected(first.as_bytes()),
            "tables.to_h { |table| [table.node, table.columns.to_h { |column| [column.name, column.alias] }] }\n"
        );
    }

    #[test]
    fn skipped_below_target_ruby_version() {
        let mut options = std::collections::HashMap::new();
        options.insert(
            "TargetRubyVersion".to_string(),
            serde_yml::Value::Number(2.5.into()),
        );
        let config = crate::cop::CopConfig {
            options,
            ..crate::cop::CopConfig::default()
        };
        let (diagnostics, _) = crate::testutil::run_cop_autocorrect_with_config(
            &ReduceToHash,
            b"array.each_with_object({}) { |elem, hash| hash[elem.id] = elem.name }\n",
            config,
        );
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn indentation_follows_the_receiver_column() {
        assert_eq!(
            corrected(
                b"def foo\n  array.inject({}) do |hash, elem|\n    hash[elem.id] = elem.name\n    hash\n  end\nend\n"
            ),
            "def foo\n  array.to_h do |elem|\n    [elem.id, elem.name]\n  end\nend\n"
        );
    }
}
