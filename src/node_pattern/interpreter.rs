//! NodePattern interpreter — runtime evaluation of patterns against Prism AST nodes.
//!
//! Given a NodePattern string and a Prism AST node, determine whether the
//! pattern matches the node. This is used by the verifier to detect drift
//! between RuboCop's NodePattern definitions and our hand-written Rust cops.
//!
//! ## Supported patterns
//!
//! NodeMatch, Wildcard, Rest, NilPredicate, SymbolLiteral, IntLiteral,
//! StringLiteral, TrueLiteral, FalseLiteral, NilLiteral, Alternatives,
//! Subsequence, AnyOrder, Conjunction, Negation, Capture, TypePredicate, Ident.
//!
//! ## Any-order groups
//!
//! `<a b ...>` matches when every term matches a distinct child, in any order.
//! The matcher backtracks over child-to-term assignments (see
//! [`assign_any_order`]) where RuboCop compiles a greedy first-fit loop; the
//! parser caps a group at `ANY_ORDER_MAX_TERMS` non-rest terms so the search
//! cannot blow up.
//!
//! ## Captures
//!
//! `$` captures are real: the parser numbers them in source order and the
//! matcher binds each slot in a [`MatchEnv`], which journals writes so a
//! capture made inside a rejected union branch or `...` split is unwound (see
//! `captures.rs`). [`match_with_captures`] returns the bound values;
//! [`interpret_pattern`] keeps the boolean-only entry point.
//!
//! Known divergences from RuboCop's compiler, in addition to the stubs below:
//!
//! - `(int $_)` / `(float $_)` bind the literal node, not the numeric value.
//!
//! ## Resolution
//!
//! `pred?` resolves against the builtin registry ([`super::predicates`]);
//! `#helper` gives the owner's [`Resolver`] first refusal and falls back to the
//! registry, so the matchers `node.rb` defines on `Node` itself (`#literal?`,
//! `#global_const?`) work with no owner at all. `%1` / `%name` come from
//! [`Params`], `%Const` from the resolver. A name nothing explains is a
//! **compile error** ([`PatternError::UnknownHelper`] and friends), never a
//! silent `true`.
//!
//! ## Deferred
//!
//! ParentRef (^) and DescendRef (`) still return true. Captures nested under
//! those stubs are bound optimistically too.

use super::ancestors;
use super::captures::{CaptureValue, Captures, MatchEnv, dup_node};
use super::lexer::Lexer;
use super::parser::{COMPLEX_SEQ_HEAD, Parser, PatternError, PatternNode, RepeatKind};
use super::predicates::{self, Arg, Arity, NodeId, PredCtx, PredTarget};
use super::resolve::{NoResolver, Params, Resolver};

/// A child slot in the NodePattern positional matching.
///
/// Node children are heterogeneous: some are AST nodes, some are name/value
/// bytes, and some are absent (the `nil?` predicate).
#[derive(Debug)]
pub enum MatchChild<'pr> {
    /// A child AST node (receiver, body, condition, etc.)
    Node(ruby_prism::Node<'pr>),
    /// An absent child — `nil?` matches this.
    Absent,
    /// A name or value as raw bytes (method name, variable name, symbol value).
    Name(&'pr [u8]),
    /// A Parser-gem child node that Prism does not materialize, reduced to its
    /// type and its single value.
    ///
    /// Prism stores some Parser-gem subtrees as flat data on the parent: a
    /// regexp's body and options are a location plus a flag bitset, not
    /// `(str …)` and `(regopt …)` children; a numbered block's parameter count
    /// is a `u8`; `it` is a flag on the parameters node. Those children are
    /// synthesized so the patterns that address them still match.
    Synthetic {
        /// The Parser-gem type this stands in for (`str`, `regopt`, `int`, …).
        parser_type: &'static str,
        /// The value the synthesized node carries.
        value: &'pr [u8],
    },
}

/// Evaluate a term whose meaning comes from outside the pattern text.
///
/// This is the single seam the four match dispatchers ([`matches_node`],
/// [`matches_absent`], [`matches_name`], [`matches_synthetic`]) route their
/// unresolved terms through.
///
/// - `pred?` is a method on the matched node, so it resolves against the
///   builtin registry only (`node_pattern_subcompiler.rb:80-82`).
/// - `#helper` is a method on the pattern's owner, so the resolver gets first
///   refusal; a name the owner does not claim falls back to the registry,
///   which is what makes `#global_const?(:Proc)` work for the matchers
///   `node.rb` defines on `Node` itself (`:84-86`).
/// - `%param` / `%Const` / a regexp are atoms compared with `===` against the
///   child slot (`:107-109`).
/// - `^` and `` ` `` are still optimistic; the ancestors PR replaces them.
///
/// Every name reaching here has already been accepted by
/// [`collect_unresolved`] at compile time, so an unknown name is an internal
/// inconsistency and fails closed rather than matching.
fn matches_deferred<'pr>(
    pattern: &PatternNode,
    target: &PredTarget<'_, 'pr>,
    env: &mut MatchEnv<'pr, '_>,
) -> bool {
    // Capture discipline: a `$` nested inside an argument list still owns a
    // slot, so a rejected attempt unwinds like any other.
    let mark = env.mark();
    if eval_deferred(pattern, target, env) {
        return true;
    }
    env.rollback(mark);
    false
}

/// [`matches_deferred`] without the capture bookkeeping.
fn eval_deferred<'pr>(
    pattern: &PatternNode,
    target: &PredTarget<'_, 'pr>,
    env: &mut MatchEnv<'pr, '_>,
) -> bool {
    match pattern {
        PatternNode::Predicate { name, args } => {
            let Some(builtin) = predicates::lookup(name) else {
                return false;
            };
            let args = eval_args(args, env);
            (builtin.eval)(&PredCtx::new(env.chain()), target, &args)
        }
        PatternNode::HelperCall { name, args } => {
            let resolver = env.resolver();
            let args = eval_args(args, env);
            if let Some(matcher) = resolver.matcher(name) {
                // A named matcher is itself a pattern: its `%1` is this call's
                // first argument, and its captures are its own. It is applied
                // to the child slot, which may be a name rather than a node —
                // rubocop-rspec's `#Examples.all` takes the method symbol.
                return matcher.matches_target(
                    target,
                    env.chain(),
                    &Params::positional(args),
                    resolver,
                );
            }
            let Some(builtin) = predicates::lookup(name) else {
                return false;
            };
            (builtin.eval)(&PredCtx::new(env.chain()), target, &args)
        }
        PatternNode::ParamNumber(number) => {
            let arg = env.positional_param(*number);
            arg_matches_target(&arg, target)
        }
        PatternNode::ParamNamed(name) => {
            let arg = env.named_param(name);
            arg_matches_target(&arg, target)
        }
        PatternNode::ParamConst(name) => {
            let arg = env
                .resolver()
                .constant(name)
                .cloned()
                .unwrap_or(Arg::Unresolved);
            arg_matches_target(&arg, target)
        }
        PatternNode::Regexp { body, flags } => arg_matches_target(
            &Arg::Regexp {
                body: body.clone(),
                flags: flags.clone(),
            },
            target,
        ),
        PatternNode::ParentRef(inner) => matches_ascend(inner, target, env),
        PatternNode::DescendRef(inner) => matches_descend(inner, target, env),
        _ => false,
    }
}

/// `^pattern` — match `pattern` against the target's parent.
///
/// Upstream is `(a = access_node) && (a = a.parent) && <pattern on a>`
/// (`node_pattern_subcompiler.rb:30-35`): a target with no parent fails, and
/// the parent is then an ordinary node position, so `^^x` is `^` applied
/// again to the parent.
///
/// Making `^^` work means the ascended term has to see the *parent's* chain,
/// not the child's. The chain is therefore truncated to the parent's ancestors
/// for the duration of the inner match and restored afterwards; the tail it
/// gives up is the Prism nodes between parent and target, which the Parser gem
/// does not have (`ancestors.rs`).
fn matches_ascend<'pr>(
    inner: &PatternNode,
    target: &PredTarget<'_, 'pr>,
    env: &mut MatchEnv<'pr, '_>,
) -> bool {
    // A non-node target is a name or an absent child; neither has a parent
    // pointer upstream (`nil.parent` raises, and the guard clause rejects it).
    if target.node().is_none() {
        return false;
    }
    let Some(index) = ancestors::visible_index(env.chain(), 0) else {
        return false;
    };
    let parent = dup_node(&env.chain()[index]);
    let outer: Vec<ruby_prism::Node<'pr>> = env.chain()[..index].iter().map(dup_node).collect();
    let saved = env.replace_chain(outer);
    let matched = matches_node(inner, &parent, env);
    env.replace_chain(saved);
    matched
}

/// The depth `` ` `` is allowed to search.
///
/// Upstream's `NodePattern.descend` is unbounded (`node_pattern.rb:60-73`).
/// The interpreter caps it for the same reason the parser caps `<>` arity:
/// a pattern is data, and a pathological one must not turn into a quadratic
/// walk of a whole file. Nothing in the vendored corpus looks deeper than a
/// handful of levels.
const DESCEND_MAX_DEPTH: usize = 32;

/// `` `pattern `` — match `pattern` against the target or any of its
/// descendants.
///
/// `NodePattern.descend` yields the element itself first and then recurses
/// into `children` (`node_pattern.rb:60-73`), so a bare `` `x `` also matches
/// when the target *is* an `x`. A non-node element is yielded but not
/// descended into, which is why the walk enumerates [`MatchChild`]s: those are
/// exactly Parser's `children`.
fn matches_descend<'pr>(
    inner: &PatternNode,
    target: &PredTarget<'_, 'pr>,
    env: &mut MatchEnv<'pr, '_>,
) -> bool {
    // Upstream yields the element itself first.
    let here = match target {
        PredTarget::Node(node) => matches_node(inner, node, env),
        PredTarget::Absent => matches_absent(inner, env),
        // A `$` under `` ` `` on a name slot binds nothing: the empty slice is
        // the only `'pr`-lived bytes available here, and the case cannot
        // arise from a vendored pattern (`` ` `` is always written against a
        // node position).
        PredTarget::Name(bytes) => matches_name(inner, bytes, b"", env),
        PredTarget::Synthetic { parser_type, value } => {
            matches_synthetic(inner, parser_type, value, env)
        }
    };
    if here {
        return true;
    }
    let Some(node) = target.node() else {
        return false;
    };
    descend_children(inner, node, 0, env)
}

/// The recursive half of [`matches_descend`]: every child of `node`, then
/// their children.
fn descend_children<'pr>(
    inner: &PatternNode,
    node: &ruby_prism::Node<'pr>,
    depth: usize,
    env: &mut MatchEnv<'pr, '_>,
) -> bool {
    if depth >= DESCEND_MAX_DEPTH {
        return false;
    }
    let children = descend_child_slots(node);
    if children.is_empty() {
        return false;
    }
    env.enter(node);
    let matched = children.iter().any(|child| {
        let mark = env.mark();
        if matches_child(inner, child, env) {
            return true;
        }
        env.rollback(mark);
        match child {
            MatchChild::Node(child_node) => descend_children(inner, child_node, depth + 1, env),
            _ => false,
        }
    });
    env.leave();
    matched
}

/// The match child for a `body` slot, with Prism's `StatementsNode` wrapper
/// peeled off a one-statement body.
///
/// Parser builds a `begin` only for a statement *list*: `def m; x; end` is
/// `(def :m (args) (send nil :x))`, not `(def :m (args) (begin (send nil :x)))`
/// — the same rule [`super::ancestors`] applies to the enclosing-node chain.
/// Prism always wraps a body in a `StatementsNode`, so without this every
/// upstream pattern that spells a single-statement body — `(block $(call _
/// {:max_by :min_by}) (args (arg $_x)) (lvar _x))`, say — silently fails to
/// match. A list of two or more statements *is* a Parser `begin` and is left
/// alone.
fn body_child<'pr>(body: Option<ruby_prism::Node<'pr>>) -> MatchChild<'pr> {
    let Some(node) = body else {
        return MatchChild::Absent;
    };
    if let Some(statements) = node.as_statements_node() {
        let mut only: Option<ruby_prism::Node<'pr>> = None;
        for (index, statement) in statements.body().iter().enumerate() {
            if index > 0 {
                return MatchChild::Node(node);
            }
            only = Some(statement);
        }
        if let Some(statement) = only {
            return MatchChild::Node(statement);
        }
    }
    MatchChild::Node(node)
}

/// Everything `descend` should walk into below `node`.
///
/// Parser's `children` for a block is `[send, args, body]`, and Prism's
/// `CallNode` *is* both that block node and its `send` child. Taking the
/// `block` view alone would re-enter the node through its own first child;
/// taking the `send` view alone would miss the block's parameters and body.
/// So both views are enumerated, minus the self-referential sequence head.
fn descend_child_slots<'pr>(node: &ruby_prism::Node<'pr>) -> Vec<MatchChild<'pr>> {
    let mut slots = Vec::new();
    if let Some(parser_type) = parser_type_for_node(node)
        && let Some(children) = get_children(parser_type, node)
    {
        slots.extend(children);
    }
    if let Some(block_type) = block_type_of(node)
        && let Some(children) = get_children(block_type, node)
    {
        slots.extend(children.into_iter().skip(1));
    }
    slots
}

/// Reduce a `#call` / `pred?` argument to an atom.
///
/// Upstream compiles arguments with the `AtomSubcompiler`
/// (`compiler/atom_subcompiler.rb`): literals stay literals, a `{}` of
/// literals becomes a `Set`, and a `%param` becomes whatever the caller
/// passed. Anything that is not an atom (a nested sequence, say) upstream
/// turns into a lambda; here it is [`Arg::Unresolved`], which never matches.
fn eval_arg(pattern: &PatternNode, env: &MatchEnv<'_, '_>) -> Arg {
    match pattern {
        PatternNode::SymbolLiteral(name) => Arg::Symbol(name.clone()),
        PatternNode::StringLiteral(text) => Arg::Str(text.clone()),
        PatternNode::IntLiteral(value) => Arg::Int(*value),
        PatternNode::FloatLiteral(text) => text.parse::<f64>().map_or(Arg::Unresolved, Arg::Float),
        PatternNode::Regexp { body, flags } => Arg::Regexp {
            body: body.clone(),
            flags: flags.clone(),
        },
        // A node type used as an atom is a bare name, e.g. `#foo(bar)`.
        PatternNode::Ident(name) => Arg::Symbol(name.clone()),
        PatternNode::Alternatives(alts) => {
            Arg::Set(alts.iter().map(|alt| eval_arg(alt, env)).collect())
        }
        PatternNode::ParamNumber(number) => env.positional_param(*number),
        PatternNode::ParamNamed(name) => env.named_param(name),
        PatternNode::ParamConst(name) => env
            .resolver()
            .constant(name)
            .cloned()
            .unwrap_or(Arg::Unresolved),
        _ => Arg::Unresolved,
    }
}

/// Reduce a whole argument list.
fn eval_args(args: &[PatternNode], env: &MatchEnv<'_, '_>) -> Vec<Arg> {
    args.iter().map(|arg| eval_arg(arg, env)).collect()
}

/// `arg === access_element` — how upstream matches a parameter, a constant or
/// a regexp against the child slot (`node_pattern_subcompiler.rb:107-109`).
fn arg_matches_target(arg: &Arg, target: &PredTarget<'_, '_>) -> bool {
    if let Arg::Unresolved = arg {
        return false;
    }
    if let Some(bytes) = target.value_bytes() {
        return arg.accepts_value(bytes);
    }
    let Some(node) = target.node() else {
        return false;
    };
    match arg {
        // A `Set`/literal compared against a node only matches when the node
        // is the corresponding literal, which is what `===` does in Ruby.
        Arg::Set(items) => items.iter().any(|item| arg_matches_target(item, target)),
        Arg::Int(value) => {
            literal_text(node).and_then(|text| text.parse::<i64>().ok()) == Some(*value)
        }
        Arg::Float(value) => {
            literal_text(node).and_then(|text| text.parse::<f64>().ok()) == Some(*value)
        }
        Arg::Symbol(_) | Arg::Str(_) | Arg::Regexp { .. } => {
            literal_bytes(node).is_some_and(|bytes| arg.accepts_value(bytes))
        }
        // `node === other` is `==` upstream, i.e. structural equality; no
        // vendored pattern uses a node-valued atom that way (the only
        // node-valued arguments go to `equal?`), so identity is the
        // conservative reading.
        Arg::Node(id) => *id == NodeId::of(node),
        Arg::Unresolved => false,
    }
}

/// The decoded value of a literal node, for comparison against an atom.
fn literal_bytes<'pr>(node: &ruby_prism::Node<'pr>) -> Option<&'pr [u8]> {
    if let Some(string) = node.as_string_node() {
        return Some(string.content_loc().as_slice());
    }
    if let Some(symbol) = node.as_symbol_node() {
        return symbol
            .value_loc()
            .map(|loc| loc.as_slice())
            .or_else(|| Some(symbol.location().as_slice()));
    }
    if let Some(constant) = node.as_constant_read_node() {
        return Some(constant.name().as_slice());
    }
    None
}

/// Source text of a numeric literal node, underscores stripped by the caller.
fn literal_text(node: &ruby_prism::Node<'_>) -> Option<String> {
    if node.as_integer_node().is_none() && node.as_float_node().is_none() {
        return None;
    }
    std::str::from_utf8(node.location().as_slice())
        .ok()
        .map(|text| text.replace('_', ""))
}

/// A name a pattern refers to but neither the registry nor the resolver knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unresolved {
    /// `#name` — a cop-local matcher the resolver did not supply.
    Helper(String),
    /// `name?` — a node predicate with no registry entry.
    Predicate(String),
    /// `%Const` / bare `Const` — a constant the resolver did not supply.
    Constant(String),
}

impl Unresolved {
    /// The bare name, without its sigil.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Unresolved::Helper(name) | Unresolved::Predicate(name) | Unresolved::Constant(name) => {
                name
            }
        }
    }
}

impl std::fmt::Display for Unresolved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unresolved::Helper(name) => write!(f, "#{name}"),
            Unresolved::Predicate(name) => write!(f, "{name}"),
            Unresolved::Constant(name) => write!(f, "%{name}"),
        }
    }
}

/// Walk `pattern` and collect every name `resolver` and the builtin registry
/// both fail to explain, in source order.
///
/// This is the compile-time check that turns an unknown helper into an error
/// instead of a silent `true`.
pub fn collect_unresolved(
    pattern: &PatternNode,
    resolver: &dyn Resolver,
    out: &mut Vec<Unresolved>,
) {
    match pattern {
        PatternNode::Predicate { name, args } => {
            if predicates::lookup(name).is_none() {
                out.push(Unresolved::Predicate(name.clone()));
            }
            for arg in args {
                collect_unresolved(arg, resolver, out);
            }
        }
        PatternNode::HelperCall { name, args } => {
            if resolver.matcher(name).is_none() && predicates::lookup(name).is_none() {
                out.push(Unresolved::Helper(name.clone()));
            }
            for arg in args {
                collect_unresolved(arg, resolver, out);
            }
        }
        PatternNode::ParamConst(name) => {
            if resolver.constant(name).is_none() {
                out.push(Unresolved::Constant(name.clone()));
            }
        }
        PatternNode::NodeMatch { children, .. } => {
            for child in children {
                collect_unresolved(child, resolver, out);
            }
        }
        PatternNode::Alternatives(items)
        | PatternNode::Conjunction(items)
        | PatternNode::Subsequence(items)
        | PatternNode::AnyOrder(items) => {
            for item in items {
                collect_unresolved(item, resolver, out);
            }
        }
        PatternNode::Negation(inner)
        | PatternNode::ParentRef(inner)
        | PatternNode::DescendRef(inner)
        | PatternNode::Repetition { inner, .. } => collect_unresolved(inner, resolver, out),
        PatternNode::Capture { inner, .. } => collect_unresolved(inner, resolver, out),
        _ => {}
    }
}

/// Check the arity of every resolved predicate against its registry entry.
fn check_arities(pattern: &PatternNode) -> Result<(), PatternError> {
    let check = |name: &String, args: &Vec<PatternNode>| -> Result<(), PatternError> {
        if let Some(builtin) = predicates::lookup(name) {
            let expected = match builtin.arity {
                Arity::Nullary => 0,
                Arity::Unary => 1,
            };
            if args.len() != expected {
                return Err(PatternError::PredicateArity {
                    name: name.clone(),
                    expected,
                    found: args.len(),
                });
            }
        }
        Ok(())
    };
    match pattern {
        PatternNode::Predicate { name, args } | PatternNode::HelperCall { name, args } => {
            check(name, args)?;
            for arg in args {
                check_arities(arg)?;
            }
        }
        PatternNode::NodeMatch { children, .. } => {
            for child in children {
                check_arities(child)?;
            }
        }
        PatternNode::Alternatives(items)
        | PatternNode::Conjunction(items)
        | PatternNode::Subsequence(items)
        | PatternNode::AnyOrder(items) => {
            for item in items {
                check_arities(item)?;
            }
        }
        PatternNode::Negation(inner)
        | PatternNode::ParentRef(inner)
        | PatternNode::DescendRef(inner)
        | PatternNode::Repetition { inner, .. } => check_arities(inner)?,
        PatternNode::Capture { inner, .. } => check_arities(inner)?,
        _ => {}
    }
    Ok(())
}

