//! NodePattern DSL parser.
//!
//! Parses a token stream into a `PatternNode` AST.

use super::lexer::Token;

#[derive(Debug, Clone)]
pub enum PatternNode {
    /// (node_type child1 child2 ...)
    NodeMatch {
        node_type: String,
        children: Vec<PatternNode>,
    },
    /// {a | b | c}
    Alternatives(Vec<PatternNode>),
    /// A multi-term `{}` branch: the `a b` of `{a b | c}`.
    ///
    /// RuboCop's builder groups each `|`-separated run of terms into a
    /// subsequence (`builder.rb:57-68`); it matches that run of children in
    /// order and can therefore consume more (or fewer) than one child.
    Subsequence(Vec<PatternNode>),
    /// [a b c]
    Conjunction(Vec<PatternNode>),
    /// `<a b ...>` — an any-order group: every term must match a distinct
    /// child, in any order, and a trailing `...` soaks up the rest.
    ///
    /// The vector mirrors RuboCop's `AnyOrder` node children
    /// (`node_pattern/node.rb:179-201`): the trailing rest term, when present,
    /// is the last element, so arity is `children.len()` without a rest and
    /// `(children.len() - 1)..∞` with one.
    AnyOrder(Vec<PatternNode>),
    /// `$pattern` — binds the matched value to a numbered capture slot.
    ///
    /// Slots are allocated by the parser in `$`-occurrence order (pre-order,
    /// left to right), mirroring RuboCop's compiler
    /// (`vendor/rubocop-ast/lib/rubocop/ast/node_pattern/compiler.rb:71-100`).
    Capture {
        /// Zero-based capture slot index.
        slot: usize,
        /// The pattern whose matched value is captured.
        inner: Box<PatternNode>,
    },
    /// _
    Wildcard,
    /// ...
    Rest,
    /// !pattern
    Negation(Box<PatternNode>),
    /// `#helper` / `#helper(arg, …)` — `parser.y`'s `tFUNCTION_CALL args`.
    ///
    /// In RuboCop the call goes to the object the pattern was defined on (the
    /// cop, or `Node` itself for the matchers in `node.rb`), with the matched
    /// node prepended to the argument list
    /// (`node_pattern_subcompiler.rb:84-86`).
    HelperCall {
        /// Method name as written, `?` included; `Const.method` for the
        /// const-qualified form (`#Examples.all`).
        name: String,
        /// Argument patterns. RuboCop compiles these as *atoms*
        /// (`atom_subcompiler.rb`) — literals, `%param` refs, or a `{}` union
        /// of literals, which upstream turns into a `Set`.
        args: Vec<PatternNode>,
    },
    /// `pred?` / `pred?(arg, …)` — `parser.y`'s `tPREDICATE args`.
    ///
    /// The call goes to the matched node itself
    /// (`node_pattern_subcompiler.rb:80-82`), so these resolve against the
    /// rubocop-ast `Node` predicate registry rather than against the cop.
    Predicate {
        /// Method name as written, `?` included.
        name: String,
        /// Argument patterns, as for [`PatternNode::HelperCall`].
        args: Vec<PatternNode>,
    },
    /// :symbol
    SymbolLiteral(String),
    /// Integer literal
    IntLiteral(i64),
    /// Float literal
    FloatLiteral(String),
    /// String literal
    StringLiteral(String),
    /// nil? — receiver is nil / no receiver
    NilPredicate,
    /// true literal node
    TrueLiteral,
    /// false literal node
    FalseLiteral,
    /// nil literal node
    NilLiteral,
    /// `%1`, or a bare `%` (which upstream maps to `%1`) — a positional
    /// pattern parameter (`tPARAM_NUMBER`).
    ParamNumber(usize),
    /// `%name` — a named pattern parameter (`tPARAM_NAMED`).
    ParamNamed(String),
    /// `%Const` or a bare `Const` — a constant reference (`tPARAM_CONST`).
    ///
    /// Upstream emits the constant verbatim into the compiled code and matches
    /// it with `===`, so `%RuboCop::AST::Node` is an `is_a?` test and
    /// `%SOME_SET` a membership test. Resolution is therefore owner-specific
    /// and goes through the same hook as cop-local helpers.
    ParamConst(String),
    /// `/body/flags` — a regexp literal, matched with `Regexp#===`.
    Regexp {
        /// Regexp source between the slashes, escapes intact.
        body: String,
        /// The `imxo` flag letters that followed the closing slash.
        flags: String,
    },
    /// Type predicate: int?, str?, sym?, etc.
    TypePredicate(String),
    /// ^pattern — parent node
    ParentRef(Box<PatternNode>),
    /// `pattern — descend
    DescendRef(Box<PatternNode>),
    /// An identifier used in certain contexts (e.g., inside alternatives for node types)
    Ident(String),
}

