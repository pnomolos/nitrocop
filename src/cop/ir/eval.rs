//! Evaluator for compiled [`Expr`] trees (design §3.1-3.2).
//!
//! Evaluation is total: every operator has a defined result for every operand
//! shape, and a type mismatch yields [`Value::Nil`] / `false` rather than a
//! panic — a panicking cop is worse than a missing one (§2.1).
//!
//! [`EvalCtx`] is per-node, stack-allocated and read-only apart from the
//! quantifier variable slots, which live in a `RefCell` because `eval` takes
//! `&EvalCtx`.
//!
//! ## Ancestors
//!
//! `ancestors` is `&[]` until `BatchedCopWalker` maintains an ancestor stack
//! (design §3.3, separate PR). Everything that reads it — `parent`,
//! `parent_type`, `over: ancestors`, `pred: root?`, `pred: value_used?` —
//! degrades to the "no enclosing node" answer, which is documented per
//! operator. Wiring it later is one field.

use std::borrow::Cow;
use std::cell::RefCell;

use ruby_prism::Visit;

use crate::node_pattern::captures::dup_node;
use crate::node_pattern::{Captures, NoResolver, Params, PredCtx, PredTarget, Resolver};
use crate::parse::source::SourceFile;

use super::expr::{
    Attr, CmpOp, Collection, CompiledDoc, Expr, Haystack, Intrinsic, Lit, MatcherRef, QuantKind,
    Target,
};
use super::schema::{ConstScalar, ConstValue};

/// A runtime value. Byte slices borrow from the parsed source or the compiled
/// document wherever possible.
#[derive(Debug)]
pub enum Value<'pr> {
    Bool(bool),
    Int(i64),
    Str(Cow<'pr, [u8]>),
    Sym(Cow<'pr, [u8]>),
    Node(ruby_prism::Node<'pr>),
    /// A `loc` part: a byte range with no node of its own, produced by
    /// `{ attr: [x, "loc", "dot"] }`. Only the position attributes read it.
    Loc(usize, usize),
    List(Vec<Value<'pr>>),
    Nil,
}

// `ruby_prism::Node` is a non-owning handle that implements neither `Copy` nor
// `Clone` (see `node_pattern::captures::dup_node`), so `Clone` is written out.
impl Clone for Value<'_> {
    fn clone(&self) -> Self {
        match self {
            Value::Bool(b) => Value::Bool(*b),
            Value::Int(i) => Value::Int(*i),
            Value::Str(bytes) => Value::Str(bytes.clone()),
            Value::Sym(bytes) => Value::Sym(bytes.clone()),
            Value::Node(node) => Value::Node(dup_node(node)),
            Value::Loc(start, end) => Value::Loc(*start, *end),
            Value::List(items) => Value::List(items.clone()),
            Value::Nil => Value::Nil,
        }
    }
}

impl<'pr> Value<'pr> {
    fn str(bytes: &'pr [u8]) -> Self {
        Value::Str(Cow::Borrowed(bytes))
    }

    /// Ruby truthiness: only `nil` and `false` are falsey.
    #[must_use]
    pub fn truthy(&self) -> bool {
        !matches!(self, Value::Nil | Value::Bool(false))
    }

    /// The byte range this value occupies: a node's own extent, or a `loc`
    /// part's. Everything else has none.
    #[must_use]
    pub fn span(&self) -> Option<(usize, usize)> {
        match self {
            Value::Node(node) => {
                Some((node.location().start_offset(), node.location().end_offset()))
            }
            Value::Loc(start, end) => Some((*start, *end)),
            _ => None,
        }
    }

    /// The node this value holds, if it is one.
    #[must_use]
    pub fn node(&self) -> Option<&ruby_prism::Node<'pr>> {
        match self {
            Value::Node(node) => Some(node),
            _ => None,
        }
    }

    /// Text form, for comparison, `lookup` keys and `regex`: a string or
    /// symbol's bytes, or a node's verbatim source.
    #[must_use]
    pub fn bytes(&self) -> Option<Cow<'pr, [u8]>> {
        match self {
            Value::Str(bytes) | Value::Sym(bytes) => Some(bytes.clone()),
            Value::Node(node) => Some(Cow::Borrowed(node.location().as_slice())),
            _ => None,
        }
    }
}

static EMPTY_PARAMS: std::sync::LazyLock<Params> = std::sync::LazyLock::new(Params::new);
static NO_RESOLVER: NoResolver = NoResolver;

/// Everything an expression may read while evaluating against one node.
pub struct EvalCtx<'a, 'pr> {
    node: ruby_prism::Node<'pr>,
    /// Enclosing nodes, innermost **last**. Empty until the walker carries one.
    pub ancestors: &'a [ruby_prism::Node<'pr>],
    /// The file, for `.source`, `.line` and `.column`.
    pub src: &'pr SourceFile,
    /// Captures bound by the hook's matcher.
    pub captures: Option<&'a Captures<'pr>>,
    /// Values of the hook's `bind:` entries, in declaration order.
    pub binds: &'a [Value<'pr>],
    /// Resolved `config:` values, indexed like `CompiledDoc::config_names`.
    pub config: &'pr [serde_yml::Value],
    /// The compiled document, for matchers, predicates, constants and regexes.
    pub doc: &'pr CompiledDoc,
    /// `%param` bindings and `#helper` resolution for `matches`.
    pub params: &'a Params,
    pub resolver: &'a dyn Resolver,
    vars: RefCell<Vec<Option<ruby_prism::Node<'pr>>>>,
}

impl<'a, 'pr> EvalCtx<'a, 'pr> {
    /// A context over `node` with no captures, binds, ancestors or config
    /// overrides; the `with_*` builders fill in what a hook actually has.
    #[must_use]
    pub fn new(node: &ruby_prism::Node<'pr>, src: &'pr SourceFile, doc: &'pr CompiledDoc) -> Self {
        Self {
            node: dup_node(node),
            ancestors: &[],
            src,
            captures: None,
            binds: &[],
            config: &doc.config_defaults,
            doc,
            params: &EMPTY_PARAMS,
            resolver: &NO_RESOLVER,
            vars: RefCell::new((0..doc.var_count).map(|_| None).collect()),
        }
    }

