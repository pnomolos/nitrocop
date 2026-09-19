use ruby_prism::Visit;

use crate::cop::shared::method_identifier_predicates::is_setter_method;
use crate::cop::shared::util::{begins_its_line, is_modifier_if, is_modifier_unless, is_ternary};
use crate::cop::{Cop, CopConfig};
use crate::diagnostic::Diagnostic;
use crate::parse::source::SourceFile;

/// Checks the indentation of the right hand side operand in binary operations
/// that span more than one line.
///
/// ## Corpus fix (2026-09-19) — faithful port pass
///
/// Starting point (fresh-fork oracle): default `aligned` 19 FP / 39 FN over
/// 47,080 matches; `EnforcedStyle: indented` 7 FP / 14 FN over 8,663 matches.
///
/// This cop shares RuboCop's `MultilineExpressionIndentation` mixin with
/// `Layout/MultilineMethodCallIndentation`, and — like that cop before its own
/// port pass — everything here had accumulated as text heuristics standing in
/// for mixin code that translates directly once the Prism/parser-gem node shape
/// differences are spelled out. The cop is now a line-by-line port of
/// `vendor/rubocop/lib/rubocop/cop/layout/multiline_operation_indentation.rb`
/// plus `lib/rubocop/cop/mixin/multiline_expression_indentation.rb`.
///
/// The whole algorithm fits in a paragraph:
///
/// ```text
/// offending_range(node, lhs, rhs, style):
///   return false unless begins_its_line?(rhs)
///   return false if not_for_this_cop?(node)
///   correct_column = should_align?(node, rhs, style)
///                      ? node.loc.column
///                      : indentation(lhs) + correct_indentation(node)
///   rhs unless correct_column == rhs.column
/// ```
///
/// Findings that drove the divergence, in rough order of corpus weight:
///
/// * **`indentation(node)` is `source_line =~ /\S/`, which counts tabs.**
///   `shared::util::indentation_of` counts spaces only, so every continuation
///   line in a tab-indented file was measured against column 0. This is the
///   whole metasm / mamiya / txt2html FN cluster (25 of the 39 default FNs):
///   those files indent with hard tabs, so RuboCop's expected column was
///   `1 (tab) + 2` while nitrocop computed `0 + 2` and then accepted the actual
///   column. Same root cause as the sibling cop's cluster 2.
/// * **`left_hand_side` is the identity for this cop.** It climbs
///   `while lhs.parent&.call_type? && lhs.parent.loc.dot`, and the parent of
///   `node.receiver` is always `node` itself, which by `relevant_node?` has no
///   dot. So `lhs` is literally `node.receiver` (sends) or `node.lhs`
///   (`and`/`or`), and `indentation(lhs)` is the indentation of the line that
///   operand starts on — no walking up visually continued lines, no
///   "previous continuation anchor", in any `EnforcedStyle`.
/// * **`should_align?` has no "method argument" *fallback*; it has an
///   `argument_in_method_call` *rule*, and it is the last one.** The old code
///   ordered it first and then only accepted `right_col == left_col`, which
///   both over- and under-fired. Ported order: assignment-RHS-begins-its-line
///   (in every style) → style must be `aligned` → keyword ancestor or
///   assignment ancestor → `argument_in_method_call` that is not a
///   `def` modifier.
/// * **`postfix_conditional?` is `node.if_type? && node.modifier_form?`.**
///   Modifier `while`/`until` are *not* postfix for this purpose, so
///   `foo while a &&\n  b` still gets the doubled `IndentationWidth`.
/// * **`UNALIGNED_RHS_TYPES` is `if while until for return array kwbegin`** —
///   `case` / `case ... in` are deliberately absent, so an assigned `case`
///   pulls its assignment base into the `when` branches. `kwbegin` is only a
///   real `begin ... end`: Prism also produces a `BeginNode` for a `def` or
///   block body that carries `rescue`/`ensure`, where the parser gem has no
///   wrapper node at all, so the port must require `begin_keyword_loc`.
/// * **`not_for_this_cop?`**: `grouped_expression?` is any `begin` node with a
///   `begin` location — Prism's `ParenthesesNode` *and* `EmbeddedStatementsNode`
///   (`#{ ... }`). `inside_arg_list_parentheses?` is `ancestor.send_type? &&
///   ancestor.parenthesized?`, i.e. a real `(` argument list only, never
///   `[...]`; Prism reports `opening_loc` for index calls too, so the delimiter
///   byte has to be checked. This replaces the source-scanning paren finder
///   that `EnforcedStyle: indented` still used, which counted parentheses
///   inside comments and string literals (the treetop and sup FPs).
/// * **`kw_node_with_special_indentation` skips ternaries** and matches only
///   when the node is inside `indented_keyword_expression(ancestor)` — the
///   condition for `if`/`unless`/`while`/`until`, the collection for `for`, the
///   arguments for `return`. It is an AST ancestor walk; the old lexical
///   "does the line start with `if `" scan produced the `elsif` / modifier
///   confusion behind the jenkins_api_client and mongoid_denormalize FNs.
/// * **`argument_in_method_call` breaks at the first `block` ancestor.** In
///   Prism a call carrying a block is one `CallNode` with a `BlockNode` child,
///   so the `BlockNode` is still reached first from inside the body and the
///   break is equivalent — but `numblock` / `itblock` are *separate parser
///   types* that `each_ancestor(:send, :block)` does not match, so a Prism
///   `BlockNode` whose parameters are `NumberedParametersNode` /
///   `ItParametersNode` must not break the walk (the same applies to
///   `disqualified_rhs?`'s `block_type?` test).
/// * **`relevant_node?` is not an operator allow-list.** RuboCop's filter is
///   "a `send` with a receiver, no dot, and a first argument", which also picks
///   up `[]=` and user-defined operators. Two parser-gem shapes have to be
///   subtracted instead of enumerated: `Builders::Default#match_op` emits
///   `match_with_lvasgn` (never a `send`) whenever the left operand is a
///   *static* regexp literal — `static_regexp_captures` returns an array, and
///   an empty array is truthy, so this is every non-interpolated regexp, not
///   just the named-capture ones; and `begin ... end while cond` is
///   `while_post` / `until_post`, a type that is in neither
///   `KEYWORD_ANCESTOR_TYPES` nor `UNALIGNED_RHS_TYPES` (Prism models it as a
///   `WhileNode` / `UntilNode` carrying the begin-modifier flag).
/// * **`SendNode#arguments` includes a block-pass argument.** Prism moves `&blk`
///   out of the argument list into `CallNode#block` as a `BlockArgumentNode`, so
///   `argument_in_method_call` misses `inputs.map &a >>\n  b` unless it is
///   put back.
/// * **Messages**: `used_indentation` is `rhs.column - indentation(lhs)` and
///   may be negative; `correct_indentation(node)` is the bare number (`Width`,
///   or `Width + Layout/IndentationWidth Width` for prefix keywords), not an
///   absolute column. `keyword_message_tail` uses `node.loc.keyword.source`, so
///   an `elsif` reports as `` `elsif` ``, and `return` reports as "a condition
///   in a `return` statement" (RuboCop only special-cases `for`).
///
/// ### Removed as redundant
///
/// `is_inside_parentheses_by_source_scan`, `keyword_context_on_line`,
/// `modifier_keyword`, `line_ends_with_assignment_operator`,
/// `has_assignment_before_col`, `line_ends_with_logical_operator`, the
/// `OPERATOR_METHODS` allow-list and the `OperatorContext` thread-local cache
/// they fed have no RuboCop counterpart and are subsumed by the ported ancestor
/// walks. The operator allow-list in particular was wrong in both directions:
/// RuboCop's filter is "a send with a receiver, no dot, and a first argument",
/// which also covers `[]=` and any user-defined operator. Do not reintroduce
/// them without corpus evidence.
///
/// ### Validation
///
/// 97 corpus repos cloned at their `bench/corpus/manifest.jsonl` SHAs — the 28
/// carrying the oracle's default and `indented` examples plus the rest of the
/// session's clone set as a regression guard — both tools run with
/// oracle-identical invocation and diffed on `(path, line)` exactly as
/// `bench/corpus/diff_results.py` does. On the 40-repo core subset the
/// pre-change binary reproduced the oracle's default numbers exactly
/// (19 FP / 39 FN, and per repo). After: default 0 FP / 0 FN over 3,591
/// matches, `indented` 0 FP / 0 FN over 4,417 matches, and 0 message
/// mismatches in either config.
pub struct MultilineOperationIndentation;

