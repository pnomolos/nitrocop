use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;
use ruby_prism::Visit;

use crate::cop::shared::node_type::{
    INTERPOLATED_STRING_NODE, INTERPOLATED_X_STRING_NODE, PARENTHESES_NODE, PROGRAM_NODE,
    STATEMENTS_NODE, STRING_NODE, X_STRING_NODE, node_type_tag,
};
use crate::cop::{Cop, CopConfig};
use crate::correction::Correction;
use crate::diagnostic::Diagnostic;
use crate::parse::codemap::CodeMap;
use crate::parse::source::SourceFile;

const COP_NAME: &str = "Style/DirectiveScope";

/// Checks for `rubocop:` directive regions that wrap exactly one statement and
/// could use the tighter next-statement form (`Style/DirectiveScope`, added in
/// RuboCop 1.90, changed in 1.91, `Enabled: pending`, `SafeAutoCorrect: false`).
///
/// Three conversions, one per opening directive shape:
///
/// - `disable`/`todo` + matching `enable` → `disable-next`/`todo-next`
/// - `push` with signed args + balancing `pop` → `disable-next` (all `-`),
///   `enable-next` (all `+`) or `next` (mixed)
/// - `enable` + matching re-`disable` inside an open disabled region →
///   `enable-next`
///
/// A region qualifies only when the directive sits immediately above the
/// statement and the closer immediately below it: any other line inside the
/// region (a comment, a blank line) may carry offenses of the suppressed cops
/// that the tighter scope would no longer cover.
///
/// ## Infrastructure this cop had to bring with it
///
/// `src/parse/directives.rs` only understands `disable`/`enable`/`todo`; it has
/// no notion of RuboCop 1.90's `disable-next`, `todo-next`, `enable-next`,
/// `push`, `pop` or `next` modes, no signed (`+`/`-`) arguments, no `--` reason
/// text, and it keys ranges without recording which directive opened them. So
/// this cop carries a self-contained port of `RuboCop::DirectiveComment` plus
/// the parts of `RuboCop::CommentConfig` it needs (`analyze`,
/// `CommentConfig::DisableNext`, `CommentConfig::PushPop`,
/// `statement_scope_after`, `comment_only_line?`). If nitrocop ever grows
/// first-class support for the 1.90 directive modes, this port is what should
/// move into `src/parse/directives.rs`.
///
/// ## Deliberate divergences (all behaviour-preserving for this cop)
///
/// - **Cop names are not qualified or department-expanded.** RuboCop keys
///   `cop_disabled_line_ranges` by `Registry.qualified_cop_name`, expanding
///   `Metrics` into every cop of the department. This port keys by the raw name
///   as written. Every comparison this cop makes is between two directives'
///   `raw_cop_names` (`check_pair`, `re_disable_below`), so a department and its
///   expansion always pair up or fail together — `# rubocop:disable Metrics` /
///   `# rubocop:enable Metrics` still matches, and
///   `# rubocop:disable Metrics/AbcSize` / `# rubocop:enable Metrics` still
///   doesn't. `# rubocop:disable all` behaves the same way, under the key
///   `all`.
/// - **`inject_disabled_cops_directives` is not replicated.** RuboCop seeds an
///   open range from `-Float::INFINITY` for every cop that config disables, so a
///   lone `# rubocop:enable Foo` at the top of a file "closes" something when
///   `Foo` is off in config. A cop cannot see other cops' `Enabled` state here,
///   so `check_enable_pair` misses that case (a false negative, never a false
///   positive). It does not affect the corpus oracle, whose
///   `baseline_rubocop.yml` enables every cop. It is also why fixtures must be
///   verified with a full `rubocop` run and not `--only Style/DirectiveScope`:
///   `--only` disables every other cop, which injects those ranges and makes
///   `enable_disable_outside_region` report an offense that a normal run does
///   not.
/// - **`prevent_directive_disabling?`** (dropping
///   `Style/DisableCopsWithinSourceCodeDirective` from the ranges when it is
///   explicitly `Enabled: true`) needs the same cross-cop config access and is
///   likewise omitted.
/// - **`nitrocop:` directives are not recognised.** RuboCop's marker regexp is
///   `rubocop`-only, and suggesting `disable-next` for a `nitrocop:` directive
///   would point at a mode `src/parse/directives.rs` cannot parse.
///
/// ## Prism-vs-Parser quirks
///
/// - `comment_only_line?` is "this line bears no non-comment token" in RuboCop.
///   Prism exposes no token stream, so it is computed by masking every comment
///   span (inline and `=begin`/`=end`) out of the source and asking whether
///   anything but whitespace is left on the line. The two differ only for lines
///   inside a multi-line string or heredoc body, which RuboCop calls
///   comment-only (no token starts there) and this port calls code. A
///   `# rubocop:` directive can never be one of those lines, and the scan in
///   `attached_code_line` always meets the literal's opening line first, so the
///   difference is unreachable.
/// - `statement_starting_at` skips `begin_type?` nodes. Parser's `begin` covers
///   both implicit statement sequences and parenthesised expressions, so the
///   Prism equivalents `StatementsNode` and `ParenthesesNode` are both skipped
///   (plus `ProgramNode`, which Parser has no node for at all). Prism's extra
///   node types (`ArgumentsNode`, `BlockParametersNode`, …) are harmless: the
///   candidate is chosen by `max_by(&:last_line)` and they never outlive their
///   parent.
/// - `statement_end_line` special-cases heredocs via `loc.heredoc_end.line`.
///   Prism's heredoc `StringNode` location covers only the `<<~EOS` operator
///   (`foo(<<~EOS)` is a one-line node even when the body runs on), so the
///   subtree scan takes `closing_loc().start_offset()`'s line for heredoc
///   string nodes. `closing_loc().end_offset()` would be one line too far — it
///   includes the terminator's newline.
pub struct DirectiveScope;

