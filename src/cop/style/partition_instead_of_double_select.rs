use ruby_prism::Visit;

use crate::cop::shared::node_type::{
    BLOCK_NODE, CALL_AND_WRITE_NODE, CALL_OPERATOR_WRITE_NODE, CALL_OR_WRITE_NODE,
    CLASS_VARIABLE_AND_WRITE_NODE, CLASS_VARIABLE_OPERATOR_WRITE_NODE,
    CLASS_VARIABLE_OR_WRITE_NODE, CLASS_VARIABLE_WRITE_NODE, CONSTANT_AND_WRITE_NODE,
    CONSTANT_OPERATOR_WRITE_NODE, CONSTANT_OR_WRITE_NODE, CONSTANT_PATH_AND_WRITE_NODE,
    CONSTANT_PATH_OPERATOR_WRITE_NODE, CONSTANT_PATH_OR_WRITE_NODE, CONSTANT_PATH_WRITE_NODE,
    CONSTANT_WRITE_NODE, GLOBAL_VARIABLE_AND_WRITE_NODE, GLOBAL_VARIABLE_OPERATOR_WRITE_NODE,
    GLOBAL_VARIABLE_OR_WRITE_NODE, GLOBAL_VARIABLE_WRITE_NODE, INDEX_AND_WRITE_NODE,
    INDEX_OPERATOR_WRITE_NODE, INDEX_OR_WRITE_NODE, INSTANCE_VARIABLE_AND_WRITE_NODE,
    INSTANCE_VARIABLE_OPERATOR_WRITE_NODE, INSTANCE_VARIABLE_OR_WRITE_NODE,
    INSTANCE_VARIABLE_WRITE_NODE, IT_PARAMETERS_NODE, LOCAL_VARIABLE_AND_WRITE_NODE,
    LOCAL_VARIABLE_OPERATOR_WRITE_NODE, LOCAL_VARIABLE_OR_WRITE_NODE, LOCAL_VARIABLE_WRITE_NODE,
    MULTI_WRITE_NODE, NUMBERED_PARAMETERS_NODE, node_type_tag,
};
use crate::cop::{Cop, CopConfig};
use crate::correction::Correction;
use crate::diagnostic::Diagnostic;
use crate::parse::codemap::CodeMap;
use crate::parse::source::SourceFile;

const COP_NAME: &str = "Style/PartitionInsteadOfDoubleSelect";
const SELECT_METHODS: [&[u8]; 3] = [b"select", b"filter", b"find_all"];

