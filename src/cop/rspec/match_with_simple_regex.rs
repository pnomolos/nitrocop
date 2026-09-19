use crate::cop::shared::node_type::CALL_NODE;
use crate::cop::shared::util::RSPEC_DEFAULT_INCLUDE;
use crate::cop::{Cop, CopConfig};
use crate::diagnostic::{Diagnostic, Severity};
use crate::parse::source::SourceFile;

/// Escape leaders that `Regexp::Parser` does *not* turn into an
/// `EscapeSequence::Literal`.
///
/// Derived by exhaustively probing regexp_parser 2.11.3 (the version resolved
/// by `rubocop-rspec`'s `regexp_parser >= 2.0` dependency) with `a\<c>b` for
/// every printable ASCII `<c>`:
/// - `A B G K R S W X Z b z` — anchors / character types / `\K`
/// - `D H` — character types
/// - `a c e f n r t v` — ASCII escapes and `\c…` control
/// - `d h s w` — character types
/// - `x` — hex, `u` — codepoint, `0`-`9` — octal or backreference
/// - `C M P p` — meta/control/unicode-property (these raise in the probe but
///   are non-literal with their full syntax)
///
/// Every other escape — including `\.`, `\*`, `\/`, `\\`, `\ `, `\#`, `\@`, and
/// "ineffectual" alphabetic escapes such as `\j`, `\Q`, `\E` — is an
/// `EscapeSequence::Literal` whose `char` is the escaped character itself.
const NON_LITERAL_ESCAPES: &[u8] = b"0123456789ABCDGHKMPRSWXZabcdefhnprstuvwxz";

/// Unescaped characters that make a regexp non-literal. Note that `]` and `}`
/// are *not* included: regexp_parser scans a stray closing bracket or brace as
/// an ordinary `Literal`, matching Ruby. `{` is handled separately — it is only
/// a quantifier when it opens a well-formed interval (see `interval_len`).
const METACHARACTERS: &[u8] = b".*+?()[|^$";

/// Enforces `include` over `match` when the matcher's regexp is a plain string
/// literal — no anchors, character classes, quantifiers, alternations or
/// metacharacters.
///
/// Upstream classifies "simple" with `Regexp::Parser`: the regexp must have no
/// interpolation, no regexp options, and every top-level expression must be an
/// unquantified `Regexp::Expression::Literal` or
/// `Regexp::Expression::EscapeSequence::Literal`. nitrocop has no Regexp::Parser
/// equivalent, so this is a hand-written conservative scanner over the raw
/// regexp source (see `NON_LITERAL_ESCAPES` / `METACHARACTERS` for the exact
/// classification, derived by probing regexp_parser 2.11.3).
///
/// Known conservative gaps versus upstream (all produce false *negatives*,
/// never false positives):
/// - `\g` and `\k` followed by `<` are rejected. Upstream raises
///   `Regexp::Parser::ParserError` on `\g<1>` / `\k<x>` (i.e. RuboCop crashes on
///   the file); nitrocop declines to register an offense instead.
/// - Anything else that would make `Regexp::Parser.parse` raise — a stray `)`,
///   a bare `\1` backreference — is likewise rejected rather than replicated as
///   a crash.
///
/// Upstream's `to_string_literal` is replicated verbatim, including its bug:
/// it never escapes backslashes, so `match(/;\\}/)` corrects to the invalid
/// Ruby `include(';}\\')`. nitrocop emits the same correction, but its
/// "corrected source must still parse" guard then discards the whole file's
/// corrections, so `-A` is a no-op on such files instead of writing broken Ruby.
/// Offense detection and the message are unaffected.
///
/// Prism-vs-Parser quirks:
/// - Upstream matches parser's `regexp` node type, which covers interpolated
///   regexps too, and then rejects them via `interpolation?`. Prism splits these
///   into `RegularExpressionNode` and `InterpolatedRegularExpressionNode`, so
///   requiring the former is equivalent.
/// - parser exposes options as `regopt` children; Prism folds them into the
///   node flags *and* into `closing_loc`, so "has options" is
///   `closing_loc.len() > 1` (`/foo/i` closes with `/i`, `%r{foo}x` with `}x`).
/// - `RegexpNode#content` is `str_content`, i.e. the *raw* regexp source, which
///   is Prism's `content_loc()` — not `unescaped()`.
pub struct MatchWithSimpleRegex;

