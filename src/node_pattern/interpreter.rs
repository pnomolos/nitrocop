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
//! - A pattern list without `...` still tolerates extra trailing children,
//!   which RuboCop rejects on arity; with `...` present the arity is exact.
//!
//! ## Deferred
//!
//! HelperCall (#method) and ParamRef (%1) always return true (optimistic).
//! ParentRef (^) and DescendRef (`) always return true. Captures nested under
//! those stubs are bound optimistically too.

use super::captures::{CaptureValue, Captures, MatchEnv, dup_node};
use super::lexer::Lexer;
use super::parser::{Parser, PatternNode};

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
}

/// A parsed NodePattern plus the number of capture slots it allocates.
#[derive(Debug, Clone)]
pub struct CompiledPattern {
    ast: PatternNode,
    capture_count: usize,
}

impl CompiledPattern {
    /// Lex and parse `pattern_str`, returning `None` on a parse error or an
    /// invalid pattern (e.g. `{}` branches with different capture counts).
    #[must_use]
    pub fn compile(pattern_str: &str) -> Option<Self> {
        let mut lexer = Lexer::new(pattern_str);
        let mut parser = Parser::new(lexer.tokenize());
        let ast = parser.parse()?;
        Some(Self {
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
        let mut env = MatchEnv::new(self.capture_count);
        matches_node(&self.ast, node, &mut env)
    }

    /// Match `node`, returning the bound captures on success.
    ///
    /// The returned [`Captures`] always has [`CompiledPattern::capture_count`]
    /// slots; a slot can still be unbound if its capture sits under a stubbed
    /// term (`#pred`, `%param`, `^`, `` ` ``).
    #[must_use]
    pub fn match_captures<'pr>(&self, node: &ruby_prism::Node<'pr>) -> Option<Captures<'pr>> {
        let mut env = MatchEnv::new(self.capture_count);
        if matches_node(&self.ast, node, &mut env) {
            Some(env.into_captures())
        } else {
            None
        }
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

/// Get the NodePattern type name for a Prism node.
///
/// Returns the Parser gem type name (e.g. "send", "block", "if") that
/// corresponds to this Prism node, or `None` if unmapped.
fn parser_type_for_node(node: &ruby_prism::Node<'_>) -> Option<&'static str> {
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
        ruby_prism::Node::BlockNode { .. } => Some("block"),
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
        ruby_prism::Node::BeginNode { .. } => Some("begin"),
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
        ruby_prism::Node::ParametersNode { .. } => Some("args"),
        _ => None,
    }
}

/// Build the children list for a node given its Parser gem type.
///
/// The returned `Vec<MatchChild>` matches NodePattern positional semantics.
/// For `send`: `[receiver_or_Absent, Name(method_name), arg1, arg2, ...]`
fn get_children<'pr>(
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
        }
        "block" | "any_block" => {
            let block = node.as_block_node()?;
            // In Prism, the call is the parent of the BlockNode, not a child.
            // We push Absent here and let the pattern wildcard match it.
            // Most NodePattern block patterns use _ or a specific call pattern
            // which we handle permissively in Phase 1.
            children.push(MatchChild::Absent);
            match block.parameters() {
                Some(p) => children.push(MatchChild::Node(p)),
                None => children.push(MatchChild::Absent),
            }
            match block.body() {
                Some(b) => children.push(MatchChild::Node(b)),
                None => children.push(MatchChild::Absent),
            }
        }
        "def" => {
            let def = node.as_def_node()?;
            children.push(MatchChild::Name(def.name().as_slice()));
            match def.parameters() {
                Some(p) => children.push(MatchChild::Node(p.as_node())),
                None => children.push(MatchChild::Absent),
            }
            match def.body() {
                Some(b) => children.push(MatchChild::Node(b)),
                None => children.push(MatchChild::Absent),
            }
        }
        "defs" => {
            let def = node.as_def_node()?;
            match def.receiver() {
                Some(r) => children.push(MatchChild::Node(r)),
                None => children.push(MatchChild::Absent),
            }
            children.push(MatchChild::Name(def.name().as_slice()));
            match def.parameters() {
                Some(p) => children.push(MatchChild::Node(p.as_node())),
                None => children.push(MatchChild::Absent),
            }
            match def.body() {
                Some(b) => children.push(MatchChild::Node(b)),
                None => children.push(MatchChild::Absent),
            }
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
            let b = node.as_begin_node()?;
            match b.statements() {
                Some(s) => children.push(MatchChild::Node(s.as_node())),
                None => children.push(MatchChild::Absent),
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
            let lv = node.as_local_variable_read_node()?;
            children.push(MatchChild::Name(lv.name().as_slice()));
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
            // Content matched via special-case
        }
        "class" => {
            let c = node.as_class_node()?;
            children.push(MatchChild::Node(c.constant_path()));
            match c.superclass() {
                Some(s) => children.push(MatchChild::Node(s)),
                None => children.push(MatchChild::Absent),
            }
            match c.body() {
                Some(b) => children.push(MatchChild::Node(b)),
                None => children.push(MatchChild::Absent),
            }
        }
        "module" => {
            let m = node.as_module_node()?;
            children.push(MatchChild::Node(m.constant_path()));
            match m.body() {
                Some(b) => children.push(MatchChild::Node(b)),
                None => children.push(MatchChild::Absent),
            }
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
            let l = node.as_lambda_node()?;
            match l.parameters() {
                Some(p) => children.push(MatchChild::Node(p)),
                None => children.push(MatchChild::Absent),
            }
            match l.body() {
                Some(b) => children.push(MatchChild::Node(b)),
                None => children.push(MatchChild::Absent),
            }
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
            // ParametersNode — no positional children in simple patterns
        }
        _ => return None,
    }