/// Checks for consecutive `select`/`filter`/`find_all` and `reject` calls on the
/// same receiver with the same block body, where `partition` would do
/// (`Style/PartitionInsteadOfDoubleSelect`, new in RuboCop 1.85,
/// `Enabled: pending`, `Safe: false`). Also catches two `select`s (or two
/// `reject`s) where one block negates the other with `!`.
///
/// The cop is `Safe: false` (not `SafeAutoCorrect: false`): `Hash#partition`
/// returns nested arrays rather than a hash, a receiver with side effects is
/// evaluated once instead of twice, and a custom `select`/`reject` need not come
/// with a matching `partition`. That is a property of the cop, so it stays in
/// config rather than in `safe_autocorrect()`.
///
/// The two calls must be *sibling statements*: RuboCop's `node_container` only
/// accepts a candidate whose parent is a `begin` node, or an assignment whose
/// own parent is a `begin` node, and then pairs it with `left_sibling`.
/// Autocorrection additionally requires both statements to be `lvasgn` — an
/// ivar/gvar/constant target, or a bare call with no assignment, still reports
/// but is left alone.
///
/// ## RuboCop quirks replicated
///
/// - **`begin ... end` bodies are skipped.** Parser calls an explicit
///   `begin`/`end` a `kwbegin`, not a `begin`, so `node_container` rejects its
///   direct children and the cop never fires there — while a `def` body with two
///   statements *is* a `begin` and does fire. Prism uses `BeginNode` for both,
///   distinguished here by the handler clauses: a `BeginNode` carrying
///   `rescue`/`else`/`ensure` corresponds to Parser's `(rescue (begin …) …)`
///   (so its statements are scanned), while one without them is a plain
///   `kwbegin` (so they are not). This is the same rule
///   `Style/CombinableLoops` documents.
/// - **Block "type" equality is strict.** `same_block_contents?` and
///   `negated_predicate?` both start with `block1.type == block2.type`, so a
///   `{ |x| … }` never matches a `{ _1 … }` or a `{ it … }` even with an
///   identical body. Parser has three node types here (`block`, `numblock`,
///   `itblock`); Prism has one `BlockNode` whose `parameters()` is
///   `BlockParametersNode`, `NumberedParametersNode` or `ItParametersNode`, so
///   the kind is read off the parameters instead. A block with no parameters at
///   all is Parser's `block` with an empty `(args)`, so it counts as regular.
/// - **`symbol_proc_method?`** is `(block _ (args (arg _name)) (send (lvar
///   _name) $_method_name))`: exactly one plain required parameter, and a body
///   that is a bare receiver-and-message send with no arguments. `&.` is a
///   `csend` and does not match the pattern.
///
/// ## Prism-vs-Parser notes
///
/// - Parser's `block` node wraps the send; Prism's `CallNode` owns the block and
///   its location already spans `receiver.method { … }`, so the `CallNode` plays
///   the role of both RuboCop's `block` node and its `send_node`
///   (`loc.selector` becomes `message_loc()`).
/// - `&:sym` is Parser's last *argument* (a `block_pass` node) but Prism's
///   `CallNode#block` (a `BlockArgumentNode`), so `last_argument&.block_pass_type?`
///   becomes "the call's block slot holds a `BlockArgumentNode`".
/// - **AST equality is approximated by a normalised source key.** RuboCop
///   compares receivers, block parameters, block bodies and `&:sym` arguments
///   with Parser's `Node#==`, which ignores formatting. nitrocop has no
///   structural node equality, so this cop compares source text with whitespace
///   dropped wherever `CodeMap` says the offset is code and with comment spans
///   removed — `{ |x| x>0 }` and `{ |x| x > 0 }` therefore still match. Two
///   residual gaps, both false negatives: literals that differ only in spelling
///   (`"x"` vs `'x'`) compare unequal, and a comment whose text differs inside
///   an otherwise identical block also compares unequal.
pub struct PartitionInsteadOfDoubleSelect;

impl Cop for PartitionInsteadOfDoubleSelect {
    fn name(&self) -> &'static str {
        COP_NAME
    }

    fn supports_autocorrect(&self) -> bool {
        true
    }

    fn check_source(
        &self,
        source: &SourceFile,
        parse_result: &ruby_prism::ParseResult<'_>,
        code_map: &CodeMap,
        _config: &CopConfig,
        diagnostics: &mut Vec<Diagnostic>,
        corrections: Option<&mut Vec<Correction>>,
    ) {
        let mut walker = Walker {
            cop: self,
            source,
            code_map,
            diagnostics,
            corrections,
        };
        walker.visit(&parse_result.node());
    }
}

struct Walker<'a, 'd> {
    cop: &'a PartitionInsteadOfDoubleSelect,
    source: &'a SourceFile,
    code_map: &'a CodeMap,
    diagnostics: &'d mut Vec<Diagnostic>,
    corrections: Option<&'d mut Vec<Correction>>,
}

impl<'pr> Visit<'pr> for Walker<'_, '_> {
    /// Prism's generated visitors reach a `StatementsNode` through
    /// `visit_statements_node` rather than through the `Node` dispatcher, so the
    /// `visit_branch_node_enter` hook never fires for the program's own
    /// statement list. Override the typed visitor instead.
    fn visit_statements_node(&mut self, node: &ruby_prism::StatementsNode<'pr>) {
        self.check_statements(node);
        for child in &node.body() {
            self.visit(&child);
        }
    }

    /// A plain `begin`/`end` is Parser's `kwbegin`, whose children are
    /// statements in their own right rather than a `begin` sequence, so its
    /// statement list is walked without being paired up.
    fn visit_begin_node(&mut self, node: &ruby_prism::BeginNode<'pr>) {
        if is_plain_kwbegin(node) {
            if let Some(statements) = node.statements() {
                for child in &statements.body() {
                    self.visit(&child);
                }
            }
            return;
        }
        ruby_prism::visit_begin_node(self, node);
    }
}