/// `correct_indentation` adds `Layout/IndentationWidth`'s `Width` on top of
/// this cop's own `IndentationWidth` for prefix keywords. nitrocop cops do not
/// see other cops' configuration; the corpus baseline and RuboCop's own default
/// both use 2. Same constant as the sibling cop uses for the same reason.
const LAYOUT_INDENTATION_WIDTH: usize = 2;

impl Cop for MultilineOperationIndentation {
    fn name(&self) -> &'static str {
        "Layout/MultilineOperationIndentation"
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
        let mut visitor = OperationVisitor {
            cop: self,
            source,
            aligned_style: config.get_str("EnforcedStyle", "aligned") == "aligned",
            width: config.get_usize("IndentationWidth", LAYOUT_INDENTATION_WIDTH),
            diagnostics: Vec::new(),
            ancestors: Vec::new(),
        };
        visitor.visit(&parse_result.node());
        diagnostics.extend(visitor.diagnostics);
    }
}

struct OperationVisitor<'a, 'pr> {
    cop: &'a MultilineOperationIndentation,
    source: &'a SourceFile,
    aligned_style: bool,
    width: usize,
    diagnostics: Vec<Diagnostic>,
    ancestors: Vec<ruby_prism::Node<'pr>>,
}

/// A byte range, standing in for a parser `Source::Range`.
#[derive(Clone, Copy)]
struct Span {
    start: usize,
    end: usize,
}