/// Why a pattern was rejected.
///
/// Mirrors the `NodePattern::Invalid` cases RuboCop raises at compile time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternError {
    /// `{}` branches declared different numbers of captures.
    ///
    /// RuboCop: `Invalid: each branch must have same number of captures`
    /// (`compiler.rb:82-95`).
    UnbalancedUnionCaptures {
        /// Captures declared by the first branch.
        expected: usize,
        /// Captures declared by the offending branch.
        found: usize,
    },
    /// A `<>` any-order group put `...` anywhere but last.
    ///
    /// The grammar is `'<' node_pattern_list opt_rest '>'` (`parser.y:48`), so
    /// at most one rest term and only in final position.
    AnyOrderRestNotLast,
    /// A `<>` any-order group declared more terms than the matcher will
    /// search over.
    ///
    /// Matching `<>` is an assignment problem: the interpreter backtracks over
    /// child-to-term assignments, which is worst-case factorial in the number
    /// of terms. Every `<>` pattern in the vendored cop set has at most four
    /// terms, so patterns above [`ANY_ORDER_MAX_TERMS`] are rejected at compile
    /// time rather than risking a blow-up at match time.
    AnyOrderTooManyTerms {
        /// Non-rest terms the group declared.
        found: usize,
        /// The supported maximum, [`ANY_ORDER_MAX_TERMS`].
        max: usize,
    },
    /// The pattern did not parse and no more specific error was recorded.
    Syntax,
    /// `#name` resolved to neither an owner-supplied matcher nor a builtin.
    ///
    /// Upstream this is a `NoMethodError` the first time the pattern runs;
    /// here it is a compile error, so a cop-local helper can never be silently
    /// treated as "always true".
    UnknownHelper {
        /// The name as written, without the `#`.
        name: String,
    },
    /// `name?` is not a predicate `RuboCop::AST::Node` defines.
    UnknownPredicate {
        /// The name as written, `?` included.
        name: String,
    },
    /// `%Const` (or a bare `Const`) that the resolver does not know.
    UnknownConstant {
        /// The constant path as written, without the `%`.
        name: String,
    },
    /// A builtin was called with the wrong number of arguments.
    PredicateArity {
        /// The predicate name.
        name: String,
        /// Arguments the registry entry declares.
        expected: usize,
        /// Arguments the pattern passed.
        found: usize,
    },
}

/// Maximum number of non-rest terms a `<>` any-order group may declare.
pub const ANY_ORDER_MAX_TERMS: usize = 8;