    Some(children)
}

/// Match a PatternNode against a MatchChild (dispatcher).
fn matches_child<'pr>(
    pattern: &PatternNode,
    child: &MatchChild<'pr>,
    env: &mut MatchEnv<'pr>,
) -> bool {
    match child {
        MatchChild::Node(node) => matches_node(pattern, node, env),
        MatchChild::Absent => matches_absent(pattern, env),
        MatchChild::Name(bytes) => matches_name(pattern, bytes, bytes, env),
    }
}

/// The value a `$` binds for a given child, used when `$...` captures a run.
fn capture_value_for<'pr>(child: &MatchChild<'pr>) -> CaptureValue<'pr> {
    match child {
        MatchChild::Node(node) => CaptureValue::Node(dup_node(node)),
        MatchChild::Absent => CaptureValue::Absent,
        MatchChild::Name(bytes) => CaptureValue::Name(bytes),
    }
}

/// Match a pattern against a Prism AST node.
fn matches_node<'pr>(
    pattern: &PatternNode,
    node: &ruby_prism::Node<'pr>,
    env: &mut MatchEnv<'pr>,
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

        PatternNode::TypePredicate(typ) => parser_type_for_node(node) == Some(typ.as_str()),

        PatternNode::Ident(name) => parser_type_for_node(node) == Some(name.as_str()),

        PatternNode::NodeMatch {
            node_type,
            children: pattern_children,
        } => {
            let actual_type = parser_type_for_node(node);
            let type_matches = actual_type == Some(node_type.as_str())
                || (node_type == "any_block" && actual_type == Some("block"));

            if !type_matches {
                return false;
            }

            let mark = env.mark();

            // Value-only nodes: (int 42), (str "foo"), (sym :bar)
            if !pattern_children.is_empty() {
                match node_type.as_str() {
                    "int" => {
                        if matches_node(&pattern_children[0], node, env) {
                            return true;
                        }
                        env.rollback(mark);
                        return false;
                    }
                    "str" => {
                        if let Some(str_node) = node.as_string_node() {
                            // Compare against the unescaped value but capture a
                            // `'pr`-lived slice of the source.
                            let captured = str_node.content_loc().as_slice();
                            if matches_name(
                                &pattern_children[0],
                                str_node.unescaped(),
                                captured,
                                env,
                            ) {
                                return true;
                            }
                        }
                        env.rollback(mark);
                        return false;
                    }
                    "sym" => {
                        if let Some(sym) = node.as_symbol_node() {
                            let captured = sym
                                .value_loc()
                                .map_or_else(|| sym.location().as_slice(), |loc| loc.as_slice());
                            if matches_name(&pattern_children[0], sym.unescaped(), captured, env) {
                                return true;
                            }
                        }
                        env.rollback(mark);
                        return false;
                    }
                    _ => {}
                }
            }

            let effective_type = if node_type == "any_block" {
                "block"
            } else {
                node_type.as_str()
            };
            let Some(actual_children) = get_children(effective_type, node) else {
                return pattern_children.is_empty();
            };

            if matches_children_list(pattern_children, &actual_children, env) {
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

        PatternNode::HelperCall(_) => true,
        PatternNode::ParamRef(_) => true,
        PatternNode::ParentRef(_) => true,
        PatternNode::DescendRef(_) => true,

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

        // Likewise `<>`: it consumes a run of children, so it is only
        // meaningful as a term of a child list (RuboCop forbids it in sequence
        // head position too — `ForbidInSeqHead`, `node.rb:179`).
        PatternNode::AnyOrder(_) => false,

        PatternNode::Rest => true,
    }
}

