//! Builtin NodePattern predicate registry.
//!
//! A NodePattern term like `literal?` or `method?(:freeze)` compiles, upstream,
//! to a plain Ruby method call on whatever the pattern is currently looking at
//! (`compiler/node_pattern_subcompiler.rb:80-86`). For `pred?` the receiver is
//! the matched node itself, so the method has to exist on
//! `RuboCop::AST::Node`; for `#helper` the receiver is the object the pattern
//! was defined on — a cop, or `Node` itself for the matchers in `node.rb`.
//!
//! This module is the Rust side of that: a name → function table over the
//! rubocop-ast `Node` predicates, backed wherever possible by the shared
//! helpers nitrocop's hand-written cops already use
//! (`src/cop/shared/{util, literal_predicates, method_identifier_predicates,
//! method_dispatch_predicates, access_modifier_predicates, node_type_groups,
//! predicate_operator_predicates}.rs`). Nothing here reimplements logic that
//! lives there; it adapts it to the pattern-matcher's calling convention.
//!
//! ## Calling convention
//!
//! The design sketches `fn(&PredCtx, &Node, &[Arg]) -> bool`, but a predicate
//! is not always applied to a node: `(str empty?)` sends `empty?` to the
//! string's *value* and `(int positive?)` to the integer's, because
//! `access_element` is the child slot, not a node
//! (`node_pattern_subcompiler.rb:121-123`). The target is therefore a
//! [`PredTarget`], with [`PredTarget::Node`] the common case; [`node_pred`]
//! adapts a plain `fn(&Node) -> bool` into the signature.
//!
//! ## Type predicates are not in this table
//!
//! `send_type?`, `numeric_type?`, `any_block_type?` and friends never reach
//! the registry: the lexer turns every `*_type?` into `Token::TypePredicate`
//! and the interpreter answers it through `concrete_type`, which already
//! understands rubocop-ast's `GROUP_FOR_TYPE` groups. `nil?`, `true?` and
//! `false?` likewise have their own tokens.
//!
//! ## Deliberately absent
//!
//! | Predicate | Why |
//! |---|---|
//! | `value_used?`, `root?`, `chained?`, `argument?`, `macro?`, `guard_clause?`, `sibling_index` | need the ancestor chain (`node.rb:704-721`); the walker does not carry one yet |
//! | `def_modifier?`, `pure?` | recursive over `children`, and unused by any vendored pattern |
//! | `equal?` | identity against a `%param`; it needs the parameter bindings, not the node |
//! | `const_name`, `receiver`, `arguments`, `first_line`, `column` | attribute readers, not predicates: they belong to the IR expression layer, which needs values rather than booleans |
//!
//! `receiver?` and `arguments?` do not exist in rubocop-ast either, but the IR
//! `when:` layer wants them and they are one-liners, so they are registered and
//! flagged [`Source::Extension`] — a pattern using one would raise
//! `NoMethodError` under real RuboCop, so nothing that matches here can
//! diverge on a pattern RuboCop actually runs.

use crate::cop::shared::access_modifier_predicates as amp;
use crate::cop::shared::literal_predicates as lit;
use crate::cop::shared::method_dispatch_predicates as mdp;
use crate::cop::shared::method_identifier_predicates as mip;
use crate::cop::shared::node_type::{self, node_type_tag};
use crate::cop::shared::node_type_groups as groups;
use crate::cop::shared::util;

/// An evaluated argument to a `#helper` / `pred?` call.
///
/// Upstream compiles arguments as *atoms* — values that answer `===`
/// (`compiler/atom_subcompiler.rb`) — so a `{}` union of literals becomes a
/// `Set` and a bare constant becomes the constant itself.
#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    /// `:name`
    Symbol(String),
    /// `"text"`
    Str(String),
    /// `42`
    Int(i64),
    /// `1.5`
    Float(f64),
    /// `/body/flags`, kept unparsed.
    Regexp {
        /// Regexp source between the slashes.
        body: String,
        /// The `imxo` flags that followed it.
        flags: String,
    },
    /// `{:a :b}` — upstream's `NodePattern::Sets[…]`, matched by membership.
    Set(Vec<Arg>),
    /// An argument that could not be reduced to an atom: a `%param` with no
    /// binding, a constant the resolver does not know, or a nested pattern.
    Unresolved,
}

impl Arg {
    /// Whether this argument, used as a matcher, accepts `name`.
    ///
    /// This is upstream's `arg === value` for the symbol/string/set cases, the
    /// only shapes any vendored pattern passes to a name-matching predicate.
    /// An [`Arg::Unresolved`] never matches, so an unbound parameter fails
    /// closed rather than silently accepting everything.
    #[must_use]
    pub fn accepts_name(&self, name: &[u8]) -> bool {
        match self {
            Arg::Symbol(s) | Arg::Str(s) => s.as_bytes() == name,
            Arg::Set(items) => items.iter().any(|item| item.accepts_name(name)),
            _ => false,
        }
    }

    /// The integer this argument carries, if it is a numeric atom.
    #[must_use]
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Arg::Int(n) => Some(*n),
            _ => None,
        }
    }
}

/// What a predicate is being applied to.
///
/// `access_element` in upstream's compiler is whatever sits in the child slot,
/// which is a node only some of the time.
#[derive(Debug)]
pub enum PredTarget<'a, 'pr> {
    /// A present AST node.
    Node(&'a ruby_prism::Node<'pr>),
    /// An absent child — upstream's `nil`, on which almost every predicate
    /// raises, so predicates answer `false` here unless they say otherwise.
    Absent,
    /// A name or value byte slice (method name, symbol value, string value).
    Name(&'a [u8]),
    /// A Parser-gem child Prism does not materialize, reduced to its type and
    /// its single value.
    Synthetic {
        /// The Parser-gem type this stands in for (`str`, `regopt`, `int`, …).
        parser_type: &'static str,
        /// The value the synthesized node carries.
        value: &'a [u8],
    },
}

impl<'a, 'pr> PredTarget<'a, 'pr> {
    /// The node this target holds, if it is one.
    #[must_use]
    pub fn node(&self) -> Option<&'a ruby_prism::Node<'pr>> {
        match self {
            PredTarget::Node(node) => Some(node),
            _ => None,
        }
    }

    /// The raw bytes this target carries: a name/value slice directly, or the
    /// source text of a node.
    #[must_use]
    pub fn value_bytes(&self) -> Option<&'a [u8]> {
        match self {
            PredTarget::Name(bytes) | PredTarget::Synthetic { value: bytes, .. } => Some(bytes),
            _ => None,
        }
    }
}

/// Everything a predicate may read besides its target and arguments.
///
/// Only the ancestor chain so far, and it is always empty until the walker
/// carries one (design §3.3). Predicates that need it are not registered.
#[derive(Debug, Default, Clone, Copy)]
pub struct PredCtx<'a, 'pr> {
    /// Enclosing nodes, innermost last.
    pub ancestors: &'a [ruby_prism::Node<'pr>],
}

impl PredCtx<'_, '_> {
    /// A context with no ancestors.
    #[must_use]
    pub fn empty() -> Self {
        Self { ancestors: &[] }
    }
}

