//! Typed expression tree for `when:`, `bind:` and `predicates[].expr`, and the
//! compiler that produces it from the YAML surface syntax ([`ExprSyntax`]).
//!
//! This is design §2.1 made real. The language is **total by construction**:
//!
//! * no recursion — named `predicates:` may reference each other through
//!   `matches`, and that reference graph is validated to be a DAG
//!   ([`IrErrorKind::Cycle`]);
//! * no user-defined functions — every callable name resolves at compile time
//!   to a [`crate::node_pattern::predicates`] builtin, an [`Intrinsic`], a
//!   compiled NodePattern, or a compiled predicate expression;
//! * the only iteration is the bounded quantifier family (`any_of`, `all_of`,
//!   `none_of`, `count`), whose `over:` collection is a finite slice of a
//!   finite AST and whose `descendants` form carries an explicit depth cap;
//! * operator nesting is capped at [`MAX_EXPR_DEPTH`].
//!
//! Everything a runtime evaluation needs is resolved to an index here, so the
//! compiled tree is immutable, allocation-free to traverse, and `Send + Sync`.
//!
//! ## Reference resolution
//!
//! The loader used to walk the untyped YAML and check that `$capture`,
//! `cfg.Key`, `bind.Name` and `consts.Table` heads resolved. Those guarantees
//! now live in [`CompileCtx`], which carries the visible capture names, the
//! binds declared *so far* (so a bind may only reference an earlier one), and
//! the document's config/constant declarations.
//!
//! ## Matcher compilation order
//!
//! `matchers:` compile in **two passes**: every name is declared, then every
//! pattern is compiled against that declaration set. Compilation only asks
//! whether a `#helper` name is *known* ([`crate::node_pattern::collect_unresolved`]),
//! and matching resolves it against the finished table through
//! [`DocResolver`], so a matcher may refer to one declared later, to a
//! mutually recursive partner, or to itself. That is upstream's own rule —
//! `def_node_matcher` defines methods on the cop class, and a method body may
//! name any other — and `Style/RedundantStructKeywordInit`'s
//! `keyword_init?` = `{#redundant_keyword_init? #keyword_init_false?}` is the
//! shape a single pass could not express, because `?` sorts before `_`.
//!
//! What that gives up is the acyclicity a single pass enforced for free. A
//! matcher that recurses without descending (`a: "#a"`) overflows the stack at
//! match time, exactly as upstream's method would; a matcher that recurses
//! through `^`, or into a child, terminates because the chain and the subtree
//! are finite. `predicates:` keep the DAG rule — their cycles are still
//! rejected at load ([`IrErrorKind::Cycle`]) — because a predicate expression
//! has no such descending step to make the recursion well-founded.

use regex::RegexBuilder;

use crate::node_pattern::{Arg, Arity, CompiledPattern, Resolver, predicates};

use super::load::{IrError, IrErrorKind};
use super::schema::{ConfigType, ConstScalar, ConstTable, ExprSyntax, IrDocument, MAX_EXPR_DEPTH};

/// Largest `descendants(N)` depth an expression may ask for.
pub const MAX_DESCEND_DEPTH: u8 = 8;

/// Attribute vocabulary. Documented in `docs/COP_IR.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attr {
    /// `.method_name` — callee name of a call, or a definition's name.
    MethodName,
    /// `.name` — the node's own name (const, def, variable, call).
    Name,
    /// `.receiver` — a call's receiver, else `Nil`.
    Receiver,
    /// `.body` — a def/block/class/module body, else `Nil`.
    Body,
    /// `.arg_count` — number of call arguments.
    ArgCount,
    /// `{attr: [x, "arg", N]}` — the Nth call argument, else `Nil`.
    Arg(usize),
    /// `.first_argument`.
    FirstArgument,
    /// `.last_argument`.
    LastArgument,
    /// `.source` — the node's verbatim source text.
    Source,
    /// `.line` — 1-based line of the start of the node or loc part.
    Line,
    /// `.last_line` — 1-based line of its end (Parser's `Range#last_line`).
    LastLine,
    /// `.column` — 0-based column of its start.
    Column,
    /// `.last_column` — 0-based column of its end.
    LastColumn,
    /// `.loc.<part>` — one of the node's `loc` sub-ranges, as a location
    /// value. The same part vocabulary anchors use, so
    /// `{ attr: [node.loc.dot, line] }` and `node.loc.dot.start` name the
    /// same range.
    Loc(LocPart),
    /// `.value` — a literal node's value (string/symbol/int/bool/nil).
    Value,
    /// `.type` — Parser-gem type name.
    Type,
    /// `.parent_type` — Parser-gem type name of the enclosing node.
    ParentType,
    /// `.left_sibling` / `.right_sibling` — the adjacent Parser-gem child of
    /// this node's own parent, or `Nil` when there is none (or when it is a
    /// name rather than a node, as a `send`'s method name is).
    LeftSibling,
    RightSibling,
    /// `.first_child` / `.last_child` — direct children, in source order.
    FirstChild,
    /// See [`Attr::FirstChild`].
    LastChild,
}