impl Span {
    fn of(node: &ruby_prism::Node<'_>) -> Self {
        let loc = node.location();
        Span {
            start: loc.start_offset(),
            end: loc.end_offset(),
        }
    }

    /// RuboCop's `within_node?(inner, outer)`, as `outer.contains(inner)`.
    fn contains(&self, inner: Span) -> bool {
        inner.start >= self.start && inner.end <= self.end
    }
}

/// The keyword ancestor found by `kw_node_with_special_indentation`.
struct KeywordNode {
    keyword: String,
    /// `postfix_conditional?`: `node.if_type? && node.modifier_form?`. Note
    /// that modifier `while` / `until` are *not* postfix conditionals.
    postfix: bool,
}

impl<'pr> Visit<'pr> for OperationVisitor<'_, 'pr> {
    fn visit_branch_node_enter(&mut self, node: ruby_prism::Node<'pr>) {
        self.ancestors.push(node);
    }

    fn visit_branch_node_leave(&mut self) {
        self.ancestors.pop();
    }

    fn visit_leaf_node_enter(&mut self, _node: ruby_prism::Node<'pr>) {}

    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
        self.check_send(node);
        ruby_prism::visit_call_node(self, node);
    }

    fn visit_and_node(&mut self, node: &ruby_prism::AndNode<'pr>) {
        // `on_and` → `check_and_or`: the LHS is used verbatim, not run through
        // `left_hand_side`.
        let current = node.as_node();
        self.check(&current, &node.left(), Span::of(&node.right()));
        ruby_prism::visit_and_node(self, node);
    }

    fn visit_or_node(&mut self, node: &ruby_prism::OrNode<'pr>) {
        let current = node.as_node();
        self.check(&current, &node.left(), Span::of(&node.right()));
        ruby_prism::visit_or_node(self, node);
    }
}

impl OperationVisitor<'_, '_> {
    /// `MultilineExpressionIndentation#on_send` plus this cop's
    /// `relevant_node?` and `right_hand_side`.
    fn check_send(&mut self, node: &ruby_prism::CallNode<'_>) {
        // `return if !node.receiver || node.method?(:[])`
        let Some(receiver) = node.receiver() else {
            return;
        };
        if node.name().as_slice() == b"[]" {
            return;
        }
        // `relevant_node?`: `!node.loc.dot`. Unary operations are excluded
        // explicitly there, but they carry no argument so the
        // `right_hand_side` guard below already covers them.
        if node.call_operator_loc().is_some() {
            return;
        }
        // `Builders::Default#match_op` emits `match_with_lvasgn`, not `send`,
        // whenever the left operand is a *static* regexp literal — the captured
        // names are declared as local variables, and an empty name list is
        // still truthy in Ruby, so this covers every non-interpolated regexp,
        // not just the ones with named captures. `on_send` never sees those,
        // while Prism keeps a plain `CallNode` unless there are named captures
        // (when it wraps it in a `MatchWriteNode`).
        if node.name().as_slice() == b"=~" && receiver.as_regular_expression_node().is_some() {
            return;
        }
        // `right_hand_side(send_node)` is `send_node.first_argument`.
        let Some(first_argument) = parser_arguments(node).into_iter().next() else {
            return;
        };

        let current = node.as_node();
        // `left_hand_side(node.receiver)` climbs
        // `while lhs.parent&.call_type? && lhs.parent.loc.dot` — the parent of
        // `node.receiver` is `node`, which has no dot, so the loop never runs.
        self.check(&current, &receiver, Span::of(&first_argument));
    }