/// A parsed NodePattern plus the number of capture slots it allocates.
#[derive(Debug, Clone)]
pub struct CompiledPattern {
    ast: PatternNode,
    capture_count: usize,
}

impl CompiledPattern {
    /// Lex, parse and resolve `pattern_str` against the builtin registry alone.
    ///
    /// Returns `None` on a parse error, an invalid pattern (`{}` branches with
    /// different capture counts), or a `#helper` / `pred?` / `%Const` no
    /// builtin explains. Use [`CompiledPattern::compile_with`] to supply the
    /// owner's matchers and constants, and to see *why* a pattern was
    /// rejected.
    #[must_use]
    pub fn compile(pattern_str: &str) -> Option<Self> {
        Self::compile_with(pattern_str, &NoResolver).ok()
    }

    /// Lex, parse and resolve `pattern_str`, with `resolver` supplying the
    /// owner-specific names.
    ///
    /// # Errors
    ///
    /// [`PatternError::Syntax`] when the pattern does not parse,
    /// [`PatternError::UnknownHelper`] / [`PatternError::UnknownPredicate`] /
    /// [`PatternError::UnknownConstant`] naming the first name nothing
    /// explains, or [`PatternError::PredicateArity`] when a builtin is called
    /// with the wrong number of arguments.
    pub fn compile_with(pattern_str: &str, resolver: &dyn Resolver) -> Result<Self, PatternError> {
        let mut lexer = Lexer::new(pattern_str);
        let mut parser = Parser::new(lexer.tokenize());
        let Some(ast) = parser.parse() else {
            return Err(parser.error().cloned().unwrap_or(PatternError::Syntax));
        };
        let mut unresolved = Vec::new();
        collect_unresolved(&ast, resolver, &mut unresolved);
        if let Some(first) = unresolved.into_iter().next() {
            return Err(match first {
                Unresolved::Helper(name) => PatternError::UnknownHelper { name },
                Unresolved::Predicate(name) => PatternError::UnknownPredicate { name },
                Unresolved::Constant(name) => PatternError::UnknownConstant { name },
            });
        }
        check_arities(&ast)?;
        Ok(Self {
            ast,
            capture_count: parser.capture_count(),
        })
    }

    /// Number of `$` capture slots in this pattern.
    #[must_use]
    pub fn capture_count(&self) -> usize {
        self.capture_count
    }

    /// The parsed pattern AST.
    #[must_use]
    pub fn ast(&self) -> &PatternNode {
        &self.ast
    }

    /// Whether the pattern matches `node`, discarding captures.
    #[must_use]
    pub fn matches(&self, node: &ruby_prism::Node<'_>) -> bool {
        self.matches_in(node, &Params::new(), &NoResolver)
    }

    /// Whether the pattern matches `node`, with `params` bound to its
    /// `%param` references and `resolver` answering its `#helper` calls.
    #[must_use]
    pub fn matches_in(
        &self,
        node: &ruby_prism::Node<'_>,
        params: &Params,
        resolver: &dyn Resolver,
    ) -> bool {
        self.matches_with_ancestors(node, &[], params, resolver)
    }

    /// [`CompiledPattern::matches_in`] with the node's ancestor chain,
    /// outermost first — what `^`, `root?` and `value_used?` need.
    ///
    /// This is the entry point an ancestor-aware cop uses: hand it the slice
    /// `Cop::check_node_with_ancestors` was given.
    #[must_use]
    pub fn matches_with_ancestors<'pr>(
        &self,
        node: &ruby_prism::Node<'pr>,
        ancestors: &[ruby_prism::Node<'pr>],
        params: &Params,
        resolver: &dyn Resolver,
    ) -> bool {
        let mut env =
            MatchEnv::with_inputs(self.capture_count, params, resolver, ancestors).with_root(node);
        matches_node(&self.ast, node, &mut env)
    }

    /// Whether the pattern matches whatever sits in a child slot.
    ///
    /// `#helper` is applied to `access_element`, which is a node only some of
    /// the time — rubocop-rspec's `#Examples.all` is handed the method symbol,
    /// and `#rspec?` in receiver position can be handed an absent child. The
    /// callee's captures are its own and are discarded, as upstream's are: a
    /// function call compiles to a boolean, not to a binding.
    pub(crate) fn matches_target<'pr>(
        &self,
        target: &PredTarget<'_, 'pr>,
        ancestors: &[ruby_prism::Node<'pr>],
        params: &Params,
        resolver: &dyn Resolver,
    ) -> bool {
        let mut env = MatchEnv::with_inputs(self.capture_count, params, resolver, ancestors);
        if let Some(node) = target.node() {
            // `%0` inside the callee is *its* `param0`, i.e. the value the
            // matcher was applied to (`method_definer.rb:10-17`).
            env = env.with_root(node);
        }
        match target {
            PredTarget::Node(node) => matches_node(&self.ast, node, &mut env),
            PredTarget::Absent => matches_absent(&self.ast, &mut env),
            PredTarget::Name(bytes) => matches_name(&self.ast, bytes, b"", &mut env),
            PredTarget::Synthetic { parser_type, value } => {
                matches_synthetic(&self.ast, parser_type, value, &mut env)
            }
        }
    }

    /// [`CompiledPattern::match_captures`] with `%param` bindings and a
    /// resolver.
    #[must_use]
    pub fn match_captures_in<'pr>(
        &self,
        node: &ruby_prism::Node<'pr>,
        params: &Params,
        resolver: &dyn Resolver,
    ) -> Option<Captures<'pr>> {
        self.match_captures_with_ancestors(node, &[], params, resolver)
    }

    /// [`CompiledPattern::match_captures_in`] with the node's ancestor chain.
    #[must_use]
    pub fn match_captures_with_ancestors<'pr>(
        &self,
        node: &ruby_prism::Node<'pr>,
        ancestors: &[ruby_prism::Node<'pr>],
        params: &Params,
        resolver: &dyn Resolver,
    ) -> Option<Captures<'pr>> {
        let mut env =
            MatchEnv::with_inputs(self.capture_count, params, resolver, ancestors).with_root(node);
        if matches_node(&self.ast, node, &mut env) {
            Some(env.into_captures())
        } else {
            None
        }
    }

    /// Match `node`, returning the bound captures on success.
    ///
    /// The returned [`Captures`] always has [`CompiledPattern::capture_count`]
    /// slots; a slot can still be unbound if its capture sits under a stubbed
    /// term (`#pred`, `%param`, `^`, `` ` ``).
    #[must_use]
    pub fn match_captures<'pr>(&self, node: &ruby_prism::Node<'pr>) -> Option<Captures<'pr>> {
        self.match_captures_in(node, &Params::new(), &NoResolver)
    }
}

/// Evaluate a NodePattern string against a Prism AST node.
///
/// Returns `true` if the pattern matches the node, `false` otherwise.
/// Returns `false` on parse error.
pub fn interpret_pattern(pattern_str: &str, node: &ruby_prism::Node<'_>) -> bool {
    CompiledPattern::compile(pattern_str).is_some_and(|pattern| pattern.matches(node))
}

/// Evaluate a NodePattern string and return the values bound by its `$`
/// captures, or `None` if the pattern does not parse or does not match.
#[must_use]
pub fn match_with_captures<'pr>(
    pattern_str: &str,
    node: &ruby_prism::Node<'pr>,
) -> Option<Captures<'pr>> {
    CompiledPattern::compile(pattern_str)?.match_captures(node)
}

/// Parser-gem type → the group type it also answers to.
///
/// Verbatim from rubocop-ast's `GROUP_FOR_TYPE`
/// (`vendor/rubocop-ast/lib/rubocop/ast/node.rb:89-129`); `(call …)`,
/// `(any_block …)`, `numeric?` and friends are compiled to a `<group>_type?`
/// test, so a group name is usable anywhere a type name is.
const GROUP_FOR_TYPE: &[(&str, &str)] = &[
    ("def", "any_def"),
    ("defs", "any_def"),
    ("arg", "argument"),
    ("optarg", "argument"),
    ("restarg", "argument"),
    ("kwarg", "argument"),
    ("kwoptarg", "argument"),
    ("kwrestarg", "argument"),
    ("blockarg", "argument"),
    ("forward_arg", "argument"),
    ("shadowarg", "argument"),
    ("true", "boolean"),
    ("false", "boolean"),
    ("int", "numeric"),
    ("float", "numeric"),
    ("rational", "numeric"),
    ("complex", "numeric"),
    ("str", "any_str"),
    ("dstr", "any_str"),
    ("xstr", "any_str"),
    ("sym", "any_sym"),
    ("dsym", "any_sym"),
    ("irange", "range"),
    ("erange", "range"),
    ("send", "call"),
    ("csend", "call"),
    ("block", "any_block"),
    ("numblock", "any_block"),
    ("itblock", "any_block"),
    ("match_pattern", "any_match_pattern"),
    ("match_pattern_p", "any_match_pattern"),
];

/// The group type `parser_type` belongs to, if any.
fn group_of(parser_type: &str) -> Option<&'static str> {
    GROUP_FOR_TYPE
        .iter()
        .find(|(ty, _)| *ty == parser_type)
        .map(|(_, group)| *group)
}

/// Whether a node of Parser type `actual` answers to the pattern type
/// `pattern_type`, directly or through its group.
fn type_answers(actual: &str, pattern_type: &str) -> bool {
    actual == pattern_type || group_of(actual) == Some(pattern_type)
}