/// The `node.loc.<part>` vocabulary, shared by anchors (`load.rs`'s
/// `anchor_segments`, `cop.rs`'s `parse_anchor`/`part_loc`) and by expressions
/// (`{ attr: [x, "loc", "dot"] }` / `x.loc.dot`). One list, so a part that
/// resolves in an anchor resolves in a guard and vice versa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocPart {
    Expression,
    Selector,
    Dot,
    Keyword,
    EndKeyword,
    Operator,
    Begin,
    End,
}

impl LocPart {
    /// Every part name, for diagnostics.
    pub const NAMES: &'static [&'static str] = &[
        "expression",
        "selector",
        "dot",
        "keyword",
        "end_keyword",
        "operator",
        "begin",
        "end",
    ];

    /// The part a `loc` path segment names, if any.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "expression" => Self::Expression,
            "selector" => Self::Selector,
            "dot" => Self::Dot,
            "keyword" => Self::Keyword,
            "end_keyword" => Self::EndKeyword,
            "operator" => Self::Operator,
            "begin" => Self::Begin,
            "end" => Self::End,
            _ => return None,
        })
    }

    /// The name this part is written with.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Expression => "expression",
            Self::Selector => "selector",
            Self::Dot => "dot",
            Self::Keyword => "keyword",
            Self::EndKeyword => "end_keyword",
            Self::Operator => "operator",
            Self::Begin => "begin",
            Self::End => "end",
        }
    }
}

fn attr_from_name(name: &str) -> Option<Attr> {
    Some(match name {
        "method_name" => Attr::MethodName,
        "name" => Attr::Name,
        "receiver" => Attr::Receiver,
        "body" => Attr::Body,
        "arg_count" => Attr::ArgCount,
        "first_argument" => Attr::FirstArgument,
        "last_argument" => Attr::LastArgument,
        "source" => Attr::Source,
        "line" => Attr::Line,
        "last_line" => Attr::LastLine,
        "column" => Attr::Column,
        "last_column" => Attr::LastColumn,
        "value" => Attr::Value,
        "type" => Attr::Type,
        "parent_type" => Attr::ParentType,
        "left_sibling" => Attr::LeftSibling,
        "right_sibling" => Attr::RightSibling,
        "first_child" => Attr::FirstChild,
        "last_child" => Attr::LastChild,
        _ => return None,
    })
}

/// A literal operand.
#[derive(Debug, Clone, PartialEq)]
pub enum Lit {
    Str(String),
    Sym(String),
    Int(i64),
    Bool(bool),
    Nil,
}

/// Comparison operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// Quantifier flavour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuantKind {
    AnyOf,
    AllOf,
    NoneOf,
    Count,
}

/// The finite node sequence a quantifier ranges over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Collection {
    /// `args` — a call's arguments.
    Args,
    /// `ancestors` — enclosing nodes, innermost first.
    Ancestors,
    /// `descendants(N)` — every node at most `N` levels below; `children` is
    /// the `N = 1` spelling.
    Descendants(u8),
}

/// A value reference resolved to a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// `node` — the node the hook matched.
    Node,
    /// `parent` — the innermost ancestor, `Nil` at the root.
    Parent,
    /// `$name` — a NodePattern capture slot.
    Capture(usize),
    /// `bind.Name`.
    Bind(usize),
    /// `cfg.Key`.
    Cfg(usize),
    /// `consts.Table`.
    Const(usize),
    /// A quantifier's `var:`.
    Var(usize),
}

/// What `matches` applies: a compiled NodePattern, or a named predicate expr.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatcherRef {
    Pattern(usize),
    Predicate(usize),
}

/// Predicates the NodePattern registry deliberately omits (`predicates.rs`
/// "Type predicates are not in this table" / "Deliberately absent"), because a
/// pattern answers them through the lexer or because they need the ancestor
/// chain. The `when:` layer needs them, so they are compiled directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intrinsic {
    /// `type?(a, b, …)` and every `<t>_type?` spelling.
    Type(Vec<String>),
    /// `value_used?` — approximated from the ancestor chain; see [`Intrinsic`].
    ValueUsed,
    /// `root?` — no enclosing node.
    Root,
}