impl std::fmt::Display for PatternError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PatternError::UnbalancedUnionCaptures { expected, found } => write!(
                f,
                "each branch of {{}} must have the same number of captures (expected {expected}, found {found})"
            ),
            PatternError::AnyOrderRestNotLast => {
                write!(f, "`...` is only allowed as the last term of a <> group")
            }
            PatternError::AnyOrderTooManyTerms { found, max } => write!(
                f,
                "<> any-order group has {found} terms, more than the supported maximum of {max}"
            ),
            PatternError::Syntax => write!(f, "pattern does not parse"),
            PatternError::UnknownHelper { name } => write!(
                f,
                "unknown helper `#{name}`: not a builtin predicate, and the resolver supplied no matcher for it"
            ),
            PatternError::UnknownPredicate { name } => {
                write!(f, "unknown node predicate `{name}`")
            }
            PatternError::UnknownConstant { name } => {
                write!(f, "unknown constant `%{name}`")
            }
            PatternError::PredicateArity {
                name,
                expected,
                found,
            } => write!(
                f,
                "`{name}` takes {expected} argument(s), but the pattern passed {found}"
            ),
        }
    }
}

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    /// Number of capture slots allocated so far (RuboCop's `Compiler#captures`).
    captures: usize,
    /// First error encountered; a pattern with an error never yields an AST.
    error: Option<PatternError>,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self {
            tokens,
            pos: 0,
            captures: 0,
            error: None,
        }
    }

    /// Number of capture slots the last parse allocated.
    pub fn capture_count(&self) -> usize {
        self.captures
    }

    /// The error that rejected the pattern, if any.
    pub fn error(&self) -> Option<&PatternError> {
        self.error.as_ref()
    }

    /// Allocate the next capture slot (RuboCop's `Compiler#new_capture`).
    fn new_capture(&mut self) -> usize {
        let slot = self.captures;
        self.captures += 1;
        slot
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn advance(&mut self) -> Option<&Token> {
        let tok = self.tokens.get(self.pos)?;
        self.pos += 1;
        Some(tok)
    }

    fn expect(&mut self, expected: &Token) -> bool {
        if self.peek() == Some(expected) {
            self.advance();
            true
        } else {
            false
        }
    }

    pub fn parse(&mut self) -> Option<PatternNode> {
        let node = self.parse_node();
        // Inner errors are not threaded through every `Option` return, so a
        // recorded error vetoes the whole pattern here.
        if self.error.is_some() {
            return None;
        }
        node
    }

    fn parse_node(&mut self) -> Option<PatternNode> {
        let tok = self.peek()?.clone();

        match tok {
            Token::LParen => self.parse_sequence(),
            Token::LBrace => self.parse_alternatives(),
            Token::LBracket => self.parse_conjunction(),
            Token::LAngle => self.parse_any_order(),
            Token::Capture => {
                self.advance();
                // RuboCop allocates the slot before compiling the captured
                // term, so nested captures are numbered outside-in.
                let slot = self.new_capture();
                let inner = self.parse_node()?;
                Some(PatternNode::Capture {
                    slot,
                    inner: Box::new(inner),
                })
            }
            Token::Negation => {
                self.advance();
                let inner = self.parse_node()?;
                Some(PatternNode::Negation(Box::new(inner)))
            }
            Token::Caret => {
                self.advance();
                let inner = self.parse_node()?;
                Some(PatternNode::ParentRef(Box::new(inner)))
            }
            Token::Backtick => {
                self.advance();
                let inner = self.parse_node()?;
                Some(PatternNode::DescendRef(Box::new(inner)))
            }
            Token::Wildcard => {
                self.advance();
                Some(PatternNode::Wildcard)
            }
            Token::Rest => {
                self.advance();
                Some(PatternNode::Rest)
            }
            Token::NilPredicate => {
                self.advance();
                Some(PatternNode::NilPredicate)
            }
            Token::TruePredicate => {
                self.advance();
                Some(PatternNode::TrueLiteral)
            }
            Token::FalsePredicate => {
                self.advance();
                Some(PatternNode::FalseLiteral)
            }
            Token::TypePredicate(ref name) => {
                let name = name.clone();
                self.advance();
                Some(PatternNode::TypePredicate(name))
            }
            Token::HelperCall(ref name) => {
                let name = name.clone();
                self.advance();
                let args = self.parse_arg_list();
                Some(PatternNode::HelperCall { name, args })
            }
            Token::Predicate(ref name) => {
                let name = name.clone();
                self.advance();
                let args = self.parse_arg_list();
                Some(PatternNode::Predicate { name, args })
            }
            Token::SymbolLiteral(ref name) => {
                let name = name.clone();
                self.advance();
                Some(PatternNode::SymbolLiteral(name))
            }
            Token::IntLiteral(n) => {
                self.advance();
                Some(PatternNode::IntLiteral(n))
            }
            Token::FloatLiteral(ref s) => {
                let s = s.clone();
                self.advance();
                Some(PatternNode::FloatLiteral(s))
            }
            Token::StringLiteral(ref s) => {
                let s = s.clone();
                self.advance();
                Some(PatternNode::StringLiteral(s))
            }
            Token::Ident(ref name) => {
                let name = name.clone();
                self.advance();
                match name.as_str() {
                    "nil" => Some(PatternNode::NilLiteral),
                    "true" => Some(PatternNode::TrueLiteral),
                    "false" => Some(PatternNode::FalseLiteral),
                    _ => Some(PatternNode::Ident(name)),
                }
            }
            Token::ParamNumber(n) => {
                self.advance();
                Some(PatternNode::ParamNumber(n))
            }
            Token::ParamNamed(ref s) => {
                let s = s.clone();
                self.advance();
                Some(PatternNode::ParamNamed(s))
            }
            Token::ParamConst(ref s) => {
                let s = s.clone();
                self.advance();
                Some(PatternNode::ParamConst(s))
            }
            Token::Regexp {
                ref body,
                ref flags,
            } => {
                let (body, flags) = (body.clone(), flags.clone());
                self.advance();
                Some(PatternNode::Regexp { body, flags })
            }
            _ => None,
        }
    }

    /// Parse the optional argument list of a `#call` / `pred?`.
    ///
    /// `parser.y`: `args: | tARG_LIST arg_list ')'` and
    /// `arg_list: node_pattern | arg_list ',' node_pattern`. The lexer only
    /// emits [`Token::ArgList`] when the `(` touched the call name, so an
    /// absent `tARG_LIST` here means the call simply has no arguments.
    fn parse_arg_list(&mut self) -> Vec<PatternNode> {
        if !self.expect(&Token::ArgList) {
            return Vec::new();
        }
        let mut args = Vec::new();
        while self.peek().is_some() && self.peek() != Some(&Token::RParen) {
            if self.peek() == Some(&Token::Comma) {
                self.advance();
                continue;
            }
            let Some(arg) = self.parse_node() else { break };
            args.push(arg);
        }
        self.expect(&Token::RParen);
        args
    }

    fn parse_sequence(&mut self) -> Option<PatternNode> {
        self.expect(&Token::LParen);

        // First element is the node type (or could be a complex expression)
        let first = self.parse_node()?;

        // Determine if this is a node match or something else
        let node_type = match &first {
            PatternNode::Ident(name) => Some(name.clone()),
            _ => None,
        };

        let mut children = Vec::new();

        // Parse remaining children
        while self.peek().is_some() && self.peek() != Some(&Token::RParen) {
            if let Some(child) = self.parse_node() {
                children.push(child);
            } else {
                break;
            }
        }

        self.expect(&Token::RParen);

        if let Some(nt) = node_type {
            Some(PatternNode::NodeMatch {
                node_type: nt,
                children,
            })
        } else {
            // Non-identifier first element (e.g. alternatives) — wrap in _complex
            let mut all = vec![first];
            all.extend(children);
            Some(PatternNode::NodeMatch {
                node_type: "_complex".to_string(),
                children: all,
            })
        }
    }

    /// Parse `{a b c}` / `{a b | c}`.
    ///
    /// Branch shape follows RuboCop's builder (`builder.rb:57-68`): without `|`
    /// every term is its own branch, with `|` each separated run becomes one
    /// branch (a [`PatternNode::Subsequence`] when it holds several terms).
    ///
    /// Capture slots are shared across branches: every branch restarts from the
    /// slot base the union entered with, and all branches must allocate the same
    /// number of slots — RuboCop's `Compiler#enforce_same_captures`
    /// (`compiler.rb:82-95`).
    fn parse_alternatives(&mut self) -> Option<PatternNode> {
        self.expect(&Token::LBrace);

        let base = self.captures;
        // Terms are parsed with the slot counter running continuously; the
        // per-branch re-basing happens below, once `|` has told us how the
        // terms group into branches.
        let mut groups: Vec<Vec<(PatternNode, usize, usize)>> = vec![Vec::new()];
        let mut saw_pipe = false;

        while self.peek().is_some() && self.peek() != Some(&Token::RBrace) {
            if self.peek() == Some(&Token::Pipe) {
                self.advance();
                saw_pipe = true;
                groups.push(Vec::new());
                continue;
            }
            let start = self.captures;
            let Some(node) = self.parse_node() else { break };
            let allocated = self.captures - start;
            groups
                .last_mut()
                .expect("groups is never empty")
                .push((node, start, allocated));
        }

        self.expect(&Token::RBrace);

        if !saw_pipe {
            // `{a b c}` — each term is a branch of its own.
            groups = groups
                .pop()
                .unwrap_or_default()
                .into_iter()
                .map(|term| vec![term])
                .collect();
        }

        let mut alts = Vec::with_capacity(groups.len());
        let mut branch_captures: Option<usize> = None;

        for group in groups {
            let mut terms = Vec::with_capacity(group.len());
            let mut allocated = 0;
            for (mut term, start, count) in group {
                // Re-base this term's slots onto the branch's own range.
                let shift = start - (base + allocated);
                if shift > 0 {
                    shift_capture_slots(&mut term, shift);
                }
                allocated += count;
                terms.push(term);
            }

            match branch_captures {
                None => branch_captures = Some(allocated),
                Some(expected) if expected != allocated => {
                    self.error
                        .get_or_insert(PatternError::UnbalancedUnionCaptures {
                            expected,
                            found: allocated,
                        });
                    return None;
                }
                Some(_) => {}
            }

            alts.push(if terms.len() == 1 {
                terms.pop().expect("single-term branch")
            } else {
                PatternNode::Subsequence(terms)
            });
        }

        self.captures = base + branch_captures.unwrap_or(0);
        Some(PatternNode::Alternatives(alts))
    }

    /// Parse `<a b ...>` (grammar: `opt_capture '<' node_pattern_list opt_rest '>'`,
    /// `parser.y:48-55`).
    ///
    /// An enclosing `$` is handled by [`Parser::parse_node`]'s capture arm, so
    /// the group's own slot is allocated before its terms' — matching
    /// `emit_capture`, which takes its storage slot before compiling the
    /// wrapped list (`compiler/sequence_subcompiler.rb:155-162`).
    fn parse_any_order(&mut self) -> Option<PatternNode> {
        self.expect(&Token::LAngle);

        let mut children = Vec::new();
        while self.peek().is_some() && self.peek() != Some(&Token::RAngle) {
            let node = self.parse_node()?;
            children.push(node);
        }
        if !self.expect(&Token::RAngle) {
            return None;
        }

        // `node_pattern_list opt_rest`: a rest term is only legal last.
        let rest_positions: Vec<usize> = children
            .iter()
            .enumerate()
            .filter(|(_, child)| is_rest_term(child))
            .map(|(i, _)| i)
            .collect();
        if rest_positions.len() > 1 || rest_positions.iter().any(|i| *i + 1 != children.len()) {
            self.error.get_or_insert(PatternError::AnyOrderRestNotLast);
            return None;
        }

        let terms = children.len() - rest_positions.len();
        if terms > ANY_ORDER_MAX_TERMS {
            self.error
                .get_or_insert(PatternError::AnyOrderTooManyTerms {
                    found: terms,
                    max: ANY_ORDER_MAX_TERMS,
                });
            return None;
        }

        Some(PatternNode::AnyOrder(children))
    }

    fn parse_conjunction(&mut self) -> Option<PatternNode> {
        self.expect(&Token::LBracket);
        let mut items = Vec::new();

        while self.peek().is_some() && self.peek() != Some(&Token::RBracket) {
            if let Some(node) = self.parse_node() {
                items.push(node);
            } else {
                break;
            }
        }

        self.expect(&Token::RBracket);
        Some(PatternNode::Conjunction(items))
    }
}