/// The Parser-gem block type a node answers to *as a block node*.
///
/// Prism has no node for Parser's `block`: `foo { }` is a `CallNode` whose
/// `block` is a `BlockNode`, and `-> { }` is a `LambdaNode`. Parser instead
/// wraps the call: `(block (send nil :foo) (args) body)`. A `CallNode` carrying
/// a literal block therefore answers to *both* `send`/`csend` (Parser's inner
/// send node, which is also what `on_send` visits) and `block` — which is a
/// deliberate over-match: `(send …)` matched directly against a Parser `block`
/// node is false upstream and true here.
pub(crate) fn block_type_of(node: &ruby_prism::Node<'_>) -> Option<&'static str> {
    if let Some(call) = node.as_call_node() {
        let block = call.block()?;
        return Some(block_node_type(&block.as_block_node()?));
    }
    if node.as_lambda_node().is_some() {
        return Some("block");
    }
    None
}

/// `block` / `numblock` / `itblock` for a Prism `BlockNode`.
///
/// Parser gives `{ _1 }` and `{ it }` their own node types; Prism keeps one
/// `BlockNode` and varies the parameters node.
fn block_node_type(block: &ruby_prism::BlockNode<'_>) -> &'static str {
    match block.parameters() {
        Some(ruby_prism::Node::NumberedParametersNode { .. }) => "numblock",
        Some(ruby_prism::Node::ItParametersNode { .. }) => "itblock",
        _ => "block",
    }
}

/// The concrete Parser type to read children for when `node` is matched
/// against a pattern written for `pattern_type`, or `None` if it does not
/// answer to that type at all.
fn concrete_type(node: &ruby_prism::Node<'_>, pattern_type: &str) -> Option<&'static str> {
    if let Some(actual) = parser_type_for_node(node)
        && type_answers(actual, pattern_type)
    {
        return Some(actual);
    }
    if let Some(block_type) = block_type_of(node)
        && type_answers(block_type, pattern_type)
    {
        return Some(block_type);
    }
    None
}

/// Whether `node` answers to the Parser type `pattern_type`.
pub(crate) fn node_has_type(node: &ruby_prism::Node<'_>, pattern_type: &str) -> bool {
    concrete_type(node, pattern_type).is_some()
}

/// Get the NodePattern type name for a Prism node.
///
/// Returns the Parser gem type name (e.g. "send", "block", "if") that
/// corresponds to this Prism node, or `None` if unmapped.
pub(crate) fn parser_type_for_node(node: &ruby_prism::Node<'_>) -> Option<&'static str> {
    // send vs csend: both are CallNode, distinguished by &. operator
    if let Some(call) = node.as_call_node() {
        return if call
            .call_operator_loc()
            .is_some_and(|loc| loc.as_slice() == b"&.")
        {
            Some("csend")
        } else {
            Some("send")
        };
    }

    // Use matches! for everything else
    match node {
        // Parser spells the implicit `it` of a `{ it }` block `(lvar :it)`.
        ruby_prism::Node::ItLocalVariableReadNode { .. } => Some("lvar"),
        ruby_prism::Node::BlockNode { .. } => node.as_block_node().as_ref().map(block_node_type),
        ruby_prism::Node::DefNode { .. } => {
            // def vs defs: defs has a receiver
            if let Some(def) = node.as_def_node() {
                if def.receiver().is_some() {
                    Some("defs")
                } else {
                    Some("def")
                }
            } else {
                Some("def")
            }
        }
        ruby_prism::Node::ConstantReadNode { .. } => Some("const"),
        ruby_prism::Node::ConstantPathNode { .. } => Some("const"),
        // Parser splits Prism's `BeginNode`: `begin … end` is `kwbegin`, while
        // an implicit body wrapper is just the statement list.
        ruby_prism::Node::BeginNode { .. } => Some(
            if node
                .as_begin_node()
                .is_some_and(|begin| begin.begin_keyword_loc().is_some())
            {
                "kwbegin"
            } else {
                "begin"
            },
        ),
        // Parser's `begin` is a statement list; Prism spells that three ways.
        ruby_prism::Node::StatementsNode { .. }
        | ruby_prism::Node::ParenthesesNode { .. }
        | ruby_prism::Node::EmbeddedStatementsNode { .. } => Some("begin"),
        ruby_prism::Node::AssocNode { .. } => Some("pair"),
        ruby_prism::Node::HashNode { .. } => Some("hash"),
        // `foo(k: 1)` is a `KeywordHashNode` in Prism and a plain `hash` in the
        // Parser gem.
        ruby_prism::Node::KeywordHashNode { .. } => Some("hash"),
        ruby_prism::Node::LocalVariableReadNode { .. } => Some("lvar"),
        ruby_prism::Node::InstanceVariableReadNode { .. } => Some("ivar"),
        ruby_prism::Node::ClassVariableReadNode { .. } => Some("cvar"),
        ruby_prism::Node::GlobalVariableReadNode { .. } => Some("gvar"),
        ruby_prism::Node::SymbolNode { .. } => Some("sym"),
        ruby_prism::Node::StringNode { .. } => Some("str"),
        ruby_prism::Node::IntegerNode { .. } => Some("int"),
        ruby_prism::Node::FloatNode { .. } => Some("float"),
        ruby_prism::Node::TrueNode { .. } => Some("true"),
        ruby_prism::Node::FalseNode { .. } => Some("false"),
        ruby_prism::Node::NilNode { .. } => Some("nil"),
        ruby_prism::Node::SelfNode { .. } => Some("self"),
        ruby_prism::Node::ArrayNode { .. } => Some("array"),
        ruby_prism::Node::IfNode { .. } => Some("if"),
        ruby_prism::Node::CaseNode { .. } => Some("case"),
        ruby_prism::Node::WhenNode { .. } => Some("when"),
        ruby_prism::Node::WhileNode { .. } => Some("while"),
        ruby_prism::Node::UntilNode { .. } => Some("until"),
        ruby_prism::Node::ForNode { .. } => Some("for"),
        ruby_prism::Node::ReturnNode { .. } => Some("return"),
        ruby_prism::Node::YieldNode { .. } => Some("yield"),
        ruby_prism::Node::AndNode { .. } => Some("and"),
        ruby_prism::Node::OrNode { .. } => Some("or"),
        ruby_prism::Node::RegularExpressionNode { .. } => Some("regexp"),
        ruby_prism::Node::ClassNode { .. } => Some("class"),
        ruby_prism::Node::ModuleNode { .. } => Some("module"),
        ruby_prism::Node::LocalVariableWriteNode { .. } => Some("lvasgn"),
        ruby_prism::Node::InstanceVariableWriteNode { .. } => Some("ivasgn"),
        ruby_prism::Node::ConstantWriteNode { .. } => Some("casgn"),
        ruby_prism::Node::SplatNode { .. } => Some("splat"),
        ruby_prism::Node::SuperNode { .. } => Some("super"),
        ruby_prism::Node::ForwardingSuperNode { .. } => Some("zsuper"),
        ruby_prism::Node::LambdaNode { .. } => Some("lambda"),
        ruby_prism::Node::InterpolatedStringNode { .. } => Some("dstr"),
        ruby_prism::Node::InterpolatedSymbolNode { .. } => Some("dsym"),
        ruby_prism::Node::ParametersNode { .. } | ruby_prism::Node::BlockParametersNode { .. } => {
            Some("args")
        }

        // Parameters (`argument` group).
        ruby_prism::Node::RequiredParameterNode { .. } => Some("arg"),
        ruby_prism::Node::OptionalParameterNode { .. } => Some("optarg"),
        ruby_prism::Node::RestParameterNode { .. } => Some("restarg"),
        ruby_prism::Node::RequiredKeywordParameterNode { .. } => Some("kwarg"),
        ruby_prism::Node::OptionalKeywordParameterNode { .. } => Some("kwoptarg"),
        ruby_prism::Node::KeywordRestParameterNode { .. } => Some("kwrestarg"),
        ruby_prism::Node::BlockParameterNode { .. } => Some("blockarg"),
        ruby_prism::Node::ForwardingParameterNode { .. } => Some("forward_arg"),
        ruby_prism::Node::BlockLocalVariableNode { .. } => Some("shadowarg"),
        ruby_prism::Node::MultiTargetNode { .. } => Some("mlhs"),

        ruby_prism::Node::BlockArgumentNode { .. } => Some("block_pass"),
        ruby_prism::Node::AssocSplatNode { .. } => Some("kwsplat"),
        ruby_prism::Node::CaseMatchNode { .. } => Some("case_match"),
        ruby_prism::Node::InNode { .. } => Some("in_pattern"),
        ruby_prism::Node::SingletonClassNode { .. } => Some("sclass"),
        ruby_prism::Node::MultiWriteNode { .. } => Some("masgn"),
        ruby_prism::Node::ClassVariableWriteNode { .. } => Some("cvasgn"),
        ruby_prism::Node::GlobalVariableWriteNode { .. } => Some("gvasgn"),
        ruby_prism::Node::NextNode { .. } => Some("next"),
        ruby_prism::Node::BreakNode { .. } => Some("break"),
        ruby_prism::Node::DefinedNode { .. } => Some("defined?"),
        ruby_prism::Node::RescueNode { .. } => Some("resbody"),
        ruby_prism::Node::RationalNode { .. } => Some("rational"),
        ruby_prism::Node::ImaginaryNode { .. } => Some("complex"),

        ruby_prism::Node::XStringNode { .. } | ruby_prism::Node::InterpolatedXStringNode { .. } => {
            Some("xstr")
        }
        ruby_prism::Node::InterpolatedRegularExpressionNode { .. } => Some("regexp"),
        // `..` vs `...` is a flag on Prism's `RangeNode`.
        ruby_prism::Node::RangeNode { .. } => Some(
            if node.as_range_node().is_some_and(|r| r.is_exclude_end()) {
                "erange"
            } else {
                "irange"
            },
        ),

        // `x += 1` / `x ||= 1` / `x &&= 1`. Prism has one node type per
        // assignment target kind; Parser has one per operator kind.
        ruby_prism::Node::LocalVariableOperatorWriteNode { .. }
        | ruby_prism::Node::InstanceVariableOperatorWriteNode { .. }
        | ruby_prism::Node::ClassVariableOperatorWriteNode { .. }
        | ruby_prism::Node::GlobalVariableOperatorWriteNode { .. }
        | ruby_prism::Node::ConstantOperatorWriteNode { .. }
        | ruby_prism::Node::ConstantPathOperatorWriteNode { .. }
        | ruby_prism::Node::CallOperatorWriteNode { .. }
        | ruby_prism::Node::IndexOperatorWriteNode { .. } => Some("op_asgn"),
        ruby_prism::Node::LocalVariableOrWriteNode { .. }
        | ruby_prism::Node::InstanceVariableOrWriteNode { .. }
        | ruby_prism::Node::ClassVariableOrWriteNode { .. }
        | ruby_prism::Node::GlobalVariableOrWriteNode { .. }
        | ruby_prism::Node::ConstantOrWriteNode { .. }
        | ruby_prism::Node::ConstantPathOrWriteNode { .. }
        | ruby_prism::Node::CallOrWriteNode { .. }
        | ruby_prism::Node::IndexOrWriteNode { .. } => Some("or_asgn"),
        ruby_prism::Node::LocalVariableAndWriteNode { .. }
        | ruby_prism::Node::InstanceVariableAndWriteNode { .. }
        | ruby_prism::Node::ClassVariableAndWriteNode { .. }
        | ruby_prism::Node::GlobalVariableAndWriteNode { .. }
        | ruby_prism::Node::ConstantAndWriteNode { .. }
        | ruby_prism::Node::ConstantPathAndWriteNode { .. }
        | ruby_prism::Node::CallAndWriteNode { .. }
        | ruby_prism::Node::IndexAndWriteNode { .. } => Some("and_asgn"),
        _ => None,
    }
}

/// The `max_numparam` child of a Parser `numblock`, as decimal digits.
fn numbered_parameter_count(params: Option<&ruby_prism::Node<'_>>) -> &'static [u8] {
    const DIGITS: [&[u8]; 10] = [b"0", b"1", b"2", b"3", b"4", b"5", b"6", b"7", b"8", b"9"];
    let maximum = params
        .and_then(ruby_prism::Node::as_numbered_parameters_node)
        .map_or(0, |p| p.maximum());
    DIGITS[usize::from(maximum).min(9)]
}

/// The `regopt` child of a Parser `regexp`, as its option letters.
fn regexp_options(ignore_case: bool, extended: bool, multi_line: bool) -> &'static [u8] {
    const OPTIONS: [&[u8]; 8] = [b"", b"i", b"m", b"im", b"x", b"ix", b"mx", b"imx"];
    OPTIONS[usize::from(ignore_case) | usize::from(multi_line) << 1 | usize::from(extended) << 2]
}

/// The operator and value of an `op_asgn` / `or_asgn` / `and_asgn` node.
///
/// The operator is `None` for `||=` and `&&=`, which Parser spells as the node
/// type rather than as a child.
fn operator_write_parts<'pr>(
    node: &ruby_prism::Node<'pr>,
) -> Option<(Option<&'pr [u8]>, ruby_prism::Node<'pr>)> {
    macro_rules! binary {
        ($accessor:ident) => {
            if let Some(typed) = node.$accessor() {
                return Some((Some(typed.binary_operator().as_slice()), typed.value()));
            }
        };
    }
    macro_rules! logical {
        ($accessor:ident) => {
            if let Some(typed) = node.$accessor() {
                return Some((None, typed.value()));
            }
        };
    }

    binary!(as_local_variable_operator_write_node);
    binary!(as_instance_variable_operator_write_node);
    binary!(as_class_variable_operator_write_node);
    binary!(as_global_variable_operator_write_node);
    binary!(as_constant_operator_write_node);
    binary!(as_constant_path_operator_write_node);
    binary!(as_call_operator_write_node);
    binary!(as_index_operator_write_node);

    logical!(as_local_variable_or_write_node);
    logical!(as_instance_variable_or_write_node);
    logical!(as_class_variable_or_write_node);
    logical!(as_global_variable_or_write_node);
    logical!(as_constant_or_write_node);
    logical!(as_constant_path_or_write_node);
    logical!(as_call_or_write_node);
    logical!(as_index_or_write_node);

    logical!(as_local_variable_and_write_node);
    logical!(as_instance_variable_and_write_node);
    logical!(as_class_variable_and_write_node);
    logical!(as_global_variable_and_write_node);
    logical!(as_constant_and_write_node);
    logical!(as_constant_path_and_write_node);
    logical!(as_call_and_write_node);
    logical!(as_index_and_write_node);

    None
}

/// Build the children list for a node given its Parser gem type.
///
/// The returned `Vec<MatchChild>` matches NodePattern positional semantics.
/// For `send`: `[receiver_or_Absent, Name(method_name), arg1, arg2, ...]`
pub(crate) fn get_children<'pr>(
    parser_type: &str,
    node: &ruby_prism::Node<'pr>,
) -> Option<Vec<MatchChild<'pr>>> {
    let mut children = Vec::new();

    match parser_type {
        "send" | "csend" => {
            let call = node.as_call_node()?;
            // Receiver
            match call.receiver() {
                Some(r) => children.push(MatchChild::Node(r)),
                None => children.push(MatchChild::Absent),
            }
            // Method name
            children.push(MatchChild::Name(call.name().as_slice()));
            // Arguments (flattened)
            if let Some(args) = call.arguments() {
                for arg in args.arguments().iter() {
                    children.push(MatchChild::Node(arg));
                }
            }
            // `&blk` is the last argument upstream; Prism hangs it off `block`
            // alongside literal blocks, so only a block *argument* counts.
            if let Some(block) = call.block()
                && block.as_block_argument_node().is_some()
            {
                children.push(MatchChild::Node(block));
            }
        }
        "block" | "numblock" | "itblock" => {
            // Parser: `(block send-node args body)`, `(numblock send-node
            // max_numparam body)`, `(itblock send-node :it body)`.
            let (params, body) = if let Some(call) = node.as_call_node() {
                // The Parser `block` node wraps the call, so the call is this
                // very node minus its block (Prism keeps no separate send node).
                let block = call.block()?.as_block_node()?;
                children.push(MatchChild::Node(dup_node(node)));
                (block.parameters(), block.body())
            } else if let Some(block) = node.as_block_node() {
                // Reached directly (e.g. via `call.block()`): Prism's `BlockNode`
                // has no back-pointer, so the send child is unavailable and only
                // `_` can match it.
                children.push(MatchChild::Absent);
                (block.parameters(), block.body())
            } else if let Some(lambda) = node.as_lambda_node() {
                // Parser: `-> { }` is `(block (lambda) (args) body)`.
                children.push(MatchChild::Synthetic {
                    parser_type: "lambda",
                    value: b"",
                });
                (lambda.parameters(), lambda.body())
            } else {
                return None;
            };

            match parser_type {
                "numblock" => children.push(MatchChild::Synthetic {
                    parser_type: "int",
                    value: numbered_parameter_count(params.as_ref()),
                }),
                "itblock" => children.push(MatchChild::Synthetic {
                    parser_type: "sym",
                    value: b"it",
                }),
                _ => match params {
                    Some(p) => children.push(MatchChild::Node(p)),
                    None => children.push(MatchChild::Absent),
                },
            }
            children.push(body_child(body));
        }
        "def" => {
            let def = node.as_def_node()?;
            children.push(MatchChild::Name(def.name().as_slice()));
            // Parser always builds an `(args)` node, empty or not, so
            // `(def :m (args) …)` is how upstream spells a parameterless
            // definition; Prism has no node there at all.
            match def.parameters() {
                Some(p) => children.push(MatchChild::Node(p.as_node())),
                None => children.push(MatchChild::Synthetic {
                    parser_type: "args",
                    value: b"",
                }),
            }
            children.push(body_child(def.body()));
        }
        "defs" => {
            let def = node.as_def_node()?;
            match def.receiver() {
                Some(r) => children.push(MatchChild::Node(r)),
                None => children.push(MatchChild::Absent),
            }
            children.push(MatchChild::Name(def.name().as_slice()));
            // Parser always builds an `(args)` node, empty or not, so
            // `(def :m (args) …)` is how upstream spells a parameterless
            // definition; Prism has no node there at all.
            match def.parameters() {
                Some(p) => children.push(MatchChild::Node(p.as_node())),
                None => children.push(MatchChild::Synthetic {
                    parser_type: "args",
                    value: b"",
                }),
            }
            children.push(body_child(def.body()));
        }
        "const" => {
            // Parser gem: (const parent :Name) — parent is nil for bare constants.
            // Prism splits into ConstantReadNode (bare) and ConstantPathNode (qualified).
            if let Some(c) = node.as_constant_read_node() {
                children.push(MatchChild::Absent); // nil parent
                children.push(MatchChild::Name(c.name().as_slice()));
            } else if let Some(cp) = node.as_constant_path_node() {
                match cp.parent() {
                    Some(p) => children.push(MatchChild::Node(p)),
                    None => children.push(MatchChild::Absent), // :: prefix
                }
                match cp.name() {
                    Some(n) => children.push(MatchChild::Name(n.as_slice())),
                    None => children.push(MatchChild::Absent),
                }
            } else {
                return None;
            }
        }
        "begin" => {
            // Parser's `begin` is a flat statement list, so the statements are
            // the children — `(x % 2)` is `(begin (send …))`, not a node
            // wrapping a list node.
            let statements = if let Some(stmts) = node.as_statements_node() {
                Some(stmts)
            } else if let Some(parens) = node.as_parentheses_node() {
                parens.body().and_then(|b| b.as_statements_node())
            } else if let Some(embedded) = node.as_embedded_statements_node() {
                embedded.statements()
            } else {
                node.as_begin_node()?.statements()
            };
            if let Some(stmts) = statements {
                for stmt in stmts.body().iter() {
                    children.push(MatchChild::Node(stmt));
                }
            }
        }
        "kwbegin" => {
            // Parser: `(kwbegin stmt …)`. A `rescue`/`ensure` clause is a child
            // of the Parser node too; Prism keeps them in sibling fields, so
            // only the statements are exposed here.
            if let Some(stmts) = node.as_begin_node()?.statements() {
                for stmt in stmts.body().iter() {
                    children.push(MatchChild::Node(stmt));
                }
            }
        }
        "pair" => {
            let assoc = node.as_assoc_node()?;
            children.push(MatchChild::Node(assoc.key()));
            children.push(MatchChild::Node(assoc.value()));
        }
        "hash" => {
            if let Some(hash) = node.as_hash_node() {
                for elem in hash.elements().iter() {
                    children.push(MatchChild::Node(elem));
                }
            } else if let Some(kw) = node.as_keyword_hash_node() {
                for elem in kw.elements().iter() {
                    children.push(MatchChild::Node(elem));
                }
            } else {
                return None;
            }
        }
        "lvar" => {
            // Parser has no node for the implicit `it` of a `{ it }` block: it
            // is `(lvar :it)`, which is what `(itblock … (lvar :it))` and
            // `Style/RedundantMinMaxBy`'s `itblock` matcher expect. Prism gives
            // it its own type with no name accessor, so the name is literal.
            if node.as_it_local_variable_read_node().is_some() {
                children.push(MatchChild::Name(b"it"));
            } else {
                let lv = node.as_local_variable_read_node()?;
                children.push(MatchChild::Name(lv.name().as_slice()));
            }
        }
        "ivar" => {
            let iv = node.as_instance_variable_read_node()?;
            children.push(MatchChild::Name(iv.name().as_slice()));
        }
        "cvar" => {
            let cv = node.as_class_variable_read_node()?;
            children.push(MatchChild::Name(cv.name().as_slice()));
        }
        "gvar" => {
            let gv = node.as_global_variable_read_node()?;
            children.push(MatchChild::Name(gv.name().as_slice()));
        }
        "sym" => {
            // Symbol node — value matched via special-case in the interpreter
        }
        "str" => {
            // String node — content matched via special-case
        }
        "int" | "float" => {
            // Value-only nodes — matched via special-case
        }
        "true" | "false" | "nil" | "self" | "zsuper" => {
            // No children
        }
        "array" => {
            let arr = node.as_array_node()?;
            for elem in arr.elements().iter() {
                children.push(MatchChild::Node(elem));
            }
        }
        "if" => {
            let if_node = node.as_if_node()?;
            children.push(MatchChild::Node(if_node.predicate()));
            match if_node.statements() {
                Some(s) => children.push(MatchChild::Node(s.as_node())),
                None => children.push(MatchChild::Absent),
            }
            match if_node.subsequent() {
                Some(s) => children.push(MatchChild::Node(s)),
                None => children.push(MatchChild::Absent),
            }
        }
        "case" => {
            let case = node.as_case_node()?;
            match case.predicate() {
                Some(p) => children.push(MatchChild::Node(p)),
                None => children.push(MatchChild::Absent),
            }
            for cond in case.conditions().iter() {
                children.push(MatchChild::Node(cond));
            }
            match case.else_clause() {
                Some(e) => children.push(MatchChild::Node(e.as_node())),
                None => children.push(MatchChild::Absent),
            }
        }
        "when" => {
            let when = node.as_when_node()?;
            for cond in when.conditions().iter() {
                children.push(MatchChild::Node(cond));
            }
            match when.statements() {
                Some(s) => children.push(MatchChild::Node(s.as_node())),
                None => children.push(MatchChild::Absent),
            }
        }
        "while" => {
            let w = node.as_while_node()?;
            children.push(MatchChild::Node(w.predicate()));
            match w.statements() {
                Some(s) => children.push(MatchChild::Node(s.as_node())),
                None => children.push(MatchChild::Absent),
            }
        }
        "until" => {
            let u = node.as_until_node()?;
            children.push(MatchChild::Node(u.predicate()));
            match u.statements() {
                Some(s) => children.push(MatchChild::Node(s.as_node())),
                None => children.push(MatchChild::Absent),
            }
        }
        "for" => {
            let f = node.as_for_node()?;
            children.push(MatchChild::Node(f.index()));
            children.push(MatchChild::Node(f.collection()));
            match f.statements() {
                Some(s) => children.push(MatchChild::Node(s.as_node())),
                None => children.push(MatchChild::Absent),
            }
        }
        "return" => {
            let r = node.as_return_node()?;
            if let Some(args) = r.arguments() {
                for arg in args.arguments().iter() {
                    children.push(MatchChild::Node(arg));
                }
            }
        }
        "yield" => {
            let y = node.as_yield_node()?;
            if let Some(args) = y.arguments() {
                for arg in args.arguments().iter() {
                    children.push(MatchChild::Node(arg));
                }
            }
        }
        "and" => {
            let a = node.as_and_node()?;
            children.push(MatchChild::Node(a.left()));
            children.push(MatchChild::Node(a.right()));
        }
        "or" => {
            let o = node.as_or_node()?;
            children.push(MatchChild::Node(o.left()));
            children.push(MatchChild::Node(o.right()));
        }
        "regexp" => {
            // Parser: `(regexp (str "body") … (regopt :i :m))`. Prism keeps a
            // plain regexp's body as a location and its options as flags, so
            // both are synthesized. Multi-flag regexps are exposed as one
            // `regopt` value ("im"), which no vendored pattern inspects — every
            // one of them writes `(regopt)`, `(regopt _)` or `(regopt $...)`.
            if let Some(re) = node.as_regular_expression_node() {
                children.push(MatchChild::Synthetic {
                    parser_type: "str",
                    value: re.content_loc().as_slice(),
                });
                children.push(MatchChild::Synthetic {
                    parser_type: "regopt",
                    value: regexp_options(
                        re.is_ignore_case(),
                        re.is_extended(),
                        re.is_multi_line(),
                    ),
                });
            } else if let Some(re) = node.as_interpolated_regular_expression_node() {
                for part in re.parts().iter() {
                    children.push(MatchChild::Node(part));
                }
                children.push(MatchChild::Synthetic {
                    parser_type: "regopt",
                    value: regexp_options(
                        re.is_ignore_case(),
                        re.is_extended(),
                        re.is_multi_line(),
                    ),
                });
            } else {
                return None;
            }
        }
        "xstr" => {
            // Parser: `` `foo` `` is `(xstr (str "foo"))`.
            if let Some(x) = node.as_x_string_node() {
                children.push(MatchChild::Synthetic {
                    parser_type: "str",
                    value: x.content_loc().as_slice(),
                });
            } else if let Some(x) = node.as_interpolated_x_string_node() {
                for part in x.parts().iter() {
                    children.push(MatchChild::Node(part));
                }
            } else {
                return None;
            }
        }
        // Parser: `(irange from to)` / `(erange from to)`; either end may be nil.
        "irange" | "erange" => {
            let range = node.as_range_node()?;
            for end in [range.left(), range.right()] {
                match end {
                    Some(n) => children.push(MatchChild::Node(n)),
                    None => children.push(MatchChild::Absent),
                }
            }
        }
        "block_pass" => {
            // Parser: `(block_pass expr)`; `(block_pass nil)` when anonymous.
            match node.as_block_argument_node()?.expression() {
                Some(e) => children.push(MatchChild::Node(e)),
                None => children.push(MatchChild::Absent),
            }
        }
        "kwsplat" => match node.as_assoc_splat_node()?.value() {
            Some(v) => children.push(MatchChild::Node(v)),
            None => children.push(MatchChild::Absent),
        },
        "case_match" => {
            // Parser: `(case_match expr in_pattern… else)`.
            let case = node.as_case_match_node()?;
            match case.predicate() {
                Some(p) => children.push(MatchChild::Node(p)),
                None => children.push(MatchChild::Absent),
            }
            for condition in case.conditions().iter() {
                children.push(MatchChild::Node(condition));
            }
            match case.else_clause() {
                Some(e) => children.push(MatchChild::Node(e.as_node())),
                None => children.push(MatchChild::Absent),
            }
        }
        "in_pattern" => {
            // Parser: `(in_pattern pattern guard body)`. Prism folds the guard
            // into the pattern node (`IfNode`/`UnlessNode`), so the guard slot
            // is always absent here and `in x if y` presents as `(in_pattern
            // (if …) nil? body)`.
            let in_node = node.as_in_node()?;
            children.push(MatchChild::Node(in_node.pattern()));
            children.push(MatchChild::Absent);
            match in_node.statements() {
                Some(s) => children.push(MatchChild::Node(s.as_node())),
                None => children.push(MatchChild::Absent),
            }
        }
        "sclass" => {
            // Parser: `(sclass expr body)`.
            let sclass = node.as_singleton_class_node()?;
            children.push(MatchChild::Node(sclass.expression()));
            children.push(body_child(sclass.body()));
        }
        "masgn" => {
            // Parser: `(masgn (mlhs target…) value)`. Prism's `MultiWriteNode`
            // holds the targets inline with no `mlhs` node to stand in for, so
            // the first child is absent and only `_` can match it.
            let masgn = node.as_multi_write_node()?;
            children.push(MatchChild::Absent);
            children.push(MatchChild::Node(masgn.value()));
        }
        "mlhs" => {
            // Parser: `(mlhs target…)` — a nested destructuring target.
            let mlhs = node.as_multi_target_node()?;
            for target in mlhs.lefts().iter() {
                children.push(MatchChild::Node(target));
            }
            if let Some(rest) = mlhs.rest() {
                children.push(MatchChild::Node(rest));
            }
            for target in mlhs.rights().iter() {
                children.push(MatchChild::Node(target));
            }
        }
        // Parser: `(op-asgn (lvasgn :x) :+ value)`, `(or-asgn (lvasgn :x) value)`.
        // Prism folds the target into the write node itself, so the target slot
        // is absent; the operator and value are exact.
        "op_asgn" | "or_asgn" | "and_asgn" => {
            let (operator, value) = operator_write_parts(node)?;
            children.push(MatchChild::Absent);
            if let Some(operator) = operator {
                children.push(MatchChild::Name(operator));
            }
            children.push(MatchChild::Node(value));
        }
        "cvasgn" | "gvasgn" => {
            let (name, value) = if let Some(cv) = node.as_class_variable_write_node() {
                (cv.name(), cv.value())
            } else {
                let gv = node.as_global_variable_write_node()?;
                (gv.name(), gv.value())
            };
            children.push(MatchChild::Name(name.as_slice()));
            children.push(MatchChild::Node(value));
        }
        "next" | "break" => {
            let arguments = if let Some(n) = node.as_next_node() {
                n.arguments()
            } else {
                node.as_break_node()?.arguments()
            };
            if let Some(args) = arguments {
                for arg in args.arguments().iter() {
                    children.push(MatchChild::Node(arg));
                }
            }
        }
        "defined?" => {
            children.push(MatchChild::Node(node.as_defined_node()?.value()));
        }
        "resbody" => {
            // Parser: `(resbody (array exception…) var body)`. Prism keeps the
            // exception list flat, with no `array` node to stand in for, so the
            // first child is absent.
            let resbody = node.as_rescue_node()?;
            children.push(MatchChild::Absent);
            match resbody.reference() {
                Some(r) => children.push(MatchChild::Node(r)),
                None => children.push(MatchChild::Absent),
            }
            match resbody.statements() {
                Some(s) => children.push(MatchChild::Node(s.as_node())),
                None => children.push(MatchChild::Absent),
            }
        }
        "rational" | "complex" => {
            // Value-only nodes — matched via the literal special-cases.
        }
        "class" => {
            let c = node.as_class_node()?;
            children.push(MatchChild::Node(c.constant_path()));
            match c.superclass() {
                Some(s) => children.push(MatchChild::Node(s)),
                None => children.push(MatchChild::Absent),
            }
            children.push(body_child(c.body()));
        }
        "module" => {
            let m = node.as_module_node()?;
            children.push(MatchChild::Node(m.constant_path()));
            children.push(body_child(m.body()));
        }
        "lvasgn" => {
            let lv = node.as_local_variable_write_node()?;
            children.push(MatchChild::Name(lv.name().as_slice()));
            children.push(MatchChild::Node(lv.value()));
        }
        "ivasgn" => {
            let iv = node.as_instance_variable_write_node()?;
            children.push(MatchChild::Name(iv.name().as_slice()));
            children.push(MatchChild::Node(iv.value()));
        }
        "casgn" => {
            let cw = node.as_constant_write_node()?;
            children.push(MatchChild::Name(cw.name().as_slice()));
            children.push(MatchChild::Node(cw.value()));
        }
        "splat" => {
            let s = node.as_splat_node()?;
            match s.expression() {
                Some(e) => children.push(MatchChild::Node(e)),
                None => children.push(MatchChild::Absent),
            }
        }
        "super" => {
            let s = node.as_super_node()?;
            if let Some(args) = s.arguments() {
                for arg in args.arguments().iter() {
                    children.push(MatchChild::Node(arg));
                }
            }
        }
        "lambda" => {
            // Parser's `lambda` node is the bare `->`; the parameters and body
            // belong to the `block` node wrapping it.
            node.as_lambda_node()?;
        }
        "dstr" => {
            let isn = node.as_interpolated_string_node()?;
            for part in isn.parts().iter() {
                children.push(MatchChild::Node(part));
            }
        }
        "dsym" => {
            let isn = node.as_interpolated_symbol_node()?;
            for part in isn.parts().iter() {
                children.push(MatchChild::Node(part));
            }
        }
        "args" => {
            // Parser: `(args (arg :a) (optarg :b …) …)`, in declaration order.
            let params = if let Some(block_params) = node.as_block_parameters_node() {
                block_params.parameters()
            } else {
                node.as_parameters_node()
            };
            if let Some(params) = params {
                for p in params.requireds().iter() {
                    children.push(MatchChild::Node(p));
                }
                for p in params.optionals().iter() {
                    children.push(MatchChild::Node(p));
                }
                if let Some(rest) = params.rest() {
                    children.push(MatchChild::Node(rest));
                }
                for p in params.posts().iter() {
                    children.push(MatchChild::Node(p));
                }
                for p in params.keywords().iter() {
                    children.push(MatchChild::Node(p));
                }
                if let Some(kwrest) = params.keyword_rest() {
                    children.push(MatchChild::Node(kwrest));
                }
                if let Some(block) = params.block() {
                    children.push(MatchChild::Node(block.as_node()));
                }
            }
            if let Some(block_params) = node.as_block_parameters_node() {
                for local in block_params.locals().iter() {
                    children.push(MatchChild::Node(local));
                }
            }
        }
        // Parser: `(arg :name)`, `(kwarg :name)`, `(shadowarg :name)`.
        "arg" | "kwarg" | "shadowarg" => {
            let name = if let Some(a) = node.as_required_parameter_node() {
                a.name()
            } else if let Some(k) = node.as_required_keyword_parameter_node() {
                k.name()
            } else {
                node.as_block_local_variable_node()?.name()
            };
            children.push(MatchChild::Name(name.as_slice()));
        }
        // Parser: `(optarg :name default)`.
        "optarg" | "kwoptarg" => {
            let (name, value) = if let Some(o) = node.as_optional_parameter_node() {
                (o.name(), o.value())
            } else {
                let k = node.as_optional_keyword_parameter_node()?;
                (k.name(), k.value())
            };
            children.push(MatchChild::Name(name.as_slice()));
            children.push(MatchChild::Node(value));
        }
        // Parser: `(restarg :name)`, or `(restarg)` when anonymous.
        "restarg" | "kwrestarg" | "blockarg" => {
            let name = if let Some(r) = node.as_rest_parameter_node() {
                r.name()
            } else if let Some(k) = node.as_keyword_rest_parameter_node() {
                k.name()
            } else {
                node.as_block_parameter_node()?.name()
            };
            if let Some(name) = name {
                children.push(MatchChild::Name(name.as_slice()));
            }
        }
        "forward_arg" => {
            node.as_forwarding_parameter_node()?;
        }
        _ => return None,
    }

    Some(children)
}

/// Helpers shared by the unit tests in this module and in
/// [`super::ancestors`] / [`super::predicates`].
#[cfg(test)]
pub mod test_support {
    use super::dup_node;
    use ruby_prism::{Node, Visit};

    /// Parse `source`, find the first node whose source text is exactly
    /// `needle`, and hand it plus its raw Prism ancestor chain (outermost
    /// first) to `f`.
    ///
    /// This is the shape [`crate::cop::walker::BatchedCopWalker`] hands a cop
    /// that opted into ancestors, so tests exercise the same chain the runtime
    /// produces.
    pub fn with_node_and_chain<R>(
        source: &str,
        needle: &str,
        f: impl FnOnce(&Node<'_>, &[Node<'_>]) -> R,
    ) -> R {
        struct Finder<'pr> {
            needle: &'pr [u8],
            chain: Vec<Node<'pr>>,
            found: Option<(Node<'pr>, Vec<Node<'pr>>)>,
        }

        impl<'pr> Finder<'pr> {
            fn check(&mut self, node: &Node<'pr>) {
                // `ProgramNode` and `StatementsNode` share their source range
                // with the expression they wrap, so a search by source text
                // would find the wrapper. Neither is a Parser node when it
                // wraps a single statement anyway.
                if node.as_program_node().is_some() || node.as_statements_node().is_some() {
                    return;
                }
                if self.found.is_none() && node.location().as_slice() == self.needle {
                    self.found = Some((dup_node(node), self.chain.iter().map(dup_node).collect()));
                }
            }
        }

        impl<'pr> Visit<'pr> for Finder<'pr> {
            fn visit_branch_node_enter(&mut self, node: Node<'pr>) {
                self.check(&node);
                self.chain.push(node);
            }

            fn visit_branch_node_leave(&mut self) {
                self.chain.pop();
            }

            fn visit_leaf_node_enter(&mut self, node: Node<'pr>) {
                self.check(&node);
            }
        }

        let result = ruby_prism::parse(source.as_bytes());
        let mut finder = Finder {
            needle: needle.as_bytes(),
            chain: Vec::new(),
            found: None,
        };
        finder.visit(&result.node());
        let (node, chain) = finder
            .found
            .unwrap_or_else(|| panic!("no node with source text {needle:?} in {source:?}"));
        f(&node, &chain)
    }

    /// [`with_node_and_chain`], ancestors only.
    pub fn chain_at<R>(source: &str, needle: &str, f: impl FnOnce(&[Node<'_>]) -> R) -> R {
        with_node_and_chain(source, needle, |_, chain| f(chain))
    }
}

/// Match a PatternNode against a MatchChild (dispatcher).
fn matches_child<'pr>(
    pattern: &PatternNode,
    child: &MatchChild<'pr>,
    env: &mut MatchEnv<'pr, '_>,
) -> bool {
    match child {
        MatchChild::Node(node) => matches_node(pattern, node, env),
        MatchChild::Absent => matches_absent(pattern, env),
        MatchChild::Name(bytes) => matches_name(pattern, bytes, bytes, env),
        MatchChild::Synthetic { parser_type, value } => {
            matches_synthetic(pattern, parser_type, value, env)
        }
    }
}