/// The shape every builtin has.
pub type PredFn = fn(&PredCtx<'_, '_>, &PredTarget<'_, '_>, &[Arg]) -> bool;

/// How many arguments a builtin takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arity {
    /// No argument list, e.g. `literal?`.
    Nullary,
    /// Exactly one argument, e.g. `method?(:freeze)`.
    Unary,
}

/// Where a builtin comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A method that exists on `RuboCop::AST::Node` (or one of its mixins).
    RubocopAst,
    /// A nitrocop-only convenience, reachable from the IR expression layer but
    /// not from any pattern RuboCop itself compiles.
    Extension,
}

/// One entry of the registry.
#[derive(Debug, Clone, Copy)]
pub struct Builtin {
    /// Name exactly as it appears in a pattern, `?` included.
    pub name: &'static str,
    /// Argument count the name accepts.
    pub arity: Arity,
    /// Where the definition comes from.
    pub source: Source,
    /// The shared helper (or upstream definition) this delegates to, for the
    /// `--list-ir-predicates` style documentation the IR work will want.
    pub backing: &'static str,
    /// The implementation.
    pub eval: PredFn,
}

/// Look a builtin up by the name written in the pattern.
///
/// Returns `None` for an unknown name; callers turn that into a compile error
/// rather than an optimistic match.
#[must_use]
pub fn lookup(name: &str) -> Option<&'static Builtin> {
    BUILTINS
        .binary_search_by_key(&name, |builtin| builtin.name)
        .ok()
        .map(|index| &BUILTINS[index])
}

/// Every builtin, sorted by name.
#[must_use]
pub fn all() -> &'static [Builtin] {
    BUILTINS
}

/// Adapt a `fn(&Node) -> bool` into a [`PredFn`], answering `false` for a
/// non-node target.
macro_rules! node_pred {
    ($f:expr) => {
        |_ctx: &PredCtx<'_, '_>, target: &PredTarget<'_, '_>, _args: &[Arg]| -> bool {
            target.node().is_some_and(|node| $f(node))
        }
    };
}

/// Adapt a `fn(&CallNode) -> bool` into a [`PredFn`].
macro_rules! call_pred {
    ($f:expr) => {
        |_ctx: &PredCtx<'_, '_>, target: &PredTarget<'_, '_>, _args: &[Arg]| -> bool {
            target
                .node()
                .and_then(ruby_prism::Node::as_call_node)
                .is_some_and(|call| $f(&call))
        }
    };
}

/// Adapt a `fn(&[u8]) -> bool` over the dispatched method name into a
/// [`PredFn`]. Mirrors `MethodIdentifierPredicates`, which is mixed into the
/// send *and* def node classes.
macro_rules! method_name_pred {
    ($f:expr) => {
        |_ctx: &PredCtx<'_, '_>, target: &PredTarget<'_, '_>, _args: &[Arg]| -> bool {
            method_name_of(target).is_some_and(|name| $f(name))
        }
    };
}

/// The dispatched method name of a target.
///
/// `MethodIdentifierPredicates` is mixed into `SendNode`, `DefNode` and
/// `DefsNode`, so `send`, `csend`, `def` and `defs` all answer these. A bare
/// name slot (`(send _ operator_method?)`) answers with itself.
fn method_name_of<'a>(target: &PredTarget<'a, '_>) -> Option<&'a [u8]> {
    if let Some(bytes) = target.value_bytes() {
        return Some(bytes);
    }
    let node = target.node()?;
    if let Some(call) = node.as_call_node() {
        return Some(call.name().as_slice());
    }
    if let Some(def) = node.as_def_node() {
        return Some(def.name().as_slice());
    }
    None
}

/// Source text of a node.
fn node_source<'pr>(node: &ruby_prism::Node<'pr>) -> &'pr [u8] {
    node.location().as_slice()
}

// ---------------------------------------------------------------------------
// Literals — `src/cop/shared/literal_predicates.rs`
// ---------------------------------------------------------------------------

/// The node types `def_recursive_literal_predicate` recurses through
/// (`node.rb:129-131`: `OPERATOR_KEYWORDS + COMPOSITE_LITERALS + %i[begin pair]`).
fn is_literal_recursive_type(node: &ruby_prism::Node<'_>) -> bool {
    matches!(
        node_type_tag(node),
        node_type::AND_NODE
            | node_type::OR_NODE
            | node_type::INTERPOLATED_STRING_NODE
            | node_type::X_STRING_NODE
            | node_type::INTERPOLATED_X_STRING_NODE
            | node_type::INTERPOLATED_SYMBOL_NODE
            | node_type::ARRAY_NODE
            | node_type::HASH_NODE
            | node_type::KEYWORD_HASH_NODE
            | node_type::RANGE_NODE
            | node_type::REGULAR_EXPRESSION_NODE
            | node_type::INTERPOLATED_REGULAR_EXPRESSION_NODE
            | node_type::STATEMENTS_NODE
            | node_type::PARENTHESES_NODE
            | node_type::BEGIN_NODE
            | node_type::ASSOC_NODE
    )
}

/// `LITERAL_RECURSIVE_METHODS` — `COMPARISON_OPERATORS + %i[* ! <=>]`.
fn is_literal_recursive_method(name: &[u8]) -> bool {
    mip::is_comparison_method(name) || matches!(name, b"*" | b"!" | b"<=>")
}

/// Shared body of `recursive_literal?` / `recursive_basic_literal?`
/// (`node.rb:131-155`).
fn recursive_literal(node: &ruby_prism::Node<'_>, basic: bool) -> bool {
    if let Some(call) = node.as_call_node() {
        if !is_literal_recursive_method(call.name().as_slice()) {
            return false;
        }
        let Some(receiver) = call.receiver() else {
            return false;
        };
        if !recursive_literal(&receiver, basic) {
            return false;
        }
        return call.arguments().is_none_or(|args| {
            args.arguments()
                .iter()
                .all(|arg| recursive_literal(&arg, basic))
        });
    }
    if is_literal_recursive_type(node) {
        return child_nodes(node)
            .iter()
            .all(|child| recursive_literal(child, basic));
    }
    if basic {
        lit::is_basic_literal(node)
    } else {
        lit::is_literal(node)
    }
}

/// The node children of a composite literal, for the recursive predicates.
///
/// Only the shapes [`is_literal_recursive_type`] admits need covering.
fn child_nodes<'pr>(node: &ruby_prism::Node<'pr>) -> Vec<ruby_prism::Node<'pr>> {
    if let Some(array) = node.as_array_node() {
        return array.elements().iter().collect();
    }
    if let Some(hash) = node.as_hash_node() {
        return hash.elements().iter().collect();
    }
    if let Some(hash) = node.as_keyword_hash_node() {
        return hash.elements().iter().collect();
    }
    if let Some(assoc) = node.as_assoc_node() {
        return vec![assoc.key(), assoc.value()];
    }
    if let Some(statements) = node.as_statements_node() {
        return statements.body().iter().collect();
    }
    if let Some(parens) = node.as_parentheses_node() {
        return parens.body().into_iter().collect();
    }
    if let Some(begin) = node.as_begin_node() {
        return begin
            .statements()
            .map(|statements| statements.body().iter().collect())
            .unwrap_or_default();
    }
    if let Some(and) = node.as_and_node() {
        return vec![and.left(), and.right()];
    }
    if let Some(or) = node.as_or_node() {
        return vec![or.left(), or.right()];
    }
    if let Some(dstr) = node.as_interpolated_string_node() {
        return dstr.parts().iter().collect();
    }
    if let Some(dsym) = node.as_interpolated_symbol_node() {
        return dsym.parts().iter().collect();
    }
    if let Some(range) = node.as_range_node() {
        return range.left().into_iter().chain(range.right()).collect();
    }
    Vec::new()
}

