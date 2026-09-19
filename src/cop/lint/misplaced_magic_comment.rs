use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

use crate::cop::{Cop, CopConfig};
use crate::correction::Correction;
use crate::diagnostic::{Diagnostic, Severity};
use crate::parse::codemap::CodeMap;
use crate::parse::source::SourceFile;

const MSG_ENCODING: &str = "The `encoding` magic comment is ignored unless placed on the first \
                            line (or below a shebang on the first line).";
const MSG_AFTER_CODE: &str = "The `frozen_string_literal` magic comment is ignored after any code.";
const MSG_ABOVE_SHEBANG: &str = "A magic comment above a shebang renders the shebang ineffective.";

/// Checks for magic comments placed where Ruby silently ignores them
/// (`Lint/MisplacedMagicComment`, new in RuboCop 1.91, `Enabled: pending`,
/// `SafeAutoCorrect: false`).
///
/// ## Semantics (ported 1:1 from `lib/rubocop/cop/lint/misplaced_magic_comment.rb`)
///
/// Three independent checks run over `processed_source.comments`:
///
/// 1. `encoding` is only honored on line 1, or line 2 when line 1 is a shebang.
///    Anywhere else it is silently ignored, so it is flagged — unless every
///    preceding line is itself a valid magic comment (that run is
///    `Lint/OrderedMagicComments`' job, not this cop's).
/// 2. `frozen_string_literal` is honored anywhere before the first code token,
///    so it is only flagged when it starts *after* that token (this includes a
///    trailing comment on the first code line).
/// 3. A `#!`-comment on a line after line 1, at column 0, preceded only by
///    magic comments, is flagged: the magic comment above it killed the shebang.
///
/// `shareable_constant_value` is never flagged (Ruby scopes it, mid-file use is
/// intentional), and `rbs_inline`/`warn_indent`/`typed` are parsed only so that
/// `MagicComment#valid?` agrees with RuboCop when scanning preceding lines.
///
/// ## RuboCop quirks replicated
///
/// - `magic_comment_shaped?` is `/\A#(?![^#]*#)/`: the comment must start with
///   `#` and contain no second `#`. This keeps documentation that merely quotes
///   a magic comment (`#   # -*- coding: UTF-8 -*-`) from being flagged. The
///   `regex` crate has no lookahead, so it is expressed as
///   `text.starts_with('#') && !text[1..].contains('#')`, which is equivalent.
/// - Prose opening with `Encoding:` parses as a `SimpleComment` encoding
///   directive (`# Encoding: force given encoding` yields the token `force`), so
///   the cop additionally requires `Encoding.find(name)` to succeed. Ruby's
///   encoding registry is not available here, so `ENCODING_NAMES` embeds
///   `Encoding.name_list` from the Ruby in `mise.toml` (4.0.2) — the same list
///   RuboCop's `Encoding.find` consults. New aliases in a future Ruby would need
///   a refresh; the list has been stable for years.
/// - `MagicComment::EditorComment#match` builds its keyword regexps *without*
///   the `i` flag, so `# -*- Coding: utf-8 -*-` is **not** an encoding comment
///   while `# Coding: utf-8` (SimpleComment, `/io`) is. Replicated exactly.
/// - `SimpleComment#encoding` requires a literal `": "` (colon + one space)
///   before the token, so `# coding:utf-8` and `# coding:  utf-8` do not parse.
/// - `VimComment#encoding` only answers when the comment has more than one
///   token (`# vim: fileencoding=x` alone is ignored by Vim, and by RuboCop).
///
/// ## Prism-vs-Parser notes
///
/// - `first_code_token` is RuboCop's first non-comment lexer token. Prism has no
///   token stream here, so the start offset of the first statement of the
///   `ProgramNode` is used instead. The two agree for every construct where a
///   statement's source range starts at its first token, which is all of them in
///   practice (`(1)` → `ParenthesesNode` starts at `(`, heredocs start at `<<~`).
/// - `=begin`/`=end` blocks are `EmbDocComment`s in Prism just as they are
///   `type: :document` comments in Parser; both are skipped by
///   `magic_comment_shaped?` because they do not start with `#`.
pub struct MisplacedMagicComment;

impl Cop for MisplacedMagicComment {
    fn name(&self) -> &'static str {
        "Lint/MisplacedMagicComment"
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn supports_autocorrect(&self) -> bool {
        true
    }