/// A compiled guard/bind expression. Immutable, index-resolved, `Send + Sync`.
#[derive(Debug, Clone)]
pub enum Expr {
    Lit(Lit),
    Path(Target),
    Attr {
        of: Box<Expr>,
        attr: Attr,
    },
    Pred {
        of: Box<Expr>,
        pred: &'static predicates::Builtin,
        args: Vec<Arg>,
    },
    Intrinsic {
        of: Box<Expr>,
        which: Intrinsic,
    },
    Matches {
        of: Box<Expr>,
        matcher: MatcherRef,
    },
    Regex {
        of: Box<Expr>,
        re: usize,
    },
    All(Vec<Expr>),
    Any(Vec<Expr>),
    Not(Box<Expr>),
    Cmp {
        op: CmpOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    In {
        needle: Box<Expr>,
        haystack: Haystack,
    },
    If {
        cond: Box<Expr>,
        then: Box<Expr>,
        els: Box<Expr>,
    },
    Lookup {
        table: usize,
        key: Box<Expr>,
    },
    Quant {
        kind: QuantKind,
        of: Box<Expr>,
        over: Collection,
        var: usize,
        body: Box<Expr>,
    },
}

/// What `in:` tests membership against.
///
/// A literal sequence is a list of expressions, one per member. The set forms
/// name a whole collection instead — a `constants:` table or a `string_array`
/// config key — so a cop can test against something a `.rubocop.yml` supplies
/// without spelling its members in the document.
#[derive(Debug, Clone)]
pub enum Haystack {
    /// `in: [x, [a, b, c]]`.
    Items(Vec<Expr>),
    /// `in: [x, consts.NAME]` / `in: [x, cfg.Key]` — an expression evaluating
    /// to a list.
    Set(Box<Expr>),
}

/// One hook's compiled expressions, positionally parallel to `IrDocument::hooks`.
#[derive(Debug, Clone, Default)]
pub struct CompiledHook {
    /// Compiled `when:` guard.
    pub when: Option<Expr>,
    /// Compiled `bind:` entries, in declaration order.
    pub binds: Vec<(String, Expr)>,
}

/// Everything a document compiles to that the runtime needs by index.
#[derive(Debug)]
pub struct CompiledDoc {
    pub matcher_names: Vec<String>,
    pub matchers: Vec<CompiledPattern>,
    pub predicate_names: Vec<String>,
    pub predicates: Vec<Expr>,
    pub regexes: Vec<regex::Regex>,
    pub const_names: Vec<String>,
    pub consts: Vec<ConstTable>,
    /// Each `constants:` table as the `Arg::Set` of its members, indexed like
    /// `const_names`.
    ///
    /// This is what a `%TABLE` reference inside a pattern resolves to, which is
    /// how an upstream matcher that says `%KIND_METHODS` stays byte-identical:
    /// upstream's constant is a frozen `Set` and `%CONST` matches it with
    /// `===`, i.e. membership.
    pub const_args: Vec<Arg>,
    pub config_names: Vec<String>,
    /// Declared `config:` defaults, indexed like `config_names`. The runtime
    /// overrides these from `CopConfig`; tests evaluate against them directly.
    pub config_defaults: Vec<serde_yml::Value>,
    pub hooks: Vec<CompiledHook>,
    /// Number of quantifier variable slots any expression may bind.
    pub var_count: usize,
}

impl CompiledDoc {
    /// Index of a compiled matcher by name.
    #[must_use]
    pub fn matcher_id(&self, name: &str) -> Option<usize> {
        self.matcher_names.iter().position(|n| n == name)
    }
}

/// Resolves `#helper` against the *declared* matcher names and `%TABLE`
/// against the document's `constants:`.
///
/// Every declared name resolves to the same stand-in pattern, which is enough
/// for compilation: `collect_unresolved` only asks whether a name is known,
/// and the stand-in is never matched against — [`DocResolver`] answers at
/// match time, from the finished table.
struct DeclaredResolver<'a> {
    names: &'a [String],
    stand_in: &'a CompiledPattern,
    const_names: &'a [String],
    const_args: &'a [Arg],
}

impl Resolver for DeclaredResolver<'_> {
    fn matcher(&self, name: &str) -> Option<&CompiledPattern> {
        self.names
            .iter()
            .any(|declared| declared == name)
            .then_some(self.stand_in)
    }

    fn constant(&self, name: &str) -> Option<&Arg> {
        let index = self.const_names.iter().position(|n| n == name)?;
        self.const_args.get(index)
    }
}

/// The same resolution, against a document that is already compiled: what the
/// runtime hands the matcher so `#helper` and `%TABLE` mean the same thing at
/// match time as they did at load time.
pub struct DocResolver<'a>(pub &'a CompiledDoc);

impl Resolver for DocResolver<'_> {
    fn matcher(&self, name: &str) -> Option<&CompiledPattern> {
        let index = self.0.matcher_names.iter().position(|n| n == name)?;
        self.0.matchers.get(index)
    }

    fn constant(&self, name: &str) -> Option<&Arg> {
        let index = self.0.const_names.iter().position(|n| n == name)?;
        self.0.const_args.get(index)
    }
}

