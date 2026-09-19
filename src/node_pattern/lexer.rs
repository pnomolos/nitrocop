//! NodePattern DSL lexer.
//!
//! Tokenizes RuboCop NodePattern strings like `(send nil? :expect ...)`.

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    LParen,
    RParen,
    LBrace,                // {
    RBrace,                // }
    LBracket,              // [
    RBracket,              // ]
    Capture,               // $
    Wildcard,              // _
    Rest,                  // ...
    Negation,              // !
    Pipe,                  // | inside alternatives
    HelperCall(String),    // #method_name or #method_name?
    SymbolLiteral(String), // :sym
    IntLiteral(i64),
    FloatLiteral(String),
    StringLiteral(String),
    NilPredicate,          // nil?
    TruePredicate,         // true?
    FalsePredicate,        // false?
    TypePredicate(String), // int?, str?, sym?, etc.
    /// A node predicate method call — `lexer.rex`'s `tPREDICATE`
    /// (`IDENTIFIER?`), sent to the *matched node*.
    ///
    /// The name keeps its `?`. Type predicates (`send_type?`, `nil?`, …) are
    /// lexed as [`Token::TypePredicate`] / [`Token::NilPredicate`] instead.
    Predicate(String),
    Ident(String), // node type names: send, block, def, etc.
    /// `%1`, or a bare `%` — `lexer.rex`'s `tPARAM_NUMBER`, which maps `%` to
    /// `%1`. The value is the 1-based positional parameter index; `%0` exists
    /// too and stays 0.
    ParamNumber(usize),
    /// `%name` — `lexer.rex`'s `tPARAM_NAMED` (`%[a-z_]+`).
    ParamNamed(String),
    /// `%Const` or a bare `Const` — `lexer.rex`'s `tPARAM_CONST`
    /// (`%?([A-Z:][a-zA-Z_:]+)`; the `%` is optional, so `RuboCop::AST::Node`
    /// on its own is a constant reference too).
    ParamConst(String),
    /// `/body/flags` — `lexer.rex`'s `tREGEXP`.
    Regexp {
        /// The regexp source between the slashes, escapes intact.
        body: String,
        /// The `imxo` flag letters that followed the closing slash.
        flags: String,
    },
    /// `(` opening a function-call/predicate argument list — `lexer.rex`'s
    /// `tARG_LIST`, emitted only in the `:ARG` state, i.e. when the `(`
    /// immediately follows `#call` or `pred?` with no intervening whitespace.
    ArgList,
    Comma,    // , between arguments
    Caret,    // ^ (parent node ref)
    Backtick, // ` (descend operator)
    LAngle,   // < (any-order group)
    RAngle,   // > (any-order group)
}