// ---------------------------------------------------------------------------
// Variables and assignment — `node.rb:461-477`
// ---------------------------------------------------------------------------

/// `REFERENCES` — `%i[nth_ref back_ref]`.
fn is_reference(node: &ruby_prism::Node<'_>) -> bool {
    matches!(
        node_type_tag(node),
        node_type::NUMBERED_REFERENCE_READ_NODE | node_type::BACK_REFERENCE_READ_NODE
    )
}

/// `EQUALS_ASSIGNMENTS` — `%i[lvasgn ivasgn cvasgn gvasgn casgn masgn]`.
fn is_equals_asgn(node: &ruby_prism::Node<'_>) -> bool {
    matches!(
        node_type_tag(node),
        node_type::LOCAL_VARIABLE_WRITE_NODE
            | node_type::INSTANCE_VARIABLE_WRITE_NODE
            | node_type::CLASS_VARIABLE_WRITE_NODE
            | node_type::GLOBAL_VARIABLE_WRITE_NODE
            | node_type::CONSTANT_WRITE_NODE
            | node_type::CONSTANT_PATH_WRITE_NODE
            | node_type::MULTI_WRITE_NODE
    )
}

/// `SHORTHAND_ASSIGNMENTS` — `%i[op_asgn or_asgn and_asgn]`.
fn is_shorthand_asgn(node: &ruby_prism::Node<'_>) -> bool {
    groups::is_assignment_type(node_type_tag(node)) && !is_equals_asgn(node)
}

/// `assignment_or_similar?` — `{assignment? (send _recv :<< ...)}`
/// (`node.rb:425-427`).
fn is_assignment_or_similar(node: &ruby_prism::Node<'_>) -> bool {
    if groups::is_assignment_type(node_type_tag(node)) {
        return true;
    }
    node.as_call_node()
        .is_some_and(|call| call.name().as_slice() == b"<<" && call.receiver().is_some())
}

// ---------------------------------------------------------------------------
// Conditionals and keywords — `node.rb:481-513`
// ---------------------------------------------------------------------------

/// `POST_CONDITION_LOOP_TYPES` — `%i[while_post until_post]`, i.e.
/// `begin … end while cond`. Prism keeps one node type and flags it.
fn is_post_condition_loop(node: &ruby_prism::Node<'_>) -> bool {
    node.as_while_node()
        .is_some_and(|while_node| while_node.is_begin_modifier())
        || node
            .as_until_node()
            .is_some_and(|until_node| until_node.is_begin_modifier())
}

/// `LOOP_TYPES` — `POST_CONDITION_LOOP_TYPES + %i[while until for]`.
fn is_loop_keyword(node: &ruby_prism::Node<'_>) -> bool {
    groups::is_loop_type(node_type_tag(node))
}

/// `OPERATOR_KEYWORDS` — `%i[and or]`. The node has to be spelled with the
/// keyword, not `&&` / `||`; that distinction is
/// `predicate_operator_predicates`.
fn is_operator_keyword(node: &ruby_prism::Node<'_>) -> bool {
    matches!(
        node_type_tag(node),
        node_type::AND_NODE | node_type::OR_NODE
    )
}

/// `SPECIAL_KEYWORDS` — `%w[__FILE__ __LINE__ __ENCODING__]`, which Prism
/// gives dedicated node types.
fn is_special_keyword(node: &ruby_prism::Node<'_>) -> bool {
    matches!(
        node_type_tag(node),
        node_type::SOURCE_FILE_NODE | node_type::SOURCE_LINE_NODE | node_type::SOURCE_ENCODING_NODE
    )
}

/// `KEYWORDS` minus the operator keywords, which `keyword?` handles specially.
fn is_plain_keyword_type(node: &ruby_prism::Node<'_>) -> bool {
    matches!(
        node_type_tag(node),
        node_type::ALIAS_METHOD_NODE
            | node_type::ALIAS_GLOBAL_VARIABLE_NODE
            | node_type::BREAK_NODE
            | node_type::CASE_NODE
            | node_type::CASE_MATCH_NODE
            | node_type::CLASS_NODE
            | node_type::DEF_NODE
            | node_type::DEFINED_NODE
            | node_type::ELSE_NODE
            | node_type::ENSURE_NODE
            | node_type::FOR_NODE
            | node_type::IF_NODE
            | node_type::UNLESS_NODE
            | node_type::MODULE_NODE
            | node_type::NEXT_NODE
            | node_type::POST_EXECUTION_NODE
            | node_type::REDO_NODE
            | node_type::RESCUE_NODE
            | node_type::RETRY_NODE
            | node_type::RETURN_NODE
            | node_type::SELF_NODE
            | node_type::SUPER_NODE
            | node_type::FORWARDING_SUPER_NODE
            | node_type::UNDEF_NODE
            | node_type::UNTIL_NODE
            | node_type::WHEN_NODE
            | node_type::WHILE_NODE
            | node_type::YIELD_NODE
            | node_type::BEGIN_NODE
    )
}

/// `keyword?` — `node.rb:501-507`.
///
/// `and`/`or` only count when the source spells the keyword
/// (`loc.operator.is?(type.to_s)`), which is
/// `predicate_operator_predicates::is_semantic_*`. A `not` send counts too.
fn is_keyword(node: &ruby_prism::Node<'_>) -> bool {
    use crate::cop::shared::predicate_operator_predicates as pop;

    if is_special_keyword(node) || is_prefix_not(node) {
        return true;
    }
    if let Some(and) = node.as_and_node() {
        return pop::is_semantic_and(&and);
    }
    if let Some(or) = node.as_or_node() {
        return pop::is_semantic_or(&or);
    }
    is_plain_keyword_type(node)
}

/// `modifier_form?` — `loc.end.nil?` on the conditional/loop mixin
/// (`modifier_node.rb:12-14`), restricted to `if`/`unless` for `IfNode`
/// (`if_node.rb:87-89`), so a ternary is not a modifier form.
fn is_modifier_form(node: &ruby_prism::Node<'_>) -> bool {
    if let Some(if_node) = node.as_if_node() {
        return util::is_modifier_if(&if_node);
    }
    if let Some(unless_node) = node.as_unless_node() {
        return util::is_modifier_unless(&unless_node);
    }
    if let Some(while_node) = node.as_while_node() {
        return while_node.closing_loc().is_none();
    }
    if let Some(until_node) = node.as_until_node() {
        return until_node.closing_loc().is_none();
    }
    false
}

// ---------------------------------------------------------------------------
// Dispatch — `method_dispatch_node.rb:57-247`
// ---------------------------------------------------------------------------