    /// `offending_range` + `check` + `message`.
    fn check(&mut self, node: &ruby_prism::Node<'_>, lhs: &ruby_prism::Node<'_>, rhs: Span) {
        if !begins_its_line(self.source, rhs.start) {
            return;
        }
        if self.not_for_this_cop(node) {
            return;
        }

        let (rhs_line, rhs_col) = self.source.offset_to_line_col(rhs.start);
        let lhs_indent = self.indentation(lhs);
        let should_align = self.should_align(node, rhs);
        let correct_indentation = self.correct_indentation(node);
        let correct_column = if should_align {
            self.source
                .offset_to_line_col(node.location().start_offset())
                .1
        } else {
            lhs_indent + correct_indentation
        };
        if correct_column == rhs_col {
            return;
        }

        let what = self.operation_description(node, rhs);
        let message = if should_align {
            format!("Align the operands of {what} spanning multiple lines.")
        } else {
            let used = rhs_col as isize - lhs_indent as isize;
            format!(
                "Use {correct_indentation} (not {used}) spaces for indenting {what} spanning multiple lines."
            )
        };
        self.diagnostics
            .push(self.cop.diagnostic(self.source, rhs_line, rhs_col, message));
    }

    /// `Alignment#indentation(node)` → `node.source_range.source_line =~ /\S/`.
    /// Tabs count as one character, exactly like a parser-gem column.
    fn indentation(&self, node: &ruby_prism::Node<'_>) -> usize {
        let (line, _) = self
            .source
            .offset_to_line_col(node.location().start_offset());
        let bytes = self.source.lines().nth(line - 1).unwrap_or(b"");
        bytes
            .iter()
            .position(|&b| b != b' ' && b != b'\t')
            .unwrap_or(0)
    }

    /// `MultilineExpressionIndentation#correct_indentation`.
    fn correct_indentation(&self, node: &ruby_prism::Node<'_>) -> usize {
        match self.kw_node_with_special_indentation(node) {
            Some(kw) if !kw.postfix => self.width + LAYOUT_INDENTATION_WIDTH,
            _ => self.width,
        }
    }

    /// `MultilineOperationIndentation#should_align?`.
    fn should_align(&self, node: &ruby_prism::Node<'_>, rhs: Span) -> bool {
        let assignment_node = self.part_of_assignment_rhs(rhs);
        if let Some(assignment) = &assignment_node {
            // `CheckAssignment.extract_rhs`.
            if let Some(assignment_rhs) = extract_rhs(assignment) {
                if begins_its_line(self.source, assignment_rhs.start) {
                    return true;
                }
            }
        }

        if !self.aligned_style {
            return false;
        }
        if assignment_node.is_some() || self.kw_node_with_special_indentation(node).is_some() {
            return true;
        }

        // `argument_in_method_call(node, :with_or_without_parentheses)` followed
        // by `node.respond_to?(:def_modifier?) && !node.def_modifier?` — `nil`
        // and the `false` produced by the `block` break both answer `false`.
        self.argument_in_method_call(node)
            .is_some_and(|call| !is_def_modifier(&call))
    }

    /// `MultilineExpressionIndentation#operation_description`.
    fn operation_description(&self, node: &ruby_prism::Node<'_>, rhs: Span) -> String {
        if let Some(kw) = self.kw_node_with_special_indentation(node) {
            // `keyword_message_tail`.
            let kind = if kw.keyword == "for" {
                "collection"
            } else {
                "condition"
            };
            let article = if kw.keyword.starts_with('i') || kw.keyword.starts_with('u') {
                "an"
            } else {
                "a"
            };
            return format!("a {kind} in {article} `{}` statement", kw.keyword);
        }
        if self.part_of_assignment_rhs(rhs).is_some() {
            return "an expression in an assignment".to_string();
        }
        "an expression".to_string()
    }

    /// `MultilineExpressionIndentation#not_for_this_cop?`.
    fn not_for_this_cop(&self, node: &ruby_prism::Node<'_>) -> bool {
        let span = Span::of(node);
        self.ancestors.iter().rev().skip(1).any(|ancestor| {
            is_grouped_expression(ancestor) || self.inside_arg_list_parentheses(span, ancestor)
        })
    }

