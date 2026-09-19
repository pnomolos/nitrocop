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
    /// [a b c]
    Conjunction(Vec<PatternNode>),
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
    /// #helper_method
    HelperCall(String),
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
    /// %param
    ParamRef(String),
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
}

impl std::fmt::Display for PatternError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PatternError::UnbalancedUnionCaptures { expected, found } => write!(
                f,
                "each branch of {{}} must have the same number of captures (expected {expected}, found {found})"
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
                Some(PatternNode::HelperCall(name))
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
            Token::ParamRef(ref s) => {
                let s = s.clone();
                self.advance();
                Some(PatternNode::ParamRef(s))
            }
            _ => None,
        }
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

    /// Parse `{a b c}` / `{a | b}`.
    ///
    /// Capture slots are shared across branches: every branch restarts from the
    /// slot base the union entered with, and all branches must allocate the same
    /// number of slots — RuboCop's `Compiler#enforce_same_captures`
    /// (`compiler.rb:82-95`).
    fn parse_alternatives(&mut self) -> Option<PatternNode> {
        self.expect(&Token::LBrace);
        let mut alts = Vec::new();

        let base = self.captures;
        let mut branch_captures: Option<usize> = None;

        while self.peek().is_some() && self.peek() != Some(&Token::RBrace) {
            // Skip pipe separators
            if self.peek() == Some(&Token::Pipe) {
                self.advance();
                continue;
            }
            self.captures = base;
            let Some(node) = self.parse_node() else { break };
            alts.push(node);

            let allocated = self.captures - base;
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
        }

        self.captures = base + branch_captures.unwrap_or(0);
        self.expect(&Token::RBrace);
        Some(PatternNode::Alternatives(alts))
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
        PatternNode::Negation(inner) => format!("!{}", pattern_summary(inner)),
        PatternNode::HelperCall(name) => format!("#{name}"),
        PatternNode::TypePredicate(t) => format!("{t}?"),
        PatternNode::ParamRef(p) => format!("%{p}"),
        PatternNode::ParentRef(inner) => format!("^{}", pattern_summary(inner)),
        PatternNode::DescendRef(inner) => format!("`{}", pattern_summary(inner)),
        PatternNode::Ident(name) => name.clone(),
    }
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
            PatternNode::Capture { slot, inner } => match {
                assert_eq!(slot, 0);
                *inner
            } {
                PatternNode::Alternatives(alts) => {
                    assert_eq!(alts.len(), 2);
                    assert!(matches!(&alts[0], PatternNode::SymbolLiteral(s) if s == "first"));
                    assert!(matches!(&alts[1], PatternNode::SymbolLiteral(s) if s == "take"));
                }
                _ => panic!("Expected Alternatives inside Capture"),
            },
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
                assert!(matches!(&children[0], PatternNode::HelperCall(n) if n == "expect?"));
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
                PatternNode::Alternatives(items) | PatternNode::Conjunction(items) => {
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
}