    /// `SafeAutoCorrect: false` — moving the comment to its effective position
    /// activates it, changing the source encoding or string mutability.
    fn safe_autocorrect(&self) -> bool {
        false
    }

    fn check_source(
        &self,
        source: &SourceFile,
        parse_result: &ruby_prism::ParseResult<'_>,
        _code_map: &CodeMap,
        _config: &CopConfig,
        diagnostics: &mut Vec<Diagnostic>,
        mut corrections: Option<&mut Vec<Correction>>,
    ) {
        if source.as_bytes().is_empty() {
            return;
        }

        let lines: Vec<&str> = source
            .lines()
            .map(|l| std::str::from_utf8(l).unwrap_or(""))
            .collect();
        let ctx = FileContext {
            lines: &lines,
            first_code_offset: first_code_offset(parse_result),
        };

        let comments: Vec<CommentInfo> = parse_result
            .comments()
            .filter_map(|comment| {
                let loc = comment.location();
                let text = source.try_byte_slice(loc.start_offset(), loc.end_offset())?;
                let (line, column) = source.offset_to_line_col(loc.start_offset());
                Some(CommentInfo {
                    text,
                    line,
                    column,
                    start: loc.start_offset(),
                    end: loc.end_offset(),
                })
            })
            .collect();

        let mut found: Vec<(Diagnostic, Option<(Correction, Correction)>)> = Vec::new();

        if let Some(shebang) = ctx.magic_comment_above_shebang(&comments) {
            found.push((
                self.diagnostic(
                    source,
                    shebang.line,
                    shebang.column,
                    MSG_ABOVE_SHEBANG.to_string(),
                ),
                None,
            ));
        }

        for comment in &comments {
            if !magic_comment_shaped(comment.text) {
                continue;
            }
            let magic = MagicComment::parse(comment.text);
            if !magic.valid {
                continue;
            }

            if let Some(encoding) = &magic.encoding {
                if !known_encoding(encoding) {
                    continue;
                }
                if comment.line == ctx.effective_encoding_line()
                    && ctx.comment_starts_line(comment.line)
                {
                    continue;
                }
                if ctx.preceded_only_by_magic_comments(comment.line) {
                    continue;
                }
                found.push((
                    self.diagnostic(
                        source,
                        comment.line,
                        comment.column,
                        MSG_ENCODING.to_string(),
                    ),
                    ctx.move_comment(source, comment),
                ));
            } else if magic.frozen_string_literal.is_some() {
                let Some(first_code) = ctx.first_code_offset else {
                    continue;
                };
                if comment.start <= first_code {
                    continue;
                }
                found.push((
                    self.diagnostic(
                        source,
                        comment.line,
                        comment.column,
                        MSG_AFTER_CODE.to_string(),
                    ),
                    ctx.move_comment(source, comment),
                ));
            }
        }

        found.sort_by_key(|(d, _)| (d.location.line, d.location.column));

        for (mut diag, correction) in found {
            if let (Some(corr), Some(sink)) = (correction, corrections.as_deref_mut()) {
                sink.push(corr.0);
                sink.push(corr.1);
                diag.corrected = true;
            }
            diagnostics.push(diag);
        }
    }
}

struct CommentInfo<'a> {
    text: &'a str,
    /// 1-indexed.
    line: usize,
    /// 0-indexed byte column.
    column: usize,
    start: usize,
    end: usize,
}

struct FileContext<'a> {
    lines: &'a [&'a str],
    first_code_offset: Option<usize>,
}