    /// The node the hook matched.
    #[must_use]
    pub fn node(&self) -> &ruby_prism::Node<'pr> {
        &self.node
    }

    #[must_use]
    pub fn with_ancestors(mut self, ancestors: &'a [ruby_prism::Node<'pr>]) -> Self {
        self.ancestors = ancestors;
        self
    }

    #[must_use]
    pub fn with_captures(mut self, captures: &'a Captures<'pr>) -> Self {
        self.captures = Some(captures);
        self
    }

    #[must_use]
    pub fn with_binds(mut self, binds: &'a [Value<'pr>]) -> Self {
        self.binds = binds;
        self
    }

    #[must_use]
    pub fn with_config(mut self, config: &'pr [serde_yml::Value]) -> Self {
        self.config = config;
        self
    }

    #[must_use]
    pub fn with_params(mut self, params: &'a Params, resolver: &'a dyn Resolver) -> Self {
        self.params = params;
        self.resolver = resolver;
        self
    }

    /// The same context re-pointed at `node`, with fresh quantifier slots.
    ///
    /// Used by `matches` against a named predicate, which is evaluated with
    /// `node` rebound to the sub-node. The ancestor slice is carried over
    /// unchanged: it describes the hook's node, so `parent` inside a predicate
    /// applied to a sub-node is an approximation until §3.3 lands.
    fn rebound(&self, node: ruby_prism::Node<'pr>) -> EvalCtx<'a, 'pr> {
        EvalCtx {
            node,
            ancestors: self.ancestors,
            src: self.src,
            captures: self.captures,
            binds: self.binds,
            config: self.config,
            doc: self.doc,
            params: self.params,
            resolver: self.resolver,
            vars: RefCell::new((0..self.doc.var_count).map(|_| None).collect()),
        }
    }

    fn parent(&self) -> Option<&ruby_prism::Node<'pr>> {
        self.ancestors.last()
    }
}

/// Evaluate `expr` against `ctx`.
#[must_use]
pub fn eval<'pr>(expr: &Expr, ctx: &EvalCtx<'_, 'pr>) -> Value<'pr> {
    match expr {
        Expr::Lit(lit) => match lit {
            Lit::Str(text) => Value::Str(Cow::Owned(text.clone().into_bytes())),
            Lit::Sym(text) => Value::Sym(Cow::Owned(text.clone().into_bytes())),
            Lit::Int(i) => Value::Int(*i),
            Lit::Bool(b) => Value::Bool(*b),
            Lit::Nil => Value::Nil,
        },
        Expr::Path(target) => path(*target, ctx),
        Expr::Attr { of, attr } => attr_of(&eval(of, ctx), *attr, ctx),
        Expr::Pred { of, pred, args } => {
            let value = eval(of, ctx);
            let target = match &value {
                Value::Node(node) => PredTarget::Node(node),
                Value::Nil => PredTarget::Absent,
                other => match other.bytes() {
                    Some(Cow::Borrowed(bytes)) => PredTarget::Name(bytes),
                    _ => return Value::Bool(false),
                },
            };
            Value::Bool((pred.eval)(&PredCtx::new(ctx.ancestors), &target, args))
        }
        Expr::Intrinsic { of, which } => Value::Bool(intrinsic(&eval(of, ctx), which, ctx)),
        Expr::Matches { of, matcher } => Value::Bool(matches(&eval(of, ctx), *matcher, ctx)),
        Expr::Regex { of, re } => {
            let Some(bytes) = eval(of, ctx).bytes() else {
                return Value::Bool(false);
            };
            let matched = std::str::from_utf8(&bytes)
                .ok()
                .is_some_and(|text| ctx.doc.regexes[*re].is_match(text));
            Value::Bool(matched)
        }
        Expr::All(items) => Value::Bool(items.iter().all(|item| eval(item, ctx).truthy())),
        Expr::Any(items) => Value::Bool(items.iter().any(|item| eval(item, ctx).truthy())),
        Expr::Not(inner) => Value::Bool(!eval(inner, ctx).truthy()),
        Expr::Cmp { op, lhs, rhs } => Value::Bool(compare(*op, &eval(lhs, ctx), &eval(rhs, ctx))),
        Expr::In { needle, haystack } => {
            let value = eval(needle, ctx);
            let held = match haystack {
                Haystack::Items(items) => items
                    .iter()
                    .any(|item| compare(CmpOp::Eq, &value, &eval(item, ctx))),
                // A set reference evaluates to a `List`; anything else (a
                // config key whose value is absent, say) holds nothing.
                Haystack::Set(set) => match eval(set, ctx) {
                    Value::List(items) => items.iter().any(|item| compare(CmpOp::Eq, &value, item)),
                    _ => false,
                },
            };
            Value::Bool(held)
        }
        Expr::If { cond, then, els } => {
            if eval(cond, ctx).truthy() {
                eval(then, ctx)
            } else {
                eval(els, ctx)
            }
        }
        Expr::Lookup { table, key } => {
            let Some(bytes) = eval(key, ctx).bytes() else {
                return Value::Nil;
            };
            let Ok(text) = std::str::from_utf8(&bytes) else {
                return Value::Nil;
            };
            // Only a map has values to look up; a list-valued table is a
            // membership set, so `lookup` on one is `nil`.
            match ctx.doc.consts.get(*table).and_then(|t| t.get(text)) {
                Some(ConstValue::Str(s)) => Value::str(s.as_bytes()),
                Some(ConstValue::Int(i)) => Value::Int(*i),
                Some(ConstValue::Bool(b)) => Value::Bool(*b),
                Some(ConstValue::List(items)) => {
                    Value::List(items.iter().map(|s| Value::str(s.as_bytes())).collect())
                }
                None => Value::Nil,
            }
        }
        Expr::Quant {
            kind,
            of,
            over,
            var,
            body,
        } => quantify(*kind, &eval(of, ctx), *over, *var, body, ctx),
    }
}