impl Cop for DirectiveScope {
    fn name(&self) -> &'static str {
        COP_NAME
    }

    fn supports_autocorrect(&self) -> bool {
        true
    }

    /// `SafeAutoCorrect: false` — the suppression scope shrinks to the
    /// statement, so offenses on the directive lines themselves resurface.
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
        // `CommentConfig#initialize`: `@no_directives = !raw_source.include?('rubocop')`.
        if !memchr_contains(source.as_bytes(), b"rubocop") {
            return;
        }
        let Some(ctx) = Ctx::build(source, parse_result) else {
            return;
        };
        let ranges = ctx.analyze();

        for (index, directive) in ctx.directives.iter().enumerate() {
            if !ctx.comment_only_line(directive.line) {
                continue;
            }

            let found = if directive.plain_disable() {
                ctx.check_pair(directive, index, &ranges)
            } else if directive.signed_push() {
                ctx.check_push_pop(directive, index)
            } else if directive.plain_enable() {
                ctx.check_enable_pair(directive, &ranges)
            } else {
                None
            };

            let Some(offense) = found else { continue };

            let mut diag =
                self.diagnostic(source, directive.line, directive.column, offense.message);
            if let (Some(sink), Some(edits)) = (corrections.as_deref_mut(), offense.corrections) {
                for edit in edits {
                    sink.push(edit);
                }
                diag.corrected = true;
            }
            diagnostics.push(diag);
        }
    }
}

struct Offense {
    message: String,
    corrections: Option<Vec<Correction>>,
}

fn memchr_contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

// ---------------------------------------------------------------------------
// `RuboCop::DirectiveComment`
// ---------------------------------------------------------------------------

/// `[A-Za-z]\w+(/[A-Za-z]\w+)*`, with `\w` pinned to ASCII as in Ruby.
const CN: &str = r"(?:[A-Za-z][0-9A-Za-z_]+/)*(?:[A-Za-z][0-9A-Za-z_]+)";

/// `AVAILABLE_MODES` sorted longest-first, so `disable-next` is never matched as
/// `disable` (`-` is a word boundary, so `disable\b` would otherwise win).
const MODES: &str = "disable-next|enable-next|todo-next|disable|enable|todo|push|next|pop";

static DIRECTIVE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"#\s*rubocop\s*:\s*({MODES})\b(?:\s+(all|(?:{CN}\s*,\s*)*{CN})|\s+([+\-]{CN}(?:\s+[+\-]{CN})*))?"
    ))
    .unwrap()
});
/// `pre_match.match?(/\A#\s*\z/)` — a directive nested under another `#`
/// (`#   # rubocop:disable Foo`) is not a directive.
static NESTED_PREFIX_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\A#\s*\z").unwrap());
static COP_SPLIT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r",\s*").unwrap());
static DISABLE_WORD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bdisable\b").unwrap());
static TODO_WORD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\btodo\b").unwrap());
static ENABLE_WORD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\benable\b").unwrap());

struct Directive {
    /// 1-indexed line of the comment.
    line: usize,
    /// 0-indexed byte column of the `#`.
    column: usize,
    start: usize,
    end: usize,
    text: String,
    mode: Option<String>,
    cops: Option<String>,
    reason: Option<String>,
}

impl Directive {
    fn parse(text: &str, line: usize, column: usize, start: usize, end: usize) -> Self {
        let mut mode = None;
        let mut cops = None;
        let mut reason = None;

        if let Some(caps) = DIRECTIVE_RE.captures(text) {
            let whole = caps.get(0).unwrap();
            // `@match_data = pre_match.match?(/\A#\s*\z/) ? nil : match_data`
            if !NESTED_PREFIX_RE.is_match(&text[..whole.start()]) {
                mode = Some(caps[1].to_string());
                cops = caps
                    .get(2)
                    .or_else(|| caps.get(3))
                    .map(|m| m.as_str().to_string());
                let tail = text[whole.end()..].trim_start();
                if let Some(rest) = tail.strip_prefix("--") {
                    let rest = rest.trim();
                    if !rest.is_empty() {
                        reason = Some(rest.to_string());
                    }
                }
            }
        }

        Self {
            line,
            column,
            start,
            end,
            text: text.to_string(),
            mode,
            cops,
            reason,
        }
    }