impl FileContext<'_> {
    fn shebang(&self) -> bool {
        self.lines.first().is_some_and(|l| l.starts_with("#!"))
    }

    /// First line that may carry an effective `encoding` comment.
    fn effective_encoding_line(&self) -> usize {
        if self.shebang() { 2 } else { 1 }
    }

    fn comment_starts_line(&self, line: usize) -> bool {
        self.lines
            .get(line - 1)
            .is_some_and(|l| l.trim_start().starts_with('#'))
    }

    fn preceded_only_by_magic_comments(&self, line: usize) -> bool {
        (1..line).all(|n| {
            let text = self.lines.get(n - 1).copied().unwrap_or("");
            (n == 1 && text.starts_with("#!")) || MagicComment::parse(text).valid
        })
    }

    /// The first `#!` comment below line 1 at column 0 that is preceded only by
    /// magic comments — i.e. a shebang the magic comments above it disabled.
    fn magic_comment_above_shebang<'c, 'b>(
        &self,
        comments: &'c [CommentInfo<'b>],
    ) -> Option<&'c CommentInfo<'b>> {
        let shebang = comments
            .iter()
            .find(|c| c.line > 1 && c.text.starts_with("#!") && c.column == 0)?;
        self.preceded_only_by_magic_comments(shebang.line)
            .then_some(shebang)
    }

    /// Build the (removal, insertion) correction pair that relocates `comment`
    /// to `effective_encoding_line`.
    fn move_comment(
        &self,
        source: &SourceFile,
        comment: &CommentInfo<'_>,
    ) -> Option<(Correction, Correction)> {
        let target_line = self.effective_encoding_line();
        if target_line > self.lines.len() {
            return None;
        }

        let removal = if self.comment_starts_line(comment.line) {
            // range_by_whole_lines(..., include_final_newline: true).
            // `line_start_offset` answers 0 past the last line, so clamp to EOF.
            let start = source.line_start_offset(comment.line);
            let next = source.line_start_offset(comment.line + 1);
            let end = if next > start {
                std::cmp::min(next, source.as_bytes().len())
            } else {
                source.as_bytes().len()
            };
            (start, end)
        } else {
            // range_with_surrounding_space(side: :left): consume ` `/`\t`, then `\n`.
            let bytes = source.as_bytes();
            let mut start = comment.start;
            while start > 0 && matches!(bytes[start - 1], b' ' | b'\t') {
                start -= 1;
            }
            while start > 0 && bytes[start - 1] == b'\n' {
                start -= 1;
            }
            (start, comment.end)
        };

        let target_start = source.line_start_offset(target_line);
        let insert = if comment.line < target_line {
            // insert_after(line_range(target_line), "\n#{text}")
            let line_end = target_start + self.lines.get(target_line - 1).map_or(0, |l| l.len());
            Correction {
                start: line_end,
                end: line_end,
                replacement: format!("\n{}", comment.text),
                cop_name: "Lint/MisplacedMagicComment",
                cop_index: 0,
            }
        } else {
            // insert_before(line_range(target_line), "#{text}\n")
            Correction {
                start: target_start,
                end: target_start,
                replacement: format!("{}\n", comment.text),
                cop_name: "Lint/MisplacedMagicComment",
                cop_index: 0,
            }
        };

        Some((
            Correction {
                start: removal.0,
                end: removal.1,
                replacement: String::new(),
                cop_name: "Lint/MisplacedMagicComment",
                cop_index: 0,
            },
            insert,
        ))
    }
}

/// Start offset of the first code token, mirroring
/// `processed_source.sorted_tokens.find { |token| !token.comment? }`.
fn first_code_offset(parse_result: &ruby_prism::ParseResult<'_>) -> Option<usize> {
    let node = parse_result.node();
    let program = node.as_program_node()?;
    let first = program.statements().body().iter().next()?;
    Some(first.location().start_offset())
}

/// `/\A#(?![^#]*#)/` — starts with `#` and carries no second `#`.
fn magic_comment_shaped(text: &str) -> bool {
    text.starts_with('#') && !text[1..].contains('#')
}

// ---------------------------------------------------------------------------
// Port of `RuboCop::MagicComment`
// ---------------------------------------------------------------------------

const TOKEN: &str = r"([[:alnum:]\-_]+)";

macro_rules! lazy_re {
    ($name:ident, $pat:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($pat).unwrap());
    };
    ($name:ident, fmt $pat:expr) => {
        static $name: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(&format!($pat, t = TOKEN)).unwrap());
    };
}

lazy_re!(EMACS_RE, r"-\*-(.+)-\*-");
lazy_re!(VIM_RE, r"#\s*vim:\s*(.+)");

// `EditorComment#match` is built without the `i` flag — case matters.
lazy_re!(EMACS_ENCODING, fmt r"^(?:en)?coding\s*:\s*{t}$");
lazy_re!(EMACS_FSL, fmt r"^frozen[_-]string[_-]literal\s*:\s*{t}$");
lazy_re!(EMACS_WARN_INDENT, fmt r"^warn[_-]indent\s*:\s*{t}$");
lazy_re!(EMACS_SCV, fmt r"^shareable[_-]constant[_-]value\s*:\s*{t}$");
lazy_re!(VIM_ENCODING, fmt r"^fileencoding\s*=\s*{t}$");