fn path<'pr>(target: Target, ctx: &EvalCtx<'_, 'pr>) -> Value<'pr> {
    match target {
        Target::Node => Value::Node(dup_node(&ctx.node)),
        Target::Parent => ctx
            .parent()
            .map_or(Value::Nil, |p| Value::Node(dup_node(p))),
        Target::Capture(slot) => match ctx.captures.and_then(|caps| caps.get(slot)) {
            Some(crate::node_pattern::CaptureValue::Node(node)) => Value::Node(dup_node(node)),
            Some(crate::node_pattern::CaptureValue::Name(bytes)) => Value::str(bytes),
            _ => Value::Nil,
        },
        Target::Bind(slot) => ctx.binds.get(slot).cloned().unwrap_or(Value::Nil),
        Target::Cfg(slot) => ctx.config.get(slot).map_or(Value::Nil, from_yaml),
        // A table as a value is the list of its members, which is what
        // `in: consts.NAME` tests against. (`lookup`'s first operand is
        // consumed by the compiler and never evaluated.)
        Target::Const(slot) => ctx.doc.consts.get(slot).map_or(Value::Nil, |table| {
            Value::List(
                table
                    .members()
                    .into_iter()
                    .map(|member| match member {
                        ConstScalar::Str(text) => Value::Sym(Cow::Owned(
                            text.strip_prefix(':').unwrap_or(&text).as_bytes().to_vec(),
                        )),
                        ConstScalar::Int(value) => Value::Int(value),
                        ConstScalar::Bool(value) => Value::Bool(value),
                    })
                    .collect(),
            )
        }),
        Target::Var(slot) => ctx.vars.borrow()[slot]
            .as_ref()
            .map_or(Value::Nil, |node| Value::Node(dup_node(node))),
    }
}

fn from_yaml<'pr>(value: &'pr serde_yml::Value) -> Value<'pr> {
    match value {
        serde_yml::Value::String(text) => Value::str(text.as_bytes()),
        serde_yml::Value::Bool(b) => Value::Bool(*b),
        serde_yml::Value::Number(n) => n.as_i64().map_or(Value::Nil, Value::Int),
        serde_yml::Value::Sequence(items) => Value::List(items.iter().map(from_yaml).collect()),
        _ => Value::Nil,
    }
}

fn attr_of<'pr>(value: &Value<'pr>, attr: Attr, ctx: &EvalCtx<'_, 'pr>) -> Value<'pr> {
    if attr == Attr::ParentType {
        // Sugar for `parent.type`.
        return ctx
            .parent()
            .and_then(crate::node_pattern::parser_type_name)
            .map_or(Value::Nil, |name| Value::str(name.as_bytes()));
    }
    // The position family reads a byte range, so it applies to a `loc` part
    // exactly as it does to a node. `last_line`/`last_column` are Parser's
    // `Range#last_line`/`#last_column`: the line and column of `end_pos`.
    if matches!(
        attr,
        Attr::Line | Attr::LastLine | Attr::Column | Attr::LastColumn
    ) {
        let Some((start, end)) = value.span() else {
            return Value::Nil;
        };
        let at_start = matches!(attr, Attr::Line | Attr::Column);
        let (line, column) = ctx
            .src
            .offset_to_line_col(if at_start { start } else { end });
        let wants_line = matches!(attr, Attr::Line | Attr::LastLine);
        return Value::Int(if wants_line { line } else { column } as i64);
    }
    let Some(node) = value.node() else {
        return Value::Nil;
    };
    let call = node.as_call_node();
    let args = || {
        call.as_ref()
            .and_then(ruby_prism::CallNode::arguments)
            .map(|list| list.arguments().iter().collect::<Vec<_>>())
            .unwrap_or_default()
    };
    match attr {
        Attr::MethodName => match (&call, node.as_def_node()) {
            (Some(call), _) => Value::Sym(Cow::Borrowed(call.name().as_slice())),
            (None, Some(def)) => Value::Sym(Cow::Borrowed(def.name().as_slice())),
            _ => Value::Nil,
        },
        Attr::Name => name_of(node),
        Attr::Receiver => call
            .as_ref()
            .and_then(ruby_prism::CallNode::receiver)
            .map_or(Value::Nil, Value::Node),
        Attr::Body => body_of(node),
        Attr::ArgCount => Value::Int(args().len() as i64),
        Attr::Arg(index) => args()
            .into_iter()
            .nth(index)
            .map_or(Value::Nil, Value::Node),
        Attr::FirstArgument => args().into_iter().next().map_or(Value::Nil, Value::Node),
        Attr::LastArgument => args()
            .into_iter()
            .next_back()
            .map_or(Value::Nil, Value::Node),
        Attr::Source => Value::str(node.location().as_slice()),
        Attr::Loc(part) => super::cop::part_loc(node, part).map_or(Value::Nil, |loc| {
            Value::Loc(loc.start_offset(), loc.end_offset())
        }),
        Attr::Value => literal_value(node),
        Attr::Type => crate::node_pattern::parser_type_name(node)
            .map_or(Value::Nil, |name| Value::str(name.as_bytes())),
        Attr::LeftSibling => sibling(node, ctx, -1),
        Attr::RightSibling => sibling(node, ctx, 1),
        Attr::FirstChild => descendants(node, 1)
            .into_iter()
            .next()
            .map_or(Value::Nil, Value::Node),
        Attr::LastChild => descendants(node, 1)
            .into_iter()
            .next_back()
            .map_or(Value::Nil, Value::Node),
        Attr::ParentType | Attr::Line | Attr::LastLine | Attr::Column | Attr::LastColumn => {
            unreachable!("handled above")
        }
    }
}

/// `Node#left_sibling` / `#right_sibling`, over the enclosing-node chain.
///
/// The chain is the only place the node's parent can come from, so the parent
/// is the innermost entry that actually has this node as a direct Parser-gem
/// child — which is `ancestors.last()` for `node` and one above it for
/// `parent`, without either having to say which it is. A node that is not on
/// the chain at all (a `$capture` from deeper in the match) has no reachable
/// parent and answers `nil`.
fn sibling<'pr>(node: &ruby_prism::Node<'pr>, ctx: &EvalCtx<'_, 'pr>, offset: isize) -> Value<'pr> {
    for parent in ctx.ancestors.iter().rev() {
        if crate::node_pattern::interpreter::is_parser_child(node, parent) {
            return crate::node_pattern::interpreter::parser_sibling(node, parent, offset)
                .map_or(Value::Nil, Value::Node);
        }
    }
    Value::Nil
}

fn name_of<'pr>(node: &ruby_prism::Node<'pr>) -> Value<'pr> {
    macro_rules! try_name {
        ($($accessor:ident),* $(,)?) => {
            $(if let Some(inner) = node.$accessor() {
                return Value::Sym(Cow::Borrowed(inner.name().as_slice()));
            })*
        };
    }
    try_name!(
        as_call_node,
        as_def_node,
        as_constant_read_node,
        as_local_variable_read_node,
        as_local_variable_write_node,
        as_instance_variable_read_node,
        as_instance_variable_write_node,
        as_class_variable_read_node,
        as_global_variable_read_node,
    );
    if let Some(symbol) = node.as_symbol_node() {
        return symbol
            .value_loc()
            .map_or(Value::Nil, |loc| Value::Sym(Cow::Borrowed(loc.as_slice())));
    }
    // The implicit `it` of a `{ it }` block is `(lvar :it)` to Parser, so its
    // name is `it`; Prism's node has no name accessor to read it from.
    if node.as_it_local_variable_read_node().is_some() {
        return Value::Sym(Cow::Borrowed(b"it"));
    }
    Value::Nil
}