fn is_plain_kwbegin(node: &ruby_prism::BeginNode<'_>) -> bool {
    node.rescue_clause().is_none() && node.else_clause().is_none() && node.ensure_clause().is_none()
}

impl Walker<'_, '_> {
    fn check_statements(&mut self, statements: &ruby_prism::StatementsNode<'_>) {
        let body: Vec<ruby_prism::Node<'_>> = statements.body().iter().collect();
        for index in 1..body.len() {
            self.check_pair(&body[index - 1], &body[index]);
        }
    }

    fn check_pair(&mut self, previous: &ruby_prism::Node<'_>, current: &ruby_prism::Node<'_>) {
        let Some(node) = dispatch_candidate(current) else {
            return;
        };
        let Some(sibling) = extract_candidate(previous) else {
            return;
        };
        if !self.same_receiver(&node, &sibling) || !self.matching_pair(&node, &sibling) {
            return;
        }

        let loc = current.location();
        let (line, column) = self.source.offset_to_line_col(loc.start_offset());
        let mut diagnostic = self.cop.diagnostic(
            self.source,
            line,
            column,
            format!(
                "Use `partition` instead of consecutive `{}` and `{}` calls.",
                String::from_utf8_lossy(sibling.method_name()),
                String::from_utf8_lossy(node.method_name()),
            ),
        );

        if let Some(edits) = self.autocorrect(&node, &sibling, current, previous)
            && let Some(sink) = self.corrections.as_deref_mut()
        {
            sink.extend(edits);
            diagnostic.corrected = true;
        }
        self.diagnostics.push(diagnostic);
    }

    fn key(&self, loc: &ruby_prism::Location<'_>) -> Vec<u8> {
        normalized_key(
            self.source,
            self.code_map,
            loc.start_offset(),
            loc.end_offset(),
        )
    }

    fn node_key(&self, node: &ruby_prism::Node<'_>) -> Vec<u8> {
        self.key(&node.location())
    }

    fn same_receiver(&self, node: &Candidate<'_>, sibling: &Candidate<'_>) -> bool {
        match (node.call.receiver(), sibling.call.receiver()) {
            (None, None) => true,
            (Some(a), Some(b)) => self.node_key(&a) == self.node_key(&b),
            _ => false,
        }
    }

    fn matching_pair(&self, node: &Candidate<'_>, sibling: &Candidate<'_>) -> bool {
        (complementary_pair(node, sibling) && self.equivalent_predicate(node, sibling))
            || (node.method_name() == sibling.method_name()
                && self.negated_predicate(node, sibling))
    }

    fn equivalent_predicate(&self, node: &Candidate<'_>, sibling: &Candidate<'_>) -> bool {
        match (node.block(), sibling.block()) {
            (Some(a), Some(b)) => self.same_block_contents(&a, &b),
            (Some(a), None) => self.block_matches_block_pass(&a, sibling),
            (None, Some(b)) => self.block_matches_block_pass(&b, node),
            (None, None) => match (node.block_argument(), sibling.block_argument()) {
                (Some(a), Some(b)) => self.key(&a.location()) == self.key(&b.location()),
                _ => false,
            },
        }
    }

    fn same_block_contents(
        &self,
        first: &ruby_prism::BlockNode<'_>,
        second: &ruby_prism::BlockNode<'_>,
    ) -> bool {
        if block_kind(first) != block_kind(second) {
            return false;
        }
        if block_kind(first) == BlockKind::Regular
            && self.parameters_key(first) != self.parameters_key(second)
        {
            return false;
        }
        self.body_key(first) == self.body_key(second)
    }

    fn parameters_key(&self, block: &ruby_prism::BlockNode<'_>) -> Option<Vec<u8>> {
        block.parameters().map(|p| self.key(&p.location()))
    }

    fn body_key(&self, block: &ruby_prism::BlockNode<'_>) -> Option<Vec<u8>> {
        block.body().map(|b| self.node_key(&b))
    }