// `SimpleComment` extractors all use `/io`.
lazy_re!(
    SIMPLE_ENCODING,
    fmt r"(?i)^\s*\#\s*(?:frozen_string_literal:\s*(?:true|false))?\s*(?:en)?coding: {t}"
);
lazy_re!(SIMPLE_FSL, fmt r"(?i)^\s*#\s*frozen[_-]string[_-]literal:\s*{t}\s*$");
lazy_re!(SIMPLE_RBS_INLINE, fmt r"(?i)^\s*#\s*rbs_inline:\s*{t}\s*$");
lazy_re!(SIMPLE_WARN_INDENT, fmt r"(?i)^\s*#\s*warn[_-]indent:\s*{t}\s*$");
lazy_re!(SIMPLE_SCV, fmt r"(?i)^\s*#\s*shareable[_-]constant[_-]value:\s*{t}\s*$");
lazy_re!(SIMPLE_TYPED, fmt r"(?i)^\s*#\s*typed:\s*{t}\s*$");

struct MagicComment {
    encoding: Option<String>,
    frozen_string_literal: Option<String>,
    valid: bool,
}

impl MagicComment {
    fn parse(text: &str) -> Self {
        let (encoding, frozen_string_literal, other) = if let Some(caps) = EMACS_RE.captures(text) {
            let tokens: Vec<&str> = caps[1].split(';').map(str::trim).collect();
            let encoding = match_editor_token(&tokens, &EMACS_ENCODING);
            let fsl = match_editor_token(&tokens, &EMACS_FSL);
            let other = match_editor_token(&tokens, &EMACS_WARN_INDENT).is_some()
                || match_editor_token(&tokens, &EMACS_SCV).is_some();
            (encoding, fsl, other)
        } else if let Some(caps) = VIM_RE.captures(text) {
            let tokens: Vec<&str> = caps[1].split(", ").map(str::trim).collect();
            // `fileencoding` only takes effect with at least one other token.
            let encoding = if tokens.len() > 1 {
                match_editor_token(&tokens, &VIM_ENCODING)
            } else {
                None
            };
            (encoding, None, false)
        } else {
            let encoding = capture(&SIMPLE_ENCODING, text);
            let fsl = capture(&SIMPLE_FSL, text);
            let rbs_inline = capture(&SIMPLE_RBS_INLINE, text)
                .is_some_and(|v| matches!(v.as_str(), "enabled" | "disabled"));
            let other = rbs_inline
                || capture(&SIMPLE_WARN_INDENT, text).is_some()
                || capture(&SIMPLE_SCV, text).is_some()
                || capture(&SIMPLE_TYPED, text).is_some();
            (encoding, fsl, other)
        };

        let any = encoding.is_some() || frozen_string_literal.is_some() || other;
        Self {
            encoding,
            frozen_string_literal,
            valid: text.starts_with('#') && any,
        }
    }
}

fn capture(re: &Regex, text: &str) -> Option<String> {
    re.captures(text).map(|c| c[1].to_string())
}

/// `EditorComment#match`: first token matching the keyword, value downcased.
fn match_editor_token(tokens: &[&str], re: &Regex) -> Option<String> {
    tokens
        .iter()
        .find_map(|t| re.captures(t).map(|c| c[1].to_lowercase()))
}