    fn mode_is(&self, name: &str) -> bool {
        self.mode.as_deref() == Some(name)
    }

    fn disable_next(&self) -> bool {
        self.mode_is("disable-next") || self.mode_is("todo-next")
    }

    fn disabled(&self) -> bool {
        self.mode_is("disable") || self.mode_is("todo") || self.disable_next()
    }

    fn enable_next(&self) -> bool {
        self.mode_is("enable-next")
    }

    fn enabled(&self) -> bool {
        self.mode_is("enable") || self.enable_next()
    }

    fn is_push(&self) -> bool {
        self.mode_is("push")
    }

    fn is_pop(&self) -> bool {
        self.mode_is("pop")
    }

    fn is_next(&self) -> bool {
        self.mode_is("next")
    }

    fn all_cops(&self) -> bool {
        self.cops.as_deref() == Some("all")
    }

    fn plain_disable(&self) -> bool {
        self.disabled() && !self.disable_next()
    }

    fn plain_enable(&self) -> bool {
        self.enabled() && !self.enable_next() && !self.all_cops()
    }

    fn signed_push(&self) -> bool {
        self.is_push() && !self.signed_args().is_empty()
    }

    /// `(cops || '').split(/,\s*/)` — Ruby's `split` on `""` yields `[]`.
    fn raw_cop_names(&self) -> Vec<String> {
        let Some(cops) = self.cops.as_deref() else {
            return Vec::new();
        };
        if cops.is_empty() {
            return Vec::new();
        }
        COP_SPLIT_RE.split(cops).map(|s| s.to_string()).collect()
    }

    fn sorted_cop_names(&self) -> Vec<String> {
        let mut names = self.raw_cop_names();
        names.sort();
        names
    }

    /// `+`/`-` arguments of a `push` or `next`, grouped by operation.
    fn signed_args(&self) -> BTreeMap<char, Vec<String>> {
        let mut args: BTreeMap<char, Vec<String>> = BTreeMap::new();
        if !(self.is_push() || self.is_next()) {
            return args;
        }
        let Some(cops) = self.cops.as_deref() else {
            return args;
        };
        for spec in cops.split_whitespace() {
            let mut chars = spec.chars();
            let Some(op) = chars.next() else { continue };
            if op != '+' && op != '-' {
                continue;
            }
            args.entry(op).or_default().push(chars.as_str().to_string());
        }
        args
    }
}

// ---------------------------------------------------------------------------
// `RuboCop::CommentConfig` analysis
// ---------------------------------------------------------------------------

/// `Float::INFINITY`, for a disable that is never closed.
const INFINITY: i64 = i64::MAX;

#[derive(Clone, Copy)]
struct DRange {
    /// Kept for fidelity with `RuboCop::CommentConfig::DirectiveRange`; this cop
    /// only ever inspects `end` and `directive`.
    #[allow(dead_code)]
    begin: i64,
    end: i64,
    /// Index of the directive that opened the range, when one did.
    directive: Option<usize>,
}

#[derive(Clone, Default)]
struct CopAnalysis {
    line_ranges: Vec<DRange>,
    start_line: Option<i64>,
    start_directive: Option<usize>,
}

impl CopAnalysis {
    fn close(&self, line: i64) -> Vec<DRange> {
        let mut ranges = self.line_ranges.clone();
        if let Some(begin) = self.start_line {
            ranges.push(DRange {
                begin,
                end: line,
                directive: self.start_directive,
            });
        }
        ranges
    }
}

type Analyses = BTreeMap<String, CopAnalysis>;

struct NodeSpan {
    depth: usize,
    first_line: usize,
    last_line: usize,
    /// `last_line`, but a heredoc answers with its terminator's line.
    end_line: usize,
    tag: u8,
}

struct SpanCollector<'a> {
    source: &'a SourceFile,
    depth: usize,
    spans: Vec<NodeSpan>,
}

impl SpanCollector<'_> {
    fn record(&mut self, node: &ruby_prism::Node<'_>) {
        let loc = node.location();
        let first_line = self.source.offset_to_line_col(loc.start_offset()).0;
        let last_line = self
            .source
            .offset_to_line_col(loc.end_offset().saturating_sub(1))
            .0;
        let tag = node_type_tag(node);
        let end_line = heredoc_end_line(node, tag, self.source).unwrap_or(last_line);
        self.spans.push(NodeSpan {
            depth: self.depth,
            first_line,
            last_line,
            end_line,
            tag,
        });
    }
}

impl<'pr> Visit<'pr> for SpanCollector<'_> {
    fn visit_branch_node_enter(&mut self, node: ruby_prism::Node<'pr>) {
        self.record(&node);
        self.depth += 1;
    }

    fn visit_branch_node_leave(&mut self) {
        self.depth -= 1;
    }

    fn visit_leaf_node_enter(&mut self, node: ruby_prism::Node<'pr>) {
        self.record(&node);
    }
}