    /// `inside_arg_list_parentheses?`: `ancestor.send_type? &&
    /// ancestor.parenthesized?`, and strictly inside the parentheses.
    ///
    /// Prism reports `opening_loc` / `closing_loc` for `foo[bar]` and
    /// `foo[bar] = baz` as well, so the delimiter byte has to be checked —
    /// `SendNode#parenthesized?` is `loc.begin&.is?('(')`.
    fn inside_arg_list_parentheses(&self, span: Span, ancestor: &ruby_prism::Node<'_>) -> bool {
        let Some(call) = ancestor.as_call_node() else {
            return false;
        };
        let (Some(opening), Some(closing)) = (call.opening_loc(), call.closing_loc()) else {
            return false;
        };
        if self.source.as_bytes().get(opening.start_offset()) != Some(&b'(') {
            return false;
        }
        span.start > opening.start_offset() && span.end < closing.end_offset()
    }

    /// `MultilineExpressionIndentation#kw_node_with_special_indentation`.
    fn kw_node_with_special_indentation(&self, node: &ruby_prism::Node<'_>) -> Option<KeywordNode> {
        let span = Span::of(node);
        for ancestor in self.ancestors.iter().rev().skip(1) {
            if let Some(if_node) = ancestor.as_if_node() {
                if is_ternary(&if_node) {
                    continue;
                }
                if Span::of(&if_node.predicate()).contains(span) {
                    return Some(KeywordNode {
                        keyword: self.keyword_source(if_node.if_keyword_loc()),
                        postfix: is_modifier_if(&if_node),
                    });
                }
            } else if let Some(unless_node) = ancestor.as_unless_node() {
                if Span::of(&unless_node.predicate()).contains(span) {
                    return Some(KeywordNode {
                        keyword: "unless".to_string(),
                        postfix: is_modifier_unless(&unless_node),
                    });
                }
            } else if let Some(while_node) = ancestor.as_while_node() {
                if while_node.is_begin_modifier() {
                    continue;
                }
                if Span::of(&while_node.predicate()).contains(span) {
                    return Some(KeywordNode {
                        keyword: "while".to_string(),
                        postfix: false,
                    });
                }
            } else if let Some(until_node) = ancestor.as_until_node() {
                if until_node.is_begin_modifier() {
                    continue;
                }
                if Span::of(&until_node.predicate()).contains(span) {
                    return Some(KeywordNode {
                        keyword: "until".to_string(),
                        postfix: false,
                    });
                }
            } else if let Some(for_node) = ancestor.as_for_node() {
                if Span::of(&for_node.collection()).contains(span) {
                    return Some(KeywordNode {
                        keyword: "for".to_string(),
                        postfix: false,
                    });
                }
            } else if let Some(return_node) = ancestor.as_return_node() {
                // `indented_keyword_expression` is `node.children.first`, which
                // for a `return` is the single value, or the implicit `array`
                // of values — both have the span of Prism's `ArgumentsNode`.
                if let Some(arguments) = return_node.arguments() {
                    if Span::of(&arguments.as_node()).contains(span) {
                        return Some(KeywordNode {
                            keyword: "return".to_string(),
                            postfix: false,
                        });
                    }
                }
            }
        }
        None
    }

    fn keyword_source(&self, loc: Option<ruby_prism::Location<'_>>) -> String {
        loc.map(|l| {
            String::from_utf8_lossy(&self.source.as_bytes()[l.start_offset()..l.end_offset()])
                .into_owned()
        })
        .unwrap_or_else(|| "if".to_string())
    }

    /// `MultilineExpressionIndentation#part_of_assignment_rhs`. Returns the
    /// assignment-like ancestor, as a `(kind, span)` pair so the borrow of the
    /// ancestor stack does not escape.
    fn part_of_assignment_rhs(&self, candidate: Span) -> Option<AssignmentAncestor> {
        for ancestor in self.ancestors.iter().rev().skip(1) {
            if disqualified_rhs(candidate, ancestor) {
                return None;
            }
            if let Some(found) = valid_rhs(candidate, ancestor) {
                return Some(found);
            }
        }
        None
    }

    /// `MultilineExpressionIndentation#argument_in_method_call`, reduced to the
    /// `:with_or_without_parentheses` kind this cop uses. Returns the span of
    /// the enclosing call so the borrow does not escape.
    fn argument_in_method_call(&self, node: &ruby_prism::Node<'_>) -> Option<MethodCallArgument> {
        let span = Span::of(node);
        for ancestor in self.ancestors.iter().rev().skip(1) {
            if is_parser_block(ancestor) {
                // `break false if a.block_type?`
                return None;
            }
            let Some(call) = ancestor.as_call_node() else {
                continue;
            };
            if is_setter_method(call.name().as_slice()) {
                continue;
            }
            let contains = parser_arguments(&call)
                .iter()
                .any(|arg| Span::of(arg).contains(span));
            if contains {
                return Some(MethodCallArgument {
                    receiverless: call.receiver().is_none(),
                    first_argument_is_def: first_argument_chain_is_def(&call),
                });
            }
        }
        None
    }
}