    /// `block_matches_block_pass?`: the block is a `{ |x| x.foo }` symbol proc
    /// and the other call passes `&:foo`.
    fn block_matches_block_pass(
        &self,
        block: &ruby_prism::BlockNode<'_>,
        other: &Candidate<'_>,
    ) -> bool {
        let Some(method_name) = symbol_proc_method(block) else {
            return false;
        };
        let Some(block_argument) = other.block_argument() else {
            return false;
        };
        let Some(expression) = block_argument.expression() else {
            return false;
        };
        expression
            .as_symbol_node()
            .and_then(|s| s.unescaped().to_vec().into())
            .is_some_and(|value: Vec<u8>| value == method_name)
    }

    fn negated_predicate(&self, node: &Candidate<'_>, sibling: &Candidate<'_>) -> bool {
        let (Some(first), Some(second)) = (node.block(), sibling.block()) else {
            return false;
        };
        if block_kind(&first) != block_kind(&second) {
            return false;
        }
        if block_kind(&first) == BlockKind::Regular
            && self.parameters_key(&first) != self.parameters_key(&second)
        {
            return false;
        }
        self.negated_body(&first, &second) || self.negated_body(&second, &first)
    }

    /// `negated_body?`: `first`'s body is `!X` and `X` equals `second`'s body.
    fn negated_body(
        &self,
        first: &ruby_prism::BlockNode<'_>,
        second: &ruby_prism::BlockNode<'_>,
    ) -> bool {
        let Some(body) = block_body_expression(first) else {
            return false;
        };
        let Some(call) = body.as_call_node() else {
            return false;
        };
        if call.name().as_slice() != b"!" {
            return false;
        }
        let Some(receiver) = call.receiver() else {
            return false;
        };
        let Some(other) = block_body_expression(second) else {
            return false;
        };
        self.node_key(&receiver) == self.node_key(&other)
    }

    // -- autocorrection ------------------------------------------------------

    fn autocorrect(
        &self,
        node: &Candidate<'_>,
        sibling: &Candidate<'_>,
        container: &ruby_prism::Node<'_>,
        sibling_container: &ruby_prism::Node<'_>,
    ) -> Option<Vec<Correction>> {
        let container_var = local_variable_target(container)?;
        let sibling_var = local_variable_target(sibling_container)?;

        let (select_var, reject_var, partition_call) = if complementary_pair(node, sibling) {
            let partition = if is_select_method(sibling.method_name()) {
                sibling
            } else {
                node
            };
            let order = if is_select_method(sibling.method_name()) {
                (sibling_var, container_var)
            } else {
                (container_var, sibling_var)
            };
            (order.0, order.1, partition)
        } else {
            // `negation_partition_args`: the non-negated block is the partition
            // receiver, and whichever side is truthy names the first variable.
            let (Some(node_block), Some(sibling_block)) = (node.block(), sibling.block()) else {
                return None;
            };
            let node_is_negated = self.negated_body(&node_block, &sibling_block);
            let node_is_truthy = is_select_method(node.method_name()) != node_is_negated;
            let partition = if node_is_negated { sibling } else { node };
            if node_is_truthy {
                (container_var, sibling_var, partition)
            } else {
                (sibling_var, container_var, partition)
            }
        };

        let replacement = format!(
            "{}, {} = {}",
            String::from_utf8_lossy(select_var),
            String::from_utf8_lossy(reject_var),
            self.build_partition_call(partition_call)?
        );

        let sibling_loc = sibling_container.location();
        let (line, _) = self
            .source
            .offset_to_line_col(container.location().start_offset());
        let (last_line, _) = self
            .source
            .offset_to_line_col(container.location().end_offset().saturating_sub(1));
        let removal_start = self.source.line_start_offset(line);
        let next_line_start = self.source.line_start_offset(last_line + 1);
        let removal_end = if next_line_start > removal_start {
            next_line_start.min(self.source.as_bytes().len())
        } else {
            self.source.as_bytes().len()
        };

        Some(vec![
            Correction {
                start: sibling_loc.start_offset(),
                end: sibling_loc.end_offset(),
                replacement,
                cop_name: COP_NAME,
                cop_index: 0,
            },
            Correction {
                start: removal_start,
                end: removal_end,
                replacement: String::new(),
                cop_name: COP_NAME,
                cop_index: 0,
            },
        ])
    }