/// `node.loc.heredoc_end.line` for the Prism string nodes that can be heredocs.
fn heredoc_end_line(node: &ruby_prism::Node<'_>, tag: u8, source: &SourceFile) -> Option<usize> {
    let closing = match tag {
        STRING_NODE => {
            let n = node.as_string_node()?;
            is_heredoc_opening(source, n.opening_loc().as_ref())?;
            n.closing_loc()
        }
        INTERPOLATED_STRING_NODE => {
            let n = node.as_interpolated_string_node()?;
            is_heredoc_opening(source, n.opening_loc().as_ref())?;
            n.closing_loc()
        }
        X_STRING_NODE => {
            let n = node.as_x_string_node()?;
            is_heredoc_opening(source, Some(&n.opening_loc()))?;
            Some(n.closing_loc())
        }
        INTERPOLATED_X_STRING_NODE => {
            let n = node.as_interpolated_x_string_node()?;
            is_heredoc_opening(source, Some(&n.opening_loc()))?;
            Some(n.closing_loc())
        }
        _ => return None,
    }?;
    Some(source.offset_to_line_col(closing.start_offset()).0)
}

fn is_heredoc_opening(
    source: &SourceFile,
    opening: Option<&ruby_prism::Location<'_>>,
) -> Option<()> {
    let opening = opening?;
    let text = source.try_byte_slice(opening.start_offset(), opening.end_offset())?;
    text.starts_with("<<").then_some(())
}

struct Ctx<'a> {
    source: &'a SourceFile,
    lines: Vec<&'a str>,
    /// `non_comment_token_line_numbers.none?(line)`, per 1-indexed line.
    comment_only: Vec<bool>,
    directives: Vec<Directive>,
    /// 1-indexed line → index into `directives` (`comment_at_line`).
    directive_at_line: BTreeMap<usize, usize>,
    spans: Vec<NodeSpan>,
}

impl<'a> Ctx<'a> {
    fn build(source: &'a SourceFile, parse_result: &ruby_prism::ParseResult<'_>) -> Option<Self> {
        let lines: Vec<&str> = source
            .lines()
            .map(|l| std::str::from_utf8(l).unwrap_or(""))
            .collect();

        // `comment_only_line?`: blank out every comment span, then ask whether
        // anything but whitespace is left on the line.
        let mut masked = source.as_bytes().to_vec();
        let mut directives = Vec::new();
        let mut directive_at_line = BTreeMap::new();
        for comment in parse_result.comments() {
            let loc = comment.location();
            let (start, end) = (loc.start_offset(), loc.end_offset().min(masked.len()));
            let Some(text) = source.try_byte_slice(start, loc.end_offset()) else {
                continue;
            };
            let (line, column) = source.offset_to_line_col(start);
            directive_at_line.entry(line).or_insert(directives.len());
            directives.push(Directive::parse(
                text,
                line,
                column,
                start,
                loc.end_offset(),
            ));
            for byte in &mut masked[start..end] {
                if *byte != b'\n' {
                    *byte = b' ';
                }
            }
        }

        let mut comment_only = vec![true; lines.len() + 2];
        for (index, line) in masked.split(|&b| b == b'\n').enumerate() {
            if index + 1 >= comment_only.len() {
                break;
            }
            comment_only[index + 1] = !line.iter().any(|b| !b.is_ascii_whitespace());
        }

        let mut collector = SpanCollector {
            source,
            depth: 0,
            spans: Vec::new(),
        };
        collector.visit(&parse_result.node());

        Some(Self {
            source,
            lines,
            comment_only,
            directives,
            directive_at_line,
            spans: collector.spans,
        })
    }

    fn comment_only_line(&self, line: usize) -> bool {
        self.comment_only.get(line).copied().unwrap_or(true)
    }

    fn directive_at_line(&self, line: usize) -> Option<&Directive> {
        self.directive_at_line
            .get(&line)
            .map(|&index| &self.directives[index])
    }

    // -- `CommentConfig::DisableNext` ---------------------------------------

    /// The line range of the statement a next-statement directive on `line`
    /// scopes to, or `None` when nothing is attached.
    fn statement_scope_after(&self, line: usize) -> Option<(usize, usize)> {
        if !self.comment_only_line(line) {
            return None;
        }
        let code_line = self.attached_code_line(line)?;
        Some(self.statement_bounds_at(code_line))
    }

    /// Comment-only lines chain (so directives can stack); a blank line breaks
    /// the attachment.
    fn attached_code_line(&self, directive_line: usize) -> Option<usize> {
        for line in (directive_line + 1)..=self.lines.len() {
            if !self.comment_only_line(line) {
                return Some(line);
            }
            if self.lines[line - 1].trim_start().is_empty() {
                return None;
            }
        }
        None
    }