pub struct Lexer<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn advance(&mut self) -> Option<u8> {
        let ch = self.input.get(self.pos).copied()?;
        self.pos += 1;
        Some(ch)
    }

    fn skip_whitespace(&mut self) {
        while self.pos < self.input.len() {
            let ch = self.input[self.pos];
            if ch == b' ' || ch == b'\t' || ch == b'\n' || ch == b'\r' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn read_while(&mut self, pred: impl Fn(u8) -> bool) -> String {
        let start = self.pos;
        while self.pos < self.input.len() && pred(self.input[self.pos]) {
            self.pos += 1;
        }
        String::from_utf8_lossy(&self.input[start..self.pos]).into_owned()
    }

    /// First byte of a `#function_call` name (`CALL` in `lexer.rex`).
    fn is_call_start(ch: u8) -> bool {
        ch.is_ascii_alphabetic() || ch == b'_'
    }

    /// Consume a `#{...}` interpolation, honouring nested braces.
    fn skip_interpolation(&mut self) {
        self.advance(); // opening `{`
        let mut depth = 1usize;
        while depth > 0 {
            let Some(ch) = self.advance() else { break };
            match ch {
                b'{' => depth += 1,
                b'}' => depth -= 1,
                _ => {}
            }
        }
    }

    fn is_ident_char(ch: u8) -> bool {
        ch.is_ascii_alphanumeric() || ch == b'_' || ch == b'-'
    }

    /// A byte that can appear inside `CONST_NAME` (`/[A-Z:][a-zA-Z_:]+/`).
    fn is_const_char(ch: u8) -> bool {
        ch.is_ascii_alphabetic() || ch == b'_' || ch == b':'
    }

    /// Read `CONST_NAME` at the cursor, or `None` when what follows is too
    /// short to be one (the macro needs at least two characters).
    fn read_const_name(&mut self) -> Option<String> {
        let start = self.pos;
        let name = self.read_while(Self::is_const_char);
        if name.len() >= 2 {
            Some(name)
        } else {
            self.pos = start;
            None
        }
    }

    /// Lex the three `%param` forms, cursor just past the `%`.
    ///
    /// `lexer.rex` tries them in this order: `%?(CONST_NAME)` → `tPARAM_CONST`,
    /// `%([a-z_]+)` → `tPARAM_NAMED`, `%(\d*)` → `tPARAM_NUMBER` with an empty
    /// digit run meaning `1`.
    fn read_param(&mut self) -> Token {
        if self
            .peek()
            .is_some_and(|c| c.is_ascii_uppercase() || c == b':')
            && let Some(name) = self.read_const_name()
        {
            return Token::ParamConst(name);
        }
        if self
            .peek()
            .is_some_and(|c| c.is_ascii_lowercase() || c == b'_')
        {
            let name = self.read_while(|c| c.is_ascii_lowercase() || c == b'_');
            return Token::ParamNamed(name);
        }
        let digits = self.read_while(|c| c.is_ascii_digit());
        // `lexer.rex`: `emit(:tPARAM_NUMBER) { |s| s.empty? ? 1 : s.to_i }`.
        Token::ParamNumber(digits.parse::<usize>().unwrap_or(1))
    }

    /// Lex `/body/flags` at the cursor, or `None` when there is no closing
    /// slash (in which case the caller falls back to skipping the byte).
    ///
    /// `lexer.rex`: `REGEXP = /\/(#{REGEXP_BODY})(?<!\\)\/([imxo]*)/`.
    fn read_regexp(&mut self) -> Option<Token> {
        let start = self.pos + 1;
        let mut end = start;
        loop {
            match self.input.get(end) {
                None => return None,
                Some(b'\\') => end += 2,
                Some(b'/') => break,
                Some(_) => end += 1,
            }
        }
        let body = String::from_utf8_lossy(&self.input[start..end]).into_owned();
        self.pos = end + 1;
        let flags = self.read_while(|c| b"imxo".contains(&c));
        Some(Token::Regexp { body, flags })
    }

    pub fn tokenize(&mut self) -> Vec<Token> {
        let mut tokens = Vec::new();
        // `lexer.rex`'s `:ARG` state: set right after `tFUNCTION_CALL` /
        // `tPREDICATE`, and cleared by the very next scan. Only a `(` that
        // follows with no whitespace is an argument list; `#fn (seq)` is a
        // call followed by a sequence.
        let mut arg_state = false;

        loop {
            if std::mem::take(&mut arg_state) && self.peek() == Some(b'(') {
                self.advance();
                tokens.push(Token::ArgList);
                continue;
            }
            self.skip_whitespace();
            let Some(ch) = self.peek() else { break };

            match ch {
                b'(' => {
                    self.advance();
                    tokens.push(Token::LParen);
                }
                b')' => {
                    self.advance();
                    tokens.push(Token::RParen);
                }
                b'{' => {
                    self.advance();
                    tokens.push(Token::LBrace);
                }
                b'}' => {
                    self.advance();
                    tokens.push(Token::RBrace);
                }
                b'[' => {
                    self.advance();
                    tokens.push(Token::LBracket);
                }
                b']' => {
                    self.advance();
                    tokens.push(Token::RBracket);
                }
                b'$' => {
                    self.advance();
                    tokens.push(Token::Capture);
                }
                b'|' => {
                    self.advance();
                    tokens.push(Token::Pipe);
                }
                b'^' => {
                    self.advance();
                    tokens.push(Token::Caret);
                }
                b'`' => {
                    self.advance();
                    tokens.push(Token::Backtick);
                }
                b'<' => {
                    self.advance();
                    tokens.push(Token::LAngle);
                }
                b'>' => {
                    self.advance();
                    tokens.push(Token::RAngle);
                }
                b'!' => {
                    self.advance();
                    tokens.push(Token::Negation);
                }
                b'.' => {
                    // Check for ...
                    if self.pos + 2 < self.input.len()
                        && self.input[self.pos + 1] == b'.'
                        && self.input[self.pos + 2] == b'.'
                    {
                        self.pos += 3;
                        tokens.push(Token::Rest);
                    } else {
                        // Skip unknown
                        self.advance();
                    }
                }
                b'#' => {
                    self.advance();
                    // `#name` is a function call; anything else starts a comment
                    // that runs to end of line (`lexer.rex`: `/\#(CALL)/` is
                    // tried before `/\#.*/`).
                    if self.peek().is_some_and(Self::is_call_start) {
                        let mut name = self.read_while(|c| Self::is_ident_char(c) || c == b'?');
                        // `CALL` in `lexer.rex` is `(?:CONST_NAME\.)?IDENTIFIER[!?]?`,
                        // so `#Examples.all` is a single function-call token.
                        if name.starts_with(|c: char| c.is_ascii_uppercase())
                            && self.peek() == Some(b'.')
                        {
                            self.advance();
                            name.push('.');
                            name.push_str(
                                &self.read_while(|c| Self::is_ident_char(c) || c == b'?'),
                            );
                        }
                        tokens.push(Token::HelperCall(name));
                        arg_state = true;
                    } else if self.peek() == Some(b'{') {
                        // Ruby string interpolation left in a pattern extracted
                        // from vendor source: skip the whole `#{...}` so the
                        // braces around it stay balanced.
                        self.skip_interpolation();
                    } else {
                        self.read_while(|c| c != b'\n');
                    }
                }
                b':' => {
                    self.advance();
                    // Could be :: (cbase) — for now treat as symbol
                    if self.peek() == Some(b':') {
                        self.advance();
                        tokens.push(Token::Ident("cbase".to_string()));
                    } else {
                        // Ruby symbols can be operator method names: :==, :===, :!=,
                        // :<=>, :<=, :>=, :<<, :>>, :+, :-, :*, :/, :%, :!, :[],
                        // :[]=, :!~, :=~, :&, :|, :^, :~, :**
                        let name = if self.peek().is_some_and(|c| b"=<>!~+*&|^/%-.".contains(&c)) {
                            self.read_while(|c| b"=<>!~+*&|^/%-.[]".contains(&c))
                        } else {
                            self.read_while(|c| Self::is_ident_char(c) || c == b'?')
                        };
                        tokens.push(Token::SymbolLiteral(name));
                    }
                }
                b'%' => {
                    self.advance();
                    let param = self.read_param();
                    tokens.push(param);
                }
                b',' => {
                    self.advance();
                    tokens.push(Token::Comma);
                }
                b'/' => {
                    if let Some(regexp) = self.read_regexp() {
                        tokens.push(regexp);
                    } else {
                        self.advance();
                    }
                }
                b'\'' | b'"' => {
                    let quote = self.advance().unwrap();
                    let s = self.read_while(move |c| c != quote);
                    self.advance(); // closing quote
                    tokens.push(Token::StringLiteral(s));
                }
                b'_' => {
                    // Could be just _ (wildcard) or an identifier starting with _
                    let word = self.read_while(|c| Self::is_ident_char(c) || c == b'?');
                    if word == "_" {
                        tokens.push(Token::Wildcard);
                    } else {
                        tokens.push(Token::Ident(word));
                    }
                }
                _ if ch.is_ascii_digit()
                    || (ch == b'-'
                        && self
                            .input
                            .get(self.pos + 1)
                            .is_some_and(|c| c.is_ascii_digit())) =>
                {
                    let num_str = self.read_while(|c| c.is_ascii_digit() || c == b'-' || c == b'.');
                    if num_str.contains('.') {
                        tokens.push(Token::FloatLiteral(num_str));
                    } else if let Ok(n) = num_str.parse::<i64>() {
                        tokens.push(Token::IntLiteral(n));
                    } else {
                        tokens.push(Token::Ident(num_str));
                    }
                }
                // `CONST_NAME` without a leading `%`: `lexer.rex` makes the
                // `%` optional (`/%?(#{CONST_NAME})/`), so a bare constant
                // path is a `tPARAM_CONST` too. `NODE_TYPE`/`IDENTIFIER` both
                // start lowercase, so an uppercase word can only be this.
                _ if ch.is_ascii_uppercase() => {
                    if let Some(name) = self.read_const_name() {
                        tokens.push(Token::ParamConst(name));
                    } else {
                        let word = self.read_while(|c| Self::is_ident_char(c) || c == b'?');
                        tokens.push(Token::Ident(word));
                    }
                }
                _ if ch.is_ascii_alphabetic() => {
                    let word = self.read_while(|c| Self::is_ident_char(c) || c == b'?');
                    match word.as_str() {
                        "nil?" => tokens.push(Token::NilPredicate),
                        "true?" => tokens.push(Token::TruePredicate),
                        "false?" => tokens.push(Token::FalsePredicate),
                        "int?" => tokens.push(Token::TypePredicate("int".to_string())),
                        "str?" => tokens.push(Token::TypePredicate("str".to_string())),
                        "sym?" => tokens.push(Token::TypePredicate("sym".to_string())),
                        "float?" => tokens.push(Token::TypePredicate("float".to_string())),
                        "array?" => tokens.push(Token::TypePredicate("array".to_string())),
                        "hash?" => tokens.push(Token::TypePredicate("hash".to_string())),
                        "regexp?" => tokens.push(Token::TypePredicate("regexp".to_string())),
                        _ if word.ends_with("_type?") => {
                            // Generic _type? predicate: strip `_type?` suffix
                            let stem = &word[..word.len() - 6]; // strip "_type?"
                            tokens.push(Token::TypePredicate(stem.replace('-', "_")));
                        }
                        // `IDENTIFIER?` is `tPREDICATE`: a method sent to the
                        // matched node, and the one other token that opens an
                        // argument list.
                        _ if word.ends_with('?') => {
                            tokens.push(Token::Predicate(word));
                            arg_state = true;
                        }
                        // RuboCop compiles a node type to `#{type.tr('-', '_')}_type?`
                        // (`node_pattern_subcompiler.rb:88-90`), so `block-pass`
                        // and `block_pass` are the same type.
                        _ => tokens.push(Token::Ident(word.replace('-', "_"))),
                    }
                }
                _ => {
                    // Skip unknown characters
                    self.advance();
                }
            }
        }

        tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lexer_basic() {
        let mut lexer = Lexer::new("(send nil? :expect ...)");
        let tokens = lexer.tokenize();
        assert_eq!(tokens[0], Token::LParen);
        assert_eq!(tokens[1], Token::Ident("send".to_string()));
        assert_eq!(tokens[2], Token::NilPredicate);
        assert_eq!(tokens[3], Token::SymbolLiteral("expect".to_string()));
        assert_eq!(tokens[4], Token::Rest);
        assert_eq!(tokens[5], Token::RParen);
    }

    #[test]
    fn test_lexer_alternatives() {
        let mut lexer = Lexer::new("{:first :take}");
        let tokens = lexer.tokenize();
        assert_eq!(tokens[0], Token::LBrace);
        assert_eq!(tokens[1], Token::SymbolLiteral("first".to_string()));
        assert_eq!(tokens[2], Token::SymbolLiteral("take".to_string()));
        assert_eq!(tokens[3], Token::RBrace);
    }

    #[test]
    fn test_lexer_capture() {
        let mut lexer = Lexer::new("$_");
        let tokens = lexer.tokenize();
        assert_eq!(tokens[0], Token::Capture);
        assert_eq!(tokens[1], Token::Wildcard);
    }

    #[test]
    fn test_lexer_helper_call() {
        let mut lexer = Lexer::new("#expect?");
        let tokens = lexer.tokenize();
        assert_eq!(tokens[0], Token::HelperCall("expect?".to_string()));
    }

    #[test]
    fn test_lexer_negation() {
        let mut lexer = Lexer::new("!nil?");
        let tokens = lexer.tokenize();
        assert_eq!(tokens, vec![Token::Negation, Token::NilPredicate]);
    }

    #[test]
    fn test_lexer_conjunction() {
        let mut lexer = Lexer::new("[!nil? send_type?]");
        let tokens = lexer.tokenize();
        assert_eq!(tokens[0], Token::LBracket);
        assert_eq!(tokens[1], Token::Negation);
        assert_eq!(tokens[2], Token::NilPredicate);
        assert_eq!(tokens[3], Token::TypePredicate("send".to_string()));
        assert_eq!(tokens[4], Token::RBracket);
    }

    #[test]
    fn test_lexer_int_literal() {
        let mut lexer = Lexer::new("42");
        let tokens = lexer.tokenize();
        assert_eq!(tokens, vec![Token::IntLiteral(42)]);
    }

    #[test]
    fn test_lexer_negative_int() {
        let mut lexer = Lexer::new("-1");
        let tokens = lexer.tokenize();
        assert_eq!(tokens, vec![Token::IntLiteral(-1)]);
    }

    #[test]
    fn test_lexer_string_literal() {
        let mut lexer = Lexer::new("'hello'");
        let tokens = lexer.tokenize();
        assert_eq!(tokens, vec![Token::StringLiteral("hello".to_string())]);
    }

    #[test]
    fn test_lexer_param_number() {
        for (input, expected) in [("%1", 1), ("%2", 2), ("%0", 0), ("%", 1), ("%12", 12)] {
            let mut lexer = Lexer::new(input);
            assert_eq!(
                lexer.tokenize(),
                vec![Token::ParamNumber(expected)],
                "failed for {input}"
            );
        }
    }

    #[test]
    fn test_lexer_param_named() {
        let mut lexer = Lexer::new("%method_name");
        assert_eq!(
            lexer.tokenize(),
            vec![Token::ParamNamed("method_name".to_string())]
        );
    }

    #[test]
    fn test_lexer_param_const() {
        for input in ["%RESTRICT_ON_SEND", "RESTRICT_ON_SEND"] {
            let mut lexer = Lexer::new(input);
            assert_eq!(
                lexer.tokenize(),
                vec![Token::ParamConst("RESTRICT_ON_SEND".to_string())],
                "failed for {input}"
            );
        }
    }

    #[test]
    fn test_lexer_param_const_keeps_the_whole_path() {
        // `CONST_NAME` is `/[A-Z:][a-zA-Z_:]+/`, so `::` stays inside the token.
        let mut lexer = Lexer::new("%RuboCop::AST::Node::VARIABLES");
        assert_eq!(
            lexer.tokenize(),
            vec![Token::ParamConst(
                "RuboCop::AST::Node::VARIABLES".to_string()
            )]
        );
    }

    #[test]
    fn test_lexer_arg_list_needs_no_whitespace() {
        // `#fn(arg)` is a call with an argument list…
        let mut lexer = Lexer::new("#global_const?(:Proc)");
        assert_eq!(
            lexer.tokenize(),
            vec![
                Token::HelperCall("global_const?".to_string()),
                Token::ArgList,
                Token::SymbolLiteral("Proc".to_string()),
                Token::RParen,
            ]
        );
        // …while `#fn (seq)` is a call followed by a sequence.
        let mut lexer = Lexer::new("#foo (send nil? :bar)");
        let tokens = lexer.tokenize();
        assert_eq!(tokens[0], Token::HelperCall("foo".to_string()));
        assert_eq!(tokens[1], Token::LParen);
    }

    #[test]
    fn test_lexer_arg_list_commas() {
        let mut lexer = Lexer::new("#belongs_to?(%1, :foo)");
        assert_eq!(
            lexer.tokenize(),
            vec![
                Token::HelperCall("belongs_to?".to_string()),
                Token::ArgList,
                Token::ParamNumber(1),
                Token::Comma,
                Token::SymbolLiteral("foo".to_string()),
                Token::RParen,
            ]
        );
    }

    #[test]
    fn test_lexer_node_predicate_and_its_arg_list() {
        let mut lexer = Lexer::new("method?(:freeze)");
        assert_eq!(
            lexer.tokenize(),
            vec![
                Token::Predicate("method?".to_string()),
                Token::ArgList,
                Token::SymbolLiteral("freeze".to_string()),
                Token::RParen,
            ]
        );
        // A bare predicate stays a predicate…
        let mut lexer = Lexer::new("literal?");
        assert_eq!(
            lexer.tokenize(),
            vec![Token::Predicate("literal?".to_string())]
        );
        // …but type predicates keep their own token.
        let mut lexer = Lexer::new("str_type?");
        assert_eq!(
            lexer.tokenize(),
            vec![Token::TypePredicate("str".to_string())]
        );
    }

    #[test]
    fn test_lexer_sequence_after_predicate_is_not_an_arg_list() {
        let mut lexer = Lexer::new("[literal? (send nil? :foo)]");
        let tokens = lexer.tokenize();
        assert_eq!(tokens[1], Token::Predicate("literal?".to_string()));
        assert_eq!(tokens[2], Token::LParen);
    }

    #[test]
    fn test_lexer_regexp_literal() {
        let mut lexer = Lexer::new("(str /^foo$/i)");
        assert_eq!(
            lexer.tokenize(),
            vec![
                Token::LParen,
                Token::Ident("str".to_string()),
                Token::Regexp {
                    body: "^foo$".to_string(),
                    flags: "i".to_string(),
                },
                Token::RParen,
            ]
        );
    }

    #[test]
    fn test_lexer_division_symbol_is_not_a_regexp() {
        let mut lexer = Lexer::new("(send _ :/ _)");
        let tokens = lexer.tokenize();
        assert_eq!(tokens[3], Token::SymbolLiteral("/".to_string()));
    }

    #[test]
    fn test_lexer_type_predicates() {
        for (input, expected_type) in [
            ("int?", "int"),
            ("str?", "str"),
            ("sym?", "sym"),
            ("float?", "float"),
            ("array?", "array"),
            ("hash?", "hash"),
            ("regexp?", "regexp"),
        ] {
            let mut lexer = Lexer::new(input);
            let tokens = lexer.tokenize();
            assert_eq!(
                tokens,
                vec![Token::TypePredicate(expected_type.to_string())],
                "Failed for input: {input}"
            );
        }
    }

    #[test]
    fn test_lexer_hyphenated_node_types_are_normalized() {
        let mut lexer = Lexer::new("(block-pass (sym :foo))");
        assert_eq!(lexer.tokenize()[1], Token::Ident("block_pass".to_string()));
        let mut lexer = Lexer::new("op-asgn_type?");
        assert_eq!(
            lexer.tokenize(),
            vec![Token::TypePredicate("op_asgn".to_string())]
        );
    }

    #[test]
    fn test_lexer_cbase() {
        let mut lexer = Lexer::new("::");
        let tokens = lexer.tokenize();
        assert_eq!(tokens, vec![Token::Ident("cbase".to_string())]);
    }

    #[test]
    fn test_lexer_generic_type_predicates() {
        for (input, expected_stem) in [
            ("send_type?", "send"),
            ("block_type?", "block"),
            ("const_type?", "const"),
            ("lvar_type?", "lvar"),
            ("any_block_type?", "any_block"),
            ("range_type?", "range"),
            ("csend_type?", "csend"),
            ("def_type?", "def"),
            ("dstr_type?", "dstr"),
        ] {
            let mut lexer = Lexer::new(input);
            let tokens = lexer.tokenize();
            assert_eq!(
                tokens,
                vec![Token::TypePredicate(expected_stem.to_string())],
                "Failed for input: {input}"
            );
        }
    }

    #[test]
    fn test_lexer_complex_pattern() {
        let mut lexer = Lexer::new("(send (send nil? :expect ...) :to (send nil? :receive ...))");
        let tokens = lexer.tokenize();
        assert_eq!(tokens[0], Token::LParen);
        assert_eq!(tokens[1], Token::Ident("send".to_string()));
        assert_eq!(tokens[2], Token::LParen);
        assert_eq!(tokens[3], Token::Ident("send".to_string()));
        assert_eq!(tokens[4], Token::NilPredicate);
        assert_eq!(tokens[5], Token::SymbolLiteral("expect".to_string()));
        assert_eq!(tokens[6], Token::Rest);
        assert_eq!(tokens[7], Token::RParen);
        assert_eq!(tokens[8], Token::SymbolLiteral("to".to_string()));
    }

    #[test]
    fn test_comment_is_skipped() {
        let mut lexer = Lexer::new("(send nil? :foo) # Array.new(3) { create(:user) }");
        let tokens = lexer.tokenize();
        assert_eq!(
            tokens,
            vec![
                Token::LParen,
                Token::Ident("send".to_string()),
                Token::NilPredicate,
                Token::SymbolLiteral("foo".to_string()),
                Token::RParen,
            ]
        );
    }

    #[test]
    fn test_comment_only_runs_to_end_of_line() {
        let mut lexer = Lexer::new("{\n  (int 1) # one\n  (int 2)\n}");
        let tokens = lexer.tokenize();
        assert_eq!(tokens.iter().filter(|t| **t == Token::LParen).count(), 2);
        assert_eq!(tokens.last(), Some(&Token::RBrace));
    }

    #[test]
    fn test_lexer_any_order() {
        let mut lexer = Lexer::new("<(sym :a) ...>");
        let tokens = lexer.tokenize();
        assert_eq!(tokens[0], Token::LAngle);
        assert_eq!(tokens[1], Token::LParen);
        assert_eq!(tokens[tokens.len() - 2], Token::Rest);
        assert_eq!(tokens[tokens.len() - 1], Token::RAngle);
    }

    #[test]
    fn test_lexer_captured_any_order() {
        let mut lexer = Lexer::new("$<int str>");
        assert_eq!(
            lexer.tokenize(),
            vec![
                Token::Capture,
                Token::LAngle,
                Token::Ident("int".to_string()),
                Token::Ident("str".to_string()),
                Token::RAngle,
            ]
        );
    }

    #[test]
    fn test_lexer_operator_symbols_are_not_angle_brackets() {
        let mut lexer = Lexer::new("{:< :> :<=> :<<}");
        let tokens = lexer.tokenize();
        assert_eq!(
            tokens,
            vec![
                Token::LBrace,
                Token::SymbolLiteral("<".to_string()),
                Token::SymbolLiteral(">".to_string()),
                Token::SymbolLiteral("<=>".to_string()),
                Token::SymbolLiteral("<<".to_string()),
                Token::RBrace,
            ]
        );
    }

    #[test]
    fn test_lexer_const_qualified_function_call() {
        let mut lexer = Lexer::new("{#ExampleGroups.all #Examples.all}");
        assert_eq!(
            lexer.tokenize(),
            vec![
                Token::LBrace,
                Token::HelperCall("ExampleGroups.all".to_string()),
                Token::HelperCall("Examples.all".to_string()),
                Token::RBrace,
            ]
        );
    }

    #[test]
    fn test_function_call_is_not_a_comment() {
        let mut lexer = Lexer::new("#mixin_method?");
        assert_eq!(
            lexer.tokenize(),
            vec![Token::HelperCall("mixin_method?".to_string())]
        );
    }

    #[test]
    fn test_ruby_interpolation_is_skipped_with_balanced_braces() {
        // Patterns extracted from vendor source can still hold `#{...}`.
        let mut lexer = Lexer::new("(send nil? {#{FILTERS.join(' ')}} $_)");
        let tokens = lexer.tokenize();
        assert_eq!(
            tokens,
            vec![
                Token::LParen,
                Token::Ident("send".to_string()),
                Token::NilPredicate,
                Token::LBrace,
                Token::RBrace,
                Token::Capture,
                Token::Wildcard,
                Token::RParen,
            ]
        );
    }
}