/// `Encoding.name_list` (Ruby 4.0.2), downcased — the set `Encoding.find`
/// accepts. RuboCop calls `Encoding.find(name)` and treats `ArgumentError` as
/// "this is prose, not a directive".
static ENCODING_NAMES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "646",
        "ansi_x3.4-1968",
        "ascii",
        "ascii-8bit",
        "big5",
        "big5-hkscs",
        "big5-hkscs:2008",
        "big5-uao",
        "binary",
        "cesu-8",
        "cp1250",
        "cp1251",
        "cp1252",
        "cp1253",
        "cp1254",
        "cp1255",
        "cp1256",
        "cp1257",
        "cp1258",
        "cp437",
        "cp50220",
        "cp50221",
        "cp51932",
        "cp65000",
        "cp65001",
        "cp720",
        "cp737",
        "cp775",
        "cp850",
        "cp852",
        "cp855",
        "cp857",
        "cp860",
        "cp861",
        "cp862",
        "cp863",
        "cp864",
        "cp865",
        "cp866",
        "cp869",
        "cp874",
        "cp878",
        "cp932",
        "cp936",
        "cp949",
        "cp950",
        "cp951",
        "cswindows31j",
        "ebcdic-cp-us",
        "emacs-mule",
        "euc-cn",
        "euc-jis-2004",
        "euc-jisx0213",
        "euc-jp",
        "euc-jp-ms",
        "euc-kr",
        "euc-tw",
        "euccn",
        "eucjp",
        "eucjp-ms",
        "euckr",
        "euctw",
        "external",
        "filesystem",
        "gb12345",
        "gb18030",
        "gb1988",
        "gb2312",
        "gbk",
        "ibm037",
        "ibm437",
        "ibm720",
        "ibm737",
        "ibm775",
        "ibm850",
        "ibm852",
        "ibm855",
        "ibm857",
        "ibm860",
        "ibm861",
        "ibm862",
        "ibm863",
        "ibm864",
        "ibm865",
        "ibm866",
        "ibm869",
        "internal",
        "iso-2022-jp",
        "iso-2022-jp-2",
        "iso-2022-jp-kddi",
        "iso-8859-1",
        "iso-8859-10",
        "iso-8859-11",
        "iso-8859-13",
        "iso-8859-14",
        "iso-8859-15",
        "iso-8859-16",
        "iso-8859-2",
        "iso-8859-3",
        "iso-8859-4",
        "iso-8859-5",
        "iso-8859-6",
        "iso-8859-7",
        "iso-8859-8",
        "iso-8859-9",
        "iso2022-jp",
        "iso2022-jp2",
        "iso8859-1",
        "iso8859-10",
        "iso8859-11",
        "iso8859-13",
        "iso8859-14",
        "iso8859-15",
        "iso8859-16",
        "iso8859-2",
        "iso8859-3",
        "iso8859-4",
        "iso8859-5",
        "iso8859-6",
        "iso8859-7",
        "iso8859-8",
        "iso8859-9",
        "koi8-r",
        "koi8-u",
        "locale",
        "maccenteuro",
        "maccroatian",
        "maccyrillic",
        "macgreek",
        "maciceland",
        "macjapan",
        "macjapanese",
        "macroman",
        "macromania",
        "macthai",
        "macturkish",
        "macukraine",
        "pck",
        "shift_jis",
        "sjis",
        "sjis-docomo",
        "sjis-kddi",
        "sjis-softbank",
        "stateless-iso-2022-jp",
        "stateless-iso-2022-jp-kddi",
        "tis-620",
        "ucs-2be",
        "ucs-4be",
        "ucs-4le",
        "us-ascii",
        "utf-16",
        "utf-16be",
        "utf-16le",
        "utf-32",
        "utf-32be",
        "utf-32le",
        "utf-7",
        "utf-8",
        "utf-8-hfs",
        "utf-8-mac",
        "utf8-docomo",
        "utf8-kddi",
        "utf8-mac",
        "utf8-softbank",
        "windows-1250",
        "windows-1251",
        "windows-1252",
        "windows-1253",
        "windows-1254",
        "windows-1255",
        "windows-1256",
        "windows-1257",
        "windows-1258",
        "windows-31j",
        "windows-874",
    ]
    .into_iter()
    .collect()
});