    fn statement_bounds_at(&self, line: usize) -> (usize, usize) {
        // A code line where no statement starts (e.g. a lone `end`) scopes the
        // directive to that line alone.
        match self.statement_starting_at(line) {
            Some(index) => (line, self.statement_end_line(index)),
            None => (line, line),
        }
    }

    fn statement_starting_at(&self, line: usize) -> Option<usize> {
        let mut best: Option<usize> = None;
        for (index, span) in self.spans.iter().enumerate() {
            if span.first_line != line || is_begin_like(span.tag) {
                continue;
            }
            // `max_by` keeps the first element of a tie.
            if best.is_none_or(|b| span.last_line > self.spans[b].last_line) {
                best = Some(index);
            }
        }
        best
    }

    /// `statement.each_node.filter_map { … }.max` — the node and its subtree.
    fn statement_end_line(&self, index: usize) -> usize {
        let depth = self.spans[index].depth;
        let mut end = self.spans[index].end_line;
        for span in &self.spans[index + 1..] {
            if span.depth <= depth {
                break;
            }
            end = end.max(span.end_line);
        }
        end
    }

    // -- `CommentConfig#analyze` --------------------------------------------

    fn analyze(&self) -> BTreeMap<String, Vec<DRange>> {
        let mut analyses: Analyses = BTreeMap::new();
        let mut stack: Vec<Analyses> = Vec::new();

        for (index, directive) in self.directives.iter().enumerate() {
            if directive.is_push() {
                stack.push(analyses.clone());
                self.apply_push(&mut analyses, directive, index);
            } else if directive.is_pop() {
                if !stack.is_empty() {
                    pop_state(&mut analyses, &mut stack, directive.line as i64);
                }
            } else if directive.disable_next() {
                self.apply_disable_next(&mut analyses, directive, index);
            } else if directive.is_next() {
                self.apply_next_directive(&mut analyses, directive, index);
            } else if directive.enable_next() {
                self.apply_enable_next(&mut analyses, directive);
            } else {
                for cop in directive.raw_cop_names() {
                    let analysis = analyses.entry(cop).or_default();
                    *analysis = self.analyze_cop(analysis, directive, index);
                }
            }
        }

        analyses
            .into_iter()
            .map(|(cop, analysis)| (cop, analysis.close(INFINITY)))
            .collect()
    }

    fn analyze_cop(
        &self,
        analysis: &CopAnalysis,
        directive: &Directive,
        index: usize,
    ) -> CopAnalysis {
        let line = directive.line as i64;
        // `single_line?` is `!text.start_with?(DIRECTIVE_COMMENT_REGEXP)`; the
        // comments this cop reaches always match at position 0, so only the
        // `comment_only_line?` half can fire here.
        if !self.comment_only_line(directive.line) || !starts_with_directive(&directive.text) {
            if !directive.disabled() {
                return analysis.clone();
            }
            let mut ranges = analysis.line_ranges.clone();
            ranges.push(DRange {
                begin: line,
                end: line,
                directive: Some(index),
            });
            return CopAnalysis {
                line_ranges: ranges,
                start_line: analysis.start_line,
                start_directive: analysis.start_directive,
            };
        }

        if directive.disabled() {
            CopAnalysis {
                line_ranges: analysis.close(line),
                start_line: Some(line),
                start_directive: Some(index),
            }
        } else {
            CopAnalysis {
                line_ranges: analysis.close(line),
                start_line: None,
                start_directive: None,
            }
        }
    }

    fn apply_push(&self, analyses: &mut Analyses, directive: &Directive, index: usize) {
        let line = directive.line as i64;
        for (op, cops) in directive.signed_args() {
            for cop in cops {
                let analysis = analyses.entry(cop).or_default();
                if op == '-' && analysis.start_line.is_none() {
                    analysis.start_line = Some(line);
                    analysis.start_directive = Some(index);
                } else if op == '+' && analysis.start_line.is_some() {
                    *analysis = CopAnalysis {
                        line_ranges: analysis.close(line),
                        start_line: None,
                        start_directive: None,
                    };
                }
            }
        }
    }

    fn apply_disable_next(&self, analyses: &mut Analyses, directive: &Directive, index: usize) {
        let Some(bounds) = self.statement_scope_after(directive.line) else {
            return;
        };
        for cop in directive.raw_cop_names() {
            add_next_range(analyses, cop, bounds, index);
        }
    }

    fn apply_next_directive(&self, analyses: &mut Analyses, directive: &Directive, index: usize) {
        let Some(bounds) = self.statement_scope_after(directive.line) else {
            return;
        };
        for (op, cops) in directive.signed_args() {
            for cop in cops {
                if op == '-' {
                    add_next_range(analyses, cop, bounds, index);
                } else {
                    suspend_disable(analyses, cop, bounds);
                }
            }
        }
    }

    fn apply_enable_next(&self, analyses: &mut Analyses, directive: &Directive) {
        let Some(bounds) = self.statement_scope_after(directive.line) else {
            return;
        };
        for cop in directive.raw_cop_names() {
            suspend_disable(analyses, cop, bounds);
        }
    }

    // -- the cop proper ------------------------------------------------------