/// What `part_of_assignment_rhs` needs to report back: enough to answer
/// `CheckAssignment.extract_rhs` without keeping the node borrow alive.
struct AssignmentAncestor {
    /// Span of `extract_rhs(assignment_node)`, when there is one.
    rhs: Option<Span>,
}

/// What `should_align?` needs from `argument_in_method_call`.
struct MethodCallArgument {
    receiverless: bool,
    first_argument_is_def: bool,
}

fn extract_rhs(assignment: &AssignmentAncestor) -> Option<Span> {
    assignment.rhs
}

/// `MethodDispatchNode#def_modifier?`: `private def foo; end`. The receiver
/// must be absent and the first argument must be a `def`/`defs`, possibly
/// through further modifier sends.
fn is_def_modifier(call: &MethodCallArgument) -> bool {
    call.receiverless && call.first_argument_is_def
}

/// `SendNode#arguments`. The parser gem keeps a block-pass argument (`&blk`) in
/// the argument list, where Prism moves it out to `CallNode#block` as a
/// `BlockArgumentNode`. `argument_in_method_call` checks `a.arguments.any?`, so
/// `inputs.map &a >>\n  b` has to see the block-pass as an argument of `map`.
fn parser_arguments<'pr>(call: &ruby_prism::CallNode<'pr>) -> Vec<ruby_prism::Node<'pr>> {
    let mut arguments: Vec<ruby_prism::Node<'pr>> = call
        .arguments()
        .map(|args| args.arguments().iter().collect())
        .unwrap_or_default();
    if let Some(block) = call.block() {
        if block.as_block_argument_node().is_some() {
            arguments.push(block);
        }
    }
    arguments
}

fn first_argument_chain_is_def(call: &ruby_prism::CallNode<'_>) -> bool {
    let Some(arg) = parser_arguments(call).into_iter().next() else {
        return false;
    };
    if arg.as_def_node().is_some() {
        return true;
    }
    let Some(inner) = arg.as_call_node() else {
        return false;
    };
    if inner.receiver().is_some() {
        return false;
    }
    first_argument_chain_is_def(&inner)
}

/// `grouped_expression?`: a parser `begin` node that has a `begin` location.
/// That is `( ... )` grouping and `#{ ... }` interpolation. A `begin ... end`
/// block is a `kwbegin` node and does not qualify.
fn is_grouped_expression(node: &ruby_prism::Node<'_>) -> bool {
    node.as_parentheses_node().is_some() || node.as_embedded_statements_node().is_some()
}

/// `disqualified_rhs?`.
fn disqualified_rhs(candidate: Span, ancestor: &ruby_prism::Node<'_>) -> bool {
    if is_unaligned_rhs_type(ancestor) {
        return true;
    }
    // `ancestor.block_type? && part_of_block_body?(candidate, ancestor)`
    if let Some(body) = parser_block_body(ancestor) {
        return Span::of(&body).contains(candidate);
    }
    false
}

/// `UNALIGNED_RHS_TYPES = %i[if while until for return array kwbegin]`.
///
/// `case` and `case ... in` are deliberately absent, so the assignment base of
/// `x = case k ... when ... "a" +\n"b"` reaches the `when` branches.
fn is_unaligned_rhs_type(node: &ruby_prism::Node<'_>) -> bool {
    if node.as_if_node().is_some()
        || node.as_unless_node().is_some()
        || node.as_for_node().is_some()
        || node.as_return_node().is_some()
        || node.as_array_node().is_some()
    {
        return true;
    }
    // `begin ... end while cond` is `while_post` / `until_post` in the parser
    // gem, which is in neither `UNALIGNED_RHS_TYPES` nor
    // `KEYWORD_ANCESTOR_TYPES`. Prism models it as a `WhileNode` / `UntilNode`
    // carrying the begin-modifier flag.
    if let Some(while_node) = node.as_while_node() {
        return !while_node.is_begin_modifier();
    }
    if let Some(until_node) = node.as_until_node() {
        return !until_node.is_begin_modifier();
    }
    // `kwbegin` is only a literal `begin ... end`. Prism also uses `BeginNode`
    // for a `def` or block body carrying `rescue` / `ensure`, which the parser
    // gem represents with no wrapper node at all.
    node.as_begin_node()
        .is_some_and(|begin_node| begin_node.begin_keyword_loc().is_some())
}