/// Whether `node` is `...` or `$...`.
fn is_rest_term(node: &PatternNode) -> bool {
    match node {
        PatternNode::Rest => true,
        PatternNode::Capture { inner, .. } => matches!(**inner, PatternNode::Rest),
        _ => false,
    }
}

/// Subtract `delta` from every capture slot in `node`.
///
/// Used when a `{}` branch is re-based onto the union's shared slot range.
fn shift_capture_slots(node: &mut PatternNode, delta: usize) {
    match node {
        PatternNode::Capture { slot, inner } => {
            *slot -= delta;
            shift_capture_slots(inner, delta);
        }
        PatternNode::NodeMatch { children, .. } => {
            for child in children {
                shift_capture_slots(child, delta);
            }
        }
        PatternNode::Alternatives(items)
        | PatternNode::Conjunction(items)
        | PatternNode::Subsequence(items)
        | PatternNode::AnyOrder(items) => {
            for item in items {
                shift_capture_slots(item, delta);
            }
        }
        PatternNode::Negation(inner)
        | PatternNode::ParentRef(inner)
        | PatternNode::DescendRef(inner) => shift_capture_slots(inner, delta),
        PatternNode::HelperCall { args, .. } | PatternNode::Predicate { args, .. } => {
            for arg in args {
                shift_capture_slots(arg, delta);
            }
        }
        _ => {}
    }
}