    fn check_pair(
        &self,
        directive: &Directive,
        index: usize,
        ranges: &BTreeMap<String, Vec<DRange>>,
    ) -> Option<Offense> {
        let closing_line = self.single_closing_line(index, ranges)?;
        let enable = self.directive_at_line(closing_line)?;
        if !self.wraps_single_statement(directive, closing_line) {
            return None;
        }
        if !enable.enabled() || enable.sorted_cop_names() != directive.sorted_cop_names() {
            return None;
        }

        let mode = directive.mode.as_deref()?;
        let word_re = if mode == "todo" {
            &TODO_WORD_RE
        } else {
            &DISABLE_WORD_RE
        };
        let replacement = word_re
            .replace(&directive.text, format!("{mode}-next").as_str())
            .into_owned();

        Some(Offense {
            message: format!(
                "Use `{mode}-next` instead of a `{mode}`/`enable` pair around a single statement."
            ),
            corrections: Some(vec![
                replace_range(directive.start, directive.end, replacement),
                self.remove_line(enable.line),
            ]),
        })
    }

    fn check_push_pop(&self, directive: &Directive, index: usize) -> Option<Offense> {
        let pop_line = self.balancing_pop_line(index)?;
        if !self.comment_only_line(pop_line) {
            return None;
        }
        if !self.wraps_single_statement(directive, pop_line) {
            return None;
        }
        let pop = self.directive_at_line(pop_line)?;

        let signed = directive.signed_args();
        let ops: Vec<char> = signed.keys().copied().collect();
        let mode = match ops.as_slice() {
            ['-'] => "disable-next",
            ['+'] => "enable-next",
            _ => "next",
        };
        let body = match mode {
            "disable-next" => format!("# rubocop:disable-next {}", signed[&'-'].join(", ")),
            "enable-next" => format!("# rubocop:enable-next {}", signed[&'+'].join(", ")),
            _ => format!("# rubocop:next {}", directive.cops.as_deref().unwrap_or("")),
        };
        let replacement = match &directive.reason {
            Some(reason) => format!("{body} -- {reason}"),
            None => body,
        };

        Some(Offense {
            message: format!("Use `{mode}` instead of `push`/`pop` around a single statement."),
            corrections: Some(vec![
                replace_range(directive.start, directive.end, replacement),
                self.remove_line(pop.line),
            ]),
        })
    }

    fn check_enable_pair(
        &self,
        directive: &Directive,
        ranges: &BTreeMap<String, Vec<DRange>>,
    ) -> Option<Offense> {
        let scope = self.statement_scope_after(directive.line)?;
        if scope.0 != directive.line + 1 {
            return None;
        }
        let closing = self.re_disable_below(directive, scope.1 + 1)?;
        if !self.closed_open_disables(directive, closing, ranges) {
            return None;
        }

        let replacement = ENABLE_WORD_RE
            .replace(&directive.text, "enable-next")
            .into_owned();

        Some(Offense {
            message:
                "Use `enable-next` instead of an `enable`/`disable` pair around a single statement."
                    .to_string(),
            corrections: Some(vec![
                replace_range(directive.start, directive.end, replacement),
                self.remove_line(closing.line),
            ]),
        })
    }

    fn re_disable_below(&self, directive: &Directive, line: usize) -> Option<&Directive> {
        if !self.comment_only_line(line) {
            return None;
        }
        let closing = self.directive_at_line(line)?;
        if !closing.disabled() || closing.disable_next() {
            return None;
        }
        (closing.sorted_cop_names() == directive.sorted_cop_names()).then_some(closing)
    }

    /// Every cop of the pair must have a range the `enable` closed and a range
    /// the trailing `disable` reopened.
    fn closed_open_disables(
        &self,
        directive: &Directive,
        closing: &Directive,
        ranges: &BTreeMap<String, Vec<DRange>>,
    ) -> bool {
        let closing_index = self
            .directive_at_line
            .get(&closing.line)
            .copied()
            .unwrap_or(usize::MAX);
        directive.raw_cop_names().iter().all(|cop| {
            let Some(cop_ranges) = ranges.get(cop) else {
                return false;
            };
            cop_ranges.iter().any(|r| r.end == directive.line as i64)
                && cop_ranges
                    .iter()
                    .any(|r| r.directive == Some(closing_index))
        })
    }

    /// The single line on which every range opened by this directive ends, or
    /// `None` when the ranges disagree or never close.
    fn single_closing_line(
        &self,
        index: usize,
        ranges: &BTreeMap<String, Vec<DRange>>,
    ) -> Option<usize> {
        let mut end: Option<i64> = None;
        let mut any = false;
        for cop_ranges in ranges.values() {
            for range in cop_ranges {
                if range.directive != Some(index) {
                    continue;
                }
                any = true;
                match end {
                    None => end = Some(range.end),
                    Some(existing) if existing != range.end => return None,
                    _ => {}
                }
            }
        }
        if !any {
            return None;
        }
        let end = end?;
        (end != INFINITY && end > 0).then_some(end as usize)
    }