/// A `constants:` table as the set of its members.
///
/// A string member becomes `Arg::Symbol` (with a leading `:` stripped, so
/// `[":map", ":collect"]` and `[map, collect]` are the same set): upstream's
/// set constants are `%i[…]` and `Arg::Symbol`/`Arg::Str` compare identically
/// anyway (`predicates::Arg::accepts_value`). A bool member becomes its Ruby
/// spelling, which is how `{true}` in a pattern reads it.
#[must_use]
pub fn const_table_arg(table: &ConstTable) -> Arg {
    Arg::Set(
        table
            .members()
            .into_iter()
            .map(|member| match member {
                ConstScalar::Str(text) => {
                    Arg::Symbol(text.strip_prefix(':').unwrap_or(&text).to_string())
                }
                ConstScalar::Int(value) => Arg::Int(value),
                ConstScalar::Bool(value) => Arg::Str(value.to_string()),
            })
            .collect(),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PredState {
    Unvisited,
    Visiting,
    Done,
}

/// Compile-time environment: the name → slot resolution the loader used to do
/// by hand, plus the hoisted regex table and the predicate DAG memo.
pub struct CompileCtx<'a> {
    origin: &'a str,
    doc: &'a IrDocument,
    /// Which expression is being compiled, for error messages.
    context: String,
    /// Capture names visible here, in slot order.
    captures: Vec<String>,
    /// `bind:` names declared before the expression being compiled.
    binds: Vec<String>,
    matcher_names: Vec<String>,
    matchers: Vec<CompiledPattern>,
    predicate_names: Vec<String>,
    predicates: Vec<Option<Expr>>,
    pred_state: Vec<PredState>,
    regexes: Vec<regex::Regex>,
    const_names: Vec<String>,
    const_args: Vec<Arg>,
    config_names: Vec<String>,
    /// Quantifier variables currently in scope, innermost last.
    vars: Vec<String>,
    var_count: usize,
}

macro_rules! cerr {
    ($ctx:expr, $kind:ident, $($arg:tt)*) => {
        return Err($ctx.error(IrErrorKind::$kind, format!($($arg)*)))
    };
}

impl<'a> CompileCtx<'a> {
    /// Compile the document's `matchers:` and `predicates:`, leaving the
    /// context ready to compile hook expressions.
    ///
    /// # Errors
    ///
    /// [`IrErrorKind::Pattern`] for a matcher that does not compile,
    /// [`IrErrorKind::Cycle`] for a predicate reference cycle, and anything
    /// [`compile`] can raise for a predicate body.
    pub fn new(origin: &'a str, doc: &'a IrDocument) -> Result<Self, IrError> {
        let mut ctx = Self {
            origin,
            doc,
            context: String::new(),
            captures: Vec::new(),
            binds: Vec::new(),
            matcher_names: Vec::new(),
            matchers: Vec::new(),
            predicate_names: doc.predicates.keys().cloned().collect(),
            predicates: vec![None; doc.predicates.len()],
            pred_state: vec![PredState::Unvisited; doc.predicates.len()],
            regexes: Vec::new(),
            const_names: doc.constants.keys().cloned().collect(),
            const_args: doc.constants.values().map(const_table_arg).collect(),
            config_names: doc.config.keys().cloned().collect(),
            vars: Vec::new(),
            var_count: 0,
        };
        ctx.compile_matchers()?;
        // Predicates see every capture any matcher declares; which matcher
        // actually bound them is a per-hook question the loader answers.
        ctx.captures = doc
            .matchers
            .values()
            .flat_map(|m| m.captures.iter().cloned())
            .collect();
        for index in 0..ctx.predicate_names.len() {
            ctx.compile_predicate(index)?;
        }
        Ok(ctx)
    }

    /// Pass 1 declares every matcher name; pass 2 compiles every pattern
    /// against that set. See the module docs for why, and for what it costs.
    fn compile_matchers(&mut self) -> Result<(), IrError> {
        let stand_in = CompiledPattern::compile("_").expect("`_` is a valid pattern");
        let names: Vec<String> = self.doc.matchers.keys().cloned().collect();
        let mut compiled = Vec::with_capacity(names.len());
        for (name, decl) in &self.doc.matchers {
            let text = decl.pattern.trim();
            if text.is_empty() {
                cerr!(self, Pattern, "matcher `{name}`: empty pattern");
            }
            let resolver = DeclaredResolver {
                names: &names,
                stand_in: &stand_in,
                const_names: &self.const_names,
                const_args: &self.const_args,
            };
            match CompiledPattern::compile_with(text, &resolver) {
                Ok(pattern) => compiled.push(pattern),
                Err(e) => cerr!(self, Pattern, "matcher `{name}`: {e}"),
            }
        }
        self.matcher_names = names;
        self.matchers = compiled;
        Ok(())
    }

    /// Compile predicate `index`, recursing through `matches` references and
    /// rejecting cycles.
    fn compile_predicate(&mut self, index: usize) -> Result<(), IrError> {
        match self.pred_state[index] {
            PredState::Done => return Ok(()),
            PredState::Visiting => {
                self.context = self.predicate_names[index].clone();
                let name = &self.predicate_names[index];
                cerr!(
                    self,
                    Cycle,
                    "predicate `{name}` takes part in a `matches:` reference cycle"
                );
            }
            PredState::Unvisited => {}
        }
        self.pred_state[index] = PredState::Visiting;
        let name = self.predicate_names[index].clone();
        let decl = &self.doc.predicates[&name];
        let expr = self.compile_in(&decl.expr, &name)?;
        self.predicates[index] = Some(expr);
        self.pred_state[index] = PredState::Done;
        Ok(())
    }

    /// Compile `syntax` with `context` naming it in any error.
    ///
    /// # Errors
    ///
    /// See [`compile`].
    pub fn compile_in(&mut self, syntax: &ExprSyntax, context: &str) -> Result<Expr, IrError> {
        let previous = std::mem::replace(&mut self.context, context.to_string());
        let result = compile(syntax, self);
        self.context = previous;
        result
    }

    /// Number of `$` capture slots the named matcher's pattern allocates.
    #[must_use]
    pub fn capture_count(&self, name: &str) -> Option<usize> {
        let index = self.matcher_names.iter().position(|n| n == name)?;
        Some(self.matchers[index].capture_count())
    }

    /// Set the capture names visible to subsequent expressions and clear the
    /// bind scope (a new hook starts with no binds in scope).
    pub fn enter_hook(&mut self, captures: Vec<String>) {
        self.captures = captures;
        self.binds.clear();
    }

    /// Declare a `bind:` name, making it visible to later expressions only —
    /// which is how "binds may only reference earlier binds" is enforced.
    pub fn declare_bind(&mut self, name: &str) {
        self.binds.push(name.to_string());
    }

    /// Consume the context, producing the document's compiled tables.
    #[must_use]
    pub fn finish(self, hooks: Vec<CompiledHook>) -> CompiledDoc {
        CompiledDoc {
            matcher_names: self.matcher_names,
            matchers: self.matchers,
            predicate_names: self.predicate_names,
            predicates: self.predicates.into_iter().flatten().collect(),
            regexes: self.regexes,
            const_names: self.const_names,
            consts: self.doc.constants.values().cloned().collect(),
            const_args: self.const_args,
            config_names: self.config_names,
            config_defaults: self
                .doc
                .config
                .values()
                .map(|d| d.default.clone())
                .collect(),
            hooks,
            var_count: self.var_count,
        }
    }

    fn error(&self, kind: IrErrorKind, message: String) -> IrError {
        let message = if self.context.is_empty() {
            message
        } else {
            format!("`{}`: {message}", self.context)
        };
        IrError {
            origin: self.origin.to_string(),
            line: None,
            column: None,
            kind,
            message,
        }
    }
}

/// Compile one surface expression into a typed [`Expr`].
///
/// # Errors
///
/// [`IrErrorKind::Expr`] for a bad shape, an unknown operator, an unresolvable
/// reference or a depth-cap violation; [`IrErrorKind::Arity`] for an operator
/// or predicate applied to the wrong number of operands;
/// [`IrErrorKind::UnknownPredicate`] for a `pred:` name nothing explains;
/// [`IrErrorKind::UnknownMatcher`] for a `matches:` name nothing explains.
pub fn compile(syntax: &ExprSyntax, ctx: &mut CompileCtx<'_>) -> Result<Expr, IrError> {
    compile_value(&syntax.0, ctx, 1)
}

type Yaml = serde_yml::Value;

/// A mapping operand is either a sequence of operands or a single one.
fn operands(value: &Yaml) -> Vec<&Yaml> {
    match value {
        Yaml::Sequence(items) => items.iter().collect(),
        other => vec![other],
    }
}

fn compile_value(value: &Yaml, ctx: &mut CompileCtx<'_>, depth: usize) -> Result<Expr, IrError> {
    if depth > MAX_EXPR_DEPTH {
        cerr!(
            ctx,
            Expr,
            "expression nested deeper than {MAX_EXPR_DEPTH} levels"
        );
    }
    match value {
        Yaml::Mapping(map) => {
            if map.len() != 1 {
                cerr!(
                    ctx,
                    Expr,
                    "an expression mapping must have exactly one operator key"
                );
            }
            let (key, operand) = map.iter().next().expect("length checked above");
            let op = key.as_str().unwrap_or_default();
            compile_op(op, operand, ctx, depth)
        }
        Yaml::Sequence(_) => cerr!(
            ctx,
            Expr,
            "a sequence is an operand list, not an expression; wrap it in an operator"
        ),
        Yaml::String(text) => compile_scalar(text, ctx),
        Yaml::Bool(b) => Ok(Expr::Lit(Lit::Bool(*b))),
        Yaml::Number(n) => n
            .as_i64()
            .map(|i| Expr::Lit(Lit::Int(i)))
            .ok_or_else(|| ctx.error(IrErrorKind::Expr, format!("`{n}` is not an integer"))),
        Yaml::Null => Ok(Expr::Lit(Lit::Nil)),
        other => Err(ctx.error(IrErrorKind::Expr, format!("unsupported literal {other:?}"))),
    }
}

/// A scalar string: a path reference if its head names something, a `:symbol`
/// literal, or a plain string literal.
fn compile_scalar(text: &str, ctx: &mut CompileCtx<'_>) -> Result<Expr, IrError> {
    if let Some(sym) = text.strip_prefix(':') {
        return Ok(Expr::Lit(Lit::Sym(sym.to_string())));
    }
    let mut segments = text.split('.');
    let head = segments.next().unwrap_or_default();
    // Two-segment namespaces: the second segment is a key, never an attribute.
    let keyed = |ctx: &CompileCtx<'_>, names: &[String], what: &str| -> Result<usize, IrError> {
        let key = text.split('.').nth(1).unwrap_or_default();
        let count = text.split('.').count();
        names
            .iter()
            .position(|n| n == key)
            .filter(|_| count == 2)
            .ok_or_else(|| {
                ctx.error(
                    IrErrorKind::Expr,
                    format!("`{text}` references undeclared {what} `{key}`"),
                )
            })
    };
    let base = match head {
        "node" => Expr::Path(Target::Node),
        "parent" => Expr::Path(Target::Parent),
        "cfg" => {
            return Ok(Expr::Path(Target::Cfg(keyed(
                ctx,
                &ctx.config_names,
                "config key",
            )?)));
        }
        "bind" => return Ok(Expr::Path(Target::Bind(keyed(ctx, &ctx.binds, "bind")?))),
        "consts" => {
            return Ok(Expr::Path(Target::Const(keyed(
                ctx,
                &ctx.const_names,
                "constant table",
            )?)));
        }
        _ => {
            if let Some(name) = head.strip_prefix('$') {
                match ctx.captures.iter().position(|c| c == name) {
                    Some(slot) => Expr::Path(Target::Capture(slot)),
                    None => cerr!(ctx, Expr, "`{text}` references undeclared capture `{name}`"),
                }
            } else if let Some(slot) = ctx.vars.iter().rposition(|v| v == head) {
                Expr::Path(Target::Var(slot))
            } else {
                // Not a reference: an ordinary string literal ("is_a?", …).
                return Ok(Expr::Lit(Lit::Str(text.to_string())));
            }
        }
    };
    let rest: Vec<&str> = segments.collect();
    attr_path(base, &rest, text, ctx)
}