    /// The candidate's source with its selector swapped for `partition`.
    fn build_partition_call(&self, candidate: &Candidate<'_>) -> Option<String> {
        let loc = candidate.call.location();
        let selector = candidate.call.message_loc()?;
        let head = self
            .source
            .try_byte_slice(loc.start_offset(), selector.start_offset())?;
        let tail = self
            .source
            .try_byte_slice(selector.end_offset(), loc.end_offset())?;
        Some(format!("{head}partition{tail}"))
    }
}

/// A `select`-family/`reject` call carrying either a block or a `&:sym`.
struct Candidate<'pr> {
    call: ruby_prism::CallNode<'pr>,
}

impl<'pr> Candidate<'pr> {
    fn method_name(&self) -> &'pr [u8] {
        self.call.name().as_slice()
    }

    fn block(&self) -> Option<ruby_prism::BlockNode<'pr>> {
        self.call.block()?.as_block_node()
    }

    fn block_argument(&self) -> Option<ruby_prism::BlockArgumentNode<'pr>> {
        self.call.block()?.as_block_argument_node()
    }
}

/// `on_block` / `on_send`: the *current* statement must be a candidate method
/// call carrying a block or a block-pass.
fn dispatch_candidate<'pr>(statement: &ruby_prism::Node<'pr>) -> Option<Candidate<'pr>> {
    let candidate = extract_candidate(statement)?;
    is_candidate_method(candidate.method_name()).then_some(candidate)
}

/// `extract_candidate`: the statement itself, or an assignment's right-hand
/// side, when it is a call with a block or a `&:sym` block-pass.
fn extract_candidate<'pr>(statement: &ruby_prism::Node<'pr>) -> Option<Candidate<'pr>> {
    let call = match assignment_value(statement) {
        Some(value) => value.as_call_node()?,
        None => statement.as_call_node()?,
    };
    let block = call.block()?;
    if node_type_tag(&block) == BLOCK_NODE || block.as_block_argument_node().is_some() {
        Some(Candidate { call })
    } else {
        None
    }
}

fn is_candidate_method(name: &[u8]) -> bool {
    name == b"reject" || is_select_method(name)
}

fn is_select_method(name: &[u8]) -> bool {
    SELECT_METHODS.contains(&name)
}

fn complementary_pair(node: &Candidate<'_>, sibling: &Candidate<'_>) -> bool {
    let (first, second) = (node.method_name(), sibling.method_name());
    (is_select_method(first) && second == b"reject")
        || (first == b"reject" && is_select_method(second))
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum BlockKind {
    Regular,
    Numbered,
    It,
}

/// Parser's `block` / `numblock` / `itblock`, read off Prism's parameters slot.
fn block_kind(block: &ruby_prism::BlockNode<'_>) -> BlockKind {
    match block.parameters() {
        None => BlockKind::Regular,
        Some(parameters) => match node_type_tag(&parameters) {
            NUMBERED_PARAMETERS_NODE => BlockKind::Numbered,
            IT_PARAMETERS_NODE => BlockKind::It,
            _ => BlockKind::Regular,
        },
    }
}

/// RuboCop's `block.body`: the single statement, or the sequence node when
/// there is more than one.
fn block_body_expression<'pr>(block: &ruby_prism::BlockNode<'pr>) -> Option<ruby_prism::Node<'pr>> {
    let body = block.body()?;
    let Some(statements) = body.as_statements_node() else {
        return Some(body);
    };
    let mut iter = statements.body().iter();
    let first = iter.next()?;
    if iter.next().is_some() {
        Some(body)
    } else {
        Some(first)
    }
}

/// `(block _ (args (arg _name)) (send (lvar _name) $_method_name))` — returns
/// the method name when the block is a symbol proc in longhand.
fn symbol_proc_method(block: &ruby_prism::BlockNode<'_>) -> Option<Vec<u8>> {
    let parameters = block.parameters()?.as_block_parameters_node()?;
    // `(args (arg _name))` carries no block-local declarations (`{ |x; y| }`).
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
    let first = requireds.next()?;
    if requireds.next().is_some() {
        return None;
    }
    let parameter_name = first
        .as_required_parameter_node()?
        .name()
        .as_slice()
        .to_vec();

    let call = block_body_expression(block)?.as_call_node()?;
    if call.is_safe_navigation() || call.arguments().is_some() || call.block().is_some() {
        return None;
    }
    let receiver = call.receiver()?;
    let read = receiver.as_local_variable_read_node()?;
    (read.name().as_slice() == parameter_name).then(|| call.name().as_slice().to_vec())
}