    /// The line of the `pop` balancing this `push`, taking nesting into account.
    fn balancing_pop_line(&self, push_index: usize) -> Option<usize> {
        let reference = self.directives[push_index].line;
        let mut depth = 0i32;
        for directive in &self.directives {
            if directive.line <= reference {
                continue;
            }
            if directive.is_push() {
                depth += 1;
            } else if directive.is_pop() {
                if depth == 0 {
                    return Some(directive.line);
                }
                depth -= 1;
            }
        }
        None
    }

    fn wraps_single_statement(&self, directive: &Directive, closing_line: usize) -> bool {
        match self.statement_scope_after(directive.line) {
            Some((begin, end)) => begin == directive.line + 1 && end + 1 == closing_line,
            None => false,
        }
    }

    fn remove_line(&self, line: usize) -> Correction {
        let start = self.source.line_start_offset(line);
        let next = self.source.line_start_offset(line + 1);
        let end = if next > start {
            next.min(self.source.as_bytes().len())
        } else {
            self.source.as_bytes().len()
        };
        Correction {
            start,
            end,
            replacement: String::new(),
            cop_name: COP_NAME,
            cop_index: 0,
        }
    }
}

fn replace_range(start: usize, end: usize, replacement: String) -> Correction {
    Correction {
        start,
        end,
        replacement,
        cop_name: COP_NAME,
        cop_index: 0,
    }
}

/// Parser's `begin_type?`: implicit statement sequences and parenthesised
/// expressions. `ProgramNode` has no Parser counterpart at all.
fn is_begin_like(tag: u8) -> bool {
    tag == STATEMENTS_NODE || tag == PARENTHESES_NODE || tag == PROGRAM_NODE
}

fn starts_with_directive(text: &str) -> bool {
    DIRECTIVE_RE
        .find(text)
        .is_some_and(|whole| whole.start() == 0)
}

fn add_next_range(analyses: &mut Analyses, cop: String, bounds: (usize, usize), index: usize) {
    let analysis = analyses.entry(cop).or_default();
    analysis.line_ranges.push(DRange {
        begin: bounds.0 as i64,
        end: bounds.1 as i64,
        directive: Some(index),
    });
}

/// Punches a statement-sized hole into the currently open disable of the cop.
fn suspend_disable(analyses: &mut Analyses, cop: String, bounds: (usize, usize)) {
    let analysis = analyses.entry(cop).or_default();
    if analysis.start_line.is_none() {
        return;
    }
    *analysis = CopAnalysis {
        line_ranges: analysis.close(bounds.0 as i64 - 1),
        start_line: Some(bounds.1 as i64 + 1),
        start_directive: analysis.start_directive,
    };
}