fn known_encoding(name: &str) -> bool {
    ENCODING_NAMES.contains(name.to_lowercase().as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    crate::cop_scenario_fixture_tests!(
        MisplacedMagicComment,
        "cops/lint/misplaced_magic_comment",
        encoding_below_doc = "encoding_below_doc.rb",
        encoding_below_blank_line = "encoding_below_blank_line.rb",
        encoding_below_shebang_doc = "encoding_below_shebang_doc.rb",
        fsl_after_code = "fsl_after_code.rb",
        fsl_trailing_comment = "fsl_trailing_comment.rb",
        magic_comment_above_shebang = "magic_comment_above_shebang.rb",
    );

    macro_rules! no_offense_scenarios {
        ($($name:ident = $file:literal),+ $(,)?) => {
            $(
                #[test]
                fn $name() {
                    crate::testutil::assert_cop_no_offenses_full(
                        &MisplacedMagicComment,
                        include_bytes!(concat!(
                            "../../../tests/fixtures/cops/lint/misplaced_magic_comment/no_offense/",
                            $file
                        )),
                    );
                }
            )+
        };
    }

    no_offense_scenarios!(
        no_offense_encoding_first_line = "encoding_first_line.rb",
        no_offense_encoding_below_shebang = "encoding_below_shebang.rb",
        no_offense_fsl_below_doc = "fsl_below_doc.rb",
        no_offense_fsl_below_blank_line = "fsl_below_blank_line.rb",
        no_offense_fsl_below_embdoc = "fsl_below_embdoc.rb",
        no_offense_all_comments = "all_comments.rb",
    );

    #[test]
    fn no_offense_empty_file() {
        crate::testutil::assert_cop_no_offenses_full(&MisplacedMagicComment, b"");
    }

    fn corrected(input: &[u8]) -> String {
        let (_diags, corrections) =
            crate::testutil::run_cop_autocorrect(&MisplacedMagicComment, input);
        let cs = crate::correction::CorrectionSet::from_vec(corrections);
        String::from_utf8(cs.apply(input)).unwrap()
    }

    #[test]
    fn autocorrect_encoding_below_doc() {
        assert_eq!(
            corrected(b"# Documentation comment\n# encoding: ascii-8bit\nputs 'hello'\n"),
            "# encoding: ascii-8bit\n# Documentation comment\nputs 'hello'\n"
        );
    }

    #[test]
    fn autocorrect_encoding_below_blank_line() {
        assert_eq!(
            corrected(b"\n# encoding: ascii-8bit\nputs 'hello'\n"),
            "# encoding: ascii-8bit\n\nputs 'hello'\n"
        );
    }

    #[test]
    fn autocorrect_encoding_below_shebang_doc() {
        assert_eq!(
            corrected(
                b"#!/usr/bin/env ruby\n# Documentation comment\n# encoding: ascii-8bit\nputs 'hello'\n"
            ),
            "#!/usr/bin/env ruby\n# encoding: ascii-8bit\n# Documentation comment\nputs 'hello'\n"
        );
    }

    #[test]
    fn autocorrect_fsl_after_code() {
        assert_eq!(
            corrected(b"require 'foo'\n# frozen_string_literal: true\n"),
            "# frozen_string_literal: true\nrequire 'foo'\n"
        );
    }

    #[test]
    fn autocorrect_fsl_trailing_comment() {
        assert_eq!(
            corrected(b"require 'foo' # frozen_string_literal: true\n"),
            "# frozen_string_literal: true\nrequire 'foo'\n"
        );
    }

    #[test]
    fn magic_comment_above_shebang_is_not_corrected() {
        let input = b"# frozen_string_literal: true\n#!/usr/bin/env ruby\nputs 'hello'\n";
        let (diags, corrections) =
            crate::testutil::run_cop_autocorrect(&MisplacedMagicComment, input);
        assert_eq!(diags.len(), 1);
        assert!(corrections.is_empty());
    }

    #[test]
    fn quoted_magic_comment_is_not_shaped() {
        assert!(!magic_comment_shaped("#   # -*- coding: UTF-8 -*-"));
        assert!(magic_comment_shaped("# encoding: utf-8"));
    }

    #[test]
    fn emacs_keyword_match_is_case_sensitive() {
        assert!(
            MagicComment::parse("# -*- coding: utf-8 -*-")
                .encoding
                .is_some()
        );
        assert!(
            MagicComment::parse("# -*- Coding: utf-8 -*-")
                .encoding
                .is_none()
        );
        // SimpleComment, by contrast, is case-insensitive.
        assert!(MagicComment::parse("# Coding: utf-8").encoding.is_some());
    }

    #[test]
    fn simple_encoding_requires_single_space_after_colon() {
        assert!(MagicComment::parse("# coding: utf-8").encoding.is_some());
        assert!(MagicComment::parse("# coding:utf-8").encoding.is_none());
        assert!(MagicComment::parse("# coding:  utf-8").encoding.is_none());
    }

    #[test]
    fn vim_encoding_needs_more_than_one_token() {
        assert!(
            MagicComment::parse("# vim: fileencoding=ascii-8bit")
                .encoding
                .is_none()
        );
        assert!(
            MagicComment::parse("# vim: filetype=ruby, fileencoding=ascii-8bit")
                .encoding
                .is_some()
        );
    }

    #[test]
    fn prose_encoding_name_is_rejected() {
        assert!(!known_encoding("force"));
        assert!(known_encoding("ASCII-8BIT"));
        assert!(known_encoding("utf-8"));
    }
}