/// `special_modifier?` — a bare `private` / `protected`
/// (`method_dispatch_node.rb:91-93`).
fn is_special_modifier(call: &ruby_prism::CallNode<'_>) -> bool {
    amp::is_bare_access_modifier(call) && amp::is_special_modifier_name(call.name().as_slice())
}

/// `block_literal?` — `method_dispatch_node.rb:167-169`.
///
/// Upstream asks the *send* whether its parent is a block node; Prism hangs
/// the block off the call itself, so this is a direct field read.
fn is_block_literal(node: &ruby_prism::Node<'_>) -> bool {
    node.as_call_node()
        .is_some_and(|call| call.block().is_some())
        || node.as_lambda_node().is_some()
}

/// `lambda?` — `(any_block (send nil? :lambda) ...)` (`node.rb:597`) and
/// `block_literal? && command?(:lambda)` (`method_dispatch_node.rb:212-214`).
///
/// Both spellings land on the same Prism `CallNode`, since Prism has no
/// separate wrapper node for `lambda { }`. A stabby `->` is a `LambdaNode`,
/// which upstream's `lambda?` rejects (its send child is `(lambda)`, not
/// `(send nil? :lambda)`), so it is rejected here too.
fn is_lambda(node: &ruby_prism::Node<'_>) -> bool {
    node.as_call_node().is_some_and(|call| {
        call.block().is_some() && call.receiver().is_none() && call.name().as_slice() == b"lambda"
    })
}

/// `lambda_literal?` — a stabby lambda (`method_dispatch_node.rb:223-225`).
fn is_lambda_literal(node: &ruby_prism::Node<'_>) -> bool {
    node.as_lambda_node().is_some()
}

/// `proc?` — `node.rb:589-595`.
fn is_proc(node: &ruby_prism::Node<'_>) -> bool {
    let Some(call) = node.as_call_node() else {
        return false;
    };
    let name = call.name().as_slice();
    match call.receiver() {
        // `proc { }`
        None => name == b"proc" && call.block().is_some(),
        // `Proc.new` / `Proc.new { }`
        Some(receiver) => name == b"new" && is_global_const(&receiver, b"Proc"),
    }
}

/// `global_const?(name)` — `(const {nil? cbase} %1)` (`node.rb:605-606`).
fn is_global_const(node: &ruby_prism::Node<'_>, name: &[u8]) -> bool {
    if let Some(read) = node.as_constant_read_node() {
        return read.name().as_slice() == name;
    }
    // `::Foo` is a `ConstantPathNode` with no parent, i.e. Parser's `cbase`.
    if let Some(path) = node.as_constant_path_node() {
        return path.parent().is_none() && path.name().is_some_and(|sym| sym.as_slice() == name);
    }
    false
}

/// `prefix_not?` — `method?(:!) && loc.selector.is?('not')`
/// (`method_identifier_predicates.rb:207-209`).
fn is_prefix_not(node: &ruby_prism::Node<'_>) -> bool {
    node.as_call_node().is_some_and(|call| {
        call.name().as_slice() == b"!"
            && call
                .message_loc()
                .is_some_and(|loc| loc.as_slice() == b"not")
    })
}

/// `prefix_bang?` — `method?(:!) && loc.selector.is?('!')`.
fn is_prefix_bang(node: &ruby_prism::Node<'_>) -> bool {
    node.as_call_node().is_some_and(|call| {
        call.name().as_slice() == b"!"
            && call.message_loc().is_some_and(|loc| loc.as_slice() == b"!")
    })
}

/// `parenthesized_call?` — `loc_is?(:begin, '(')` (`node.rb:519-521`).
fn is_parenthesized_call(node: &ruby_prism::Node<'_>) -> bool {
    if let Some(call) = node.as_call_node() {
        return call.opening_loc().is_some_and(|loc| loc.as_slice() == b"(");
    }
    if let Some(sup) = node.as_super_node() {
        return sup.lparen_loc().is_some();
    }
    false
}

// ---------------------------------------------------------------------------
// Source and structure
// ---------------------------------------------------------------------------

/// Number of source lines a node spans.
fn line_count(node: &ruby_prism::Node<'_>) -> usize {
    // `ruby_prism::Location` exposes byte offsets only, so the line span is the
    // newline count of the node's own source — which is what upstream's
    // `last_line - first_line + 1` comes to.
    node_source(node).iter().filter(|b| **b == b'\n').count() + 1
}

/// `braces?` — `loc_is?(:end, '}')` (`hash_node.rb:117-119`).
///
/// Prism splits the braced and bare forms into `HashNode` and
/// `KeywordHashNode`, so the type is the answer.
fn has_braces(node: &ruby_prism::Node<'_>) -> bool {
    node.as_hash_node().is_some()
}

/// `value_omission?` — `source.end_with?(':')` (`pair_node.rb:69-71`), the
/// `{x:}` shorthand. Prism marks the omitted value with an implicit node.
fn is_value_omission(node: &ruby_prism::Node<'_>) -> bool {
    node.as_assoc_node()
        .is_some_and(|assoc| assoc.value().as_implicit_node().is_some())
}

/// The numeric value of a target, for `positive?` / `negative?` / `zero?`.
///
/// `(int positive?)` sends the predicate to the *value* upstream; this
/// interpreter passes the literal node instead (a divergence the module header
/// of `interpreter.rs` records), so both shapes are accepted.
fn numeric_value(target: &PredTarget<'_, '_>) -> Option<f64> {
    let text = match target {
        PredTarget::Node(node) => node_source(node),
        PredTarget::Name(bytes) | PredTarget::Synthetic { value: bytes, .. } => bytes,
        PredTarget::Absent => return None,
    };
    let text: String = std::str::from_utf8(text).ok()?.replace('_', "");
    text.parse::<f64>().ok()
}

/// Bytes a string-ish predicate (`empty?`, `blank?`) looks at.
fn string_value<'a>(target: &PredTarget<'a, '_>) -> Option<&'a [u8]> {
    if let Some(bytes) = target.value_bytes() {
        return Some(bytes);
    }
    let node = target.node()?;
    if let Some(string) = node.as_string_node() {
        return Some(string.content_loc().as_slice());
    }
    if let Some(symbol) = node.as_symbol_node() {
        return symbol.value_loc().map(|loc| loc.as_slice());
    }
    None
}

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