fn pop_state(analyses: &mut Analyses, stack: &mut Vec<Analyses>, line: i64) {
    let restore_point = stack.pop().unwrap_or_default();
    let cops: Vec<String> = restore_point
        .keys()
        .chain(analyses.keys())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    for cop in cops {
        let current = analyses.entry(cop.clone()).or_default().clone();
        let ranges = current.close(line - 1);
        let restored = restore_point.get(&cop);
        let next = match restored.and_then(|r| r.start_line.map(|_| r)) {
            Some(restored) => CopAnalysis {
                line_ranges: ranges,
                start_line: Some(line),
                start_directive: restored.start_directive,
            },
            None => CopAnalysis {
                line_ranges: ranges,
                start_line: None,
                start_directive: None,
            },
        };
        analyses.insert(cop, next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    crate::cop_scenario_fixture_tests!(
        DirectiveScope,
        "cops/style/directive_scope",
        disable_enable_pair = "disable_enable_pair.rb",
        todo_enable_pair = "todo_enable_pair.rb",
        push_pop_disable_only = "push_pop_disable_only.rb",
        push_pop_mixed = "push_pop_mixed.rb",
        push_pop_plus_only = "push_pop_plus_only.rb",
        enable_disable_pair = "enable_disable_pair.rb",
    );

    macro_rules! no_offense_scenarios {
        ($($name:ident = $file:literal),+ $(,)?) => {
            $(
                #[test]
                fn $name() {
                    crate::testutil::assert_cop_no_offenses_full(
                        &DirectiveScope,
                        include_bytes!(concat!(
                            "../../../tests/fixtures/cops/style/directive_scope/no_offense/",
                            $file
                        )),
                    );
                }
            )+
        };
    }

    no_offense_scenarios!(
        no_offense_enable_closes_only_some = "enable_closes_only_some.rb",
        no_offense_unclosed_disable = "unclosed_disable.rb",
        no_offense_enable_disable_outside_region = "enable_disable_outside_region.rb",
        no_offense_nested_push_pop = "nested_push_pop.rb",
        no_offense_bare_push = "bare_push.rb",
        no_offense_blank_line_inside_region = "blank_line_inside_region.rb",
        no_offense_comment_between = "comment_between_directive_and_statement.rb",
    );

    fn corrected(input: &[u8]) -> String {
        let (_diags, corrections) = crate::testutil::run_cop_autocorrect(&DirectiveScope, input);
        let cs = crate::correction::CorrectionSet::from_vec(corrections);
        String::from_utf8(cs.apply(input)).unwrap()
    }

    #[test]
    fn autocorrect_disable_enable_pair() {
        assert_eq!(
            corrected(
                b"# rubocop:disable Metrics/AbcSize\ndef foo\n  bar\nend\n# rubocop:enable Metrics/AbcSize\n"
            ),
            "# rubocop:disable-next Metrics/AbcSize\ndef foo\n  bar\nend\n"
        );
    }

    #[test]
    fn autocorrect_todo_pair_keeps_reason() {
        assert_eq!(
            corrected(
                b"# rubocop:todo Metrics/AbcSize, Metrics/MethodLength -- legacy method\ndef foo\n  bar\nend\n# rubocop:enable Metrics/AbcSize, Metrics/MethodLength\n"
            ),
            "# rubocop:todo-next Metrics/AbcSize, Metrics/MethodLength -- legacy method\ndef foo\n  bar\nend\n"
        );
    }

    #[test]
    fn autocorrect_push_pop_disable_only() {
        assert_eq!(
            corrected(
                b"# rubocop:push -Metrics/AbcSize -- special case\ndef foo\n  bar\nend\n# rubocop:pop\n"
            ),
            "# rubocop:disable-next Metrics/AbcSize -- special case\ndef foo\n  bar\nend\n"
        );
    }

    #[test]
    fn autocorrect_push_pop_mixed() {
        assert_eq!(
            corrected(
                b"# rubocop:disable Style/For\n# rubocop:push -Metrics/AbcSize +Style/For\ndef foo\nend\n# rubocop:pop\n# rubocop:enable Style/For\n"
            ),
            "# rubocop:disable Style/For\n# rubocop:next -Metrics/AbcSize +Style/For\ndef foo\nend\n# rubocop:enable Style/For\n"
        );
    }

    #[test]
    fn autocorrect_push_pop_plus_only() {
        assert_eq!(
            corrected(
                b"# rubocop:disable Metrics/AbcSize\n# rubocop:push +Metrics/AbcSize -- reviewed\ndef foo\nend\n# rubocop:pop\n# rubocop:enable Metrics/AbcSize\n"
            ),
            "# rubocop:disable Metrics/AbcSize\n# rubocop:enable-next Metrics/AbcSize -- reviewed\ndef foo\nend\n# rubocop:enable Metrics/AbcSize\n"
        );
    }

    #[test]
    fn autocorrect_enable_disable_pair() {
        assert_eq!(
            corrected(
                b"# rubocop:disable Metrics/AbcSize\n# rubocop:enable Metrics/AbcSize\ndef foo\nend\n# rubocop:disable Metrics/AbcSize\n# rubocop:enable Metrics/AbcSize\n"
            ),
            "# rubocop:disable Metrics/AbcSize\n# rubocop:enable-next Metrics/AbcSize\ndef foo\nend\n# rubocop:enable Metrics/AbcSize\n"
        );
    }

    #[test]
    fn directive_parsing_prefers_longest_mode() {
        let d = directive("# rubocop:disable-next Foo/Bar");
        assert_eq!(d.mode.as_deref(), Some("disable-next"));
        assert!(d.disable_next());
        assert!(!d.plain_disable());
    }

    #[test]
    fn nested_directive_comment_is_not_a_directive() {
        let d = directive("#   # rubocop:disable Foo/Bar");
        assert!(d.mode.is_none());
        assert!(d.raw_cop_names().is_empty());
    }

    fn directive(text: &str) -> Directive {
        Directive::parse(text, 1, 0, 0, text.len())
    }

    #[test]
    fn signed_args_are_grouped_by_operation() {
        let d = directive("# rubocop:push -Metrics/AbcSize +Style/For -Lint/Void");
        let args = d.signed_args();
        assert_eq!(
            args[&'-'],
            vec!["Metrics/AbcSize".to_string(), "Lint/Void".to_string()]
        );
        assert_eq!(args[&'+'], vec!["Style/For".to_string()]);
        assert!(d.signed_push());
    }

    #[test]
    fn reason_is_parsed_from_trailing_marker() {
        let d = directive("# rubocop:push -Metrics/AbcSize -- special case");
        assert_eq!(d.reason.as_deref(), Some("special case"));
        let plain = directive("# rubocop:disable Metrics/AbcSize");
        assert_eq!(plain.reason, None);
    }

    /// `COP_NAME_PATTERN` is `[A-Za-z]\w+`, so a one-letter segment never parses
    /// as a cop name - the directive keeps its mode but carries no cops.
    #[test]
    fn single_letter_segments_are_not_cop_names() {
        let d = directive("# rubocop:push -A/B");
        assert!(d.is_push());
        assert!(d.cops.is_none());
        assert!(!d.signed_push());
    }
}