/// Fold `[.<attr>]*` onto `base`, where `loc` consumes the segment after it as
/// a [`LocPart`] — the one place a path step is two segments wide.
fn attr_path(
    base: Expr,
    segments: &[&str],
    text: &str,
    ctx: &CompileCtx<'_>,
) -> Result<Expr, IrError> {
    let mut of = base;
    let mut index = 0;
    while index < segments.len() {
        let attr = if segments[index] == "loc" {
            let Some(name) = segments.get(index + 1) else {
                cerr!(
                    ctx,
                    Expr,
                    "`{text}`: `loc` must be followed by a part name, one of {:?}",
                    LocPart::NAMES
                );
            };
            let Some(part) = LocPart::from_name(name) else {
                cerr!(ctx, Expr, "`{text}`: unknown `loc` part `{name}`");
            };
            index += 2;
            Attr::Loc(part)
        } else {
            let Some(attr) = attr_from_name(segments[index]) else {
                cerr!(
                    ctx,
                    Expr,
                    "`{text}`: unknown attribute `{}`",
                    segments[index]
                );
            };
            index += 1;
            attr
        };
        of = Expr::Attr {
            of: Box::new(of),
            attr,
        };
    }
    Ok(of)
}

fn compile_op(
    op: &str,
    operand: &Yaml,
    ctx: &mut CompileCtx<'_>,
    depth: usize,
) -> Result<Expr, IrError> {
    let items = operands(operand);
    let sub = |index: usize, ctx: &mut CompileCtx<'_>| compile_value(items[index], ctx, depth + 1);
    let arity = |ctx: &CompileCtx<'_>, want: &str| {
        ctx.error(
            IrErrorKind::Arity,
            format!("`{op}` takes {want} operands, got {}", items.len()),
        )
    };
    match op {
        "all" | "any" => {
            let mut compiled = Vec::with_capacity(items.len());
            for index in 0..items.len() {
                compiled.push(sub(index, ctx)?);
            }
            Ok(if op == "all" {
                Expr::All(compiled)
            } else {
                Expr::Any(compiled)
            })
        }
        "not" => {
            if items.len() != 1 {
                return Err(arity(ctx, "1"));
            }
            Ok(Expr::Not(Box::new(sub(0, ctx)?)))
        }
        "eq" | "ne" | "lt" | "le" | "gt" | "ge" => {
            if items.len() != 2 {
                return Err(arity(ctx, "2"));
            }
            let cmp = match op {
                "eq" => CmpOp::Eq,
                "ne" => CmpOp::Ne,
                "lt" => CmpOp::Lt,
                "le" => CmpOp::Le,
                "gt" => CmpOp::Gt,
                _ => CmpOp::Ge,
            };
            Ok(Expr::Cmp {
                op: cmp,
                lhs: Box::new(sub(0, ctx)?),
                rhs: Box::new(sub(1, ctx)?),
            })
        }
        "in" => {
            if items.len() != 2 {
                return Err(arity(ctx, "2"));
            }
            let needle = sub(0, ctx)?;
            let haystack = match items[1] {
                Yaml::Sequence(members) => Haystack::Items(
                    members
                        .iter()
                        .map(|member| compile_value(member, ctx, depth + 1))
                        .collect::<Result<_, _>>()?,
                ),
                // A set-valued reference: the whole table or the whole config
                // list is the haystack. Anything else — a bare literal, an
                // operator — would silently degrade to a one-member test.
                other => {
                    let compiled = compile_value(other, ctx, depth + 1)?;
                    match &compiled {
                        Expr::Path(Target::Const(_)) => {}
                        Expr::Path(Target::Cfg(slot)) => {
                            let key = &ctx.config_names[*slot];
                            let ty = ctx.doc.config[key].ty;
                            if ty != ConfigType::StringArray {
                                cerr!(
                                    ctx,
                                    Expr,
                                    "`in`: config key `{key}` is `{ty}`, not `string_array`"
                                );
                            }
                        }
                        _ => cerr!(
                            ctx,
                            Expr,
                            "`in`: the second operand must be a sequence, a `consts.<Table>` or a `string_array` `cfg.<Key>`"
                        ),
                    }
                    Haystack::Set(Box::new(compiled))
                }
            };
            Ok(Expr::In {
                needle: Box::new(needle),
                haystack,
            })
        }
        "if" => {
            if items.len() != 3 {
                return Err(arity(ctx, "3"));
            }
            Ok(Expr::If {
                cond: Box::new(sub(0, ctx)?),
                then: Box::new(sub(1, ctx)?),
                els: Box::new(sub(2, ctx)?),
            })
        }
        "lit" => {
            if items.len() != 1 {
                return Err(arity(ctx, "1"));
            }
            Ok(Expr::Lit(literal(items[0], ctx)?))
        }
        "lookup" => {
            if items.len() != 2 {
                return Err(arity(ctx, "2"));
            }
            let Expr::Path(Target::Const(table)) = sub(0, ctx)? else {
                cerr!(
                    ctx,
                    Expr,
                    "`lookup`: the first operand must be a `consts.<Table>` reference"
                );
            };
            let name = &ctx.const_names[table];
            if matches!(ctx.doc.constants[name], ConstTable::List(_)) {
                cerr!(
                    ctx,
                    Expr,
                    "`lookup`: `{name}` is a list, which has no values to look up; use `in:` to test membership"
                );
            }
            Ok(Expr::Lookup {
                table,
                key: Box::new(sub(1, ctx)?),
            })
        }
        "attr" => {
            if !(2..=3).contains(&items.len()) {
                return Err(arity(ctx, "2 or 3"));
            }
            let of = sub(0, ctx)?;
            let name = items[1].as_str().unwrap_or_default();
            let attr = if name == "arg" {
                let index = items.get(2).and_then(|v| v.as_u64()).ok_or_else(|| {
                    ctx.error(
                        IrErrorKind::Arity,
                        "`attr`: `arg` needs an integer index".into(),
                    )
                })?;
                Attr::Arg(index as usize)
            } else if name == "loc" {
                let part = items
                    .get(2)
                    .and_then(|v| v.as_str())
                    .and_then(LocPart::from_name);
                let Some(part) = part else {
                    cerr!(
                        ctx,
                        Expr,
                        "`attr`: `loc` needs a part name, one of {:?}",
                        LocPart::NAMES
                    );
                };
                Attr::Loc(part)
            } else {
                match attr_from_name(name) {
                    Some(attr) if items.len() == 2 => attr,
                    Some(_) => return Err(arity(ctx, "2")),
                    None => cerr!(ctx, Expr, "`attr`: unknown attribute `{name}`"),
                }
            };
            Ok(Expr::Attr {
                of: Box::new(of),
                attr,
            })
        }
        "pred" => compile_pred(&items, ctx, depth),
        "matches" => {
            if items.len() != 2 {
                return Err(arity(ctx, "2"));
            }
            let of = sub(0, ctx)?;
            let name = items[1].as_str().unwrap_or_default().to_string();
            let matcher = if let Some(index) = ctx.matcher_names.iter().position(|n| *n == name) {
                MatcherRef::Pattern(index)
            } else if let Some(index) = ctx.predicate_names.iter().position(|n| *n == name) {
                ctx.compile_predicate(index)?;
                MatcherRef::Predicate(index)
            } else {
                cerr!(
                    ctx,
                    UnknownMatcher,
                    "`matches`: `{name}` is neither a declared matcher nor a predicate"
                );
            };
            Ok(Expr::Matches {
                of: Box::new(of),
                matcher,
            })
        }
        "regex" => {
            if !(2..=3).contains(&items.len()) {
                return Err(arity(ctx, "2 or 3"));
            }
            let of = sub(0, ctx)?;
            let body = items[1].as_str().unwrap_or_default();
            let flags = items.get(2).and_then(|v| v.as_str()).unwrap_or_default();
            let built = RegexBuilder::new(body)
                .case_insensitive(flags.contains('i'))
                .multi_line(flags.contains('m'))
                .ignore_whitespace(flags.contains('x'))
                .build();
            let Ok(compiled) = built else {
                cerr!(ctx, Expr, "`regex`: `/{body}/{flags}` does not compile");
            };
            ctx.regexes.push(compiled);
            Ok(Expr::Regex {
                of: Box::new(of),
                re: ctx.regexes.len() - 1,
            })
        }
        "any_of" | "all_of" | "none_of" | "count" => compile_quant(op, operand, ctx, depth),
        _ => cerr!(ctx, Expr, "unknown expression operator `{op}`"),
    }
}