impl MatchWithSimpleRegex {
    /// Decode the regexp source into the literal string it matches, or `None`
    /// when the regexp uses any non-literal construct.
    ///
    /// Mirrors upstream's `simple_regexp?` + `regexp_to_string` in one pass:
    /// `Literal` expressions contribute their text, `EscapeSequence::Literal`
    /// expressions contribute their `char`.
    fn literal_string(content: &[u8]) -> Option<String> {
        let mut out: Vec<u8> = Vec::with_capacity(content.len());
        let mut i = 0;

        while i < content.len() {
            let byte = content[i];

            if byte == b'\\' {
                let next = *content.get(i + 1)?;
                if NON_LITERAL_ESCAPES.contains(&next) {
                    return None;
                }
                // `\g<…>` / `\k<…>` are subexpression calls / named
                // backreferences; a bare `\g` is an ineffectual literal escape.
                if matches!(next, b'g' | b'k') && content.get(i + 2) == Some(&b'<') {
                    return None;
                }
                // The escape consumes one whole character, which may be
                // multi-byte (`\😀` is a literal escape of the emoji).
                let char_len = utf8_char_len(next);
                let end = (i + 1 + char_len).min(content.len());
                out.extend_from_slice(&content[i + 1..end]);
                i = end;
                continue;
            }

            if byte < 0x80 && METACHARACTERS.contains(&byte) {
                return None;
            }

            // `{` only quantifies when it opens `{n}`, `{n,}`, `{,m}` or
            // `{n,m}`; regexp_parser scans `{`, `{x}`, `{,}` and `{2,3,4}` as
            // ordinary literals.
            if byte == b'{' && interval_len(&content[i..]).is_some() {
                return None;
            }

            out.push(byte);
            i += 1;
        }

        String::from_utf8(out).ok()
    }

    /// Upstream `to_string_literal`. Replicated verbatim, including its lack of
    /// backslash escaping.
    fn to_string_literal(string: &str) -> String {
        if string.contains('\'') {
            format!("\"{}\"", string.replace('"', "\\\""))
        } else {
            format!("'{string}'")
        }
    }

    /// The regexp argument of a bare `match(/…/)` call, per upstream's
    /// `(send nil? :match $regexp)`.
    fn simple_regexp_argument<'pr>(
        call: &ruby_prism::CallNode<'pr>,
    ) -> Option<ruby_prism::RegularExpressionNode<'pr>> {
        if call.receiver().is_some() || call.name().as_slice() != b"match" {
            return None;
        }
        let args = call.arguments()?;
        let mut iter = args.arguments().iter();
        let first = iter.next()?;
        if iter.next().is_some() {
            return None;
        }
        let regexp = first.as_regular_expression_node()?;
        // parser's `regopt.children.any?` — Prism keeps the option letters in
        // the closing delimiter.
        if regexp.closing_loc().as_slice().len() > 1 {
            return None;
        }
        Some(regexp)
    }
}

/// Length of a well-formed interval quantifier (`{n}`, `{n,}`, `{,m}`,
/// `{n,m}`) at the start of `bytes`, or `None` when the `{` is a literal.
fn interval_len(bytes: &[u8]) -> Option<usize> {
    debug_assert_eq!(bytes.first(), Some(&b'{'));
    let mut i = 1;
    let mut digits = 0;
    let mut commas = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'0'..=b'9' => digits += 1,
            b',' if commas == 0 => commas += 1,
            b'}' => return if digits > 0 { Some(i + 1) } else { None },
            _ => return None,
        }
        i += 1;
    }
    None
}