/// The body of a parser `block` node, or `None` when `node` is not one.
///
/// A parser `block` is Prism's `BlockNode` (`{ ... }` / `do ... end`) or
/// `LambdaNode` (`-> { ... }`). Blocks using numbered parameters or `it` are
/// `numblock` / `itblock` in the parser gem — separate types that neither
/// `disqualified_rhs?` nor `argument_in_method_call` match.
fn parser_block_body<'pr>(node: &ruby_prism::Node<'pr>) -> Option<ruby_prism::Node<'pr>> {
    if let Some(block) = node.as_block_node() {
        if has_implicit_block_parameters(&block.parameters()) {
            return None;
        }
        return block.body();
    }
    if let Some(lambda) = node.as_lambda_node() {
        return lambda.body();
    }
    None
}

fn is_parser_block(node: &ruby_prism::Node<'_>) -> bool {
    if let Some(block) = node.as_block_node() {
        return !has_implicit_block_parameters(&block.parameters());
    }
    node.as_lambda_node().is_some()
}

fn has_implicit_block_parameters(parameters: &Option<ruby_prism::Node<'_>>) -> bool {
    parameters.as_ref().is_some_and(|params| {
        params.as_numbered_parameters_node().is_some() || params.as_it_parameters_node().is_some()
    })
}

/// `valid_rhs?`.
fn valid_rhs(candidate: Span, ancestor: &ruby_prism::Node<'_>) -> Option<AssignmentAncestor> {
    if let Some(call) = ancestor.as_call_node() {
        // `valid_method_rhs_candidate?`: `node.setter_method? &&
        // valid_rhs_candidate?(candidate, node.last_argument)`. For a setter
        // call, `CheckAssignment.extract_rhs` is also `last_argument`.
        if !is_setter_method(call.name().as_slice()) {
            return None;
        }
        let last = parser_arguments(&call).pop()?;
        let last_span = Span::of(&last);
        return last_span.contains(candidate).then_some(AssignmentAncestor {
            rhs: Some(last_span),
        });
    }
    let rhs = assignment_rhs(ancestor)?;
    let rhs_span = Span::of(&rhs);
    rhs_span.contains(candidate).then_some(AssignmentAncestor {
        rhs: Some(rhs_span),
    })
}