/// `Node#assignment?` (`lvasgn ivasgn cvasgn gvasgn casgn masgn op_asgn
/// or_asgn and_asgn`) plus the right-hand side RuboCop reads as
/// `children.last`.
fn assignment_value<'pr>(node: &ruby_prism::Node<'pr>) -> Option<ruby_prism::Node<'pr>> {
    match node_type_tag(node) {
        LOCAL_VARIABLE_WRITE_NODE => Some(node.as_local_variable_write_node()?.value()),
        INSTANCE_VARIABLE_WRITE_NODE => Some(node.as_instance_variable_write_node()?.value()),
        CLASS_VARIABLE_WRITE_NODE => Some(node.as_class_variable_write_node()?.value()),
        GLOBAL_VARIABLE_WRITE_NODE => Some(node.as_global_variable_write_node()?.value()),
        CONSTANT_WRITE_NODE => Some(node.as_constant_write_node()?.value()),
        CONSTANT_PATH_WRITE_NODE => Some(node.as_constant_path_write_node()?.value()),
        MULTI_WRITE_NODE => Some(node.as_multi_write_node()?.value()),
        LOCAL_VARIABLE_OPERATOR_WRITE_NODE => {
            Some(node.as_local_variable_operator_write_node()?.value())
        }
        LOCAL_VARIABLE_OR_WRITE_NODE => Some(node.as_local_variable_or_write_node()?.value()),
        LOCAL_VARIABLE_AND_WRITE_NODE => Some(node.as_local_variable_and_write_node()?.value()),
        INSTANCE_VARIABLE_OPERATOR_WRITE_NODE => {
            Some(node.as_instance_variable_operator_write_node()?.value())
        }
        INSTANCE_VARIABLE_OR_WRITE_NODE => Some(node.as_instance_variable_or_write_node()?.value()),
        INSTANCE_VARIABLE_AND_WRITE_NODE => {
            Some(node.as_instance_variable_and_write_node()?.value())
        }
        CLASS_VARIABLE_OPERATOR_WRITE_NODE => {
            Some(node.as_class_variable_operator_write_node()?.value())
        }
        CLASS_VARIABLE_OR_WRITE_NODE => Some(node.as_class_variable_or_write_node()?.value()),
        CLASS_VARIABLE_AND_WRITE_NODE => Some(node.as_class_variable_and_write_node()?.value()),
        GLOBAL_VARIABLE_OPERATOR_WRITE_NODE => {
            Some(node.as_global_variable_operator_write_node()?.value())
        }
        GLOBAL_VARIABLE_OR_WRITE_NODE => Some(node.as_global_variable_or_write_node()?.value()),
        GLOBAL_VARIABLE_AND_WRITE_NODE => Some(node.as_global_variable_and_write_node()?.value()),
        CONSTANT_OPERATOR_WRITE_NODE => Some(node.as_constant_operator_write_node()?.value()),
        CONSTANT_OR_WRITE_NODE => Some(node.as_constant_or_write_node()?.value()),
        CONSTANT_AND_WRITE_NODE => Some(node.as_constant_and_write_node()?.value()),
        CONSTANT_PATH_OPERATOR_WRITE_NODE => {
            Some(node.as_constant_path_operator_write_node()?.value())
        }
        CONSTANT_PATH_OR_WRITE_NODE => Some(node.as_constant_path_or_write_node()?.value()),
        CONSTANT_PATH_AND_WRITE_NODE => Some(node.as_constant_path_and_write_node()?.value()),
        CALL_OPERATOR_WRITE_NODE => Some(node.as_call_operator_write_node()?.value()),
        CALL_OR_WRITE_NODE => Some(node.as_call_or_write_node()?.value()),
        CALL_AND_WRITE_NODE => Some(node.as_call_and_write_node()?.value()),
        INDEX_OPERATOR_WRITE_NODE => Some(node.as_index_operator_write_node()?.value()),
        INDEX_OR_WRITE_NODE => Some(node.as_index_or_write_node()?.value()),
        INDEX_AND_WRITE_NODE => Some(node.as_index_and_write_node()?.value()),
        _ => None,
    }
}