/// Length in bytes of the UTF-8 character starting with `lead`.
fn utf8_char_len(lead: u8) -> usize {
    match lead {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

impl Cop for MatchWithSimpleRegex {
    fn name(&self) -> &'static str {
        "RSpec/MatchWithSimpleRegex"
    }

    fn default_severity(&self) -> Severity {
        Severity::Convention
    }

    fn default_include(&self) -> &'static [&'static str] {
        RSPEC_DEFAULT_INCLUDE
    }

    fn supports_autocorrect(&self) -> bool {
        true
    }

    fn interested_node_types(&self) -> &'static [u8] {
        &[CALL_NODE]
    }

    fn check_node(
        &self,
        source: &SourceFile,
        node: &ruby_prism::Node<'_>,
        _parse_result: &ruby_prism::ParseResult<'_>,
        _config: &CopConfig,
        diagnostics: &mut Vec<Diagnostic>,
        corrections: Option<&mut Vec<crate::correction::Correction>>,
    ) {
        let Some(call) = node.as_call_node() else {
            return;
        };
        let Some(regexp) = Self::simple_regexp_argument(&call) else {
            return;
        };
        let Some(literal) = Self::literal_string(regexp.content_loc().as_slice()) else {
            return;
        };

        let string_literal = Self::to_string_literal(&literal);
        let loc = node.location();
        let (line, column) = source.offset_to_line_col(loc.start_offset());
        diagnostics.push(self.diagnostic(
            source,
            line,
            column,
            format!(
                "Prefer using `include({string_literal})` when the regex is a simple string literal."
            ),
        ));

        if let Some(corrections) = corrections {
            corrections.push(crate::correction::Correction {
                start: loc.start_offset(),
                end: loc.end_offset(),
                replacement: format!("include({string_literal})"),
                cop_name: self.name(),
                cop_index: 0,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    crate::cop_fixture_tests!(MatchWithSimpleRegex, "cops/rspec/match_with_simple_regex");
    crate::cop_autocorrect_fixture_tests!(
        MatchWithSimpleRegex,
        "cops/rspec/match_with_simple_regex"
    );

    #[test]
    fn literal_string_classification() {
        let simple = |s: &str| MatchWithSimpleRegex::literal_string(s.as_bytes());
        assert_eq!(simple("foo").as_deref(), Some("foo"));
        assert_eq!(
            simple(r"http:\/\/example\.com").as_deref(),
            Some("http://example.com")
        );
        // Ineffectual alphabetic escapes are literal in regexp_parser.
        assert_eq!(simple(r"foo\jbar").as_deref(), Some("foojbar"));
        assert_eq!(simple(r"a\\b").as_deref(), Some(r"a\b"));
        assert_eq!(simple("a]b}c").as_deref(), Some("a]b}c"));
        // `{` is literal unless it opens a well-formed interval.
        assert_eq!(simple(r#"{"k":1}"#).as_deref(), Some(r#"{"k":1}"#));
        assert_eq!(simple("a{x}").as_deref(), Some("a{x}"));
        assert_eq!(simple("a{2,3,4}").as_deref(), Some("a{2,3,4}"));
        assert_eq!(simple("a{,}").as_deref(), Some("a{,}"));
        assert_eq!(simple("").as_deref(), Some(""));
        // Non-literal constructs.
        for src in [
            "^foo", "foo$", "foo.", "foo*", "fo+", "fo?", "(f)", "[fo]", "f{2}", "f{2,}", "f{,3}",
            "f{2,3}", "a|b", r"\d", r"\s", r"\n", r"\t", r"\x41", r"\b", r"\A", r"\1", r"\g<1>",
            r"\k<x>", "a)b", "a\\",
        ] {
            assert!(simple(src).is_none(), "expected {src:?} to be non-simple");
        }
    }

    #[test]
    fn string_literal_quoting() {
        assert_eq!(MatchWithSimpleRegex::to_string_literal("foo"), "'foo'");
        assert_eq!(
            MatchWithSimpleRegex::to_string_literal("it's \"working\""),
            "\"it's \\\"working\\\"\""
        );
    }
}