/// `assignment_rhs(node)` for parser `assignment?` nodes — every one of them is
/// a Prism write node whose `value` is the right hand side, which is also what
/// `CheckAssignment.extract_rhs`'s `node.expression` returns.
///
/// `ASSIGNMENTS = %i[lvasgn ivasgn cvasgn gvasgn casgn masgn op_asgn or_asgn
/// and_asgn]`. Prism splits `op_asgn` / `or_asgn` / `and_asgn` per target kind,
/// so the list below is that cross product.
fn assignment_rhs<'pr>(node: &ruby_prism::Node<'pr>) -> Option<ruby_prism::Node<'pr>> {
    macro_rules! try_write {
        ($($accessor:ident),* $(,)?) => {
            $(
                if let Some(write) = node.$accessor() {
                    return Some(write.value());
                }
            )*
        };
    }
    try_write!(
        as_local_variable_write_node,
        as_local_variable_operator_write_node,
        as_local_variable_and_write_node,
        as_local_variable_or_write_node,
        as_instance_variable_write_node,
        as_instance_variable_operator_write_node,
        as_instance_variable_and_write_node,
        as_instance_variable_or_write_node,
        as_class_variable_write_node,
        as_class_variable_operator_write_node,
        as_class_variable_and_write_node,
        as_class_variable_or_write_node,
        as_global_variable_write_node,
        as_global_variable_operator_write_node,
        as_global_variable_and_write_node,
        as_global_variable_or_write_node,
        as_constant_write_node,
        as_constant_operator_write_node,
        as_constant_and_write_node,
        as_constant_or_write_node,
        as_constant_path_write_node,
        as_constant_path_operator_write_node,
        as_constant_path_and_write_node,
        as_constant_path_or_write_node,
        as_index_operator_write_node,
        as_index_and_write_node,
        as_index_or_write_node,
        as_call_operator_write_node,
        as_call_and_write_node,
        as_call_or_write_node,
        as_multi_write_node,
    );
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::run_cop_full;

    crate::cop_fixture_tests!(
        MultilineOperationIndentation,
        "cops/layout/multiline_operation_indentation"
    );
    crate::cop_variant_fixture_tests!(
        MultilineOperationIndentation,
        "cops/layout/multiline_operation_indentation",
        indented
    );

    #[test]
    fn single_line_operation_ignored() {
        let source = b"x = 1 + 2\n";
        let diags = run_cop_full(&MultilineOperationIndentation, source);
        assert!(diags.is_empty());
    }

    #[test]
    fn or_in_def_body_no_offense() {
        let src = b"def valid?(user)\n  user.foo ||\n    user.bar\nend\n";
        let diags = run_cop_full(&MultilineOperationIndentation, src);
        assert!(
            diags.is_empty(),
            "correctly indented || continuation should not flag, got: {:?}",
            diags.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn or_in_def_body_with_rescue_no_offense() {
        let src = b"  def valid_otp_attempt?(user)\n    user.validate_and_consume_otp!(user_params[:otp_attempt]) ||\n      user.invalidate_otp_backup_code!(user_params[:otp_attempt])\n  rescue OpenSSL::Cipher::CipherError\n    false\n  end\n";
        let diags = run_cop_full(&MultilineOperationIndentation, src);
        assert!(
            diags.is_empty(),
            "correctly indented || with rescue should not flag, got: {:?}",
            diags
                .iter()
                .map(|d| format!(
                    "line {} col {} {}",
                    d.location.line, d.location.column, d.message
                ))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn tab_indented_continuation_measures_from_the_tab() {
        // `indentation` is `source_line =~ /\S/`, so a leading tab counts as
        // one column and the expected indentation here is 1 + 2 = 3.
        let src = b"def foo\n\tbaz ||\n\t  qux\nend\n";
        let diags = run_cop_full(&MultilineOperationIndentation, src);
        assert!(
            diags.is_empty(),
            "tab-indented continuation should measure from the tab, got: {:?}",
            diags.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn aligned_style() {
        use crate::testutil::run_cop_full_with_config;
        use std::collections::HashMap;

        let config = CopConfig {
            options: HashMap::from([(
                "EnforcedStyle".into(),
                serde_yml::Value::String("aligned".into()),
            )]),
            ..CopConfig::default()
        };
        // Aligned with left operand in keyword condition (should_align = true)
        let src = b"if a &&\n   b\n  c\nend\n";
        let diags = run_cop_full_with_config(&MultilineOperationIndentation, src, config.clone());
        assert!(
            diags.is_empty(),
            "aligned style in keyword condition should accept operand-aligned continuation, got: {:?}",
            diags
                .iter()
                .map(|d| format!("L{}:C{} {}", d.location.line, d.location.column, &d.message))
                .collect::<Vec<_>>()
        );

        // In "aligned" style, ordinary statements still use indentation.
        let src2 = b"a &&\n  b\n";
        let diags2 = run_cop_full_with_config(&MultilineOperationIndentation, src2, config.clone());
        assert!(
            diags2.is_empty(),
            "aligned style should accept indented continuation in non-condition contexts"
        );

        // Assignment RHS uses aligned operands in aligned style.
        let src3 = b"x = a &&\n    b\n";
        let diags3 = run_cop_full_with_config(&MultilineOperationIndentation, src3, config.clone());
        assert!(
            diags3.is_empty(),
            "aligned style should accept operand-aligned continuation in assignments"
        );

        let src4 = b"x = a &&\n  b\n";
        let diags4 = run_cop_full_with_config(&MultilineOperationIndentation, src4, config);
        assert_eq!(
            diags4.len(),
            1,
            "aligned style should flag indented assignment continuations"
        );
    }

    #[test]
    fn aligned_style_accepts_modifier_keyword_alignment() {
        use crate::testutil::run_cop_full_with_config;
        use std::collections::HashMap;

        let src = b"def f\n  return if receiver.nil? &&\n            args.empty?\nend\n";
        let diags = run_cop_full_with_config(
            &MultilineOperationIndentation,
            src,
            CopConfig {
                options: HashMap::from([(
                    "EnforcedStyle".into(),
                    serde_yml::Value::String("aligned".into()),
                )]),
                ..CopConfig::default()
            },
        );
        assert!(
            diags.is_empty(),
            "modifier keyword conditions should align like RuboCop, got: {:?}",
            diags
                .iter()
                .map(|d| format!("L{}:C{} {}", d.location.line, d.location.column, &d.message))
                .collect::<Vec<_>>()
        );
    }
}