fn literal(value: &Yaml, ctx: &CompileCtx<'_>) -> Result<Lit, IrError> {
    Ok(match value {
        Yaml::String(text) => match text.strip_prefix(':') {
            Some(sym) => Lit::Sym(sym.to_string()),
            None => Lit::Str(text.clone()),
        },
        Yaml::Bool(b) => Lit::Bool(*b),
        Yaml::Null => Lit::Nil,
        Yaml::Number(n) => match n.as_i64() {
            Some(i) => Lit::Int(i),
            None => {
                return Err(ctx.error(IrErrorKind::Expr, format!("`lit`: `{n}` is not an integer")));
            }
        },
        other => {
            return Err(ctx.error(
                IrErrorKind::Expr,
                format!("`lit`: {other:?} is not a scalar"),
            ));
        }
    })
}

/// `{ pred: [<expr>, "name", <arg>…] }`.
fn compile_pred(items: &[&Yaml], ctx: &mut CompileCtx<'_>, depth: usize) -> Result<Expr, IrError> {
    if items.len() < 2 {
        cerr!(
            ctx,
            Arity,
            "`pred` takes at least 2 operands, got {}",
            items.len()
        );
    }
    let of = Box::new(compile_value(items[0], ctx, depth + 1)?);
    let name = items[1].as_str().unwrap_or_default();
    if name == "type?" || name.ends_with("_type?") {
        let mut types: Vec<String> = Vec::new();
        for item in &items[2..] {
            match item {
                Yaml::String(text) => types.push(text.trim_start_matches(':').to_string()),
                Yaml::Sequence(seq) => types.extend(
                    seq.iter()
                        .filter_map(|v| v.as_str())
                        .map(|s| s.trim_start_matches(':').to_string()),
                ),
                _ => cerr!(ctx, Expr, "`pred`: `{name}` takes node type names"),
            }
        }
        if let Some(prefix) = name.strip_suffix("_type?") {
            types.push(prefix.to_string());
        }
        if types.is_empty() {
            cerr!(ctx, Arity, "`pred`: `type?` needs at least one node type");
        }
        return Ok(Expr::Intrinsic {
            of,
            which: Intrinsic::Type(types),
        });
    }
    if let Some(which) = match name {
        "value_used?" => Some(Intrinsic::ValueUsed),
        "root?" => Some(Intrinsic::Root),
        _ => None,
    } {
        if items.len() != 2 {
            cerr!(ctx, Arity, "`pred`: `{name}` takes no arguments");
        }
        return Ok(Expr::Intrinsic { of, which });
    }
    let Some(pred) = predicates::lookup(name) else {
        cerr!(
            ctx,
            UnknownPredicate,
            "`pred`: `{name}` is not a builtin predicate"
        );
    };
    let args = items[2..]
        .iter()
        .map(|item| to_arg(item))
        .collect::<Option<Vec<Arg>>>()
        .ok_or_else(|| {
            ctx.error(
                IrErrorKind::Expr,
                format!("`pred`: `{name}` got an argument that is not an atom"),
            )
        })?;
    let want = match pred.arity {
        Arity::Nullary => 0,
        Arity::Unary => 1,
    };
    if args.len() != want {
        cerr!(
            ctx,
            Arity,
            "`pred`: `{name}` takes {want} argument(s), got {}",
            args.len()
        );
    }
    Ok(Expr::Pred { of, pred, args })
}