/// Produce a short summary string for a pattern node (for comments/debugging).
pub fn pattern_summary(node: &PatternNode) -> String {
    match node {
        PatternNode::NodeMatch {
            node_type,
            children,
        } => {
            let child_summaries: Vec<String> = children.iter().map(pattern_summary).collect();
            format!("({node_type} {})", child_summaries.join(" "))
        }
        PatternNode::Wildcard => "_".to_string(),
        PatternNode::Rest => "...".to_string(),
        PatternNode::NilPredicate => "nil?".to_string(),
        PatternNode::NilLiteral => "nil".to_string(),
        PatternNode::TrueLiteral => "true".to_string(),
        PatternNode::FalseLiteral => "false".to_string(),
        PatternNode::SymbolLiteral(s) => format!(":{s}"),
        PatternNode::IntLiteral(n) => n.to_string(),
        PatternNode::FloatLiteral(s) => s.clone(),
        PatternNode::StringLiteral(s) => format!("\"{s}\""),
        PatternNode::Capture { inner, .. } => format!("${}", pattern_summary(inner)),
        PatternNode::Alternatives(alts) => {
            let inner: Vec<String> = alts.iter().map(pattern_summary).collect();
            format!("{{{}}}", inner.join(" | "))
        }
        PatternNode::Conjunction(items) => {
            let inner: Vec<String> = items.iter().map(pattern_summary).collect();
            format!("[{}]", inner.join(" "))
        }
        PatternNode::AnyOrder(items) => {
            let inner: Vec<String> = items.iter().map(pattern_summary).collect();
            format!("<{}>", inner.join(" "))
        }
        PatternNode::Subsequence(items) => {
            let inner: Vec<String> = items.iter().map(pattern_summary).collect();
            inner.join(" ")
        }
        PatternNode::Negation(inner) => format!("!{}", pattern_summary(inner)),
        PatternNode::HelperCall { name, args } => format!("#{name}{}", arg_summary(args)),
        PatternNode::Predicate { name, args } => format!("{name}{}", arg_summary(args)),
        PatternNode::TypePredicate(t) => format!("{t}?"),
        PatternNode::ParamNumber(n) => format!("%{n}"),
        PatternNode::ParamNamed(p) => format!("%{p}"),
        PatternNode::ParamConst(p) => format!("%{p}"),
        PatternNode::Regexp { body, flags } => format!("/{body}/{flags}"),
        PatternNode::ParentRef(inner) => format!("^{}", pattern_summary(inner)),
        PatternNode::DescendRef(inner) => format!("`{}", pattern_summary(inner)),
        PatternNode::Ident(name) => name.clone(),
    }
}