/// Every builtin, **sorted by name** — [`lookup`] binary-searches it, and a
/// test asserts the ordering.
static BUILTINS: &[Builtin] = &[
    Builtin {
        name: "access_modifier?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "access_modifier_predicates::is_access_modifier_declaration",
        eval: call_pred!(amp::is_access_modifier_declaration),
    },
    Builtin {
        name: "arguments?",
        arity: Arity::Nullary,
        source: Source::Extension,
        backing: "ruby_prism::CallNode::arguments",
        eval: node_pred!(|node: &ruby_prism::Node<'_>| node
            .as_call_node()
            .is_some_and(|call| call.arguments().is_some())),
    },
    Builtin {
        name: "arithmetic_operation?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "method_identifier_predicates::is_arithmetic_operation",
        eval: method_name_pred!(mip::is_arithmetic_operation),
    },
    Builtin {
        name: "assignment?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "node_type_groups::is_assignment_type",
        eval: node_pred!(
            |node: &ruby_prism::Node<'_>| groups::is_assignment_type(node_type_tag(node))
        ),
    },
    Builtin {
        name: "assignment_method?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "method_identifier_predicates::is_assignment_method",
        eval: method_name_pred!(mip::is_assignment_method),
    },
    Builtin {
        name: "assignment_or_similar?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "node.rb:425-427",
        eval: node_pred!(is_assignment_or_similar),
    },
    Builtin {
        name: "bang_method?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "method_identifier_predicates::is_bang_method",
        eval: method_name_pred!(mip::is_bang_method),
    },
    Builtin {
        name: "bare_access_modifier?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "access_modifier_predicates::is_bare_access_modifier",
        eval: call_pred!(amp::is_bare_access_modifier),
    },
    Builtin {
        name: "basic_conditional?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "node_type_groups::is_basic_conditional_type",
        eval: node_pred!(
            |node: &ruby_prism::Node<'_>| groups::is_basic_conditional_type(node_type_tag(node))
        ),
    },
    Builtin {
        name: "basic_literal?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "literal_predicates::is_basic_literal",
        eval: node_pred!(lit::is_basic_literal),
    },
    Builtin {
        name: "binary_operation?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "method_dispatch_predicates::is_binary_operation",
        eval: call_pred!(mdp::is_binary_operation),
    },
    Builtin {
        name: "blank?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "ActiveSupport String#blank?",
        eval: |_ctx, target, _args| {
            string_value(target).is_some_and(|bytes| bytes.iter().all(|b| b.is_ascii_whitespace()))
        },
    },
    Builtin {
        name: "block_literal?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "method_dispatch_node.rb:167-169",
        eval: node_pred!(is_block_literal),
    },
    Builtin {
        name: "braces?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "hash_node.rb:117-119",
        eval: node_pred!(has_braces),
    },
    Builtin {
        name: "camel_case_method?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "method_identifier_predicates::is_camel_case_method",
        eval: method_name_pred!(mip::is_camel_case_method),
    },
    Builtin {
        name: "command?",
        arity: Arity::Unary,
        source: Source::RubocopAst,
        backing: "method_dispatch_predicates::is_command",
        eval: |_ctx, target, args| {
            let Some(call) = target.node().and_then(ruby_prism::Node::as_call_node) else {
                return false;
            };
            call.receiver().is_none()
                && args
                    .first()
                    .is_some_and(|arg| arg.accepts_name(call.name().as_slice()))
        },
    },
    Builtin {
        name: "comparison_method?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "method_identifier_predicates::is_comparison_method",
        eval: method_name_pred!(mip::is_comparison_method),
    },
    Builtin {
        name: "composite_literal?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "literal_predicates::is_composite_literal",
        eval: node_pred!(lit::is_composite_literal),
    },
    Builtin {
        name: "conditional?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "node_type_groups::is_conditional_type",
        eval: node_pred!(
            |node: &ruby_prism::Node<'_>| groups::is_conditional_type(node_type_tag(node))
        ),
    },
    Builtin {
        name: "const_receiver?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "method_dispatch_predicates::is_const_receiver",
        eval: call_pred!(mdp::is_const_receiver),
    },
    Builtin {
        name: "dot?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "method_dispatch_predicates::is_dot_call",
        eval: call_pred!(mdp::is_dot_call),
    },
    Builtin {
        name: "double_colon?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "method_dispatch_predicates::is_double_colon_call",
        eval: call_pred!(mdp::is_double_colon_call),
    },
    Builtin {
        name: "empty?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "String#empty? / Array#empty? on the child slot",
        eval: |_ctx, target, _args| {
            if let Some(bytes) = string_value(target) {
                return bytes.is_empty();
            }
            target.node().is_some_and(|node| {
                node.as_array_node()
                    .is_some_and(|array| array.elements().iter().next().is_none())
                    || node
                        .as_hash_node()
                        .is_some_and(|hash| hash.elements().iter().next().is_none())
            })
        },
    },
    Builtin {
        name: "empty_source?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "node.rb:419-421",
        eval: node_pred!(|node: &ruby_prism::Node<'_>| node_source(node).is_empty()),
    },
    Builtin {
        name: "enumerable_method?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "method_identifier_predicates::is_enumerable_method",
        eval: method_name_pred!(mip::is_enumerable_method),
    },
    Builtin {
        name: "enumerator_method?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "method_identifier_predicates::is_enumerator_method",
        eval: method_name_pred!(mip::is_enumerator_method),
    },
    Builtin {
        name: "equals_asgn?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "node.rb:466-468",
        eval: node_pred!(is_equals_asgn),
    },
    Builtin {
        name: "falsey_literal?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "literal_predicates::is_falsey_literal",
        eval: node_pred!(lit::is_falsey_literal),
    },
    Builtin {
        name: "global_const?",
        arity: Arity::Unary,
        source: Source::RubocopAst,
        backing: "node.rb:605-606",
        eval: |_ctx, target, args| {
            let Some(node) = target.node() else {
                return false;
            };
            let name = if let Some(read) = node.as_constant_read_node() {
                read.name().as_slice().to_vec()
            } else if let Some(path) = node.as_constant_path_node() {
                if path.parent().is_some() {
                    return false;
                }
                match path.name() {
                    Some(sym) => sym.as_slice().to_vec(),
                    None => return false,
                }
            } else {
                return false;
            };
            args.first().is_some_and(|arg| arg.accepts_name(&name))
        },
    },
    Builtin {
        name: "immutable_literal?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "literal_predicates::is_immutable_literal",
        eval: node_pred!(lit::is_immutable_literal),
    },
    Builtin {
        name: "implicit_call?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "method_dispatch_predicates::is_implicit_call",
        eval: call_pred!(mdp::is_implicit_call),
    },
    Builtin {
        name: "keyword?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "node.rb:501-507 + predicate_operator_predicates",
        eval: node_pred!(is_keyword),
    },
    Builtin {
        name: "lambda?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "node.rb:597",
        eval: node_pred!(is_lambda),
    },
    Builtin {
        name: "lambda_literal?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "method_dispatch_node.rb:223-225",
        eval: node_pred!(is_lambda_literal),
    },
    Builtin {
        name: "lambda_or_proc?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "node.rb:602",
        eval: node_pred!(|node: &ruby_prism::Node<'_>| is_lambda(node) || is_proc(node)),
    },
    Builtin {
        name: "literal?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "literal_predicates::is_literal",
        eval: node_pred!(lit::is_literal),
    },
    Builtin {
        name: "loop_keyword?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "node_type_groups::is_loop_type",
        eval: node_pred!(is_loop_keyword),
    },
    Builtin {
        name: "method?",
        arity: Arity::Unary,
        source: Source::RubocopAst,
        backing: "method_identifier_predicates.rb:79-81",
        eval: |_ctx, target, args| {
            let Some(name) = method_name_of(target) else {
                return false;
            };
            args.first().is_some_and(|arg| arg.accepts_name(name))
        },
    },
    Builtin {
        name: "modifier_form?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "modifier_node.rb:12-14",
        eval: node_pred!(is_modifier_form),
    },
    Builtin {
        name: "multiline?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "node.rb:413-415",
        eval: node_pred!(|node: &ruby_prism::Node<'_>| line_count(node) > 1),
    },
    Builtin {
        name: "mutable_literal?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "literal_predicates::is_mutable_literal",
        eval: node_pred!(lit::is_mutable_literal),
    },
    Builtin {
        name: "negation_method?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "method_identifier_predicates::is_negation_method",
        eval: method_name_pred!(mip::is_negation_method),
    },
    Builtin {
        name: "negative?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "Numeric#negative? on the child slot",
        eval: |_ctx, target, _args| numeric_value(target).is_some_and(|value| value < 0.0),
    },
    Builtin {
        name: "non_bare_access_modifier?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "access_modifier_predicates::is_non_bare_access_modifier",
        eval: call_pred!(amp::is_non_bare_access_modifier),
    },
    Builtin {
        name: "nonmutating_array_method?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "method_identifier_predicates::is_nonmutating_array_method",
        eval: method_name_pred!(mip::is_nonmutating_array_method),
    },
    Builtin {
        name: "nonmutating_binary_operator_method?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "method_identifier_predicates::is_nonmutating_binary_operator_method",
        eval: method_name_pred!(mip::is_nonmutating_binary_operator_method),
    },
    Builtin {
        name: "nonmutating_hash_method?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "method_identifier_predicates::is_nonmutating_hash_method",
        eval: method_name_pred!(mip::is_nonmutating_hash_method),
    },
    Builtin {
        name: "nonmutating_operator_method?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "method_identifier_predicates::is_nonmutating_operator_method",
        eval: method_name_pred!(mip::is_nonmutating_operator_method),
    },
    Builtin {
        name: "nonmutating_string_method?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "method_identifier_predicates::is_nonmutating_string_method",
        eval: method_name_pred!(mip::is_nonmutating_string_method),
    },
    Builtin {
        name: "nonmutating_unary_operator_method?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "method_identifier_predicates::is_nonmutating_unary_operator_method",
        eval: method_name_pred!(mip::is_nonmutating_unary_operator_method),
    },
    Builtin {
        name: "operator_keyword?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "node.rb:511-513",
        eval: node_pred!(is_operator_keyword),
    },
    Builtin {
        name: "operator_method?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "method_identifier_predicates::is_operator_method",
        eval: method_name_pred!(mip::is_operator_method),
    },
    Builtin {
        name: "parenthesized_call?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "node.rb:519-521",
        eval: node_pred!(is_parenthesized_call),
    },
    Builtin {
        name: "positive?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "Numeric#positive? on the child slot",
        eval: |_ctx, target, _args| numeric_value(target).is_some_and(|value| value > 0.0),
    },
    Builtin {
        name: "post_condition_loop?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "node.rb:493-495",
        eval: node_pred!(is_post_condition_loop),
    },
    Builtin {
        name: "predicate_method?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "method_identifier_predicates::is_predicate_method",
        eval: method_name_pred!(mip::is_predicate_method),
    },
    Builtin {
        name: "prefix_bang?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "method_identifier_predicates.rb:214-216",
        eval: node_pred!(is_prefix_bang),
    },
    Builtin {
        name: "prefix_not?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "method_identifier_predicates.rb:207-209",
        eval: node_pred!(is_prefix_not),
    },
    Builtin {
        name: "proc?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "node.rb:589-595",
        eval: node_pred!(is_proc),
    },
    Builtin {
        name: "receiver?",
        arity: Arity::Nullary,
        source: Source::Extension,
        backing: "ruby_prism::CallNode::receiver",
        eval: node_pred!(|node: &ruby_prism::Node<'_>| node
            .as_call_node()
            .is_some_and(|call| call.receiver().is_some())),
    },
    Builtin {
        name: "recursive_basic_literal?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "node.rb:131-155",
        eval: node_pred!(|node: &ruby_prism::Node<'_>| recursive_literal(node, true)),
    },
    Builtin {
        name: "recursive_literal?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "node.rb:131-155",
        eval: node_pred!(|node: &ruby_prism::Node<'_>| recursive_literal(node, false)),
    },
    Builtin {
        name: "reference?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "node.rb:462-464",
        eval: node_pred!(is_reference),
    },
    Builtin {
        name: "safe_navigation?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "method_dispatch_predicates::is_safe_navigation",
        eval: call_pred!(mdp::is_safe_navigation),
    },
    Builtin {
        name: "self_receiver?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "method_dispatch_predicates::is_self_receiver",
        eval: call_pred!(mdp::is_self_receiver),
    },
    Builtin {
        name: "setter_method?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "method_dispatch_predicates::is_setter_call",
        eval: call_pred!(mdp::is_setter_call),
    },
    Builtin {
        name: "shorthand_asgn?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "node.rb:470-472",
        eval: node_pred!(is_shorthand_asgn),
    },
    Builtin {
        name: "single_line?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "node.rb:417-419",
        eval: node_pred!(|node: &ruby_prism::Node<'_>| line_count(node) == 1),
    },
    Builtin {
        name: "special_keyword?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "node.rb:508-510",
        eval: node_pred!(is_special_keyword),
    },
    Builtin {
        name: "special_modifier?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "access_modifier_predicates::is_special_modifier_name",
        eval: call_pred!(is_special_modifier),
    },
    Builtin {
        name: "truthy_literal?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "literal_predicates::is_truthy_literal",
        eval: node_pred!(lit::is_truthy_literal),
    },
    Builtin {
        name: "unary_operation?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "method_dispatch_predicates::is_unary_operation",
        eval: call_pred!(mdp::is_unary_operation),
    },
    Builtin {
        name: "value_omission?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "pair_node.rb:69-71",
        eval: node_pred!(is_value_omission),
    },
    Builtin {
        name: "variable?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "node_type_groups::is_variable_type",
        eval: node_pred!(
            |node: &ruby_prism::Node<'_>| groups::is_variable_type(node_type_tag(node))
        ),
    },
    Builtin {
        name: "zero?",
        arity: Arity::Nullary,
        source: Source::RubocopAst,
        backing: "Numeric#zero? on the child slot",
        eval: |_ctx, target, _args| numeric_value(target).is_some_and(|value| value == 0.0),
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str) -> ruby_prism::ParseResult<'_> {
        ruby_prism::parse(source.as_bytes())
    }

    /// The first statement of `source`, which is what a pattern would be
    /// matched against.
    fn first_stmt<'a>(result: &'a ruby_prism::ParseResult<'a>) -> ruby_prism::Node<'a> {
        result
            .node()
            .as_program_node()
            .expect("program")
            .statements()
            .body()
            .iter()
            .next()
            .expect("at least one statement")
    }

    /// Run a builtin against the first statement of a Ruby snippet.
    fn check(name: &str, ruby: &str, args: &[Arg]) -> bool {
        let builtin = lookup(name).unwrap_or_else(|| panic!("no builtin named {name}"));
        let result = parse(ruby);
        let node = first_stmt(&result);
        (builtin.eval)(&PredCtx::empty(), &PredTarget::Node(&node), args)
    }

    /// Run a builtin against a bare name/value slot.
    fn check_name(name: &str, value: &[u8], args: &[Arg]) -> bool {
        let builtin = lookup(name).unwrap_or_else(|| panic!("no builtin named {name}"));
        (builtin.eval)(&PredCtx::empty(), &PredTarget::Name(value), args)
    }

    #[test]
    fn table_is_sorted_and_unique() {
        for pair in BUILTINS.windows(2) {
            assert!(
                pair[0].name < pair[1].name,
                "registry is not sorted: {:?} then {:?}",
                pair[0].name,
                pair[1].name
            );
        }
    }

    #[test]
    fn lookup_finds_every_entry_and_rejects_unknown_names() {
        for builtin in all() {
            assert_eq!(lookup(builtin.name).map(|b| b.name), Some(builtin.name));
        }
        assert!(lookup("definitely_not_a_predicate?").is_none());
        assert!(lookup("array_receiver?").is_none(), "cop-local helper");
    }

    #[test]
    fn type_predicates_are_not_registry_members() {
        // The lexer turns `*_type?` into `Token::TypePredicate`, which the
        // interpreter answers through `concrete_type`; registering them here
        // would be a second, divergent source of truth.
        for name in ["send_type?", "numeric_type?", "any_block_type?", "nil?"] {
            assert!(lookup(name).is_none(), "{name} should not be in the table");
        }
    }

    // -- literals ---------------------------------------------------------

    #[test]
    fn literal_family() {
        let cases: &[(&str, &str, bool)] = &[
            ("literal?", "42", true),
            ("literal?", "'a'", true),
            ("literal?", "foo", false),
            ("basic_literal?", "42", true),
            ("basic_literal?", "[1]", false),
            ("composite_literal?", "[1]", true),
            ("composite_literal?", "42", false),
            ("truthy_literal?", "42", true),
            ("truthy_literal?", "nil", false),
            ("falsey_literal?", "nil", true),
            ("falsey_literal?", "false", true),
            ("falsey_literal?", "0", false),
            ("mutable_literal?", "'a'", true),
            ("mutable_literal?", ":a", false),
            ("immutable_literal?", ":a", true),
            ("immutable_literal?", "'a'", false),
        ];
        for (name, ruby, expected) in cases {
            assert_eq!(check(name, ruby, &[]), *expected, "{name} against {ruby}");
        }
    }

    #[test]
    fn recursive_literal_family() {
        let cases: &[(&str, &str, bool)] = &[
            ("recursive_literal?", "[1, 'a']", true),
            ("recursive_literal?", "[1, foo]", false),
            ("recursive_basic_literal?", "1 == 2", true),
            // `array` is in `LITERAL_RECURSIVE_TYPES`, so both recursive
            // predicates descend into it rather than stopping at the leaf test.
            ("recursive_basic_literal?", "[1]", true),
            ("recursive_basic_literal?", "[foo]", false),
            ("recursive_basic_literal?", "1 + 2", false),
            ("recursive_basic_literal?", "42", true),
        ];
        for (name, ruby, expected) in cases {
            assert_eq!(check(name, ruby, &[]), *expected, "{name} against {ruby}");
        }
    }

    // -- variables and assignment ----------------------------------------

    #[test]
    fn assignment_family() {
        let cases: &[(&str, &str, bool)] = &[
            ("assignment?", "x = 1", true),
            ("assignment?", "x += 1", true),
            ("assignment?", "x", false),
            ("equals_asgn?", "x = 1", true),
            ("equals_asgn?", "x ||= 1", false),
            ("shorthand_asgn?", "x ||= 1", true),
            ("shorthand_asgn?", "x = 1", false),
            ("assignment_or_similar?", "x << 1", true),
            ("assignment_or_similar?", "x = 1", true),
            ("assignment_or_similar?", "x.push 1", false),
            ("variable?", "@x", true),
            ("variable?", "X", false),
            ("reference?", "$1", true),
            ("reference?", "$x", false),
        ];
        for (name, ruby, expected) in cases {
            assert_eq!(check(name, ruby, &[]), *expected, "{name} against {ruby}");
        }
    }

    // -- conditionals and keywords ---------------------------------------

    #[test]
    fn conditional_family() {
        let cases: &[(&str, &str, bool)] = &[
            ("conditional?", "if x then y end", true),
            ("conditional?", "case x; when 1 then 2; end", true),
            ("conditional?", "x", false),
            ("basic_conditional?", "while x do y end", true),
            ("basic_conditional?", "case x; when 1 then 2; end", false),
            ("loop_keyword?", "while x do y end", true),
            ("loop_keyword?", "for a in b do c end", true),
            ("loop_keyword?", "loop { }", false),
            ("post_condition_loop?", "begin; x; end while y", true),
            ("post_condition_loop?", "while y do x end", false),
            ("modifier_form?", "x if y", true),
            ("modifier_form?", "if y then x end", false),
            ("modifier_form?", "y ? x : z", false),
            ("modifier_form?", "x while y", true),
            ("operator_keyword?", "a and b", true),
            ("operator_keyword?", "a && b", true),
            ("operator_keyword?", "a", false),
            ("special_keyword?", "__FILE__", true),
            ("special_keyword?", "x", false),
        ];
        for (name, ruby, expected) in cases {
            assert_eq!(check(name, ruby, &[]), *expected, "{name} against {ruby}");
        }
    }

    #[test]
    fn keyword_distinguishes_and_from_ampersand() {
        let cases: &[(&str, bool)] = &[
            ("a and b", true),
            ("a && b", false),
            ("a or b", true),
            ("a || b", false),
            ("return 1", true),
            ("not a", true),
            ("!a", false),
            ("__LINE__", true),
            ("foo", false),
        ];
        for (ruby, expected) in cases {
            assert_eq!(
                check("keyword?", ruby, &[]),
                *expected,
                "keyword? against {ruby}"
            );
        }
    }

    // -- dispatch ---------------------------------------------------------

    #[test]
    fn dispatch_family() {
        let cases: &[(&str, &str, bool)] = &[
            ("dot?", "a.b", true),
            ("dot?", "a&.b", false),
            ("safe_navigation?", "a&.b", true),
            ("safe_navigation?", "a.b", false),
            ("double_colon?", "A::B()", true),
            ("self_receiver?", "self.b", true),
            ("self_receiver?", "a.b", false),
            ("const_receiver?", "A.b", true),
            ("const_receiver?", "a.b", false),
            ("implicit_call?", "a.(1)", true),
            ("implicit_call?", "a.call(1)", false),
            ("block_literal?", "foo { }", true),
            ("block_literal?", "foo", false),
            ("setter_method?", "a.b = 1", true),
            ("setter_method?", "a.b", false),
            ("unary_operation?", "-foo", true),
            ("unary_operation?", "a - b", false),
            ("binary_operation?", "a - b", true),
            ("binary_operation?", "-foo", false),
            ("arithmetic_operation?", "a + b", true),
            ("arithmetic_operation?", "a << b", false),
            ("parenthesized_call?", "foo(1)", true),
            ("parenthesized_call?", "foo 1", false),
            ("access_modifier?", "private", true),
            ("access_modifier?", "self.private", false),
            ("bare_access_modifier?", "private", true),
            ("bare_access_modifier?", "private :foo", false),
            ("non_bare_access_modifier?", "private :foo", true),
            ("special_modifier?", "private", true),
            ("special_modifier?", "public", false),
            ("receiver?", "a.b", true),
            ("receiver?", "b", false),
            ("arguments?", "b(1)", true),
            ("arguments?", "b", false),
        ];
        for (name, ruby, expected) in cases {
            assert_eq!(check(name, ruby, &[]), *expected, "{name} against {ruby}");
        }
    }

    #[test]
    fn lambda_and_proc_family() {
        let cases: &[(&str, &str, bool)] = &[
            ("lambda?", "lambda { }", true),
            ("lambda?", "-> { }", false),
            ("lambda?", "foo { }", false),
            ("lambda_literal?", "-> { }", true),
            ("lambda_literal?", "lambda { }", false),
            ("proc?", "proc { }", true),
            ("proc?", "Proc.new { }", true),
            ("proc?", "Proc.new", true),
            ("proc?", "foo.new", false),
            ("lambda_or_proc?", "lambda { }", true),
            ("lambda_or_proc?", "proc { }", true),
            ("lambda_or_proc?", "foo { }", false),
        ];
        for (name, ruby, expected) in cases {
            assert_eq!(check(name, ruby, &[]), *expected, "{name} against {ruby}");
        }
    }

    #[test]
    fn unary_predicates_with_arguments() {
        assert!(check("method?", "foo.bar", &[Arg::Symbol("bar".into())]));
        assert!(!check("method?", "foo.bar", &[Arg::Symbol("baz".into())]));
        assert!(check(
            "method?",
            "def bar; end",
            &[Arg::Symbol("bar".into())]
        ));
        // A `{}` union compiles to a Set upstream, matched by membership.
        assert!(check(
            "method?",
            "foo.bar",
            &[Arg::Set(vec![
                Arg::Symbol("baz".into()),
                Arg::Symbol("bar".into()),
            ])]
        ));
        assert!(check("command?", "foo 1", &[Arg::Symbol("foo".into())]));
        assert!(!check("command?", "a.foo 1", &[Arg::Symbol("foo".into())]));
        assert!(check(
            "global_const?",
            "Proc",
            &[Arg::Symbol("Proc".into())]
        ));
        assert!(check(
            "global_const?",
            "::Proc",
            &[Arg::Symbol("Proc".into())]
        ));
        assert!(!check(
            "global_const?",
            "A::Proc",
            &[Arg::Symbol("Proc".into())]
        ));
        assert!(!check(
            "global_const?",
            "Proc",
            &[Arg::Symbol("Data".into())]
        ));
        // An unresolved argument fails closed.
        assert!(!check("method?", "foo.bar", &[Arg::Unresolved]));
        // A missing argument fails closed too.
        assert!(!check("method?", "foo.bar", &[]));
    }

    #[test]
    fn method_name_predicates_read_sends_defs_and_bare_names() {
        assert!(check("operator_method?", "a + b", &[]));
        assert!(check("predicate_method?", "a.foo?", &[]));
        assert!(check("bang_method?", "a.foo!", &[]));
        assert!(check("camel_case_method?", "a.FooBar", &[]));
        assert!(check("comparison_method?", "a == b", &[]));
        assert!(check("assignment_method?", "a.b = 1", &[]));
        assert!(check("enumerator_method?", "a.each_with_index", &[]));
        assert!(check("enumerable_method?", "a.detect", &[]));
        assert!(check("negation_method?", "!a", &[]));
        assert!(check("nonmutating_array_method?", "a.flatten", &[]));
        assert!(check("nonmutating_string_method?", "a.upcase", &[]));
        assert!(check("nonmutating_hash_method?", "a.merge", &[]));
        assert!(check("nonmutating_unary_operator_method?", "-a", &[]));
        assert!(check("nonmutating_binary_operator_method?", "a * b", &[]));
        assert!(check("nonmutating_operator_method?", "a * b", &[]));
        // A `def` answers the same mixin.
        assert!(check("predicate_method?", "def foo?; end", &[]));
        // And so does a bare name slot, which is what `(send _ operator_method?)`
        // actually reaches.
        assert!(check_name("operator_method?", b"+", &[]));
        assert!(!check_name("operator_method?", b"foo", &[]));
    }

    #[test]
    fn prefix_not_and_bang() {
        assert!(check("prefix_not?", "not a", &[]));
        assert!(!check("prefix_not?", "!a", &[]));
        assert!(check("prefix_bang?", "!a", &[]));
        assert!(!check("prefix_bang?", "not a", &[]));
    }

    // -- source and structure --------------------------------------------

    #[test]
    fn source_and_structure_family() {
        let cases: &[(&str, &str, bool)] = &[
            ("single_line?", "foo", true),
            ("single_line?", "foo(\n1\n)", false),
            ("multiline?", "foo(\n1\n)", true),
            ("multiline?", "foo", false),
            ("braces?", "{ a: 1 }", true),
            ("value_omission?", "{ a: }", false),
        ];
        for (name, ruby, expected) in cases {
            assert_eq!(check(name, ruby, &[]), *expected, "{name} against {ruby}");
        }
    }

    #[test]
    fn value_omission_reads_the_pair_not_the_hash() {
        let result = parse("{ a: }");
        let node = first_stmt(&result);
        let hash = node.as_hash_node().expect("hash");
        let pair = hash.elements().iter().next().expect("one pair");
        let builtin = lookup("value_omission?").expect("registered");
        assert!((builtin.eval)(
            &PredCtx::empty(),
            &PredTarget::Node(&pair),
            &[]
        ));

        let result = parse("{ a: 1 }");
        let node = first_stmt(&result);
        let hash = node.as_hash_node().expect("hash");
        let pair = hash.elements().iter().next().expect("one pair");
        assert!(!(builtin.eval)(
            &PredCtx::empty(),
            &PredTarget::Node(&pair),
            &[]
        ));
    }

    // -- value slots ------------------------------------------------------

    #[test]
    fn value_slot_predicates() {
        // `(str empty?)` sends the predicate to the string's value.
        assert!(check_name("empty?", b"", &[]));
        assert!(!check_name("empty?", b"x", &[]));
        assert!(check("empty?", "''", &[]));
        assert!(!check("empty?", "'x'", &[]));
        assert!(check("empty?", "[]", &[]));
        assert!(!check("empty?", "[1]", &[]));
        assert!(check_name("blank?", b"  ", &[]));
        assert!(!check_name("blank?", b"x", &[]));
        // `(int positive?)` reaches the literal node in this interpreter.
        assert!(check("positive?", "1", &[]));
        assert!(!check("positive?", "0", &[]));
        assert!(check("negative?", "-1", &[]));
        assert!(check("zero?", "0", &[]));
        assert!(!check("zero?", "1", &[]));
        assert!(check("positive?", "1_000", &[]));
    }

    #[test]
    fn predicates_answer_false_for_an_absent_child() {
        for builtin in all() {
            let args = match builtin.arity {
                Arity::Nullary => Vec::new(),
                Arity::Unary => vec![Arg::Symbol("x".into())],
            };
            assert!(
                !(builtin.eval)(&PredCtx::empty(), &PredTarget::Absent, &args),
                "{} matched an absent child",
                builtin.name
            );
        }
    }
}