fn body_of<'pr>(node: &ruby_prism::Node<'pr>) -> Value<'pr> {
    if let Some(def) = node.as_def_node() {
        return def.body().map_or(Value::Nil, Value::Node);
    }
    if let Some(block) = node.as_block_node() {
        return block.body().map_or(Value::Nil, Value::Node);
    }
    if let Some(class) = node.as_class_node() {
        return class.body().map_or(Value::Nil, Value::Node);
    }
    if let Some(module) = node.as_module_node() {
        return module.body().map_or(Value::Nil, Value::Node);
    }
    Value::Nil
}

fn literal_value<'pr>(node: &ruby_prism::Node<'pr>) -> Value<'pr> {
    // `unescaped()` borrows from the typed node handle rather than from the
    // parse arena, so the content location is used instead; it is the raw
    // source bytes, which is what every comparison in a `when:` wants.
    if let Some(string) = node.as_string_node() {
        return Value::Str(Cow::Borrowed(string.content_loc().as_slice()));
    }
    if let Some(symbol) = node.as_symbol_node() {
        return symbol
            .value_loc()
            .map_or(Value::Nil, |loc| Value::Sym(Cow::Borrowed(loc.as_slice())));
    }
    if node.as_integer_node().is_some() {
        return std::str::from_utf8(node.location().as_slice())
            .ok()
            .and_then(|text| text.replace('_', "").parse::<i64>().ok())
            .map_or(Value::Nil, Value::Int);
    }
    if node.as_true_node().is_some() {
        return Value::Bool(true);
    }
    if node.as_false_node().is_some() {
        return Value::Bool(false);
    }
    Value::Nil
}

fn compare(op: CmpOp, lhs: &Value<'_>, rhs: &Value<'_>) -> bool {
    use std::cmp::Ordering;
    let ordering = match (lhs, rhs) {
        (Value::Int(a), Value::Int(b)) => Some(a.cmp(b)),
        (Value::Bool(a), Value::Bool(b)) => Some(a.cmp(b)),
        (Value::Nil, Value::Nil) => Some(Ordering::Equal),
        // Two nodes (or two `loc` parts) compare by identity, i.e. by byte
        // range — which is what `==` on Parser nodes means (design §1.6).
        (Value::Node(_) | Value::Loc(..), Value::Node(_) | Value::Loc(..)) => {
            match (lhs.span(), rhs.span()) {
                (Some(a), Some(b)) => Some(a.cmp(&b)),
                _ => None,
            }
        }
        _ => match (lhs.bytes(), rhs.bytes()) {
            (Some(a), Some(b)) => Some(a.as_ref().cmp(b.as_ref())),
            _ => None,
        },
    };
    let Some(ordering) = ordering else {
        // Incomparable operands: only `ne` holds.
        return op == CmpOp::Ne;
    };
    match op {
        CmpOp::Eq => ordering == Ordering::Equal,
        CmpOp::Ne => ordering != Ordering::Equal,
        CmpOp::Lt => ordering == Ordering::Less,
        CmpOp::Le => ordering != Ordering::Greater,
        CmpOp::Gt => ordering == Ordering::Greater,
        CmpOp::Ge => ordering != Ordering::Less,
    }
}

fn intrinsic(value: &Value<'_>, which: &Intrinsic, ctx: &EvalCtx<'_, '_>) -> bool {
    let Some(node) = value.node() else {
        return false;
    };
    match which {
        Intrinsic::Type(types) => types
            .iter()
            .any(|ty| crate::node_pattern::node_answers_to_type(node, ty)),
        Intrinsic::Root => ctx.ancestors.is_empty(),
        Intrinsic::ValueUsed => value_used(node, ctx.ancestors),
    }
}

/// `RuboCop::AST::Node#value_used?` (`rubocop-ast` `node.rb:647-667`, with
/// `begin_value_used?` at `704-707`), read off the enclosing-node chain.
///
/// Upstream is **recursive**: a statement that is not the last of its `begin`
/// has its value discarded, and the last one inherits the `begin`'s own answer.
/// A top-level statement list has no parent, so upstream's `return false if
/// parent.nil?` makes even the *trailing* top-level statement unused — which is
/// why `File.open('f')` on a line by itself is a `Style/FileOpen` offense.
///
/// Prism's extra levels are walked through rather than answered at:
///
/// * `StatementsNode` is the `begin` level, whether or not it is Parser-visible
///   (a one-statement list is trivially "last", so the rule degenerates
///   correctly);
/// * `ParenthesesNode` / `BeginNode` / `EmbeddedStatementsNode` are the
///   `begin` / `kwbegin` / `dstr` *spelling* of a list whose statements level
///   was just checked, so they carry the question one level further up, exactly
///   as upstream's `parent.value_used?` does;
/// * `ProgramNode` is the parentless root: upstream has no node there at all.
///
/// Everything else falls into upstream's `else` branch and is assumed used.
/// That keeps the reading conservative in the same direction as before for the
/// container types (`array`, `if`, `while`, …) upstream resolves recursively.
fn value_used(node: &ruby_prism::Node<'_>, ancestors: &[ruby_prism::Node<'_>]) -> bool {
    let mut start = node.location().start_offset();
    for parent in ancestors.iter().rev() {
        if let Some(statements) = parent.as_statements_node() {
            let is_last = statements
                .body()
                .iter()
                .last()
                .is_some_and(|last: ruby_prism::Node<'_>| last.location().start_offset() == start);
            if !is_last {
                return false;
            }
        } else if parent.as_program_node().is_some() {
            return false;
        } else if parent.as_parentheses_node().is_none()
            && parent.as_begin_node().is_none()
            && parent.as_embedded_statements_node().is_none()
        {
            return true;
        }
        start = parent.location().start_offset();
    }
    false
}

fn matches<'pr>(value: &Value<'pr>, matcher: MatcherRef, ctx: &EvalCtx<'_, 'pr>) -> bool {
    match matcher {
        MatcherRef::Pattern(index) => {
            let Some(pattern) = ctx.doc.matchers.get(index) else {
                return false;
            };
            let target = match value {
                Value::Node(node) => PredTarget::Node(node),
                Value::Nil => PredTarget::Absent,
                other => match other.bytes() {
                    Some(Cow::Borrowed(bytes)) => PredTarget::Name(bytes),
                    _ => return false,
                },
            };
            // The chain describes the hook's node; a `matches:` applied to a
            // sub-node therefore sees a `^` one level too shallow. Same
            // approximation `EvalCtx::rebound` documents, and the same one
            // upstream has no equivalent of because its `#helper`s are methods
            // on a node that carries its own parent pointer.
            pattern.matches_target(&target, ctx.ancestors, ctx.params, ctx.resolver)
        }
        MatcherRef::Predicate(index) => {
            let (Some(node), Some(expr)) = (value.node(), ctx.doc.predicates.get(index)) else {
                return false;
            };
            eval(expr, &ctx.rebound(dup_node(node))).truthy()
        }
    }
}