/// `both_lvasgn?` — autocorrection only applies to plain local variables.
fn local_variable_target<'pr>(node: &ruby_prism::Node<'pr>) -> Option<&'pr [u8]> {
    Some(node.as_local_variable_write_node()?.name().as_slice())
}

/// A formatting-insensitive stand-in for Parser's `Node#==`: whitespace inside
/// code is dropped and comments are removed, while string, symbol and regexp
/// content is kept verbatim.
fn normalized_key(source: &SourceFile, code_map: &CodeMap, start: usize, end: usize) -> Vec<u8> {
    let bytes = source.as_bytes();
    let end = end.min(bytes.len());
    let mut key = Vec::with_capacity(end.saturating_sub(start));
    for (offset, byte) in bytes[start..end]
        .iter()
        .enumerate()
        .map(|(i, b)| (start + i, *b))
    {
        let is_code = code_map.is_code(offset);
        // A comment is the only non-code region that is not a string literal.
        if !is_code && code_map.is_not_string(offset) {
            continue;
        }
        if is_code && byte.is_ascii_whitespace() {
            continue;
        }
        key.push(byte);
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    crate::cop_fixture_tests!(
        PartitionInsteadOfDoubleSelect,
        "cops/style/partition_instead_of_double_select"
    );
    crate::cop_autocorrect_fixture_tests!(
        PartitionInsteadOfDoubleSelect,
        "cops/style/partition_instead_of_double_select"
    );

    fn corrected(input: &[u8]) -> String {
        let (_diags, corrections) =
            crate::testutil::run_cop_autocorrect(&PartitionInsteadOfDoubleSelect, input);
        let cs = crate::correction::CorrectionSet::from_vec(corrections);
        String::from_utf8(cs.apply(input)).unwrap()
    }

    #[test]
    fn whitespace_differences_do_not_block_a_match() {
        let (diags, _) = crate::testutil::run_cop_autocorrect(
            &PartitionInsteadOfDoubleSelect,
            b"a = arr.select { |x| x>0 }\nb = arr.reject { |x| x > 0 }\n",
        );
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn explicit_begin_end_is_not_scanned() {
        let (diags, _) = crate::testutil::run_cop_autocorrect(
            &PartitionInsteadOfDoubleSelect,
            b"begin\n  a = arr.select { |x| x > 0 }\n  b = arr.reject { |x| x > 0 }\nend\n",
        );
        assert!(diags.is_empty());
    }

    #[test]
    fn def_body_is_scanned() {
        let (diags, _) = crate::testutil::run_cop_autocorrect(
            &PartitionInsteadOfDoubleSelect,
            b"def foo\n  a = arr.select { |x| x > 0 }\n  b = arr.reject { |x| x > 0 }\nend\n",
        );
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn autocorrect_reject_then_select_swaps_variable_order() {
        assert_eq!(
            corrected(
                b"negatives = arr.reject { |x| x > 0 }\npositives = arr.select { |x| x > 0 }\n"
            ),
            "positives, negatives = arr.partition { |x| x > 0 }\n"
        );
    }

    #[test]
    fn autocorrect_negated_reject_pair_swaps_variable_order() {
        assert_eq!(
            corrected(b"a = arr.reject { |x| x.positive? }\nb = arr.reject { |x| !x.positive? }\n"),
            "b, a = arr.partition { |x| x.positive? }\n"
        );
    }

    #[test]
    fn non_lvasgn_targets_are_reported_but_not_corrected() {
        let input = b"@a = arr.select { |x| x > 0 }\n@b = arr.reject { |x| x > 0 }\n";
        let (diags, corrections) =
            crate::testutil::run_cop_autocorrect(&PartitionInsteadOfDoubleSelect, input);
        assert_eq!(diags.len(), 1);
        assert!(corrections.is_empty());
    }
}