/// Match a pattern against an absent child (`nil?` predicate target).
fn matches_absent<'pr>(pattern: &PatternNode, env: &mut MatchEnv<'pr>) -> bool {
    match pattern {
        PatternNode::Wildcard => true,
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
        PatternNode::HelperCall(_) => true,
        PatternNode::ParamRef(_) => true,
        PatternNode::ParentRef(_) => true,
        PatternNode::DescendRef(_) => true,
        PatternNode::Rest => true,
        _ => false,
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
    env: &mut MatchEnv<'pr>,
) -> bool {
    match pattern {
        PatternNode::Wildcard => true,
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
        PatternNode::HelperCall(_) => true,
        PatternNode::ParamRef(_) => true,
        PatternNode::ParentRef(_) => true,
        PatternNode::DescendRef(_) => true,
        PatternNode::Rest => true,
        _ => false,
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
        PatternNode::Subsequence(_) | PatternNode::AnyOrder(_) => true,
        PatternNode::Capture { inner, .. } if matches!(**inner, PatternNode::AnyOrder(_)) => true,
        PatternNode::Alternatives(alts) => alts.iter().any(is_variadic_term),
        _ => as_rest_term(pattern).is_some(),
    }
}

/// Whether a term can consume an unbounded number of children.
fn contains_rest(pattern: &PatternNode) -> bool {
    match pattern {
        PatternNode::Alternatives(items)
        | PatternNode::Subsequence(items)
        | PatternNode::AnyOrder(items) => items.iter().any(contains_rest),
        // `$<a ...>` is unbounded exactly when the group it wraps is.
        PatternNode::Capture { inner, .. } if matches!(**inner, PatternNode::AnyOrder(_)) => {
            contains_rest(inner)
        }
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
    env: &mut MatchEnv<'pr>,
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
    env: &mut MatchEnv<'pr>,
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

/// Match a list of pattern children against a list of actual children.
///
/// A rest term (`...`, `$...`) matches a variable-length run, so the walk
/// backtracks over every split point and rewinds the captures written by a
/// rejected split. With a rest term present the arity is exact — the rest
/// absorbs the slack, as in RuboCop's sequence compiler. Without one, extra
/// trailing children are still tolerated (pre-existing permissive behaviour).
fn matches_children_list<'pr>(
    patterns: &[PatternNode],
    actuals: &[MatchChild<'pr>],
    env: &mut MatchEnv<'pr>,
) -> bool {
    let terms: Vec<&PatternNode> = patterns.iter().collect();
    let exact = patterns.iter().any(contains_rest);
    match_sequence(&terms, actuals, env, exact)
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
    env: &mut MatchEnv<'pr>,
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

    /// Helper: get first statement from parsed Ruby source.
    fn first_stmt<'a>(result: &'a ruby_prism::ParseResult<'a>) -> ruby_prism::Node<'a> {
        let root = result.node();
        let program = root.as_program_node().unwrap();
        let stmts = program.statements();
        stmts.body().iter().next().unwrap()
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
        let compiled = CompiledPattern::compile(pattern).unwrap();
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
            let compiled = CompiledPattern::compile(entry.pattern).unwrap_or_else(|| {
                panic!("{} failed to compile: {}", entry.cop_name, entry.pattern)
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
    fn test_helper_call_always_true() {
        let source = b"obj.foo";
        let result = ruby_prism::parse(source);
        let node = first_stmt(&result);

        // HelperCall patterns are always-true in Phase 1
        assert!(interpret_pattern("(send #any_helper? :foo)", &node));
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

        let compiled = CompiledPattern::compile(pattern).unwrap();
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
        let compiled = CompiledPattern::compile(pattern).unwrap();
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
        let compiled = CompiledPattern::compile(pattern).unwrap();
        assert_eq!(compiled.capture_count(), 1);

        let result = ruby_prism::parse(b"RSpec.describe 'thing', :pending do\nend");
        let node = first_stmt(&result);
        let call = node.as_call_node().unwrap().as_node();
        assert_eq!(
            captured_name(&compiled.match_captures(&call).unwrap(), 0),
            "pending"
        );

        let result = ruby_prism::parse(b"RSpec.describe 'thing', skip: true do\nend");
        let node = first_stmt(&result);
        let call = node.as_call_node().unwrap().as_node();
        assert_eq!(
            captured_name(&compiled.match_captures(&call).unwrap(), 0),
            "skip"
        );

        // No pending/skip metadata at all.
        let result = ruby_prism::parse(b"RSpec.describe 'thing', :focus do\nend");
        let node = first_stmt(&result);
        let call = node.as_call_node().unwrap().as_node();
        assert!(compiled.match_captures(&call).is_none());
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
}