fn quantify<'pr>(
    kind: QuantKind,
    subject: &Value<'pr>,
    over: Collection,
    var: usize,
    body: &Expr,
    ctx: &EvalCtx<'_, 'pr>,
) -> Value<'pr> {
    let items: Vec<ruby_prism::Node<'pr>> = match over {
        Collection::Ancestors => ctx.ancestors.iter().rev().map(dup_node).collect(),
        Collection::Args => subject
            .node()
            .and_then(ruby_prism::Node::as_call_node)
            .and_then(|call| call.arguments())
            .map(|list| list.arguments().iter().collect())
            .unwrap_or_default(),
        Collection::Descendants(depth) => subject
            .node()
            .map(|node| descendants(node, depth))
            .unwrap_or_default(),
    };
    let mut count = 0usize;
    let mut any = false;
    let mut all = true;
    for item in items {
        ctx.vars.borrow_mut()[var] = Some(item);
        let held = eval(body, ctx).truthy();
        any |= held;
        all &= held;
        count += usize::from(held);
        if matches!(kind, QuantKind::AnyOf) && any {
            break;
        }
        if matches!(kind, QuantKind::AllOf | QuantKind::NoneOf) && !all {
            break;
        }
    }
    ctx.vars.borrow_mut()[var] = None;
    match kind {
        QuantKind::AnyOf => Value::Bool(any),
        QuantKind::AllOf => Value::Bool(all),
        QuantKind::NoneOf => Value::Bool(!any),
        QuantKind::Count => Value::Int(count as i64),
    }
}