/// Render a `#call` / `pred?` argument list, empty when there is none.
fn arg_summary(args: &[PatternNode]) -> String {
    if args.is_empty() {
        return String::new();
    }
    let inner: Vec<String> = args.iter().map(pattern_summary).collect();
    format!("({})", inner.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_pattern::lexer::Lexer;

    #[test]
    fn test_parser_simple_send() {
        let mut lexer = Lexer::new("(send nil? :expect ...)");
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        let ast = parser.parse().unwrap();

        match ast {
            PatternNode::NodeMatch {
                node_type,
                children,
            } => {
                assert_eq!(node_type, "send");
                assert_eq!(children.len(), 3);
                assert!(matches!(children[0], PatternNode::NilPredicate));
                assert!(matches!(&children[1], PatternNode::SymbolLiteral(s) if s == "expect"));
                assert!(matches!(children[2], PatternNode::Rest));
            }
            _ => panic!("Expected NodeMatch"),
        }
    }

    #[test]
    fn test_parser_alternatives() {
        let mut lexer = Lexer::new("{:first | :take}");
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        let ast = parser.parse().unwrap();

        match ast {
            PatternNode::Alternatives(alts) => {
                assert_eq!(alts.len(), 2);
            }
            _ => panic!("Expected Alternatives"),
        }
    }

    #[test]
    fn test_parser_nested() {
        let mut lexer = Lexer::new("(send (send _ :where ...) :first)");
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        let ast = parser.parse().unwrap();

        match ast {
            PatternNode::NodeMatch {
                node_type,
                children,
            } => {
                assert_eq!(node_type, "send");
                assert_eq!(children.len(), 2);
                match &children[0] {
                    PatternNode::NodeMatch { node_type, .. } => {
                        assert_eq!(node_type, "send");
                    }
                    _ => panic!("Expected inner NodeMatch"),
                }
            }
            _ => panic!("Expected NodeMatch"),
        }
    }

    #[test]
    fn test_parser_conjunction() {
        let mut lexer = Lexer::new("[!nil? send_type?]");
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        let ast = parser.parse().unwrap();

        match ast {
            PatternNode::Conjunction(items) => {
                assert_eq!(items.len(), 2);
                assert!(
                    matches!(&items[0], PatternNode::Negation(inner) if matches!(**inner, PatternNode::NilPredicate))
                );
                assert!(matches!(&items[1], PatternNode::TypePredicate(t) if t == "send"));
            }
            _ => panic!("Expected Conjunction, got {:?}", pattern_summary(&ast)),
        }
    }

    #[test]
    fn test_parser_capture_symbol() {
        let mut lexer = Lexer::new("${:first :take}");
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        let ast = parser.parse().unwrap();

        match ast {
            PatternNode::Capture { slot, inner } => {
                assert_eq!(slot, 0);
                match *inner {
                    PatternNode::Alternatives(alts) => {
                        assert_eq!(alts.len(), 2);
                        assert!(matches!(&alts[0], PatternNode::SymbolLiteral(s) if s == "first"));
                        assert!(matches!(&alts[1], PatternNode::SymbolLiteral(s) if s == "take"));
                    }
                    _ => panic!("Expected Alternatives inside Capture"),
                }
            }
            _ => panic!("Expected Capture"),
        }
    }

    #[test]
    fn test_parser_nil_literal() {
        let mut lexer = Lexer::new("nil");
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        let ast = parser.parse().unwrap();
        assert!(matches!(ast, PatternNode::NilLiteral));
    }

    #[test]
    fn test_parser_helper_call() {
        let mut lexer = Lexer::new("(send #expect? _ ...)");
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        let ast = parser.parse().unwrap();

        match ast {
            PatternNode::NodeMatch {
                node_type,
                children,
            } => {
                assert_eq!(node_type, "send");
                assert!(
                    matches!(&children[0], PatternNode::HelperCall { name, args } if name == "expect?" && args.is_empty())
                );
                assert!(matches!(&children[1], PatternNode::Wildcard));
                assert!(matches!(&children[2], PatternNode::Rest));
            }
            _ => panic!("Expected NodeMatch"),
        }
    }

    #[test]
    fn test_parser_deeply_nested() {
        let mut lexer = Lexer::new("(block (send (send nil? :described_class) :new ...) (args) _)");
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        let ast = parser.parse().unwrap();

        match ast {
            PatternNode::NodeMatch {
                node_type,
                children,
            } => {
                assert_eq!(node_type, "block");
                assert_eq!(children.len(), 3);
                match &children[0] {
                    PatternNode::NodeMatch {
                        node_type,
                        children,
                    } => {
                        assert_eq!(node_type, "send");
                        match &children[0] {
                            PatternNode::NodeMatch { node_type, .. } => {
                                assert_eq!(node_type, "send");
                            }
                            _ => panic!("Expected inner send node"),
                        }
                    }
                    _ => panic!("Expected NodeMatch for send"),
                }
            }
            _ => panic!("Expected NodeMatch for block"),
        }
    }

    #[test]
    fn test_pattern_summary() {
        let node = PatternNode::NodeMatch {
            node_type: "send".to_string(),
            children: vec![
                PatternNode::Wildcard,
                PatternNode::SymbolLiteral("foo".to_string()),
                PatternNode::Rest,
            ],
        };
        let summary = pattern_summary(&node);
        assert_eq!(summary, "(send _ :foo ...)");
    }

    #[test]
    fn test_pattern_summary_alternatives() {
        let node = PatternNode::Alternatives(vec![
            PatternNode::SymbolLiteral("foo".to_string()),
            PatternNode::SymbolLiteral("bar".to_string()),
        ]);
        assert_eq!(pattern_summary(&node), "{:foo | :bar}");
    }

    #[test]
    fn test_pattern_summary_conjunction() {
        let node = PatternNode::Conjunction(vec![
            PatternNode::NilPredicate,
            PatternNode::TypePredicate("send".to_string()),
        ]);
        assert_eq!(pattern_summary(&node), "[nil? send?]");
    }

    #[test]
    fn test_pattern_summary_capture() {
        let node = PatternNode::Capture {
            slot: 0,
            inner: Box::new(PatternNode::Wildcard),
        };
        assert_eq!(pattern_summary(&node), "$_");
    }

    #[test]
    fn test_pattern_summary_negation() {
        let node = PatternNode::Negation(Box::new(PatternNode::NilPredicate));
        assert_eq!(pattern_summary(&node), "!nil?");
    }

    /// Collect `(slot, summary)` for every capture in the tree, pre-order.
    fn capture_slots(node: &PatternNode) -> Vec<(usize, String)> {
        let mut out = Vec::new();
        fn walk(node: &PatternNode, out: &mut Vec<(usize, String)>) {
            match node {
                PatternNode::Capture { slot, inner } => {
                    out.push((*slot, pattern_summary(inner)));
                    walk(inner, out);
                }
                PatternNode::NodeMatch { children, .. } => {
                    for child in children {
                        walk(child, out);
                    }
                }
                PatternNode::Alternatives(items)
                | PatternNode::Conjunction(items)
                | PatternNode::Subsequence(items) => {
                    for item in items {
                        walk(item, out);
                    }
                }
                PatternNode::Negation(inner)
                | PatternNode::ParentRef(inner)
                | PatternNode::DescendRef(inner) => walk(inner, out),
                _ => {}
            }
        }
        walk(node, &mut out);
        out
    }

    fn parse_pattern(src: &str) -> (Option<PatternNode>, usize, Option<PatternError>) {
        let mut lexer = Lexer::new(src);
        let mut parser = Parser::new(lexer.tokenize());
        let ast = parser.parse();
        (ast, parser.capture_count(), parser.error().cloned())
    }

    #[test]
    fn test_capture_slots_are_numbered_in_source_order() {
        let (ast, count, err) = parse_pattern("(send $(send $_ :% (int 2)) ${:== :!=} (int $0))");
        assert!(err.is_none());
        assert_eq!(count, 4);
        let slots = capture_slots(&ast.unwrap());
        assert_eq!(
            slots.iter().map(|(slot, _)| *slot).collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        assert_eq!(slots[0].1, "(send $_ :% (int 2))");
        assert_eq!(slots[1].1, "_");
        assert_eq!(slots[2].1, "{:== | :!=}");
        assert_eq!(slots[3].1, "0");
    }

    #[test]
    fn test_union_branches_share_capture_slots() {
        // RuboCop resets the slot counter for each branch: only one branch
        // ever runs, so both `$_` occupy slot 0 and the union declares 1.
        let (ast, count, err) = parse_pattern("{(send $_ :a) (send $_ :b)}");
        assert!(err.is_none());
        assert_eq!(count, 1);
        let slots = capture_slots(&ast.unwrap());
        assert_eq!(
            slots.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            vec![0, 0]
        );
    }

    #[test]
    fn test_captures_after_union_continue_from_branch_width() {
        let (ast, count, err) = parse_pattern("(send {(send $_ $_) (send $_ $_)} $_)");
        assert!(err.is_none());
        assert_eq!(count, 3);
        let slots = capture_slots(&ast.unwrap());
        assert_eq!(
            slots.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            vec![0, 1, 0, 1, 2]
        );
    }

    #[test]
    fn test_unbalanced_union_captures_is_rejected() {
        let (ast, _, err) = parse_pattern("{(send $_ :a) (send _ :b)}");
        assert!(ast.is_none());
        assert_eq!(
            err,
            Some(PatternError::UnbalancedUnionCaptures {
                expected: 1,
                found: 0
            })
        );
    }

    #[test]
    fn test_capture_rest_gets_its_own_slot() {
        let (ast, count, err) = parse_pattern("(send _ :foo $...)");
        assert!(err.is_none());
        assert_eq!(count, 1);
        let slots = capture_slots(&ast.unwrap());
        assert_eq!(slots, vec![(0, "...".to_string())]);
    }

    #[test]
    fn test_patterns_without_captures_have_zero_count() {
        let (_, count, err) = parse_pattern("(send nil? :require ...)");
        assert!(err.is_none());
        assert_eq!(count, 0);
    }

    #[test]
    fn test_pipe_groups_terms_into_a_subsequence_branch() {
        let (ast, count, err) = parse_pattern("{(int 1) | (int 2) (int 3)}");
        assert!(err.is_none());
        assert_eq!(count, 0);
        match ast.unwrap() {
            PatternNode::Alternatives(alts) => {
                assert_eq!(alts.len(), 2);
                assert!(matches!(alts[0], PatternNode::NodeMatch { .. }));
                match &alts[1] {
                    PatternNode::Subsequence(items) => assert_eq!(items.len(), 2),
                    other => panic!("expected subsequence, got {}", pattern_summary(other)),
                }
            }
            other => panic!("expected alternatives, got {}", pattern_summary(&other)),
        }
    }

    #[test]
    fn test_without_pipes_each_term_is_its_own_branch() {
        let (ast, _, _) = parse_pattern("{(int 1) (int 2) (int 3)}");
        match ast.unwrap() {
            PatternNode::Alternatives(alts) => {
                assert_eq!(alts.len(), 3);
                assert!(
                    alts.iter()
                        .all(|a| !matches!(a, PatternNode::Subsequence(_)))
                );
            }
            other => panic!("expected alternatives, got {}", pattern_summary(&other)),
        }
    }

    #[test]
    fn test_captures_in_a_subsequence_branch_are_numbered_within_the_branch() {
        // Style/ComparableBetween shape: both branches bind two values.
        let (ast, count, err) = parse_pattern("(send {$_ :>= $_ | $_ :<= $_})");
        assert!(err.is_none(), "unexpected error: {err:?}");
        assert_eq!(count, 2);
        let slots = capture_slots(&ast.unwrap());
        assert_eq!(
            slots.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            vec![0, 1, 0, 1]
        );
    }

    #[test]
    fn test_unbalanced_subsequence_branches_are_rejected() {
        let (ast, _, err) = parse_pattern("{$_ :>= $_ | $_ :<= _}");
        assert!(ast.is_none());
        assert_eq!(
            err,
            Some(PatternError::UnbalancedUnionCaptures {
                expected: 2,
                found: 1
            })
        );
    }

    fn parse_ok(pattern: &str) -> PatternNode {
        let mut lexer = Lexer::new(pattern);
        let mut parser = Parser::new(lexer.tokenize());
        parser.parse().expect("pattern should parse")
    }

    #[test]
    fn test_parser_helper_call_with_symbol_arg() {
        match parse_ok("#global_const?(:Proc)") {
            PatternNode::HelperCall { name, args } => {
                assert_eq!(name, "global_const?");
                assert!(matches!(&args[0], PatternNode::SymbolLiteral(s) if s == "Proc"));
                assert_eq!(args.len(), 1);
            }
            other => panic!("expected HelperCall, got {other:?}"),
        }
    }

    #[test]
    fn test_parser_helper_call_with_union_arg() {
        // Upstream compiles a `{}` of atoms into a `Set` (`atom_subcompiler.rb`).
        match parse_ok("#global_const?({:Class :Module :Struct})") {
            PatternNode::HelperCall { name, args } => {
                assert_eq!(name, "global_const?");
                match &args[0] {
                    PatternNode::Alternatives(alts) => assert_eq!(alts.len(), 3),
                    other => panic!("expected Alternatives, got {other:?}"),
                }
            }
            other => panic!("expected HelperCall, got {other:?}"),
        }
    }

    #[test]
    fn test_parser_helper_call_with_several_args() {
        match parse_ok("#belongs_to?(%1, %2)") {
            PatternNode::HelperCall { name, args } => {
                assert_eq!(name, "belongs_to?");
                assert!(matches!(args[0], PatternNode::ParamNumber(1)));
                assert!(matches!(args[1], PatternNode::ParamNumber(2)));
            }
            other => panic!("expected HelperCall, got {other:?}"),
        }
    }

    #[test]
    fn test_parser_helper_call_without_args_is_followed_by_a_sequence() {
        // `#fn (seq)` — the whitespace makes the parenthesis a sequence, so the
        // call keeps an empty argument list and the sequence is a sibling.
        match parse_ok("{#foo (send nil? :bar)}") {
            PatternNode::Alternatives(alts) => {
                assert_eq!(alts.len(), 2);
                assert!(
                    matches!(&alts[0], PatternNode::HelperCall { name, args } if name == "foo" && args.is_empty())
                );
                assert!(matches!(&alts[1], PatternNode::NodeMatch { .. }));
            }
            other => panic!("expected Alternatives, got {other:?}"),
        }
    }

    #[test]
    fn test_parser_node_predicate_with_arg() {
        match parse_ok("method?(:freeze)") {
            PatternNode::Predicate { name, args } => {
                assert_eq!(name, "method?");
                assert!(matches!(&args[0], PatternNode::SymbolLiteral(s) if s == "freeze"));
            }
            other => panic!("expected Predicate, got {other:?}"),
        }
    }

    #[test]
    fn test_parser_param_forms() {
        assert!(matches!(parse_ok("%"), PatternNode::ParamNumber(1)));
        assert!(matches!(parse_ok("%2"), PatternNode::ParamNumber(2)));
        assert!(matches!(parse_ok("%name"), PatternNode::ParamNamed(n) if n == "name"));
        assert!(
            matches!(parse_ok("%RuboCop::AST::Node::VARIABLES"), PatternNode::ParamConst(n) if n == "RuboCop::AST::Node::VARIABLES")
        );
        assert!(
            matches!(parse_ok("CANDIDATE_METHODS"), PatternNode::ParamConst(n) if n == "CANDIDATE_METHODS")
        );
    }

    #[test]
    fn test_parser_captures_inside_an_arg_list_are_numbered() {
        // `$` is legal inside an argument list (`arg_list: node_pattern`).
        let mut lexer = Lexer::new("(send $_ #foo($_) $_)");
        let mut parser = Parser::new(lexer.tokenize());
        parser.parse().expect("pattern should parse");
        assert_eq!(parser.capture_count(), 3);
    }

    #[test]
    fn test_pattern_summary_round_trips_calls_and_params() {
        assert_eq!(
            pattern_summary(&parse_ok("#global_const?(:Proc)")),
            "#global_const?(:Proc)"
        );
        assert_eq!(pattern_summary(&parse_ok("method?(:a)")), "method?(:a)");
        assert_eq!(pattern_summary(&parse_ok("%1")), "%1");
        assert_eq!(pattern_summary(&parse_ok("/ab/i")), "/ab/i");
    }
}