/// Match a pattern against a synthesized Parser-gem child
/// ([`MatchChild::Synthetic`]).
///
/// The synthesized node has one child — its value — so `(str $_)`, `(regopt :i)`
/// and the bare literal forms (`:it`, `1`) all work; anything that addresses a
/// deeper structure does not. An **empty** value stands for no children at all,
/// which is what Parser's `(lambda)` and an option-less `(regopt)` are, and
/// what the exact-arity rule in [`matches_children_list`] needs them to be.
fn matches_synthetic<'pr>(
    pattern: &PatternNode,
    parser_type: &'static str,
    value: &'pr [u8],
    env: &mut MatchEnv<'pr, '_>,
) -> bool {
    match pattern {
        PatternNode::Wildcard | PatternNode::Rest => true,
        PatternNode::Unify(name) => env.unify(name, value),
        PatternNode::Ident(name) | PatternNode::TypePredicate(name) => {
            type_answers(parser_type, name)
        }
        PatternNode::SymbolLiteral(name) => parser_type == "sym" && value == name.as_bytes(),
        PatternNode::StringLiteral(s) => parser_type == "str" && value == s.as_bytes(),
        PatternNode::IntLiteral(n) => {
            parser_type == "int"
                && std::str::from_utf8(value)
                    .ok()
                    .and_then(|text| text.parse::<i64>().ok())
                    == Some(*n)
        }
        PatternNode::NodeMatch {
            node_type,
            children,
        } => {
            if !type_answers(parser_type, node_type) {
                return false;
            }
            let mark = env.mark();
            let actuals: &[MatchChild<'pr>] = if value.is_empty() {
                &[]
            } else {
                &[MatchChild::Name(value)]
            };
            if matches_children_list(children, actuals, env) {
                return true;
            }
            env.rollback(mark);
            false
        }
        PatternNode::Alternatives(alts) => {
            for alt in alts {
                let mark = env.mark();
                if matches_synthetic(alt, parser_type, value, env) {
                    return true;
                }
                env.rollback(mark);
            }
            false
        }
        PatternNode::Conjunction(items) => {
            let mark = env.mark();
            if items
                .iter()
                .all(|item| matches_synthetic(item, parser_type, value, env))
            {
                return true;
            }
            env.rollback(mark);
            false
        }
        PatternNode::Negation(inner) => {
            let mark = env.mark();
            let inner_matched = matches_synthetic(inner, parser_type, value, env);
            env.rollback(mark);
            !inner_matched
        }
        PatternNode::Capture { slot, inner } => {
            let mark = env.mark();
            env.set(*slot, CaptureValue::Name(value));
            if matches_synthetic(inner, parser_type, value, env) {
                return true;
            }
            env.rollback(mark);
            false
        }
        _ => matches_deferred(pattern, &PredTarget::Synthetic { parser_type, value }, env),
    }
}

/// The value a `$` binds for a given child, used when `$...` captures a run.
fn capture_value_for<'pr>(child: &MatchChild<'pr>) -> CaptureValue<'pr> {
    match child {
        MatchChild::Node(node) => CaptureValue::Node(dup_node(node)),
        MatchChild::Absent => CaptureValue::Absent,
        MatchChild::Name(bytes) | MatchChild::Synthetic { value: bytes, .. } => {
            CaptureValue::Name(bytes)
        }
    }
}

/// `(<head> <rest>…)` where `head` is not a plain type name.
///
/// Upstream compiles the head term with `seq_head: true`, which changes two
/// things (`node_pattern_subcompiler.rb:112-120`): `access_element` becomes
/// `node.type` — so an atom at the head compares against the *type symbol*,
/// not against the node — while `access_node` stays the node itself, so `^`
/// and a nested sequence still see a node. Everything else is the ordinary
/// child list.
fn matches_complex_sequence<'pr>(
    head: &PatternNode,
    rest: &[PatternNode],
    node: &ruby_prism::Node<'pr>,
    env: &mut MatchEnv<'pr, '_>,
) -> bool {
    let mark = env.mark();
    if !matches_seq_head(head, node, env) {
        env.rollback(mark);
        return false;
    }
    // The head consumed no child, so the remaining terms are the whole child
    // list. Which children those are depends on the node's own Parser type.
    let Some(parser_type) = parser_type_for_node(node) else {
        env.rollback(mark);
        return false;
    };
    if let Some(matched) = match_value_only_children(parser_type, node, rest, env) {
        if matched {
            return true;
        }
        env.rollback(mark);
        return false;
    }
    let Some(children) = get_children(parser_type, node) else {
        env.rollback(mark);
        return rest.is_empty();
    };
    env.enter(node);
    let matched = matches_children_list(rest, &children, env);
    env.leave();
    if matched {
        return true;
    }
    env.rollback(mark);
    false
}

/// One term in sequence-head position.
fn matches_seq_head<'pr>(
    pattern: &PatternNode,
    node: &ruby_prism::Node<'pr>,
    env: &mut MatchEnv<'pr, '_>,
) -> bool {
    match pattern {
        PatternNode::Wildcard | PatternNode::Rest => true,
        PatternNode::Unify(name) => env.unify(name, node.location().as_slice()),
        // A type name, which is what a head term usually is.
        PatternNode::Ident(name) | PatternNode::TypePredicate(name) => node_has_type(node, name),
        // `access_node` ignores `seq_head`, so these see the node.
        PatternNode::ParentRef(_) | PatternNode::DescendRef(_) => {
            matches_deferred(pattern, &PredTarget::Node(node), env)
        }
        PatternNode::NodeMatch { .. } => matches_node(pattern, node, env),
        PatternNode::Alternatives(alts) => alts.iter().any(|alt| {
            let mark = env.mark();
            if matches_seq_head(alt, node, env) {
                return true;
            }
            env.rollback(mark);
            false
        }),
        PatternNode::Conjunction(items) => {
            let mark = env.mark();
            if items.iter().all(|item| matches_seq_head(item, node, env)) {
                return true;
            }
            env.rollback(mark);
            false
        }
        PatternNode::Negation(inner) => {
            let mark = env.mark();
            let inner_matched = matches_seq_head(inner, node, env);
            env.rollback(mark);
            !inner_matched
        }
        // `access_element` is `node.type`, so a capture at the head binds the
        // type symbol and an atom compares against it.
        _ => {
            let type_name = parser_type_for_node(node)
                .or_else(|| block_type_of(node))
                .unwrap_or("");
            matches_name(pattern, type_name.as_bytes(), type_name.as_bytes(), env)
        }
    }
}

/// The value-only node types, whose single "child" Prism stores inline:
/// `(int 42)`, `(str "foo")`, `(sym :bar)`.
///
/// Returns `None` when `effective_type` is not one of them, so the caller
/// falls through to the ordinary child list.
fn match_value_only_children<'pr>(
    effective_type: &str,
    node: &ruby_prism::Node<'pr>,
    pattern_children: &[PatternNode],
    env: &mut MatchEnv<'pr, '_>,
) -> Option<bool> {
    let first = pattern_children.first()?;
    match effective_type {
        "int" => Some(matches_node(first, node, env)),
        "str" => {
            let Some(string) = node.as_string_node() else {
                return Some(false);
            };
            // Compare against the unescaped value but capture a `'pr`-lived
            // slice of the source.
            let captured = string.content_loc().as_slice();
            Some(matches_name(first, string.unescaped(), captured, env))
        }
        "sym" => {
            let Some(symbol) = node.as_symbol_node() else {
                return Some(false);
            };
            let captured = symbol
                .value_loc()
                .map_or_else(|| symbol.location().as_slice(), |loc| loc.as_slice());
            Some(matches_name(first, symbol.unescaped(), captured, env))
        }
        _ => None,
    }
}

/// Match a pattern against a Prism AST node.
fn matches_node<'pr>(
    pattern: &PatternNode,
    node: &ruby_prism::Node<'pr>,
    env: &mut MatchEnv<'pr, '_>,
) -> bool {
    match pattern {
        PatternNode::Wildcard => true,
        PatternNode::NilPredicate => false, // Node is present, not absent

        PatternNode::SymbolLiteral(name) => {
            if let Some(sym) = node.as_symbol_node() {
                sym.unescaped() == name.as_bytes()
            } else {
                false
            }
        }

        PatternNode::IntLiteral(n) => {
            if let Some(int_node) = node.as_integer_node() {
                let loc = int_node.location();
                let src = loc.as_slice();
                let src_str = std::str::from_utf8(src).unwrap_or("");
                let cleaned: String = src_str.chars().filter(|c| *c != '_').collect();
                cleaned.parse::<i64>().ok() == Some(*n)
            } else {
                false
            }
        }

        PatternNode::StringLiteral(s) => {
            if let Some(str_node) = node.as_string_node() {
                str_node.unescaped() == s.as_bytes()
            } else {
                false
            }
        }

        PatternNode::NilLiteral => node.as_nil_node().is_some(),
        PatternNode::TrueLiteral => node.as_true_node().is_some(),
        PatternNode::FalseLiteral => node.as_false_node().is_some(),

        PatternNode::TypePredicate(typ) => node_has_type(node, typ),

        PatternNode::Ident(name) => node_has_type(node, name),

        // A node in a `_name` position unifies on its verbatim source; see
        // `MatchEnv::unify`.
        PatternNode::Unify(name) => env.unify(name, node.location().as_slice()),

        PatternNode::NodeMatch {
            node_type,
            children: pattern_children,
        } => {
            // A sequence head that is not a bare type name (`(^send …)`,
            // `({^any_block […]} …)`) is parked under the `_complex` sentinel
            // by the parser, with the real head term as the first child.
            if node_type == COMPLEX_SEQ_HEAD {
                let Some((head, rest)) = pattern_children.split_first() else {
                    return false;
                };
                return matches_complex_sequence(head, rest, node, env);
            }

            // The pattern type can be a group (`call`, `any_block`, …) or a
            // type Prism spells differently, so resolve it to the concrete type
            // whose children we read.
            let Some(effective_type) = concrete_type(node, node_type) else {
                return false;
            };

            let mark = env.mark();

            if let Some(matched) =
                match_value_only_children(effective_type, node, pattern_children, env)
            {
                if matched {
                    return true;
                }
                env.rollback(mark);
                return false;
            }

            let Some(actual_children) = get_children(effective_type, node) else {
                return pattern_children.is_empty();
            };

            // Everything below is one level deeper, so `^` and `value_used?`
            // inside a child slot see this node as their parent.
            env.enter(node);
            let matched = matches_children_list(pattern_children, &actual_children, env);
            env.leave();
            if matched {
                return true;
            }
            env.rollback(mark);
            false
        }

        PatternNode::Alternatives(alts) => {
            for alt in alts {
                let mark = env.mark();
                if matches_node(alt, node, env) {
                    return true;
                }
                env.rollback(mark);
            }
            false
        }
        PatternNode::Conjunction(items) => {
            let mark = env.mark();
            if items.iter().all(|item| matches_node(item, node, env)) {
                return true;
            }
            env.rollback(mark);
            false
        }
        PatternNode::Negation(inner) => {
            // Captures under a negation never survive: the term only succeeds
            // when the inner pattern failed.
            let mark = env.mark();
            let inner_matched = matches_node(inner, node, env);
            env.rollback(mark);
            !inner_matched
        }
        PatternNode::Capture { slot, inner } => {
            let mark = env.mark();
            env.set(*slot, CaptureValue::Node(dup_node(node)));
            if matches_node(inner, node, env) {
                return true;
            }
            env.rollback(mark);
            false
        }

        PatternNode::FloatLiteral(s) => {
            if let Some(float_node) = node.as_float_node() {
                let loc = float_node.location();
                let src = std::str::from_utf8(loc.as_slice()).unwrap_or("");
                src == s.as_str()
            } else {
                false
            }
        }

        // A subsequence only has meaning as a `{}` branch inside a child list,
        // where `match_sequence` splices it; anywhere else it matches nothing.
        PatternNode::Subsequence(_) => false,

        // Likewise `<>` and `x*`: both consume a run of children, so they are
        // only meaningful as a term of a child list (RuboCop forbids them in
        // sequence head position too — `ForbidInSeqHead`, `node.rb:143, 179`).
        PatternNode::AnyOrder(_) | PatternNode::Repetition { .. } => false,

        PatternNode::Rest => true,

        PatternNode::HelperCall { .. }
        | PatternNode::Predicate { .. }
        | PatternNode::ParamNumber(_)
        | PatternNode::ParamNamed(_)
        | PatternNode::ParamConst(_)
        | PatternNode::Regexp { .. }
        | PatternNode::ParentRef(_)
        | PatternNode::DescendRef(_) => matches_deferred(pattern, &PredTarget::Node(node), env),
    }
}

/// Match a pattern against an absent child (`nil?` predicate target).
fn matches_absent<'pr>(pattern: &PatternNode, env: &mut MatchEnv<'pr, '_>) -> bool {
    match pattern {
        PatternNode::Wildcard => true,
        // No byte string a real node or name can produce, so `_x` bound to an
        // absent child only ever unifies with another absent child.
        PatternNode::Unify(name) => env.unify(name, b"\0<absent>"),
        PatternNode::NilPredicate => true,
        PatternNode::Alternatives(alts) => {
            for alt in alts {
                let mark = env.mark();
                if matches_absent(alt, env) {
                    return true;
                }
                env.rollback(mark);
            }
            false
        }
        PatternNode::Conjunction(items) => {
            let mark = env.mark();
            if items.iter().all(|item| matches_absent(item, env)) {
                return true;
            }
            env.rollback(mark);
            false
        }
        PatternNode::Negation(inner) => {
            let mark = env.mark();
            let inner_matched = matches_absent(inner, env);
            env.rollback(mark);
            !inner_matched
        }
        PatternNode::Capture { slot, inner } => {
            let mark = env.mark();
            env.set(*slot, CaptureValue::Absent);
            if matches_absent(inner, env) {
                return true;
            }
            env.rollback(mark);
            false
        }
        PatternNode::Rest => true,
        _ => matches_deferred(pattern, &PredTarget::Absent, env),
    }
}