/// Every node at most `max_depth` levels below `node`, pre-order.
fn descendants<'pr>(node: &ruby_prism::Node<'pr>, max_depth: u8) -> Vec<ruby_prism::Node<'pr>> {
    struct Collector<'pr> {
        depth: usize,
        max: usize,
        out: Vec<ruby_prism::Node<'pr>>,
    }

    impl<'pr> Visit<'pr> for Collector<'pr> {
        fn visit_branch_node_enter(&mut self, node: ruby_prism::Node<'pr>) {
            self.depth += 1;
            if (2..=self.max + 1).contains(&self.depth) {
                self.out.push(node);
            }
        }

        fn visit_branch_node_leave(&mut self) {
            self.depth -= 1;
        }

        fn visit_leaf_node_enter(&mut self, node: ruby_prism::Node<'pr>) {
            if (1..=self.max).contains(&self.depth) {
                self.out.push(node);
            }
        }
    }

    let mut collector = Collector {
        depth: 0,
        max: max_depth as usize,
        out: Vec::new(),
    };
    collector.visit(node);
    collector.out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cop::ir::load_str;
    use std::path::PathBuf;

    /// One table row: a Ruby snippet, the matcher that finds the subject node,
    /// the `when:` guard, and the boolean it must evaluate to.
    struct Case {
        ruby: &'static str,
        pattern: &'static str,
        when: &'static str,
        /// Extra top-level document sections (`constants:`, `config:`,
        /// `predicates:`, a second matcher), verbatim.
        extra: &'static str,
        /// `captures:` list for the matcher, e.g. `"[arg]"`.
        captures: &'static str,
        /// Extra `matchers:` entries, appended after the probe matcher.
        more: &'static str,
        expect: bool,
    }

    impl Case {
        const fn new(ruby: &'static str, pattern: &'static str, when: &'static str) -> Self {
            Self {
                ruby,
                pattern,
                when,
                extra: "",
                captures: "[]",
                more: "",
                expect: true,
            }
        }

        const fn more(mut self, more: &'static str) -> Self {
            self.more = more;
            self
        }

        const fn extra(mut self, extra: &'static str) -> Self {
            self.extra = extra;
            self
        }

        const fn captures(mut self, captures: &'static str) -> Self {
            self.captures = captures;
            self
        }

        const fn falsey(mut self) -> Self {
            self.expect = false;
            self
        }

        fn document(&self) -> String {
            format!(
                "schema: 1\ncop: \"Custom/EvalProbe\"\n{}matchers:\n  probe:\n    pattern: |\n      {}\n    captures: {}\n{}hooks:\n  - on: [send]\n    match: probe\n    when: {}\n    offense:\n      location: node\n      message: \"probe\"\n",
                self.extra, self.pattern, self.captures, self.more, self.when
            )
        }
    }

    /// The first node the matcher accepts, plus the ancestor stack above it.
    ///
    /// This is the stand-in for design §3.3's walker ancestor stack: it proves
    /// [`EvalCtx::ancestors`] is all the evaluator needs from the walker.
    struct Finder<'a, 'pr> {
        pattern: &'a crate::node_pattern::CompiledPattern,
        stack: Vec<ruby_prism::Node<'pr>>,
        hit: Option<(ruby_prism::Node<'pr>, Vec<ruby_prism::Node<'pr>>)>,
    }

    impl<'pr> Finder<'_, 'pr> {
        fn probe(&mut self, node: &ruby_prism::Node<'pr>) {
            if self.hit.is_none() && self.pattern.matches(node) {
                self.hit = Some((
                    dup_node(node),
                    self.stack.iter().map(dup_node).collect::<Vec<_>>(),
                ));
            }
        }
    }

    impl<'pr> Visit<'pr> for Finder<'_, 'pr> {
        fn visit_branch_node_enter(&mut self, node: ruby_prism::Node<'pr>) {
            self.probe(&node);
            self.stack.push(node);
        }

        fn visit_branch_node_leave(&mut self) {
            self.stack.pop();
        }

        fn visit_leaf_node_enter(&mut self, node: ruby_prism::Node<'pr>) {
            self.probe(&node);
        }
    }

    fn run(case: &Case) -> Value<'static> {
        let cop = load_str(&case.document(), "probe.cop.yml")
            .unwrap_or_else(|e| panic!("document should load: {e}\n{}", case.document()));
        // Leaked so the parse result, the source and the compiled document all
        // share one lifetime; a test process is short-lived.
        let cop: &'static _ = Box::leak(Box::new(cop));
        let src: &'static SourceFile = Box::leak(Box::new(SourceFile::from_bytes(
            "probe.rb",
            case.ruby.as_bytes().to_vec(),
        )));
        let parsed: &'static _ = Box::leak(Box::new(ruby_prism::parse(src.as_bytes())));

        // `matchers:` is a BTreeMap, so the probe is not necessarily index 0.
        let probe = cop.compiled.matcher_id("probe").expect("probe matcher");
        let mut finder = Finder {
            pattern: &cop.compiled.matchers[probe],
            stack: Vec::new(),
            hit: None,
        };
        finder.visit(&parsed.node());
        let (node, ancestors) = finder
            .hit
            .unwrap_or_else(|| panic!("`{}` matched nothing in `{}`", case.pattern, case.ruby));
        let ancestors: &'static [ruby_prism::Node<'static>] = ancestors.leak();
        let captures: &'static _ = Box::leak(Box::new(
            cop.compiled.matchers[probe]
                .match_captures(&node)
                .expect("the pattern matched above"),
        ));

        // The runtime hands the matcher the document's own resolver, so a
        // `matches:` sees `#helper` and `%TABLE` exactly as the loader did.
        let resolver: &'static _ =
            Box::leak(Box::new(crate::cop::ir::expr::DocResolver(&cop.compiled)));
        let ctx = EvalCtx::new(&node, src, &cop.compiled)
            .with_ancestors(ancestors)
            .with_captures(captures)
            .with_params(&EMPTY_PARAMS, resolver);
        let when = cop.compiled.hooks[0]
            .when
            .as_ref()
            .expect("every case declares a `when:`");
        eval(when, &ctx)
    }

    #[test]
    fn operators_evaluate() {
        const TIME_NEW: &str = "(send (const nil? :Time) :new)";
        const FOO_2: &str = "(send nil? :foo ...)";
        const FILE_OPEN: &str = "(send (const nil? :File) :open ...)";
        const VALUE_USED: &str = "{ pred: [node, \"value_used?\"] }";
        let cases = [
            // --- logic ---------------------------------------------------
            Case::new("Time.new", TIME_NEW, "{ all: [true, true] }"),
            Case::new("Time.new", TIME_NEW, "{ all: [true, false] }").falsey(),
            Case::new("Time.new", TIME_NEW, "{ any: [false, true] }"),
            Case::new("Time.new", TIME_NEW, "{ any: [false, false] }").falsey(),
            Case::new("Time.new", TIME_NEW, "{ not: false }"),
            // --- comparison ----------------------------------------------
            Case::new("Time.new", TIME_NEW, "{ eq: [node.method_name, \":new\"] }"),
            Case::new("Time.new", TIME_NEW, "{ ne: [node.method_name, \":now\"] }"),
            Case::new("foo(1, 2)", FOO_2, "{ lt: [node.arg_count, 3] }"),
            Case::new("foo(1, 2)", FOO_2, "{ le: [node.arg_count, 2] }"),
            Case::new("foo(1, 2)", FOO_2, "{ gt: [node.arg_count, 1] }"),
            Case::new("foo(1, 2)", FOO_2, "{ ge: [node.arg_count, 2] }"),
            Case::new(
                "Time.new",
                TIME_NEW,
                "{ in: [node.method_name, [\":now\", \":new\"]] }",
            ),
            Case::new(
                "Time.new",
                TIME_NEW,
                "{ in: [node.method_name, [\":now\", \":utc\"]] }",
            )
            .falsey(),
            // `in:` over a whole set: a list-valued constant, a map-valued one
            // (its keys), and a `string_array` config key.
            Case::new("Time.new", TIME_NEW, "{ in: [node.method_name, consts.METHODS] }")
                .extra("constants:\n  METHODS: [\":new\", \":now\"]\n"),
            Case::new("Time.new", TIME_NEW, "{ in: [node.method_name, consts.METHODS] }")
                .extra("constants:\n  METHODS: [utc, now]\n")
                .falsey(),
            Case::new("Time.new", TIME_NEW, "{ in: [node.method_name, consts.METHODS] }")
                .extra("constants:\n  METHODS: { new: \"now\" }\n"),
            Case::new("Time.new", TIME_NEW, "{ in: [node.method_name, cfg.Allowed] }")
                .extra("config:\n  Allowed: { type: string_array, default: [\"new\"] }\n"),
            Case::new("Time.new", TIME_NEW, "{ in: [node.method_name, cfg.Allowed] }")
                .extra("config:\n  Allowed: { type: string_array, default: [] }\n")
                .falsey(),
            // A list-valued constant is also a `%NAME` membership set, which
            // is how upstream's `%i[…].to_set.freeze` constants read.
            Case::new("x.is_a?(Integer)", "(send _ _ _)", "{ matches: [node, \"kindish\"] }")
                .more("  kindish:\n    pattern: \"(send _ %KIND_METHODS _)\"\n")
                .extra("constants:\n  KIND_METHODS: [\"is_a?\", \"kind_of?\"]\n"),
            Case::new("x.foo(Integer)", "(send _ _ _)", "{ matches: [node, \"kindish\"] }")
                .more("  kindish:\n    pattern: \"(send _ %KIND_METHODS _)\"\n")
                .extra("constants:\n  KIND_METHODS: [\"is_a?\", \"kind_of?\"]\n")
                .falsey(),
            // --- values ---------------------------------------------------
            Case::new("Time.new", TIME_NEW, "{ if: [true, true, false] }"),
            Case::new("Time.new", TIME_NEW, "{ if: [false, true, false] }").falsey(),
            // `lit` forces a string that would otherwise read as a reference.
            Case::new(
                "Time.new",
                TIME_NEW,
                "{ eq: [{ lit: \"node\" }, { lit: \"node\" }] }",
            ),
            Case::new(
                "Time.new",
                TIME_NEW,
                "{ eq: [{ lookup: [consts.replacements, node.method_name] }, \"now\"] }",
            )
            .extra("constants:\n  replacements: { new: \"now\" }\n"),
            Case::new(
                "Time.new",
                TIME_NEW,
                "{ eq: [cfg.EnforcedStyle, \"is_a?\"] }",
            )
            .extra(
                "config:\n  EnforcedStyle: { type: enum, values: [\"is_a?\", \"kind_of?\"], default: \"is_a?\" }\n",
            ),
            // --- predicates / matchers ------------------------------------
            Case::new("foo { }", "(send nil? :foo)", "{ pred: [node, \"block_literal?\"] }"),
            Case::new("foo(&blk)", "(send nil? :foo ...)", "{ pred: [node, \"block_argument?\"] }"),
            Case::new("Time.new", TIME_NEW, "{ pred: [node, \"send_type?\"] }"),
            Case::new("Time.new", TIME_NEW, "{ pred: [node, \"type?\", \"send\", \"csend\"] }"),
            Case::new("Time.new", TIME_NEW, "{ pred: [node, \"root?\"] }").falsey(),
            // `value_used?` mirrors `begin_value_used?`'s recursion: a
            // statement list has no parent at the top level, so even its
            // *last* statement is unused.
            Case::new("File.open('f')", FILE_OPEN, VALUE_USED).falsey(),
            Case::new("File.open('f')\n1\n", FILE_OPEN, VALUE_USED).falsey(),
            Case::new("1\nFile.open('f')\n", FILE_OPEN, VALUE_USED).falsey(),
            Case::new("x = File.open('f')", FILE_OPEN, VALUE_USED),
            Case::new("foo(File.open('f'))", FILE_OPEN, VALUE_USED),
            Case::new("def m; File.open('f'); end", FILE_OPEN, VALUE_USED),
            Case::new("def m; 1; File.open('f'); end", FILE_OPEN, VALUE_USED),
            Case::new("def m; File.open('f'); 1; end", FILE_OPEN, VALUE_USED).falsey(),
            // `(a; b)` spells one Parser `begin`; the statements level inside
            // it still decides, and the parentheses carry the question up.
            Case::new("x = (File.open('f'); 1)", FILE_OPEN, VALUE_USED).falsey(),
            Case::new("x = (1; File.open('f'))", FILE_OPEN, VALUE_USED),
            Case::new(
                "Time.new",
                TIME_NEW,
                "{ matches: [node.receiver, \"konst\"] }",
            )
            .more("  konst:\n    pattern: \"(const nil? :Time)\"\n"),
            Case::new("Time.new", TIME_NEW, "{ matches: [node, \"is_new\"] }")
                .extra("predicates:\n  is_new:\n    expr: { eq: [node.method_name, \":new\"] }\n"),
            // `%CONST` inside a matcher resolves against the document's own
            // `constants:` — upstream's `%KIND_METHODS` spelling, verbatim.
            Case::new("x.is_a?(Integer)", "(send _ _ _)", "{ matches: [node, \"kindish\"] }")
                .more("  kindish:\n    pattern: \"(send _ %KIND_METHODS _)\"\n")
                .extra("constants:\n  KIND_METHODS: { \"is_a?\": true, \"kind_of?\": true }\n"),
            Case::new("x.foo(Integer)", "(send _ _ _)", "{ matches: [node, \"kindish\"] }")
                .more("  kindish:\n    pattern: \"(send _ %KIND_METHODS _)\"\n")
                .extra("constants:\n  KIND_METHODS: { \"is_a?\": true, \"kind_of?\": true }\n")
                .falsey(),
            // Prism's `ItLocalVariableReadNode` is Parser's `(lvar :it)`.
            Case::new("array.max_by { it }", "(itblock _ _ $_)", "{ eq: [$v.name, \":it\"] }")
                .captures("[v]"),
            Case::new("Time.new", TIME_NEW, "{ regex: [node.source, \"\\\\ATime\"] }"),
            Case::new("Time.new", TIME_NEW, "{ regex: [node.source, \"^time\", \"i\"] }"),
            Case::new("Time.new", TIME_NEW, "{ regex: [node.source, \"\\\\ADate\"] }").falsey(),
            // --- bounded quantifiers --------------------------------------
            Case::new(
                "foo(1, 2)",
                FOO_2,
                "{ any_of: { over: args, var: a, body: { eq: [a.source, \"2\"] } } }",
            ),
            Case::new(
                "foo(1, 2)",
                FOO_2,
                "{ all_of: { over: args, var: a, body: { eq: [a.type, \"int\"] } } }",
            ),
            Case::new(
                "foo(1, 2)",
                FOO_2,
                "{ none_of: { over: args, var: a, body: { eq: [a.source, \"3\"] } } }",
            ),
            Case::new(
                "foo(1, 2)",
                FOO_2,
                "{ eq: [{ count: { over: args, var: a, body: true } }, 2] }",
            ),
            Case::new(
                "def m; Time.new; end",
                TIME_NEW,
                "{ any_of: { over: ancestors, var: a, body: { eq: [a.type, \"def\"] } } }",
            ),
            // Prism's `ArgumentsNode` wrapper is traversed transparently, so
            // `children` of a call are its receiver, arguments and block.
            Case::new(
                "foo(1, 2)",
                FOO_2,
                "{ any_of: { over: children, var: c, body: { eq: [c.source, \"1\"] } } }",
            ),
            Case::new(
                "foo(bar(2))",
                FOO_2,
                "{ any_of: { over: \"descendants(2)\", var: d, body: { eq: [d.source, \"2\"] } } }",
            ),
            Case::new(
                "foo(bar(2))",
                FOO_2,
                "{ any_of: { over: \"descendants(1)\", var: d, body: { eq: [d.source, \"2\"] } } }",
            )
            .falsey(),
        ];
        for case in &cases {
            let got = run(case);
            assert_eq!(
                got.truthy(),
                case.expect,
                "`{}` over `{}` evaluated to {got:?}",
                case.when,
                case.ruby
            );
        }
    }

    #[test]
    fn attributes_evaluate() {
        const TIME_NEW: &str = "(send (const nil? :Time) :new)";
        const FOO_2: &str = "(send nil? :foo ...)";
        let cases = [
            Case::new("Time.new", TIME_NEW, "{ eq: [node.method_name, \":new\"] }"),
            Case::new("Time.new", TIME_NEW, "{ eq: [node.name, \":new\"] }"),
            Case::new(
                "Time.new",
                TIME_NEW,
                "{ eq: [node.receiver.name, \":Time\"] }",
            ),
            Case::new(
                "Time.new",
                TIME_NEW,
                "{ eq: [node.receiver.type, \"const\"] }",
            ),
            Case::new("def m; foo; end", "(def ...)", "{ ne: [node.body, null] }"),
            Case::new("foo(1, 2)", FOO_2, "{ eq: [node.arg_count, 2] }"),
            Case::new(
                "foo(1, 2)",
                FOO_2,
                "{ eq: [{ attr: [node, \"arg\", 1] }, \"2\"] }",
            ),
            Case::new(
                "foo(1, 2)",
                FOO_2,
                "{ eq: [node.first_argument.source, \"1\"] }",
            ),
            Case::new(
                "foo(1, 2)",
                FOO_2,
                "{ eq: [node.last_argument.source, \"2\"] }",
            ),
            Case::new("Time.new", TIME_NEW, "{ eq: [node.source, \"Time.new\"] }"),
            Case::new("\nTime.new", TIME_NEW, "{ eq: [node.line, 2] }"),
            Case::new("  Time.new", TIME_NEW, "{ eq: [node.column, 2] }"),
            Case::new(
                "foo(\"abc\")",
                "(send nil? :foo $(str _))",
                "{ eq: [$arg.value, \"abc\"] }",
            )
            .captures("[arg]"),
            Case::new(
                "foo(42)",
                "(send nil? :foo $(int _))",
                "{ eq: [$arg.value, 42] }",
            )
            .captures("[arg]"),
            Case::new("Time.new", TIME_NEW, "{ eq: [node.type, \"send\"] }"),
            // --- positions, on a node and on a `loc` part -----------------
            Case::new(
                "array\n  .map(&:to_s)\n  .join\n",
                "(send _ :join)",
                "{ eq: [node.receiver.last_line, 2] }",
            ),
            Case::new("Time.new", TIME_NEW, "{ eq: [node.last_line, 1] }"),
            Case::new("  Time.new", TIME_NEW, "{ eq: [node.last_column, 10] }"),
            // `Style/MapJoin`'s `receiver.last_line < map_send.loc.dot.line`:
            // the dot is on the line after the receiver ends.
            Case::new(
                "array\n  .map(&:to_s)\n",
                "(send _ :map ...)",
                "{ lt: [node.receiver.last_line, node.loc.dot.line] }",
            ),
            Case::new(
                "array.map(&:to_s)\n",
                "(send _ :map ...)",
                "{ lt: [node.receiver.last_line, node.loc.dot.line] }",
            )
            .falsey(),
            Case::new(
                "array\n  .map(&:to_s)\n",
                "(send _ :map ...)",
                "{ eq: [node.loc.dot.column, 2] }",
            ),
            // The `attr` operator spelling of the same step.
            Case::new(
                "array.map(&:to_s)\n",
                "(send _ :map ...)",
                "{ eq: [{ attr: [{ attr: [node, \"loc\", \"selector\"] }, \"column\"] }, 6] }",
            ),
            // A part the node does not have, and a part of a non-node: both nil.
            Case::new("Time.new", TIME_NEW, "{ eq: [node.loc.keyword, null] }"),
            Case::new(
                "Time.new",
                TIME_NEW,
                "{ eq: [node.method_name.line, null] }",
            ),
            Case::new(
                "x = Time.new",
                TIME_NEW,
                "{ eq: [node.parent_type, \"lvasgn\"] }",
            ),
            Case::new(
                "Time.new",
                TIME_NEW,
                "{ eq: [node.first_child.source, \"Time\"] }",
            ),
            Case::new(
                "a + b",
                "(send (call nil? :a) :+ ...)",
                "{ eq: [node.last_child.source, \"b\"] }",
            ),
        ];
        for case in &cases {
            let got = run(case);
            assert!(
                got.truthy() == case.expect,
                "`{}` over `{}` evaluated to {got:?}",
                case.when,
                case.ruby
            );
        }
    }

    /// Design §1.6 example 3, end to end: match with the real NodePattern
    /// engine, then evaluate the shipped `when:` against the match.
    #[test]
    fn file_open_guard_end_to_end() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/ir/valid/file_open.cop.yml");
        let yaml = std::fs::read_to_string(&path).unwrap();
        let pattern = "(send (const {nil? cbase} :File) :open ...)";
        // The guard is lifted byte-identically out of the shipped fixture and
        // re-wrapped in a probe document, so this exercises the real §1.6 YAML.
        let guard =
            &yaml[yaml.find("when:").unwrap() + "when:".len()..yaml.find("    offense:").unwrap()];

        for (ruby, expect) in [
            ("x = File.open(\"f\")", true),
            ("File.open(\"f\", &blk)", false),
        ] {
            let document = format!(
                "schema: 1\ncop: \"Style/FileOpen\"\nmatchers:\n  file_open:\n    pattern: |\n      {pattern}\nhooks:\n  - on: [send]\n    match: file_open\n    when:{}\n    offense:\n      location: node\n      message: \"probe\"\n",
                guard.trim_end()
            );
            let case = Case {
                ruby: Box::leak(ruby.to_string().into_boxed_str()),
                pattern,
                when: "",
                extra: "",
                captures: "[]",
                more: "",
                expect,
            };
            let cop = load_str(&document, "file_open.cop.yml").unwrap();
            let cop: &'static _ = Box::leak(Box::new(cop));
            let src: &'static SourceFile = Box::leak(Box::new(SourceFile::from_bytes(
                "probe.rb",
                case.ruby.as_bytes().to_vec(),
            )));
            let parsed: &'static _ = Box::leak(Box::new(ruby_prism::parse(src.as_bytes())));
            let mut finder = Finder {
                pattern: &cop.compiled.matchers[0],
                stack: Vec::new(),
                hit: None,
            };
            finder.visit(&parsed.node());
            let (node, ancestors) = finder.hit.expect("File.open should match");
            let ancestors: &'static [ruby_prism::Node<'static>] = ancestors.leak();
            let ctx = EvalCtx::new(&node, src, &cop.compiled).with_ancestors(ancestors);
            let when = cop.compiled.hooks[0].when.as_ref().unwrap();
            assert_eq!(
                eval(when, &ctx).truthy(),
                expect,
                "FileOpen guard over `{ruby}`"
            );
        }
    }
}