fn to_arg(value: &Yaml) -> Option<Arg> {
    Some(match value {
        Yaml::String(text) => match text.strip_prefix(':') {
            Some(sym) => Arg::Symbol(sym.to_string()),
            None => Arg::Str(text.clone()),
        },
        Yaml::Number(n) => match n.as_i64() {
            Some(i) => Arg::Int(i),
            None => Arg::Float(n.as_f64()?),
        },
        Yaml::Sequence(items) => Arg::Set(items.iter().map(to_arg).collect::<Option<_>>()?),
        _ => return None,
    })
}

/// `{ any_of: { of: <expr>, over: args, var: a, body: <expr> } }`.
fn compile_quant(
    op: &str,
    operand: &Yaml,
    ctx: &mut CompileCtx<'_>,
    depth: usize,
) -> Result<Expr, IrError> {
    let Yaml::Mapping(map) = operand else {
        cerr!(
            ctx,
            Expr,
            "`{op}`: operand must be a mapping with `over:`, `var:` and `body:`"
        );
    };
    let mut of_syntax = None;
    let (mut over_text, mut var_name, mut body_syntax) = (None, None, None);
    for (key, value) in map {
        match key.as_str().unwrap_or_default() {
            "of" => of_syntax = Some(value),
            "over" => over_text = value.as_str(),
            "var" => var_name = value.as_str(),
            "body" => body_syntax = Some(value),
            other => cerr!(ctx, Expr, "`{op}`: unknown key `{other}`"),
        }
    }
    let (Some(over_text), Some(var_name), Some(body_syntax)) = (over_text, var_name, body_syntax)
    else {
        cerr!(
            ctx,
            Expr,
            "`{op}`: `over:`, `var:` and `body:` are required"
        );
    };
    let Some(over) = collection(over_text) else {
        cerr!(
            ctx,
            Expr,
            "`{op}`: unknown collection `{over_text}` (expected `args`, `children`, `ancestors` or `descendants(N)` with N <= {MAX_DESCEND_DEPTH})"
        );
    };
    let of = match of_syntax {
        Some(value) => compile_value(value, ctx, depth + 1)?,
        None => Expr::Path(Target::Node),
    };
    let slot = ctx.vars.len();
    ctx.vars.push(var_name.to_string());
    ctx.var_count = ctx.var_count.max(ctx.vars.len());
    let body = compile_value(body_syntax, ctx, depth + 1);
    ctx.vars.pop();
    let kind = match op {
        "any_of" => QuantKind::AnyOf,
        "all_of" => QuantKind::AllOf,
        "none_of" => QuantKind::NoneOf,
        _ => QuantKind::Count,
    };
    Ok(Expr::Quant {
        kind,
        of: Box::new(of),
        over,
        var: slot,
        body: Box::new(body?),
    })
}

fn collection(text: &str) -> Option<Collection> {
    match text {
        "args" => Some(Collection::Args),
        "ancestors" => Some(Collection::Ancestors),
        "children" => Some(Collection::Descendants(1)),
        "descendants" => Some(Collection::Descendants(MAX_DESCEND_DEPTH)),
        _ => {
            let inner = text.strip_prefix("descendants(")?.strip_suffix(')')?;
            let depth: u8 = inner.trim().parse().ok()?;
            (1..=MAX_DESCEND_DEPTH)
                .contains(&depth)
                .then_some(Collection::Descendants(depth))
        }
    }
}