/// Match a pattern against a name/value byte slice (method name, variable name).
///
/// `bytes` is what the pattern is compared against; `captured` is the
/// `'pr`-lived slice a `$` binds. They differ only for literal nodes whose
/// decoded value is borrowed from a temporary node handle (`(str $_)`,
/// `(sym $_)`), where the capture uses the corresponding source slice.
fn matches_name<'pr>(
    pattern: &PatternNode,
    bytes: &[u8],
    captured: &'pr [u8],
    env: &mut MatchEnv<'pr, '_>,
) -> bool {
    match pattern {
        PatternNode::Wildcard => true,
        // The position upstream's own patterns actually unify in: a block
        // parameter's name against the `lvar` that uses it.
        PatternNode::Unify(name) => env.unify(name, bytes),
        PatternNode::SymbolLiteral(name) => bytes == name.as_bytes(),
        PatternNode::StringLiteral(s) => bytes == s.as_bytes(),
        PatternNode::Alternatives(alts) => {
            for alt in alts {
                let mark = env.mark();
                if matches_name(alt, bytes, captured, env) {
                    return true;
                }
                env.rollback(mark);
            }
            false
        }
        PatternNode::Conjunction(items) => {
            let mark = env.mark();
            if items
                .iter()
                .all(|item| matches_name(item, bytes, captured, env))
            {
                return true;
            }
            env.rollback(mark);
            false
        }
        PatternNode::Negation(inner) => {
            let mark = env.mark();
            let inner_matched = matches_name(inner, bytes, captured, env);
            env.rollback(mark);
            !inner_matched
        }
        PatternNode::Capture { slot, inner } => {
            let mark = env.mark();
            env.set(*slot, CaptureValue::Name(captured));
            if matches_name(inner, bytes, captured, env) {
                return true;
            }
            env.rollback(mark);
            false
        }
        PatternNode::Rest => true,
        _ => matches_deferred(pattern, &PredTarget::Name(bytes), env),
    }
}

/// `Some(capture_slot)` if `pattern` is a variable-length rest term — `...` or
/// `$...` — where the inner `Option` holds the slot of a captured rest.
fn as_rest_term(pattern: &PatternNode) -> Option<Option<usize>> {
    match pattern {
        PatternNode::Rest => Some(None),
        PatternNode::Capture { slot, inner } if matches!(**inner, PatternNode::Rest) => {
            Some(Some(*slot))
        }
        _ => None,
    }
}

/// Whether a term can match a number of children other than exactly one
/// (RuboCop's `Node#variadic?`).
fn is_variadic_term(pattern: &PatternNode) -> bool {
    match pattern {
        PatternNode::Subsequence(_) | PatternNode::AnyOrder(_) | PatternNode::Repetition { .. } => {
            true
        }
        PatternNode::Capture { inner, .. } if matches!(**inner, PatternNode::AnyOrder(_)) => true,
        PatternNode::Alternatives(alts) => alts.iter().any(is_variadic_term),
        _ => as_rest_term(pattern).is_some(),
    }
}

/// `Some((capture_slot, children))` if `pattern` is a `<>` any-order group,
/// optionally wrapped in a `$`.
fn as_any_order_term(pattern: &PatternNode) -> Option<(Option<usize>, &[PatternNode])> {
    match pattern {
        PatternNode::AnyOrder(items) => Some((None, items.as_slice())),
        PatternNode::Capture { slot, inner } => match &**inner {
            PatternNode::AnyOrder(items) => Some((Some(*slot), items.as_slice())),
            _ => None,
        },
        _ => None,
    }
}

/// Split an any-order group's children into its terms and its trailing rest
/// (`term_nodes` / `rest_node`, `node.rb:184-196`).
fn split_any_order(items: &[PatternNode]) -> (&[PatternNode], Option<Option<usize>>) {
    match items.last().and_then(as_rest_term) {
        Some(slot) => (&items[..items.len() - 1], Some(slot)),
        None => (items, None),
    }
}

/// Assign `actuals` to `terms` so that every term matches a distinct child.
///
/// Children are walked in order; each is tried against every still-unused term
/// and, when the group has a trailing rest, against the rest bucket. This is a
/// full backtracking search, where RuboCop compiles a greedy first-fit loop
/// (`sequence_subcompiler.rb:88-107`): the search accepts every assignment
/// first-fit accepts plus a few first-fit rejects (`<_ int>` against `(1 :a)`,
/// where the wildcard has to give up the integer). The divergence needs two
/// terms that can match the same child *and* an order that defeats first-fit;
/// no vendored `<>` pattern is in that shape.
fn assign_any_order<'pr>(
    terms: &[PatternNode],
    actuals: &[MatchChild<'pr>],
    index: usize,
    used: &mut [bool],
    leftovers: &mut Vec<usize>,
    has_rest: bool,
    env: &mut MatchEnv<'pr, '_>,
) -> bool {
    if index == actuals.len() {
        return used.iter().all(|matched| *matched);
    }
    // Every remaining term still needs a child of its own.
    let unused = used.iter().filter(|matched| !**matched).count();
    if actuals.len() - index < unused {
        return false;
    }

    for (slot, term) in terms.iter().enumerate() {
        if used[slot] {
            continue;
        }
        let mark = env.mark();
        used[slot] = true;
        if matches_child(term, &actuals[index], env)
            && assign_any_order(terms, actuals, index + 1, used, leftovers, has_rest, env)
        {
            return true;
        }
        used[slot] = false;
        env.rollback(mark);
    }

    if has_rest {
        leftovers.push(index);
        if assign_any_order(terms, actuals, index + 1, used, leftovers, has_rest, env) {
            return true;
        }
        leftovers.pop();
    }
    false
}

/// Match a `<>` group against the head of `actuals`, then the rest of the
/// sequence against what is left.
fn match_any_order<'pr>(
    group_slot: Option<usize>,
    items: &[PatternNode],
    rest_terms: &[&PatternNode],
    actuals: &[MatchChild<'pr>],
    env: &mut MatchEnv<'pr, '_>,
    exact: bool,
) -> bool {
    let (terms, rest) = split_any_order(items);
    let has_rest = rest.is_some();
    // Arity: exactly `terms.len()` children without a rest, `terms.len()..∞`
    // with one (`node.rb:197-201`).
    let max_take = if has_rest { actuals.len() } else { terms.len() };

    for take in terms.len()..=max_take {
        if take > actuals.len() {
            break;
        }
        let mark = env.mark();
        let mut used = vec![false; terms.len()];
        let mut leftovers = Vec::new();
        if assign_any_order(
            terms,
            &actuals[..take],
            0,
            &mut used,
            &mut leftovers,
            has_rest,
            env,
        ) {
            if let Some(Some(slot)) = rest {
                // A captured rest accumulates the children that matched no
                // term, in child order (`sequence_subcompiler.rb:141-152`).
                let run = leftovers
                    .iter()
                    .map(|i| capture_value_for(&actuals[*i]))
                    .collect();
                env.set(slot, CaptureValue::List(run));
            }
            if let Some(slot) = group_slot {
                // `$<...>` binds the whole run the group consumed
                // (`visit_capture`, `sequence_subcompiler.rb:155-162`).
                let run = actuals[..take].iter().map(capture_value_for).collect();
                env.set(slot, CaptureValue::List(run));
            }
            if match_sequence(rest_terms, &actuals[take..], env, exact) {
                return true;
            }
        }
        env.rollback(mark);
    }
    false
}

/// Every capture slot inside `pattern`, in slot order.
fn capture_slots(pattern: &PatternNode, out: &mut Vec<usize>) {
    match pattern {
        PatternNode::Capture { slot, inner } => {
            out.push(*slot);
            capture_slots(inner, out);
        }
        PatternNode::NodeMatch { children, .. } => {
            for child in children {
                capture_slots(child, out);
            }
        }
        PatternNode::Alternatives(items)
        | PatternNode::Conjunction(items)
        | PatternNode::Subsequence(items)
        | PatternNode::AnyOrder(items) => {
            for item in items {
                capture_slots(item, out);
            }
        }
        PatternNode::HelperCall { args, .. } | PatternNode::Predicate { args, .. } => {
            for arg in args {
                capture_slots(arg, out);
            }
        }
        PatternNode::Negation(inner)
        | PatternNode::ParentRef(inner)
        | PatternNode::DescendRef(inner)
        | PatternNode::Repetition { inner, .. } => capture_slots(inner, out),
        _ => {}
    }
}

/// Match `inner` against a run of `take` consecutive children, accumulating
/// what its captures bound on each pass.
///
/// RuboCop pushes `captures[range]` per iteration and `transpose`s at the end
/// (`sequence_subcompiler.rb:185-200`), so a `$` under a repetition binds an
/// Array with one entry per repetition — and an empty Array when the run is
/// empty, which is the case the transpose hack in upstream exists to handle.
fn match_repetition_run<'pr>(
    inner: &PatternNode,
    slots: &[usize],
    take: usize,
    actuals: &[MatchChild<'pr>],
    env: &mut MatchEnv<'pr, '_>,
) -> bool {
    let mut accumulated: Vec<Vec<CaptureValue<'pr>>> =
        (0..slots.len()).map(|_| Vec::new()).collect();
    for actual in &actuals[..take] {
        if !matches_child(inner, actual, env) {
            return false;
        }
        for (column, slot) in slots.iter().enumerate() {
            let Some(value) = env.get(*slot) else {
                continue;
            };
            accumulated[column].push(value.clone());
        }
    }
    for (column, slot) in slots.iter().enumerate() {
        env.set(
            *slot,
            CaptureValue::List(std::mem::take(&mut accumulated[column])),
        );
    }
    true
}

/// Match `term?` / `term*` / `term+` against the head of `actuals`, then the
/// rest of the sequence against what is left.
///
/// Upstream compiles a plain greedy loop that never gives a child back
/// (`sequence_subcompiler.rb:77-84, 358-364`). This tries the longest run
/// first and then shorter ones, so it accepts everything upstream accepts plus
/// the cases upstream's greed loses — `(send _ _ int* int)` against two
/// integers, say. That is the same divergence class as `<>`'s backtracking
/// assignment, and no vendored pattern is in that shape: every repetition in
/// the corpus is either last or followed by terms its own term cannot match.
fn match_repetition<'pr>(
    inner: &PatternNode,
    kind: RepeatKind,
    rest_terms: &[&PatternNode],
    actuals: &[MatchChild<'pr>],
    env: &mut MatchEnv<'pr, '_>,
    exact: bool,
) -> bool {
    let (min, max) = kind.arity();
    if actuals.len() < min {
        return false;
    }
    let mut slots = Vec::new();
    capture_slots(inner, &mut slots);
    let limit = max.min(actuals.len());

    for take in (min..=limit).rev() {
        let mark = env.mark();
        if match_repetition_run(inner, &slots, take, actuals, env)
            && match_sequence(rest_terms, &actuals[take..], env, exact)
        {
            return true;
        }
        env.rollback(mark);
    }
    false
}

/// Match a list of pattern children against a list of actual children.
///
/// A rest term (`...`, `$...`) matches a variable-length run, so the walk
/// backtracks over every split point and rewinds the captures written by a
/// rejected split.
///
/// **Arity is always exact**: every child has to be accounted for, whether or
/// not the list has a rest term. That is `compile_child_nb_guard`
/// (`compiler/sequence_subcompiler.rb:243-255`), which emits `==` for a fixed
/// list and `>=`/a range for a variadic one — in both cases a guard that no
/// unconsumed child survives.
///
/// This used to tolerate unconsumed trailing children when the list had no rest
/// term. The first cop to notice was `Style/TimeNow`, whose
/// `(call (const {nil? cbase} :Time) :new)` is upstream's spelling of
/// "`Time.new` with *no* arguments" and matched `Time.new(2026, 8, 19)` here.
fn matches_children_list<'pr>(
    patterns: &[PatternNode],
    actuals: &[MatchChild<'pr>],
    env: &mut MatchEnv<'pr, '_>,
) -> bool {
    let terms: Vec<&PatternNode> = patterns.iter().collect();
    match_sequence(&terms, actuals, env, true)
}

/// Splice `head` (a union branch or subsequence body) in front of `tail`.
fn splice<'p>(head: &'p PatternNode, tail: &[&'p PatternNode]) -> Vec<&'p PatternNode> {
    match head {
        PatternNode::Subsequence(items) => items.iter().chain(tail.iter().copied()).collect(),
        other => std::iter::once(other).chain(tail.iter().copied()).collect(),
    }
}

fn match_sequence<'pr>(
    terms: &[&PatternNode],
    actuals: &[MatchChild<'pr>],
    env: &mut MatchEnv<'pr, '_>,
    exact: bool,
) -> bool {
    let Some((term, rest_terms)) = terms.split_first() else {
        return !exact || actuals.is_empty();
    };

    // Terms that can match other than exactly one child are spliced into the
    // walk rather than handed to `matches_child`.
    if let PatternNode::Subsequence(_) = term {
        return match_sequence(&splice(term, rest_terms), actuals, env, exact);
    }
    if let PatternNode::Alternatives(alts) = term
        && alts.iter().any(is_variadic_term)
    {
        for alt in alts {
            let mark = env.mark();
            if match_sequence(&splice(alt, rest_terms), actuals, env, exact) {
                return true;
            }
            env.rollback(mark);
        }
        return false;
    }

    if let Some((group_slot, items)) = as_any_order_term(term) {
        return match_any_order(group_slot, items, rest_terms, actuals, env, exact);
    }

    if let PatternNode::Repetition { inner, kind } = term {
        return match_repetition(inner, *kind, rest_terms, actuals, env, exact);
    }

    if let Some(capture_slot) = as_rest_term(term) {
        for take in 0..=actuals.len() {
            let mark = env.mark();
            if let Some(slot) = capture_slot {
                let run = actuals[..take].iter().map(capture_value_for).collect();
                env.set(slot, CaptureValue::List(run));
            }
            if match_sequence(rest_terms, &actuals[take..], env, exact) {
                return true;
            }
            env.rollback(mark);
        }
        return false;
    }

    let Some((actual, rest_actuals)) = actuals.split_first() else {
        return false;
    };

    let mark = env.mark();
    if matches_child(term, actual, env) && match_sequence(rest_terms, rest_actuals, env, exact) {
        return true;
    }
    env.rollback(mark);
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------------
    // `^` / `` ` `` / `%0` / sequence heads
    // ---------------------------------------------------------------------

    /// Match `pattern` against the first node whose source text is `needle`,
    /// with the ancestor chain the walker would have handed a cop.
    fn matches_at(pattern: &str, source: &str, needle: &str) -> bool {
        matches_at_with(pattern, source, needle, &Params::new(), &NoResolver)
    }

    fn matches_at_with(
        pattern: &str,
        source: &str,
        needle: &str,
        params: &Params,
        resolver: &dyn Resolver,
    ) -> bool {
        let compiled = CompiledPattern::compile_with(pattern, resolver)
            .unwrap_or_else(|error| panic!("{pattern:?} should compile: {error:?}"));
        test_support::with_node_and_chain(source, needle, |node, chain| {
            compiled.matches_with_ancestors(node, chain, params, resolver)
        })
    }

    // ---------------------------------------------------------------------
    // `?` / `*` / `+` repetition
    // ---------------------------------------------------------------------

    /// Match `pattern` against the first statement of `source`.
    fn matches_src(pattern: &str, source: &str) -> bool {
        let compiled = CompiledPattern::compile(pattern)
            .unwrap_or_else(|| panic!("{pattern:?} should compile"));
        let result = ruby_prism::parse(source.as_bytes());
        compiled.matches(&first_stmt(&result))
    }

    /// Match `pattern` and return the capture slots as debug strings.
    fn captures_src(pattern: &str, source: &str) -> Option<Vec<String>> {
        let compiled = CompiledPattern::compile(pattern)
            .unwrap_or_else(|| panic!("{pattern:?} should compile"));
        let result = ruby_prism::parse(source.as_bytes());
        let captures = compiled.match_captures(&first_stmt(&result))?;
        Some(
            captures
                .iter()
                .map(|slot| match slot {
                    Some(CaptureValue::List(items)) => format!(
                        "[{}]",
                        items
                            .iter()
                            .map(describe_capture)
                            .collect::<Vec<_>>()
                            .join(", "),
                    ),
                    Some(value) => describe_capture(value),
                    None => "<unbound>".to_string(),
                })
                .collect(),
        )
    }

    fn describe_capture(value: &CaptureValue<'_>) -> String {
        match value {
            CaptureValue::Node(node) => {
                String::from_utf8_lossy(node.location().as_slice()).into_owned()
            }
            CaptureValue::Name(bytes) => String::from_utf8_lossy(bytes).into_owned(),
            CaptureValue::Absent => "<absent>".to_string(),
            CaptureValue::List(items) => format!(
                "[{}]",
                items
                    .iter()
                    .map(describe_capture)
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        }
    }

    #[test]
    fn repetition_arities() {
        let cases: &[(&str, &str, bool)] = &[
            // `?` is 0..1. Note the space: `sym?` with none is `tPREDICATE`
            // upstream (`/#{IDENTIFIER}\?/` wins over the punctuation rule),
            // which is why every vendored optional term is written `_ ?`,
            // `(sym)?` or `{…}?`.
            ("(send nil? :f sym ?)", "f", true),
            ("(send nil? :f sym ?)", "f :a", true),
            ("(send nil? :f sym ? sym)", "f :a, :b", true),
            // `*` is 0..∞ and, being unbounded, makes the list's arity exact.
            ("(send nil? :f sym*)", "f", true),
            ("(send nil? :f sym*)", "f :a, :b, :c", true),
            ("(send nil? :f sym*)", "f :a, 1", false),
            // `+` is 1..∞.
            ("(send nil? :f sym+)", "f", false),
            ("(send nil? :f sym+)", "f :a", true),
            ("(send nil? :f sym+)", "f :a, :b", true),
            ("(send nil? :f sym+)", "f 1", false),
            // A repetition is greedy but backtracks, so a following term can
            // still take a child the run could have eaten.
            ("(send nil? :f sym* sym)", "f :a, :b", true),
            // A repeated union and a repeated sequence.
            ("(send nil? :f {sym str}+)", "f :a, 'b'", true),
            ("(send nil? :f (sym :a)+)", "f :a, :a", true),
            ("(send nil? :f (sym :a)+)", "f :a, :b", false),
            // A negated term repeated (`Mixin/DigHelp::dig?`).
            ("(call _ :dig !{hash block_pass}+)", "x.dig(:a, :b)", true),
            ("(call _ :dig !{hash block_pass}+)", "x.dig", false),
            // `_?` is a wildcard repeated, not an identifier called `_?`.
            ("(send nil? :f _? sym)", "f 1, :a", true),
            ("(send nil? :f _? sym)", "f :a", true),
            // Repetition alongside `...`.
            ("(send nil? :f (sym _)* ...)", "f :a, :b, 1", true),
        ];
        for &(pattern, source, expected) in cases {
            assert_eq!(
                matches_src(pattern, source),
                expected,
                "{pattern} @ {source}"
            );
        }
    }

    #[test]
    fn a_capture_under_a_repetition_binds_a_list() {
        // RuboCop pushes `captures[range]` per pass and transposes
        // (`sequence_subcompiler.rb:185-200`), so each `$` gets one entry per
        // repetition — and an empty list when the run is empty.
        assert_eq!(
            captures_src("(send nil? :f (sym $_)+)", "f :a, :b, :c"),
            Some(vec!["[a, b, c]".to_string()]),
        );
        assert_eq!(
            captures_src("(send nil? :f $sym ?)", "f"),
            Some(vec!["[]".to_string()]),
        );
        assert_eq!(
            captures_src("(send nil? :f $sym ?)", "f :a"),
            Some(vec!["[:a]".to_string()]),
        );
        // Two captures under one repetition transpose into two lists.
        assert_eq!(
            captures_src("(send nil? :f (send nil? $_ (int $_))+)", "f x(1), y(2)"),
            Some(vec!["[x, y]".to_string(), "[1, 2]".to_string()]),
        );
        // A capture outside the repetition is untouched by it.
        assert_eq!(
            captures_src("(send nil? :f $int (sym $_)*)", "f 1, :a, :b"),
            Some(vec!["1".to_string(), "[a, b]".to_string()]),
        );
    }

    #[test]
    fn repetition_is_only_a_term_of_a_child_list() {
        // The grammar's `variadic_pattern` covers a sequence's children and a
        // union's branches, nothing else: `[…]`, `<…>` and an argument list
        // are plain `node_pattern_list`s.
        assert!(CompiledPattern::compile("(send nil? :f [sym ? str])").is_none());
        assert!(CompiledPattern::compile("(send nil? :f <sym ? str>)").is_none());
        assert!(CompiledPattern::compile("(send nil? :f literal?(sym ?))").is_none());
        // A `[…]` term *inside* a sequence is itself variadic-positioned, so
        // repeating the whole intersection is fine.
        assert!(CompiledPattern::compile("(send nil? :f [sym !nil?] ?)").is_some());
        // So is a repetition inside a union branch.
        assert!(CompiledPattern::compile("(send nil? :f {sym+ str})").is_some());
    }

    #[test]
    fn vendored_repetition_patterns_match_real_snippets() {
        let cases: &[(&str, &str, &str, bool)] = &[
            // `Lint/UselessTimes::times_call?`
            (
                "(send (int $_) :times (block-pass (sym $_))?)",
                "3.times(&:foo)",
                "with a block-pass",
                true,
            ),
            (
                "(send (int $_) :times (block-pass (sym $_))?)",
                "3.times",
                "without one",
                true,
            ),
            // `Lint/DuplicateMethods::delegate_method?`
            (
                "(send nil? :delegate ({sym str} $_)+ (hash <(pair (sym :to) {sym str}) ...>))",
                "delegate :a, :b, to: :c",
                "two delegated names",
                true,
            ),
            (
                "(send nil? :delegate ({sym str} $_)+ (hash <(pair (sym :to) {sym str}) ...>))",
                "delegate to: :c",
                "no delegated name",
                false,
            ),
            // `Rails/AttributeDefaultBlockValue::default_attribute` — the
            // `_ ?_` form, where the first wildcard is the optional one.
            (
                "(send nil? :attribute _ ?_ (hash <$(pair (sym :default) _) ...>))",
                "attribute :foo, :string, default: 1",
                "with the optional type argument",
                true,
            ),
            // `Style/HashLikeCase::hash_like_case?`
            (
                "(case _ (when ${str_type? sym_type?} $[!nil? recursive_basic_literal?])+ nil?)",
                "case x\nwhen :a then 1\nwhen :b then 2\nend",
                "two literal whens",
                true,
            ),
            // `FactoryBot/ConsistentParenthesesStyle::factory_call`, with the
            // `#factory_call?` helper dropped (it is cop-local).
            (
                "(send nil? :create {sym str send lvar} _*)",
                "create :user, name: 'x'",
                "trailing wildcard run",
                true,
            ),
            // `RSpecRails/MinitestAssertions`
            (
                "(send nil? {:assert_nil :assert_not_nil :refute_nil} $_ $_?)",
                "assert_nil foo",
                "without the message",
                true,
            ),
            (
                "(send nil? {:assert_nil :assert_not_nil :refute_nil} $_ $_?)",
                "assert_nil foo, 'msg'",
                "with the message",
                true,
            ),
            // `Performance/ReverseFirst::reverse_first_candidate?`
            (
                "(call $(call _ :reverse) :first (int _)?)",
                "x.reverse.first(3)",
                "with a count",
                true,
            ),
            (
                "(call $(call _ :reverse) :first (int _)?)",
                "x.reverse.first",
                "without a count",
                true,
            ),
        ];
        for &(pattern, source, label, expected) in cases {
            assert_eq!(
                matches_src(pattern, source),
                expected,
                "{label}: {pattern} @ {source:?}",
            );
        }
    }

    #[test]
    fn ascend_matches_the_parent() {
        let cases: &[(&str, &str, &str, bool)] = &[
            // pattern, source, node, expected
            ("^send", "foo(bar)\n", "bar", true),
            ("^def", "foo(bar)\n", "bar", false),
            ("^(send nil? :foo ...)", "foo(bar)\n", "bar", true),
            ("^(send nil? :baz ...)", "foo(bar)\n", "bar", false),
            // No parent at all.
            ("^_", "bar\n", "bar", false),
            // `^^` climbs twice; the intervening `StatementsNode` is not a
            // Parser node, so `def` is two levels up, not three.
            ("^^def", "def m; foo(bar); end\n", "bar", true),
            ("^^send", "def m; foo(bar); end\n", "bar", false),
            // `^` inside a sequence: the child's parent is the sequence node.
            ("(send nil? :foo ^send)", "foo(bar)\n", "foo(bar)", true),
        ];
        for &(pattern, source, needle, expected) in cases {
            assert_eq!(
                matches_at(pattern, source, needle),
                expected,
                "{pattern} against {needle:?} in {source:?}",
            );
        }
    }

    #[test]
    fn ascend_keeps_captures() {
        // `Style/AmbiguousEndlessMethodDefinition` captures under `^`.
        let compiled = CompiledPattern::compile("^$(if _ _ _)").unwrap();
        let matched =
            test_support::with_node_and_chain("if a then b else c end\n", "b", |node, chain| {
                compiled
                    .match_captures_with_ancestors(node, chain, &Params::new(), &NoResolver)
                    .map(|captures| captures.node(0).is_some())
            });
        assert_eq!(matched, Some(true));
    }

    #[test]
    fn descend_yields_the_element_then_its_subtree() {
        let cases: &[(&str, &str, &str, bool)] = &[
            // `descend` yields the element itself first.
            ("`(send nil? :foo)", "foo\n", "foo", true),
            ("`(send nil? :bar)", "foo(bar)\n", "foo(bar)", true),
            ("`(send nil? :baz)", "foo(bar)\n", "foo(bar)", false),
            // Several levels down.
            (
                "`(lvasgn :x _)",
                "foo { |y| [1, (x = 2)] }\n",
                "foo { |y| [1, (x = 2)] }",
                true,
            ),
            // `Rails/ReversibleMigrationMethodDefinition`. Upstream writes
            // `(def :change (args) _)`; the `(args)` term is dropped here
            // because Prism gives a parameterless `def` no `ParametersNode`
            // at all, which is a pre-existing mapping gap unrelated to `` ` ``.
            (
                "`(def :change ...)",
                "class M; def change; up; end; end\n",
                "class M; def change; up; end; end",
                true,
            ),
            (
                "`(def :change ...)",
                "class M; def other; up; end; end\n",
                "class M; def other; up; end; end",
                false,
            ),
            // `Style/SafeNavigation::and_inside_begin?`
            ("`(begin and ...)", "(a && b)\n", "(a && b)", true),
        ];
        for &(pattern, source, needle, expected) in cases {
            assert_eq!(
                matches_at(pattern, source, needle),
                expected,
                "{pattern} against {needle:?} in {source:?}",
            );
        }
    }

    #[test]
    fn descend_binds_captures_from_the_matched_descendant() {
        // `Gemspec/RequireMfa::metadata`
        let compiled = CompiledPattern::compile("`(send _ :metadata= $_)").unwrap();
        let source = "Gem::Specification.new do |spec|\n  spec.metadata = { 'a' => 'b' }\nend\n";
        let bound = test_support::with_node_and_chain(source, source.trim_end(), |node, chain| {
            compiled
                .match_captures_with_ancestors(node, chain, &Params::new(), &NoResolver)
                .and_then(|captures| {
                    captures.node(0).map(|hash| {
                        String::from_utf8_lossy(hash.location().as_slice()).into_owned()
                    })
                })
        });
        assert_eq!(bound.as_deref(), Some("{ 'a' => 'b' }"));
    }

    #[test]
    fn a_sequence_head_can_be_more_than_a_type_name() {
        let cases: &[(&str, &str, &str, bool)] = &[
            // `Performance/MethodObjectAsBlock::method_object_as_argument?`:
            // the block-pass's parent is a send, and its own child is
            // `(send _ :method sym)`.
            (
                "(^send (send _ :method sym))",
                "array.map(&method(:foo))\n",
                "&method(:foo)",
                true,
            ),
            (
                "(^send (send _ :method sym))",
                "array.map(&other(:foo))\n",
                "&other(:foo)",
                false,
            ),
            // A union at the head, both arms seq-head compiled.
            ("({send def} nil? :foo)", "foo\n", "foo", true),
            ("({^send ^def} nil? :foo)", "bar(foo)\n", "foo", true),
            ("({^def ^module} nil? :foo)", "bar(foo)\n", "foo", false),
        ];
        for &(pattern, source, needle, expected) in cases {
            assert_eq!(
                matches_at(pattern, source, needle),
                expected,
                "{pattern} against {needle:?} in {source:?}",
            );
        }
    }

    #[test]
    fn param_zero_is_the_node_the_matcher_was_called_on() {
        // `Style/RedundantParentheses::first_send_argument?` — is the node the
        // *first* argument of its enclosing send?
        let pattern = "^(send _ _ equal?(%0) ...)";
        assert!(matches_at(pattern, "foo((bar), baz)\n", "(bar)"));
        assert!(!matches_at(pattern, "foo(baz, (bar))\n", "(bar)"));

        // `Performance/RedundantMatch::only_truthiness_matters?`
        let pattern = "^({if while until case while_post until_post} equal?(%0) ...)";
        assert!(matches_at(
            pattern,
            "if foo.match(/x/)\n  1\nend\n",
            "foo.match(/x/)"
        ));
        assert!(!matches_at(
            pattern,
            "if bar\n  foo.match(/x/)\nend\n",
            "foo.match(/x/)"
        ));

        // `Style/RedundantParentheses::first_yield_argument?`
        assert!(matches_at(
            "^(yield equal?(%0) ...)",
            "yield (a), b\n",
            "(a)"
        ));
    }

    #[test]
    fn equal_compares_identity_not_structure() {
        // Two structurally identical arguments; only the first is `%0`.
        assert!(matches_at(
            "^(send _ _ equal?(%0) ...)",
            "foo((a), (a))\n",
            "(a)"
        ));
        // `equal?` with no node bound answers false rather than matching.
        assert!(!matches_at("equal?(%1)", "foo\n", "foo"));
    }

    #[test]
    fn a_recursive_matcher_terminates_by_climbing() {
        // `Style/MixinUsage::in_top_level_scope?`, verbatim. It recurses
        // through `^`, which strictly shortens the chain, so it terminates.
        const PATTERN: &str = "{root? ^[{kwbegin begin if def} #in_top_level_scope?]}";
        let owner = Owner::new(&[("in_top_level_scope?", PATTERN)], &[]);

        let cases: &[(&str, bool)] = &[
            ("include Foo\n", true),
            ("if x\n  include Foo\nend\n", true),
            ("begin\n  include Foo\nend\n", true),
            ("class C\n  include Foo\nend\n", false),
            ("module M\n  include Foo\nend\n", false),
            ("def m\n  include Foo\nend\n", true),
        ];
        for &(source, expected) in cases {
            assert_eq!(
                matches_at_with(PATTERN, source, "include Foo", &Params::new(), &owner),
                expected,
                "{source:?}",
            );
        }
    }

    /// Helper: get first statement from parsed Ruby source.
    fn first_stmt<'a>(result: &'a ruby_prism::ParseResult<'a>) -> ruby_prism::Node<'a> {
        let root = result.node();
        let program = root.as_program_node().unwrap();
        let stmts = program.statements();
        stmts.body().iter().next().unwrap()
    }

    /// A stand-in for the object a pattern was defined on: named matchers and
    /// constants, exactly what the future IR cop supplies.
    struct Owner {
        matchers: std::collections::HashMap<String, CompiledPattern>,
        constants: std::collections::HashMap<String, Arg>,
    }

    /// Resolves every declared name to the same trivial pattern.
    ///
    /// Only [`collect_unresolved`] sees it, and that only asks whether a name
    /// is known — which is what lets a set of matchers refer to each other,
    /// and to themselves (`#in_top_level_scope?`), before any of them exists.
    struct Declared<'a>(&'a [(&'a str, &'a str)], CompiledPattern);

    impl Resolver for Declared<'_> {
        fn matcher(&self, name: &str) -> Option<&CompiledPattern> {
            self.0
                .iter()
                .any(|(declared, _)| *declared == name)
                .then_some(&self.1)
        }
    }

    impl Owner {
        fn new(matchers: &[(&str, &str)], constants: &[(&str, Arg)]) -> Self {
            let declared = Declared(matchers, CompiledPattern::compile("_").unwrap());
            Self {
                matchers: matchers
                    .iter()
                    .map(|(name, pattern)| {
                        (
                            (*name).to_string(),
                            CompiledPattern::compile_with(pattern, &declared).unwrap_or_else(
                                |error| panic!("matcher {name} should compile: {error:?}"),
                            ),
                        )
                    })
                    .collect(),
                constants: constants
                    .iter()
                    .map(|(name, value)| ((*name).to_string(), value.clone()))
                    .collect(),
            }
        }
    }

    impl Resolver for Owner {
        fn matcher(&self, name: &str) -> Option<&CompiledPattern> {
            self.matchers.get(name)
        }

        fn constant(&self, name: &str) -> Option<&Arg> {
            self.constants.get(name)
        }
    }

    /// An owner that claims every name, for tests that only care that a
    /// vendored pattern *compiles* and not what its cop-local helpers mean.
    struct AnyOwner {
        wildcard: CompiledPattern,
        unresolved: Arg,
    }

    impl AnyOwner {
        fn new() -> Self {
            Self {
                wildcard: CompiledPattern::compile("_").expect("`_` compiles"),
                unresolved: Arg::Unresolved,
            }
        }
    }

    impl Resolver for AnyOwner {
        fn matcher(&self, _name: &str) -> Option<&CompiledPattern> {
            Some(&self.wildcard)
        }

        fn constant(&self, _name: &str) -> Option<&Arg> {
            Some(&self.unresolved)
        }
    }

    /// Compile `pattern` against builtins only and match it on `ruby`.
    fn matches_ruby(pattern: &str, ruby: &[u8]) -> bool {
        let compiled =
            CompiledPattern::compile(pattern).unwrap_or_else(|| panic!("{pattern} should compile"));
        let result = ruby_prism::parse(ruby);
        compiled.matches(&first_stmt(&result))
    }

    /// Match an already-compiled pattern on `ruby` with `owner` resolving.
    fn matches_with(pattern: &CompiledPattern, ruby: &[u8], owner: &dyn Resolver) -> bool {
        let result = ruby_prism::parse(ruby);
        pattern.matches_in(&first_stmt(&result), &Params::new(), owner)
    }

    /// Match an already-compiled pattern on `ruby` with `params` bound.
    fn matches_params(pattern: &CompiledPattern, ruby: &[u8], params: &Params) -> bool {
        let result = ruby_prism::parse(ruby);
        pattern.matches_in(&first_stmt(&result), params, &NoResolver)
    }

    #[test]
    fn test_send_nil_receiver() {
        // require 'foo' → (send nil? :require (str "foo"))
        let source = b"require 'foo'";
        let result = ruby_prism::parse(source);
        let node = first_stmt(&result);

        assert!(interpret_pattern("(send nil? :require ...)", &node));
        assert!(!interpret_pattern("(send nil? :include ...)", &node));
    }

    #[test]
    fn test_send_with_receiver() {
        let source = b"obj.foo";
        let result = ruby_prism::parse(source);
        let node = first_stmt(&result);

        assert!(interpret_pattern("(send _ :foo)", &node));
        assert!(!interpret_pattern("(send nil? :foo)", &node));
    }

    #[test]
    fn test_wildcard() {
        let source = b"x.bar(1)";
        let result = ruby_prism::parse(source);
        let node = first_stmt(&result);

        assert!(interpret_pattern("(send _ :bar _)", &node));
        assert!(interpret_pattern("(send _ _ ...)", &node));
    }

    #[test]
    fn test_rest() {
        let source = b"foo(1, 2, 3)";
        let result = ruby_prism::parse(source);
        let node = first_stmt(&result);

        assert!(interpret_pattern("(send nil? :foo ...)", &node));
    }

    #[test]
    fn test_if_no_else() {
        // if x; y; end → (if _ _ nil?)
        let source = b"if x; y; end";
        let result = ruby_prism::parse(source);
        let node = first_stmt(&result);

        assert!(interpret_pattern("(if _ _ nil?)", &node));
        assert!(interpret_pattern("(if _ _ _)", &node)); // wildcard also matches Absent
    }

    #[test]
    fn test_alternatives() {
        let source = b"obj.first";
        let result = ruby_prism::parse(source);
        let node = first_stmt(&result);

        assert!(interpret_pattern("(send _ {:first | :take})", &node));
        assert!(interpret_pattern("(send _ {:first :take})", &node));
    }

    #[test]
    fn test_alternatives_no_match() {
        let source = b"obj.last";
        let result = ruby_prism::parse(source);
        let node = first_stmt(&result);

        assert!(!interpret_pattern("(send _ {:first :take})", &node));
    }

    #[test]
    fn test_negation() {
        let source = b"obj.foo";
        let result = ruby_prism::parse(source);
        let node = first_stmt(&result);

        // Has a receiver (not nil?), so !nil? on receiver should match
        assert!(interpret_pattern("(send !nil? :foo)", &node));
        // nil? on receiver should NOT match
        assert!(!interpret_pattern("(send nil? :foo)", &node));
    }

    #[test]
    fn test_nested_send() {
        let source = b"obj.where.first";
        let result = ruby_prism::parse(source);
        let node = first_stmt(&result);

        assert!(interpret_pattern("(send (send _ :where) :first)", &node));
    }

    #[test]
    fn test_type_predicate() {
        let source = b"'hello'";
        let result = ruby_prism::parse(source);
        let node = first_stmt(&result);

        assert!(interpret_pattern("str?", &node));
        assert!(!interpret_pattern("int?", &node));
    }

    #[test]
    fn test_true_false_nil_literals() {
        let source_true = b"true";
        let result_true = ruby_prism::parse(source_true);
        let node_true = first_stmt(&result_true);
        assert!(interpret_pattern("true", &node_true));
        assert!(!interpret_pattern("false", &node_true));

        let source_false = b"false";
        let result_false = ruby_prism::parse(source_false);
        let node_false = first_stmt(&result_false);
        assert!(interpret_pattern("false", &node_false));

        let source_nil = b"nil";
        let result_nil = ruby_prism::parse(source_nil);
        let node_nil = first_stmt(&result_nil);
        assert!(interpret_pattern("nil", &node_nil));
    }

    // ── Captures ──────────────────────────────────────────────────────────

    /// Source text of a captured node, for readable assertions.
    fn captured_src<'a>(captures: &'a Captures<'a>, slot: usize) -> &'a str {
        let node = captures
            .node(slot)
            .unwrap_or_else(|| panic!("slot {slot} did not bind a node"));
        std::str::from_utf8(node.location().as_slice()).unwrap()
    }

    fn captured_name<'a>(captures: &'a Captures<'a>, slot: usize) -> &'a str {
        let bytes = captures
            .name(slot)
            .unwrap_or_else(|| panic!("slot {slot} did not bind a name"));
        std::str::from_utf8(bytes).unwrap()
    }

    #[test]
    fn test_single_capture_binds_node() {
        let result = ruby_prism::parse(b"obj.foo");
        let node = first_stmt(&result);

        let captures = match_with_captures("$(send _ :foo)", &node).unwrap();
        assert_eq!(captures.len(), 1);
        assert_eq!(captured_src(&captures, 0), "obj.foo");

        // A capture nested one level down binds the receiver instead.
        let captures = match_with_captures("(send $_ :foo)", &node).unwrap();
        assert_eq!(captured_src(&captures, 0), "obj");
    }

    #[test]
    fn test_capture_binds_method_name_bytes() {
        let result = ruby_prism::parse(b"obj.foo");
        let node = first_stmt(&result);

        let captures = match_with_captures("(send _ $_)", &node).unwrap();
        assert_eq!(captured_name(&captures, 0), "foo");
        assert!(captures.node(0).is_none());
    }

    #[test]
    fn test_capture_binds_absent_child() {
        let result = ruby_prism::parse(b"require 'foo'");
        let node = first_stmt(&result);

        let captures = match_with_captures("(send $nil? :require ...)", &node).unwrap();
        assert!(captures[0].is_absent());
    }

    #[test]
    fn test_multiple_captures_are_numbered_in_source_order() {
        // Style/EvenOdd, verbatim from the pattern DB.
        let pattern =
            "(send {(send $_ :% (int 2)) (begin (send $_ :% (int 2)))} ${:== :!=} (int ${0 1}))";
        let result = ruby_prism::parse(b"x % 2 == 0");
        let node = first_stmt(&result);

        let compiled = CompiledPattern::compile(pattern).unwrap();
        assert_eq!(compiled.capture_count(), 3);

        let captures = compiled.match_captures(&node).unwrap();
        assert_eq!(captured_src(&captures, 0), "x");
        assert_eq!(captured_name(&captures, 1), "==");
        assert_eq!(captured_src(&captures, 2), "0");
    }

    #[test]
    fn test_captures_inside_union_branches_share_slots() {
        let pattern = "{(send $(send _ :rstrip) :lstrip) (send $(send _ :lstrip) :rstrip)}";
        // `#rspec?` and the two `Const.method` helpers are rubocop-rspec
        // `Language` matchers, i.e. owner-supplied.
        let owner = AnyOwner::new();
        let compiled = CompiledPattern::compile_with(pattern, &owner).unwrap();
        assert_eq!(compiled.capture_count(), 1);

        let first = ruby_prism::parse(b"s.rstrip.lstrip");
        let node = first_stmt(&first);
        let captures = compiled.match_captures(&node).unwrap();
        assert_eq!(captured_src(&captures, 0), "s.rstrip");

        // The other branch writes the same slot.
        let second = ruby_prism::parse(b"s.lstrip.rstrip");
        let node = first_stmt(&second);
        let captures = compiled.match_captures(&node).unwrap();
        assert_eq!(captured_src(&captures, 0), "s.lstrip");
    }

    #[test]
    fn test_union_with_unbalanced_captures_does_not_compile() {
        assert!(CompiledPattern::compile("{(send $_ :a) (send _ :b)}").is_none());
        assert!(!interpret_pattern(
            "{(send $_ :a) (send _ :b)}",
            &first_stmt(&ruby_prism::parse(b"x.a"))
        ));
    }

    #[test]
    fn test_capture_is_unwound_when_a_union_branch_fails() {
        // Branch 1 binds the receiver (`x.c`) before its method-name check
        // fails; the winning branch must overwrite it with `x`.
        let pattern = "{(send $_ :b) (send (send $_ :c) :d)}";
        let result = ruby_prism::parse(b"x.c.d");
        let node = first_stmt(&result);

        let captures = match_with_captures(pattern, &node).unwrap();
        assert_eq!(captured_src(&captures, 0), "x");
    }

    #[test]
    fn test_capture_under_negation_never_survives() {
        let result = ruby_prism::parse(b"x.a");
        let node = first_stmt(&result);

        // `!(send $_ :b)` succeeds because the inner pattern fails; the write
        // the inner pattern made before failing must be rolled back.
        let captures = match_with_captures("!(send $_ :b)", &node).unwrap();
        assert_eq!(captures.len(), 1);
        assert!(captures.get(0).is_none());
    }

    #[test]
    fn test_capture_rest_binds_the_consumed_run() {
        let result = ruby_prism::parse(b"foo(1, 2, 3)");
        let node = first_stmt(&result);

        let captures = match_with_captures("(send nil? :foo $...)", &node).unwrap();
        let run = captures[0].as_list().unwrap();
        assert_eq!(run.len(), 3);

        // An empty run still binds, as an empty list.
        let empty = ruby_prism::parse(b"foo");
        let node = first_stmt(&empty);
        let captures = match_with_captures("(send nil? :foo $...)", &node).unwrap();
        assert!(captures[0].as_list().unwrap().is_empty());
    }

    #[test]
    fn test_capture_rest_backtracks_to_align_the_tail() {
        // The rest must give back the children the trailing term needs, and
        // the runs bound by the rejected splits must not leak.
        let result = ruby_prism::parse(b"foo(1, 2, 3)");
        let node = first_stmt(&result);

        let captures = match_with_captures("(send nil? :foo $... (int 3))", &node).unwrap();
        let run = captures[0].as_list().unwrap();
        let sources: Vec<&str> = run
            .iter()
            .map(|value| {
                std::str::from_utf8(value.as_node().unwrap().location().as_slice()).unwrap()
            })
            .collect();
        assert_eq!(sources, vec!["1", "2"]);
    }

    #[test]
    fn test_capture_after_rest() {
        let result = ruby_prism::parse(b"foo(1, 2, 3)");
        let node = first_stmt(&result);

        let captures = match_with_captures("(send nil? :foo ... $_)", &node).unwrap();
        assert_eq!(captured_src(&captures, 0), "3");
    }

    #[test]
    fn test_rest_term_makes_sequence_arity_exact() {
        let result = ruby_prism::parse(b"foo(1, 2, 3)");
        let node = first_stmt(&result);

        // `(int 1)` can only be the last child, which it is not.
        assert!(!interpret_pattern("(send nil? :foo ... (int 1))", &node));
        assert!(interpret_pattern("(send nil? :foo ... (int 3))", &node));
    }

    #[test]
    fn test_captures_inside_union_and_rest_together() {
        // Lint/SafeNavigationChain, verbatim from the pattern DB.
        let pattern = "{(send $(csend ...) $_ ...) (send $(any_block (csend ...) ...) $_ ...)}";
        let compiled = CompiledPattern::compile(pattern).unwrap();
        assert_eq!(compiled.capture_count(), 2);

        let result = ruby_prism::parse(b"x&.foo.bar");
        let node = first_stmt(&result);
        let captures = compiled.match_captures(&node).unwrap();
        assert_eq!(captured_src(&captures, 0), "x&.foo");
        assert_eq!(captured_name(&captures, 1), "bar");
    }

    #[test]
    fn test_capture_of_symbol_and_string_values() {
        let sym = ruby_prism::parse(b":foo");
        let node = first_stmt(&sym);
        let captures = match_with_captures("(sym $_)", &node).unwrap();
        assert_eq!(captured_name(&captures, 0), "foo");

        let string = ruby_prism::parse(b"'hello'");
        let node = first_stmt(&string);
        let captures = match_with_captures("(str $_)", &node).unwrap();
        assert_eq!(captured_name(&captures, 0), "hello");
    }

    #[test]
    fn test_no_captures_yields_empty_capture_set() {
        let result = ruby_prism::parse(b"obj.foo");
        let node = first_stmt(&result);

        let captures = match_with_captures("(send _ :foo)", &node).unwrap();
        assert!(captures.is_empty());
        assert!(match_with_captures("(send _ :bar)", &node).is_none());
    }

    #[test]
    fn test_pattern_db_capture_patterns_compile() {
        use crate::node_pattern::pattern_db::PATTERNS;

        let mut checked = 0;
        for entry in PATTERNS {
            if !entry.pattern.contains('$') {
                continue;
            }
            checked += 1;
            let owner = AnyOwner::new();
            let compiled =
                CompiledPattern::compile_with(entry.pattern, &owner).unwrap_or_else(|err| {
                    panic!(
                        "{} failed to compile: {} ({err})",
                        entry.cop_name, entry.pattern
                    )
                });
            assert!(
                compiled.capture_count() > 0,
                "{} has `$` but no capture slots",
                entry.cop_name
            );
            // Union branches share slots, so the slot count never exceeds the
            // number of `$` in the source pattern.
            assert!(
                compiled.capture_count() <= entry.pattern.matches('$').count(),
                "{} allocated more slots than it has `$`",
                entry.cop_name
            );
        }
        assert!(
            checked >= 10,
            "expected several capture patterns, got {checked}"
        );
    }

    #[test]
    fn test_pattern_db_capture_counts() {
        // Slot counts for real vendor patterns, checked against what RuboCop's
        // compiler would allocate (union branches share a slot range).
        for (pattern, expected) in [
            // Style/Strip — one capture per branch, shared.
            (
                "{(call $(call _ :rstrip) :lstrip) (call $(call _ :lstrip) :rstrip)}",
                1,
            ),
            // Lint/SafeNavigationChain — two per branch, shared.
            (
                "{(send $(csend ...) $_ ...) (send $(any_block (csend ...) ...) $_ ...)}",
                2,
            ),
            // Style/EvenOdd — one inside the union, then two after it.
            (
                "(send {(send $_ :% (int 2)) (begin (send $_ :% (int 2)))} ${:== :!=} (int ${0 1}))",
                3,
            ),
            // Performance/FlatMap — two shared inside the union, then two more.
            (
                "(call {$(block (call _ ${:collect :map}) ...) $(call _ ${:collect :map} (block_pass _))} ${:flatten :flatten!} $...)",
                4,
            ),
        ] {
            let compiled = CompiledPattern::compile(pattern).unwrap();
            assert_eq!(compiled.capture_count(), expected, "pattern: {pattern}");
        }
    }

    #[test]
    fn test_multi_term_union_branch_matches_a_run_of_children() {
        // `{a b | c d}` branches consume several children each.
        let pattern = "(send {$_ :>= $_ | $_ :<= $_})";
        let compiled = CompiledPattern::compile(pattern).unwrap();
        assert_eq!(compiled.capture_count(), 2);

        let ge = ruby_prism::parse(b"a >= b");
        let node = first_stmt(&ge);
        let captures = compiled.match_captures(&node).unwrap();
        assert_eq!(captured_src(&captures, 0), "a");
        assert_eq!(captured_src(&captures, 1), "b");

        let le = ruby_prism::parse(b"c <= d");
        let node = first_stmt(&le);
        let captures = compiled.match_captures(&node).unwrap();
        assert_eq!(captured_src(&captures, 0), "c");
        assert_eq!(captured_src(&captures, 1), "d");

        let other = ruby_prism::parse(b"a > b");
        let node = first_stmt(&other);
        assert!(compiled.match_captures(&node).is_none());
    }

    #[test]
    fn test_union_of_single_term_branches_still_matches_one_child() {
        let result = ruby_prism::parse(b"obj.first");
        let node = first_stmt(&result);
        assert!(interpret_pattern("(send _ {:first :take})", &node));
        assert!(!interpret_pattern("(send _ {:last :take})", &node));
    }

    #[test]
    fn test_capture_transparent() {
        let source = b"obj.foo";
        let result = ruby_prism::parse(source);
        let node = first_stmt(&result);

        // $_ should match like _
        assert!(interpret_pattern("(send $_ :foo)", &node));
        // $(send ...) should match like (send ...)
        assert!(interpret_pattern("$(send _ :foo)", &node));
    }

    #[test]
    fn test_conjunction() {
        let source = b"obj.foo";
        let result = ruby_prism::parse(source);
        let node = first_stmt(&result);

        // [!nil? send_type?] — both must match
        assert!(interpret_pattern("[!nil? send_type?]", &node));
    }

    #[test]
    fn test_int_literal() {
        let source = b"42";
        let result = ruby_prism::parse(source);
        let node = first_stmt(&result);

        assert!(interpret_pattern("(int 42)", &node));
    }

    #[test]
    fn test_string_literal_match() {
        let source = b"'hello'";
        let result = ruby_prism::parse(source);
        let node = first_stmt(&result);

        assert!(interpret_pattern("(str 'hello')", &node));
        assert!(!interpret_pattern("(str 'world')", &node));
    }

    #[test]
    fn test_symbol_literal_match() {
        let source = b":foo";
        let result = ruby_prism::parse(source);
        let node = first_stmt(&result);

        assert!(interpret_pattern("(sym :foo)", &node));
        assert!(!interpret_pattern("(sym :bar)", &node));
    }

    /// Prism wraps every body in a `StatementsNode`; Parser only builds a
    /// `begin` for a list of two or more, so a one-statement body is peeled.
    #[test]
    fn single_statement_body_is_not_a_begin() {
        let one = ruby_prism::parse(b"items.each { |x| x }");
        let node = first_stmt(&one);
        assert!(interpret_pattern("(block _ _ (lvar :x))", &node));
        assert!(!interpret_pattern("(block _ _ (begin (lvar :x)))", &node));

        let two = ruby_prism::parse(b"items.each { |x| x; x }");
        let node = first_stmt(&two);
        assert!(interpret_pattern(
            "(block _ _ (begin (lvar :x) (lvar :x)))",
            &node
        ));
        assert!(!interpret_pattern("(block _ _ (lvar :x))", &node));

        let def_one = ruby_prism::parse(b"def m; a; end");
        let node = first_stmt(&def_one);
        assert!(interpret_pattern("(def :m (args) (send nil? :a))", &node));
        assert!(!interpret_pattern(
            "(def :m (args) (begin (send nil? :a)))",
            &node
        ));

        let klass = ruby_prism::parse(b"class C; a; end");
        let node = first_stmt(&klass);
        assert!(interpret_pattern(
            "(class (const nil? :C) nil? (send nil? :a))",
            &node
        ));
    }

    /// `lexer.rex`'s `tUNIFY`: the first `_name` binds, later ones compare.
    #[test]
    fn unify_variables_bind_then_compare() {
        let same = ruby_prism::parse(b"array.max_by { |x| x }");
        let node = first_stmt(&same);
        assert!(interpret_pattern(
            "(block _ (args (arg _x)) (lvar _x))",
            &node
        ));
        assert!(interpret_pattern(
            "(block $(call _ {:max_by :min_by :minmax_by}) (args (arg $_x)) (lvar _x))",
            &node,
        ));
        // A different name is a *first* occurrence, so it binds and matches.
        assert!(interpret_pattern(
            "(block _ (args (arg _x)) (lvar _y))",
            &node
        ));

        let other = ruby_prism::parse(b"array.max_by { |x| y }");
        let node = first_stmt(&other);
        assert!(!interpret_pattern(
            "(block _ (args (arg _x)) (lvar _x))",
            &node
        ));

        // A binding made by a `...` placement that then fails must not leak
        // into the next placement the interpreter tries.
        let rest = ruby_prism::parse(b"z = 1; b = 2; a = 3; f(z, b, a, a)");
        let node = {
            let program = rest.node();
            let statements = program.as_program_node().unwrap().statements();
            statements.body().iter().last().unwrap()
        };
        assert!(interpret_pattern(
            "(send nil? :f ... (lvar _x) (lvar _x))",
            &node
        ));
        assert!(!interpret_pattern(
            "(send nil? :f ... (lvar _x) (lvar _x) (lvar _x))",
            &node
        ));
    }

    /// Parser spells the implicit `it` of a `{ it }` block `(lvar :it)`.
    #[test]
    fn implicit_it_is_an_lvar() {
        let source = ruby_prism::parse(b"array.max_by { it }");
        let node = first_stmt(&source);
        assert!(interpret_pattern("(itblock _ _ (lvar :it))", &node));
        assert!(!interpret_pattern("(itblock _ _ (lvar :other))", &node));
    }

    #[test]
    fn test_block_pattern() {
        let source = b"items.each { |x| x }";
        let result = ruby_prism::parse(source);
        let node = first_stmt(&result);

        // In Prism, `items.each { |x| x }` is a CallNode with a block.
        // The BlockNode is the block child, not the top-level statement.
        // The top-level node is the CallNode.
        assert!(interpret_pattern("(send _ :each)", &node));

        // To test BlockNode matching, get the block from the call
        let call = node.as_call_node().unwrap();
        let block = call.block().unwrap();
        // BlockNode: call child is Absent (Prism structure), params, body
        assert!(interpret_pattern("(block _ _ _)", &block));
    }

    #[test]
    fn test_def_pattern() {
        let source = b"def initialize; end";
        let result = ruby_prism::parse(source);
        let node = first_stmt(&result);

        assert!(interpret_pattern("(def :initialize ...)", &node));
        assert!(!interpret_pattern("(def :other ...)", &node));
    }

    #[test]
    fn test_and_or_patterns() {
        let source_and = b"a && b";
        let result_and = ruby_prism::parse(source_and);
        let node_and = first_stmt(&result_and);
        assert!(interpret_pattern("(and _ _)", &node_and));

        let source_or = b"a || b";
        let result_or = ruby_prism::parse(source_or);
        let node_or = first_stmt(&result_or);
        assert!(interpret_pattern("(or _ _)", &node_or));
    }

    #[test]
    fn test_unknown_helper_is_a_compile_error() {
        // Upstream this is a `NoMethodError` the first time the pattern runs.
        assert_eq!(
            CompiledPattern::compile_with("(send #any_helper? :foo)", &NoResolver).err(),
            Some(PatternError::UnknownHelper {
                name: "any_helper?".to_string()
            })
        );
        assert_eq!(
            CompiledPattern::compile_with("(send _ bogus_thing?)", &NoResolver).err(),
            Some(PatternError::UnknownPredicate {
                name: "bogus_thing?".to_string()
            })
        );
        assert_eq!(
            CompiledPattern::compile_with("(send _ :foo %CANDIDATE_METHODS)", &NoResolver).err(),
            Some(PatternError::UnknownConstant {
                name: "CANDIDATE_METHODS".to_string()
            })
        );
        // And the boolean entry point simply does not match.
        let source = b"obj.foo";
        let result = ruby_prism::parse(source);
        let node = first_stmt(&result);
        assert!(!interpret_pattern("(send #any_helper? :foo)", &node));
    }

    #[test]
    fn test_predicate_arity_is_checked() {
        assert_eq!(
            CompiledPattern::compile_with("(send _ method?)", &NoResolver).err(),
            Some(PatternError::PredicateArity {
                name: "method?".to_string(),
                expected: 1,
                found: 0,
            })
        );
        assert!(CompiledPattern::compile_with("(send _ method?(:foo))", &NoResolver).is_ok());
    }

    #[test]
    fn test_builtin_predicate_is_evaluated() {
        // `Naming/ConstantName#literal_receiver?`, first branch.
        assert!(matches_ruby("(send literal? ...)", b"1.foo"));
        assert!(!matches_ruby("(send literal? ...)", b"x.foo"));
        // A predicate on a name slot.
        assert!(matches_ruby("(send _ operator_method? _)", b"a + b"));
        assert!(!matches_ruby("(send _ operator_method? _)", b"a.foo(b)"));
        // `Mixin/SafeAssignment#setter_method?`, verbatim.
        assert!(matches_ruby("[(call ...) setter_method?]", b"a.b = 1"));
        assert!(!matches_ruby("[(call ...) setter_method?]", b"a.b"));
    }

    #[test]
    fn test_builtin_predicate_with_an_argument() {
        assert!(matches_ruby("(send _ method?(:freeze))", b"a.freeze"));
        assert!(!matches_ruby("(send _ method?(:freeze))", b"a.dup"));
        // A `{}` argument is a Set upstream; membership, not equality.
        assert!(matches_ruby("(send _ method?({:dup :freeze}))", b"a.dup"));
        assert!(!matches_ruby("(send _ method?({:dup :freeze}))", b"a.to_s"));
    }

    #[test]
    fn test_helper_call_falls_back_to_the_registry() {
        // `#global_const?` is defined on `Node` itself (`node.rb:605-606`), so
        // it resolves with no owner at all.
        assert!(matches_ruby(
            "(send #global_const?(:Proc) :new)",
            b"Proc.new"
        ));
        assert!(matches_ruby(
            "(send #global_const?(:Proc) :new)",
            b"::Proc.new"
        ));
        assert!(!matches_ruby(
            "(send #global_const?(:Proc) :new)",
            b"Data.new"
        ));
    }

    #[test]
    fn test_helper_call_resolves_to_an_owner_matcher() {
        let owner = Owner::new(&[("array_receiver?", "{array (send _ :to_a)}")], &[]);
        let pattern =
            CompiledPattern::compile_with("(send #array_receiver? :first)", &owner).unwrap();
        assert!(matches_with(&pattern, b"[1, 2].first", &owner));
        assert!(matches_with(&pattern, b"x.to_a.first", &owner));
        assert!(!matches_with(&pattern, b"x.first", &owner));
    }

    #[test]
    fn test_const_qualified_helper_resolves_through_the_owner() {
        // rubocop-rspec's `Language` modules: `#Examples.all`.
        // `Language::Examples.all` is handed the method symbol and answers
        // set membership, so the matcher sits in a name slot.
        let owner = Owner::new(&[("Examples.all", "{:it :specify}")], &[]);
        let pattern =
            CompiledPattern::compile_with("(send nil? #Examples.all ...)", &owner).unwrap();
        assert!(matches_with(&pattern, b"it('x') { }", &owner));
        assert!(!matches_with(&pattern, b"describe('x') { }", &owner));
        // Without the owner it does not compile at all.
        assert!(matches!(
            CompiledPattern::compile_with("(send nil? #Examples.all ...)", &NoResolver),
            Err(PatternError::UnknownHelper { .. })
        ));
    }

    #[test]
    fn test_helper_call_arguments_become_the_matchers_params() {
        // `#foo(:bar)` calls the matcher with `:bar` bound to its `%1`.
        let owner = Owner::new(&[("named?", "(send nil? %1)")], &[]);
        let pattern = CompiledPattern::compile_with("#named?(:foo)", &owner).unwrap();
        assert!(matches_with(&pattern, b"foo", &owner));
        assert!(!matches_with(&pattern, b"bar", &owner));
    }

    #[test]
    fn test_positional_params_bind() {
        let pattern = CompiledPattern::compile("(send nil? %1)").unwrap();
        let params = Params::positional(vec![Arg::Symbol("foo".to_string())]);
        assert!(matches_params(&pattern, b"foo", &params));
        assert!(!matches_params(&pattern, b"bar", &params));
        // A bare `%` is `%1` (`lexer.rex`).
        let bare = CompiledPattern::compile("(send nil? %)").unwrap();
        assert!(matches_params(&bare, b"foo", &params));
    }

    #[test]
    fn test_named_params_bind() {
        let pattern = CompiledPattern::compile("(send nil? %method_name)").unwrap();
        let params = Params::new().with_named("method_name", Arg::Symbol("foo".to_string()));
        assert!(matches_params(&pattern, b"foo", &params));
        assert!(!matches_params(&pattern, b"bar", &params));
    }

    #[test]
    fn test_a_param_set_matches_by_membership() {
        let pattern = CompiledPattern::compile("(send nil? %1)").unwrap();
        let params = Params::positional(vec![Arg::Set(vec![
            Arg::Symbol("foo".to_string()),
            Arg::Symbol("bar".to_string()),
        ])]);
        assert!(matches_params(&pattern, b"foo", &params));
        assert!(matches_params(&pattern, b"bar", &params));
        assert!(!matches_params(&pattern, b"baz", &params));
    }

    #[test]
    fn test_an_unbound_param_fails_closed() {
        let pattern = CompiledPattern::compile("(send nil? %1)").unwrap();
        assert!(!matches_params(&pattern, b"foo", &Params::new()));
    }

    #[test]
    fn test_constants_resolve_through_the_owner() {
        let owner = Owner::new(
            &[],
            &[(
                "CANDIDATE_METHODS",
                Arg::Set(vec![
                    Arg::Symbol("first".to_string()),
                    Arg::Symbol("last".to_string()),
                ]),
            )],
        );
        let pattern = CompiledPattern::compile_with("(send _ %CANDIDATE_METHODS)", &owner).unwrap();
        assert!(matches_with(&pattern, b"x.first", &owner));
        assert!(!matches_with(&pattern, b"x.map", &owner));
    }

    #[test]
    fn test_regexp_atom_matches_a_name() {
        let pattern = CompiledPattern::compile("(send nil? /^style_detected$/)").unwrap();
        let result = ruby_prism::parse(b"style_detected");
        assert!(pattern.matches(&first_stmt(&result)));
        let result = ruby_prism::parse(b"other");
        assert!(!pattern.matches(&first_stmt(&result)));
    }

    #[test]
    fn test_a_capture_under_a_failing_predicate_is_unwound() {
        // The union's first branch captures then fails its predicate; the
        // second branch must see a clean slot.
        let pattern = CompiledPattern::compile("{(send $_ operator_method?) (send $_ _)}").unwrap();
        let result = ruby_prism::parse(b"recv.foo");
        let captures = pattern.match_captures(&first_stmt(&result)).unwrap();
        assert_eq!(captures.len(), 1);
        assert_eq!(captured_src(&captures, 0), "recv");
    }

    #[test]
    fn test_no_match_wrong_type() {
        let source = b"42";
        let result = ruby_prism::parse(source);
        let node = first_stmt(&result);

        assert!(!interpret_pattern("(send _ :foo)", &node));
    }

    #[test]
    fn test_parse_error_returns_false() {
        let source = b"x";
        let result = ruby_prism::parse(source);
        let node = first_stmt(&result);

        // Broken pattern
        assert!(!interpret_pattern("((( broken", &node));
    }

    #[test]
    fn test_lvasgn_pattern() {
        let source = b"x = 1";
        let result = ruby_prism::parse(source);
        let node = first_stmt(&result);

        assert!(interpret_pattern("(lvasgn :x _)", &node));
        assert!(!interpret_pattern("(lvasgn :y _)", &node));
    }

    #[test]
    fn test_array_pattern() {
        let source = b"[1, 2, 3]";
        let result = ruby_prism::parse(source);
        let node = first_stmt(&result);

        assert!(interpret_pattern("(array ...)", &node));
        assert!(interpret_pattern("(array _ _ _)", &node));
    }

    #[test]
    fn test_hash_pattern() {
        let source = b"{ a: 1 }";
        let result = ruby_prism::parse(source);
        let node = first_stmt(&result);

        assert!(interpret_pattern("(hash _)", &node));
    }

    #[test]
    fn test_class_pattern() {
        let source = b"class Foo < Bar; end";
        let result = ruby_prism::parse(source);
        let node = first_stmt(&result);

        assert!(interpret_pattern("(class _ _ _)", &node));
    }

    #[test]
    fn test_module_pattern() {
        let source = b"module Foo; end";
        let result = ruby_prism::parse(source);
        let node = first_stmt(&result);

        assert!(interpret_pattern("(module _ _)", &node));
    }

    #[test]
    fn test_return_pattern() {
        let source = b"return 42";
        let result = ruby_prism::parse(source);
        let node = first_stmt(&result);

        assert!(interpret_pattern("(return _)", &node));
    }

    #[test]
    fn test_csend_pattern() {
        let source = b"obj&.foo";
        let result = ruby_prism::parse(source);
        let node = first_stmt(&result);

        assert!(interpret_pattern("(csend _ :foo)", &node));
        assert!(!interpret_pattern("(send _ :foo)", &node));
    }

    #[test]
    fn test_send_csend_alternatives() {
        let source_send = b"obj.foo";
        let result_send = ruby_prism::parse(source_send);
        let node_send = first_stmt(&result_send);

        let source_csend = b"obj&.foo";
        let result_csend = ruby_prism::parse(source_csend);
        let node_csend = first_stmt(&result_csend);

        let pat = "{(send _ :foo) (csend _ :foo)}";
        assert!(interpret_pattern(pat, &node_send));
        assert!(interpret_pattern(pat, &node_csend));
    }

    // ── `<>` any-order groups ─────────────────────────────────────────────

    #[test]
    fn test_any_order_matches_in_either_order() {
        let pattern = "(send nil? :foo <(sym _) (str _)>)";
        for source in [b"foo(:a, 'b')".as_slice(), b"foo('b', :a)".as_slice()] {
            let result = ruby_prism::parse(source);
            let node = first_stmt(&result);
            assert!(
                interpret_pattern(pattern, &node),
                "{} should match",
                String::from_utf8_lossy(source)
            );
        }
    }

    #[test]
    fn test_any_order_requires_every_term() {
        let pattern = "(send nil? :foo <(sym _) (str _)>)";
        // Missing the string element.
        let result = ruby_prism::parse(b"foo(:a, :b)");
        let node = first_stmt(&result);
        assert!(!interpret_pattern(pattern, &node));
    }

    #[test]
    fn test_any_order_without_rest_has_exact_arity() {
        // Without a trailing `...` the group consumes exactly as many children
        // as it has terms, so an extra child inside its run is fatal. (An extra
        // child *after* it is still tolerated by the pre-existing child-list
        // permissiveness documented in the module header.)
        let pattern = "(send nil? :foo <(sym _) (str _)>)";
        let result = ruby_prism::parse(b"foo(1, :a, 'b')");
        let node = first_stmt(&result);
        assert!(!interpret_pattern(pattern, &node));
    }

    #[test]
    fn test_any_order_with_rest_absorbs_extra_children() {
        let pattern = "(send nil? :foo <(sym _) ...>)";
        let result = ruby_prism::parse(b"foo(1, 'b', :a, 2)");
        let node = first_stmt(&result);
        assert!(interpret_pattern(pattern, &node));

        // Still requires the term itself.
        let result = ruby_prism::parse(b"foo(1, 'b', 2)");
        let node = first_stmt(&result);
        assert!(!interpret_pattern(pattern, &node));
    }

    #[test]
    fn test_any_order_captured_rest_collects_unmatched_children() {
        let pattern = "(send nil? :foo <(sym _) $...>)";
        let result = ruby_prism::parse(b"foo(1, :a, 'b')");
        let node = first_stmt(&result);

        let captures = match_with_captures(pattern, &node).unwrap();
        let run = captures[0].as_list().expect("rest binds a list");
        assert_eq!(run.len(), 2);
        let texts: Vec<&str> = run
            .iter()
            .map(|value| {
                std::str::from_utf8(value.as_node().unwrap().location().as_slice()).unwrap()
            })
            .collect();
        assert_eq!(texts, vec!["1", "'b'"]);
    }

    #[test]
    fn test_captured_any_order_binds_whole_run() {
        let pattern = "(send nil? :foo $<(sym _) (str _)>)";
        let result = ruby_prism::parse(b"foo('b', :a)");
        let node = first_stmt(&result);

        // `#rspec?` and the two `Const.method` helpers are rubocop-rspec
        // `Language` matchers, i.e. owner-supplied.
        let owner = AnyOwner::new();
        let compiled = CompiledPattern::compile_with(pattern, &owner).unwrap();
        assert_eq!(compiled.capture_count(), 1);
        let captures = compiled.match_captures(&node).unwrap();
        let run = captures[0].as_list().expect("group binds a list");
        assert_eq!(run.len(), 2);
    }

    #[test]
    fn test_any_order_capture_slots_are_numbered_outside_in() {
        // The `$` on the group takes the lower slot, then the inner `$`.
        let compiled = CompiledPattern::compile("(send nil? :foo $<(sym $_) ...>)").unwrap();
        assert_eq!(compiled.capture_count(), 2);

        let result = ruby_prism::parse(b"foo(1, :a)");
        let node = first_stmt(&result);
        let captures = compiled.match_captures(&node).unwrap();
        assert_eq!(captures[0].as_list().unwrap().len(), 2);
        assert_eq!(captured_name(&captures, 1), "a");
    }

    #[test]
    fn test_any_order_inside_union_branch() {
        // Both branches declare one capture, as RuboCop requires.
        let pattern = "(send nil? :foo {<(sym $_) ...> (hash <(pair (sym $_) true) ...>)})";
        // `#rspec?` and the two `Const.method` helpers are rubocop-rspec
        // `Language` matchers, i.e. owner-supplied.
        let owner = AnyOwner::new();
        let compiled = CompiledPattern::compile_with(pattern, &owner).unwrap();
        assert_eq!(compiled.capture_count(), 1);

        let result = ruby_prism::parse(b"foo(1, :skip)");
        let node = first_stmt(&result);
        assert_eq!(
            captured_name(&compiled.match_captures(&node).unwrap(), 0),
            "skip"
        );

        let result = ruby_prism::parse(b"foo(a: 1, skip: true)");
        let node = first_stmt(&result);
        assert_eq!(
            captured_name(&compiled.match_captures(&node).unwrap(), 0),
            "skip"
        );

        let result = ruby_prism::parse(b"foo(a: 1, skip: false)");
        let node = first_stmt(&result);
        assert!(compiled.match_captures(&node).is_none());
    }

    #[test]
    fn test_vendor_pending_without_reason_pattern() {
        // rubocop-rspec `RSpec/PendingWithoutReason#metadata_without_reason?`,
        // verbatim — the one vendored pattern that needs `<>`.
        let pattern = "(send #rspec?\n \
            {#ExampleGroups.all #Examples.all} ...\n \
            {\n \
              <(sym ${:pending :skip}) ...>\n \
              (hash <(pair (sym ${:pending :skip}) true) ...>)\n \
            }\n \
          )";
        // `#rspec?` and the two `Const.method` helpers are rubocop-rspec
        // `Language` matchers, i.e. owner-supplied.
        let owner = AnyOwner::new();
        let compiled = CompiledPattern::compile_with(pattern, &owner).unwrap();
        assert_eq!(compiled.capture_count(), 1);

        let result = ruby_prism::parse(b"RSpec.describe 'thing', :pending do\nend");
        let node = first_stmt(&result);
        let call = node.as_call_node().unwrap().as_node();
        assert_eq!(
            captured_name(
                &compiled
                    .match_captures_in(&call, &Params::new(), &owner)
                    .unwrap(),
                0
            ),
            "pending"
        );

        let result = ruby_prism::parse(b"RSpec.describe 'thing', skip: true do\nend");
        let node = first_stmt(&result);
        let call = node.as_call_node().unwrap().as_node();
        assert_eq!(
            captured_name(
                &compiled
                    .match_captures_in(&call, &Params::new(), &owner)
                    .unwrap(),
                0
            ),
            "skip"
        );

        // No pending/skip metadata at all.
        let result = ruby_prism::parse(b"RSpec.describe 'thing', :focus do\nend");
        let node = first_stmt(&result);
        let call = node.as_call_node().unwrap().as_node();
        assert!(
            compiled
                .match_captures_in(&call, &Params::new(), &owner)
                .is_none()
        );
    }

    #[test]
    fn test_any_order_term_cap_is_enforced_at_compile_time() {
        use super::super::parser::{ANY_ORDER_MAX_TERMS, Parser, PatternError};

        let terms = (0..=ANY_ORDER_MAX_TERMS)
            .map(|i| format!("(sym :s{i})"))
            .collect::<Vec<_>>()
            .join(" ");
        let pattern = format!("(send nil? :foo <{terms}>)");
        assert!(CompiledPattern::compile(&pattern).is_none());

        let mut lexer = Lexer::new(&pattern);
        let mut parser = Parser::new(lexer.tokenize());
        assert!(parser.parse().is_none());
        assert_eq!(
            parser.error(),
            Some(&PatternError::AnyOrderTooManyTerms {
                found: ANY_ORDER_MAX_TERMS + 1,
                max: ANY_ORDER_MAX_TERMS,
            })
        );

        // One fewer term is fine.
        let terms = (0..ANY_ORDER_MAX_TERMS)
            .map(|i| format!("(sym :s{i})"))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(CompiledPattern::compile(&format!("(send nil? :foo <{terms}>)")).is_some());
    }

    #[test]
    fn test_any_order_rest_must_be_last() {
        assert!(CompiledPattern::compile("(send nil? :foo <... (sym _)>)").is_none());
    }

    // ── Parser-gem → Prism type mapping ───────────────────────────────────

    /// `(source, must_match, must_not_match)` for each newly mapped type.
    ///
    /// The pattern is matched against the first statement of `source`.
    const MAPPING_CASES: &[(&str, &str, &str)] = &[
        // Blocks. A Prism `CallNode` carrying a block answers to the Parser
        // `block` node that wraps it, so the send child is reachable.
        (
            "items.each { |x| x }",
            "(block (send _ :each) (args (arg :x)) _)",
            "(block (send _ :map) _ _)",
        ),
        (
            "items.each { _1 }",
            "(numblock (send _ :each) 1 _)",
            "(numblock (send _ :each) 2 _)",
        ),
        (
            "items.each { it }",
            "(itblock (send _ :each) :it _)",
            "(numblock _ _ _)",
        ),
        (
            "items.each { |x| x }",
            "(any_block _ _ _)",
            "(numblock _ _ _)",
        ),
        ("items.each { _1 }", "(any_block _ _ _)", "(itblock _ _ _)"),
        ("items.each { it }", "(any_block _ _ _)", "(block _ _ _)"),
        // `-> { }` is `(block (lambda) (args) body)` upstream.
        (
            "-> (x) { x }",
            "(block (lambda) (args (arg :x)) _)",
            "(block (send nil? :lambda) _ _)",
        ),
        // `call` group.
        ("foo.bar", "(call _ :bar)", "(call _ :baz)"),
        ("foo&.bar", "(call _ :bar)", "(send _ :bar)"),
        // Statement lists and `begin … end`.
        (
            "(x % 2) == 0",
            "(send (begin (send _ :% (int 2))) :== (int 0))",
            "(send (kwbegin _) :== _)",
        ),
        (
            "begin; foo; end",
            "(kwbegin (send nil? :foo))",
            "(begin (send nil? :foo))",
        ),
        // Arguments.
        (
            "def m(a, b = 1, *c, d:, e: 2, **f, &g); end",
            "(def :m (args (arg :a) (optarg :b (int 1)) (restarg :c) (kwarg :d) (kwoptarg :e (int 2)) (kwrestarg :f) (blockarg :g)) nil?)",
            "(def :m (args (optarg :a ...) ...) nil?)",
        ),
        (
            "def m(*); end",
            "(def :m (args (restarg)) nil?)",
            "(def :m (args (restarg :x)) nil?)",
        ),
        (
            "def m(...); end",
            "(def :m (args (forward_arg)) nil?)",
            "(def :m (args (restarg)) nil?)",
        ),
        (
            "def m(a); end",
            "(def :m (args argument) nil?)",
            "(def :m (args (optarg ...)) nil?)",
        ),
        // Block pass.
        (
            "foo(&:bar)",
            "(send nil? :foo (block_pass (sym :bar)))",
            "(send nil? :foo (block_pass (sym :baz)))",
        ),
        (
            "foo(&blk)",
            "(send nil? :foo (block-pass (send nil? :blk)))",
            "(send nil? :foo (block_pass nil?))",
        ),
        // Pattern matching.
        (
            "case x; in Integer then 1; end",
            "(case_match (send nil? :x) (in_pattern (const nil? :Integer) nil? _) nil?)",
            "(case ...)",
        ),
        // Strings.
        ("`ls`", "(xstr (str 'ls'))", "(str 'ls')"),
        ("'a'", "any_str", "(dstr ...)"),
        (
            "\"a#{b}\"",
            "(dstr (str 'a') (begin (send nil? :b)))",
            "(str _)",
        ),
        // Regexps.
        (
            "/foo/i",
            "(regexp (str 'foo') (regopt :i))",
            "(regexp (str 'foo') (regopt :m))",
        ),
        (
            "/foo/",
            "(regexp (str $_) (regopt))",
            "(regexp (str 'bar') _)",
        ),
        (
            "/a#{b}/",
            "(regexp (str 'a') (begin _) (regopt))",
            "(regexp (str 'a') (regopt))",
        ),
        // Ranges.
        ("1..2", "(irange (int 1) (int 2))", "(erange ...)"),
        ("1...2", "(erange (int 1) (int 2))", "(irange ...)"),
        ("1..", "range", "(irange _ (int 2))"),
        // Multiple assignment.
        (
            "a, b = 1, 2",
            "(masgn _ (array (int 1) (int 2)))",
            "(masgn _ (int 1))",
        ),
        (
            "foo { |(a, b)| a }",
            "(block _ (args (mlhs (arg :a) (arg :b))) _)",
            "(block _ (args (arg :a)) _)",
        ),
        // Operator assignment.
        ("x += 1", "(op_asgn _ :+ (int 1))", "(op_asgn _ :- _)"),
        ("x ||= 1", "(or_asgn _ (int 1))", "(and_asgn _ _)"),
        ("x &&= 1", "(and_asgn _ (int 1))", "(op_asgn ...)"),
        // Singleton class and definitions.
        ("class << self; end", "(sclass (self) nil?)", "(class ...)"),
        (
            "def self.m; end",
            // Parser always builds an `(args)` node, empty or not.
            "(defs (self) :m (args) nil?)",
            "(def :m nil? nil?)",
        ),
        ("def m; end", "any_def", "(defs ...)"),
        // Keyword forms.
        ("yield 1", "(yield (int 1))", "(super ...)"),
        ("super 1", "(super (int 1))", "(zsuper)"),
        ("super", "(zsuper)", "(super _)"),
        ("next 1", "(next (int 1))", "(break _)"),
        (
            "foo(**opts)",
            "(send nil? :foo (hash (kwsplat (send nil? :opts))))",
            "(send nil? :foo (hash (pair ...)))",
        ),
    ];

    #[test]
    fn test_mapping_table_cases() {
        for (source, must_match, must_not_match) in MAPPING_CASES {
            let result = ruby_prism::parse(source.as_bytes());
            let node = first_stmt(&result);
            assert!(
                interpret_pattern(must_match, &node),
                "`{must_match}` should match `{source}`"
            );
            assert!(
                !interpret_pattern(must_not_match, &node),
                "`{must_not_match}` should not match `{source}`"
            );
        }
    }

    #[test]
    fn test_block_node_reached_directly_has_no_send_child() {
        // Prism's `BlockNode` has no back-pointer to its call, so only `_` can
        // match the send child when the block node is matched on its own.
        let result = ruby_prism::parse(b"items.each { |x| x }");
        let node = first_stmt(&result);
        let block = node.as_call_node().unwrap().block().unwrap();

        assert!(interpret_pattern("(block _ _ _)", &block));
        assert!(!interpret_pattern("(block (send _ :each) _ _)", &block));
    }

    #[test]
    fn test_group_types_cover_their_members() {
        for (source, group) in [
            ("1", "numeric"),
            ("1.0", "numeric"),
            ("true", "boolean"),
            ("'a'", "any_str"),
            (":a", "any_sym"),
            ("foo.bar", "call"),
            ("def m; end", "any_def"),
        ] {
            let result = ruby_prism::parse(source.as_bytes());
            let node = first_stmt(&result);
            assert!(
                interpret_pattern(group, &node),
                "`{group}` should match `{source}`"
            );
            assert!(
                interpret_pattern(&format!("{group}_type?"), &node),
                "`{group}_type?` should match `{source}`"
            );
        }
    }

    #[test]
    fn test_captures_through_synthesized_children() {
        let result = ruby_prism::parse(b"/foo/i");
        let node = first_stmt(&result);
        let captures = match_with_captures("(regexp (str $_) (regopt $_))", &node).unwrap();
        assert_eq!(captured_name(&captures, 0), "foo");
        assert_eq!(captured_name(&captures, 1), "i");

        let result = ruby_prism::parse(b"items.each { _2 }");
        let node = first_stmt(&result);
        let captures = match_with_captures("(numblock $(send _ :each) $_ _)", &node).unwrap();
        assert_eq!(captured_src(&captures, 0), "items.each { _2 }");
        assert_eq!(captured_name(&captures, 1), "2");
    }
}
