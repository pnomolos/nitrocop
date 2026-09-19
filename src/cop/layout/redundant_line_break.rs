use std::collections::HashSet;
use std::sync::LazyLock;

use regex::bytes::Regex;
use ruby_prism::Visit;

use crate::cop::{Cop, CopConfig};
use crate::diagnostic::Diagnostic;
use crate::parse::codemap::CodeMap;
use crate::parse::source::SourceFile;

/// Layout/RedundantLineBreak: Checks whether certain expressions that could fit
/// on a single line are broken up into multiple lines unnecessarily.
///
/// ## Implementation approach
/// Two-phase detection:
/// - **Phase 1 (AST)**: Visits CallNode and assignment write nodes. Uses walk-down
///   with `checked_chain_ranges` to approximate RuboCop's walk-up-to-outermost behavior.
/// - **Phase 2 (text)**: Detects backslash line continuations that could be collapsed.
///
/// ## Key differences from RuboCop
/// - RuboCop walks UP from `on_send` through parent sends, convertible blocks, and
///   binary operators to find the outermost expression. Nitrocop walks DOWN and uses
///   `checked_chain_ranges` + `part_of_reported_node` to approximate this.
/// - RuboCop's `configured_to_not_be_inspected?` only skips multiline blocks
///   (`any_descendant?(node, :any_block, &:multiline?)`). Nitrocop now matches this
///   by tracking multiline vs single-line blocks separately.
/// - RuboCop's `other_cop_takes_precedence?` is conditional on
///   `Layout/SingleLineBlockChain` being enabled. Nitrocop now mirrors that by
///   using an injected `SingleLineBlockChainEnabled` flag from config.
///
/// ## Remaining gaps (FNs)
/// - No walk-up through `AndNode`/`OrNode` (binary operators) — standalone multiline
///   `&&`/`||` expressions without assignment are not checked.
/// - No walk-up through convertible blocks (`method { ... }.chain`) — the block is not
///   merged with its send_node for length calculation.
///
/// ## Fixes applied (2026-03-09)
/// - Phase 2 now checks block and unsafe ranges before reporting backslash continuations.
/// - Added `ParenthesesNode` to unsafe ranges (maps to `:begin` in Parser AST).
/// - Fixed `too_long` method chain dot check to match RuboCop's `(?=(&)?\.\w)` regex.
/// - Split block range tracking into multiline-only (`contains_multiline_block`)
///   for more accurate InspectBlocks handling.
///
/// ## Fixes applied (2026-03-16)
/// - **Critical FP fix**: `UnsafeRangeCollector` now recurses into all node types
///   (DefNode, IfNode, CaseNode, etc.). Previously it stopped recursing when it hit
///   these nodes, so multiline strings/regexps/arrays nested inside methods or
///   conditionals were never collected as unsafe ranges. This caused ~thousands of FPs
///   in repos like slim-template (315 FPs from multiline %q{} strings inside def bodies).
/// - Added all missing operator/or/and write node visitors for instance variables,
///   class variables, global variables, constants, and constant paths (e.g.,
///   `@count += items.size`, `@@total += n`, `$var ||= compute`).
///
/// ## Fixes applied (2026-04-01)
/// - Fixed keyword-argument false negatives inside multiline chained block
///   expressions. Previously a multiline outer call like `items.each do ... end`
///   could mark its entire byte range as "checked" and suppress inner
///   multiline calls in the block body. The fix remains deliberately narrow:
///   contained calls are only unsuppressed for the validated keyword-hash
///   argument shape when they are the direct, sole statement inside a
///   multiline block body.
///
/// ## Fixes applied (2026-04-03)
/// - Extended that narrow block-body unsuppression to `map` blocks too.
///   RuboCop does flag direct multiline keyword-hash calls such as
///   `search_results.map do ... extract_uris(...) end`, but nitrocop still
///   excluded them because only `each`/`select`-style iterators were
///   allowlisted. Keeping the same structural guards and only adding `map`
///   recovers those FNs without broadening to unrelated block shapes.
///
/// ## Fixes applied (2026-04-04)
/// - **String value check**: `UnsafeRangeCollector` now uses
///   `StringNode::unescaped()` to check decoded string values for `\n`,
///   matching RuboCop's `safe_to_split?` which checks `n.value.include?("\n")`.
///   Previously only source-level newline bytes were checked, causing:
///   - FPs: strings with escape sequences like `"AT+CLAC\r\n"` were not marked
///     unsafe (source has no literal newline), so calls containing them were
///     incorrectly flagged.
///   - FNs: string concatenations with `\` line continuation like
///     `"foo" \ "bar"` were falsely marked unsafe (source spans lines), so
///     calls containing them were incorrectly suppressed.
/// - **Block body exclusion**: `checked_chain_ranges` now excludes block bodies.
///   In RuboCop, the send node's range does not include the block (the block is
///   a parent node), so calls inside block bodies are checked independently.
///   In Prism, `CallNode` includes the block, so the old code suppressed ALL
///   calls inside block bodies of chains. Now only the chain portion (up to the
///   block start) is marked as checked. Additionally, `part_of_checked_chain`
///   now checks whether a node is inside a block body within the chain range
///   (via `inside_block_body_within_chain`), so calls like `extract_uris(...)`
///   inside `search_results.map do ... end.flatten` are correctly unsuppressed
///   even when the outer chain's range encompasses the block body.
/// - **Non-convertible block handling**: When a `CallNode` has a block that is NOT
///   convertible (i.e., the call has arguments but no parentheses, like
///   `config.wrappers :default, ... do |b| ... end`), the offense check now uses
///   the "send-only" range (up to the block start) instead of the full range.
///   In RuboCop's Parser AST, `on_send` walks up through convertible blocks but
///   stops at non-convertible ones, checking only the send portion. This recovers
///   ~2,653 FNs and resolves ~616 FPs with zero regressions.
///
/// ## Fixes applied (2026-04-06)
/// - **Data structure nesting check**: `part_of_checked_chain` now uses
///   `is_nested_in_data_structure()` to allow calls nested inside hash, array,
///   or other data structures within a chain to be checked independently. In
///   RuboCop, `on_send` walks up only through parent `send_type?` nodes. Calls
///   inside hashes (`key: match_array([...])`), arrays, or splats have non-send
///   parents (pair, hash, array) that stop the walk-up, so they're checked
///   independently. Previously, nitrocop's byte-range-based `checked_chain_ranges`
///   suppressed ALL calls within a chain's range, including these nested calls.
///   Resolves ~1350 FNs (e.g., `match_array(...)` inside `have_attributes(...)`)
///   with zero regressions.
/// - **`too_long` backslash fix**: The combined-line length calculation now only
///   strips trailing line-continuation backslashes, not ALL backslash characters.
///   Previously, `combined.retain(|&b| b != b'\\')` removed content backslashes
///   like `\1_\2` in regex replacements and `\d` in character classes, making
///   the combined line appear shorter than it actually is. This caused FPs for
///   method chains containing regex patterns where the true combined length
///   exceeded 120 chars but the backslash-stripped version didn't. Resolves
///   ~594 FPs with zero regressions.
///
/// ## Fixes applied (2026-04-07)
/// - Added missing visitor handlers for `IndexOperatorWriteNode`, `IndexOrWriteNode`,
///   `IndexAndWriteNode`, `CallOperatorWriteNode`, `CallOrWriteNode`, and
///   `CallAndWriteNode`. Previously, index operator writes like `foo[bar] ||= value`
///   and call operator writes like `obj.method += value` were not visited, causing
///   FNs for these patterns. These handlers ensure the `check_assignment` method is
///   invoked for these node types, enabling detection of redundant line breaks in
///   index/call operator write expressions. Resolves ~2 FNs with zero regressions.
///
/// ## Fixes applied (2026-04-08)
/// - **Direct argument walk-up**: nested multiline calls used as direct arguments
///   of an outer multiline call are now suppressed when the outer call has already
///   been checked. RuboCop's `on_send` walks from the inner send to its parent send
///   even when that parent relation comes from an argument position, not only a
///   receiver chain. Nitrocop previously only tracked receiver chains, which caused
///   false positives for cases like `SeedDump.dump(EventInstance\n.where(...), ...)`
///   and similar backslash-heavy argument lists.
/// - **Multiline regexp safety**: Prism represents a plain regexp as a single
///   `RegularExpressionNode`, but Parser exposes newline-containing regexp bodies
///   through descendant `:str` nodes. RuboCop therefore treats multiline regexps as
///   unsafe in `safe_to_split?`. Nitrocop now explicitly marks multiline regexp
///   literals unsafe so assignments like `GROUPED_INPUT_PATTERN = /.../x.freeze`
///   are no longer falsely flagged.
///
/// ## Fixes applied (2026-04-08, second batch)
/// - **Binary operator walk-up**: RuboCop's `on_send` walks up through parent
///   `OrNode`/`AndNode` (both `||`/`&&` and `or`/`and`). After walking up,
///   `operator_keyword?` returns true for these nodes, and `require_backslash?`
///   gates the offense on the operator line ending with `\`. Without `\`, no
///   offense is registered on ANY send inside the expression. Nitrocop did not
///   perform this walk-up, so multiline calls that were the RHS of `||`/`&&`
///   (e.g., `destroy || raise(...)`, `must_not_cache? || stale?(...)`) were
///   incorrectly flagged. The fix adds `inside_binary_op_without_backslash()`
///   which walks up the ancestor stack following RuboCop's walk-up rules and
///   suppresses offenses when the binary operator line lacks a trailing `\`.
///   Resolves ~144 FPs and ~51 FNs with zero regressions.
///
/// ## Fixes applied (2026-04-09)
/// - **Trailing `&.` join length**: RuboCop's single-line suitability
///   check collapses safe-navigation chains split after a trailing `&.` like
///   `foo&.\n  bar` without inserting a space. Nitrocop only handled the
///   leading-dot form (`foo\n  .bar`), so it overestimated joined length for
///   these safe-navigation chains and skipped real offenses when the true
///   joined line fit under the configured maximum.
/// - **Unary `!` wrapper anchoring**: Prism exposes `!foo&.\n  bar` as an
///   outer unary-`!` call wrapped around the multiline send, but RuboCop
///   reports the underlying send start (`foo`, not `!`). Nitrocop now skips
///   that wrapper so the inner call is checked and anchored like RuboCop.
/// - **Unary `!` wrapper narrowing**: that skip only applies to safe-navigation
///   receiver chains. RuboCop still reports ordinary unary-negated multiline
///   sends such as `!foo.\n  bar` and `!checks.values.\n  find { ... }`, but
///   nitrocop was skipping every unary-`!` wrapper and missing those offenses.
///
/// ## Fixes applied (2026-04-09, second batch)
/// - **UTF-8 character counting**: `too_long()` and Phase 2's combined-line check
///   now measure character length instead of byte length, matching RuboCop's
///   `String#length` which counts characters. Previously, multi-byte characters
///   (CJK, accented, etc.) inflated the measured length, causing FNs for lines
///   that fit within 120 characters but exceeded 120 bytes. Fixed by adding
///   `utf8_char_count()` which counts non-continuation bytes. Resolves ~92 FNs
///   (e.g., BCDice repo with Japanese text).
/// - **Phase 2 unsafe range overlap**: Phase 2's `has_unsafe` check now detects
///   unsafe ranges that START within the backslash group but extend beyond it
///   (e.g., case/until/while expressions on the continuation line). Previously
///   only ranges fully contained within the group were detected, causing FPs for
///   patterns like `foo || \ case @mode ... end` and `parent \ until cond`.
///   Resolves ~41 FPs (e.g., ruby2js repo). Added `UntilNode`, `WhileNode`, and
///   `ForNode` visitors to `UnsafeRangeCollector` to support modifier keywords.
///
/// ## Fixes applied (2026-04-10)
/// - **String continuation merging in `too_long`**: RuboCop's `to_single_line`
///   merges adjacent string literals across backslash continuations:
///   `"foo" \ "bar"` → `"foobar"` (same quotes), `"foo" \ 'bar'` → `"foo" + 'bar'`
///   (different quotes). Nitrocop's `too_long` previously joined these lines with
///   a space, keeping both sets of quotes: `"foo" "bar"` — 2 extra characters per
///   continuation. For deeply-indented expressions with string continuations, this
///   caused the combined length to exceed 120 chars when RuboCop's version fit,
///   resulting in FNs. For example, `raise ArgumentError, "long..." \ "msg"` inside
///   a 4-level-deep nesting would measure 124 chars (nitrocop) vs 117 chars (RuboCop).
///   Added `merge_string_continuation()` helper and `prev_had_backslash` tracking in
///   `too_long` to match RuboCop's merging behavior.
/// - **Phase 2 comma-tail span**: the text-based backslash pass now keeps walking
///   through immediately-following comma-terminated lines before measuring length.
///   RuboCop judges the whole continued call (for example `attr_reader \` followed
///   by many symbol arguments), but nitrocop previously only joined the backslash
///   line with the very next line. That produced false positives for long DSL-style
///   argument lists that still obviously continued after the first continuation line.
///
/// ## Fixes applied (2026-04-11)
/// - **Non-convertible block chain suppression**: outer sends such as
///   `expect(...).to receive(:find) do ... end.and_return(...)` were still
///   marking their full Prism byte range as "checked", which suppressed the
///   inner send that actually owns the non-convertible block. RuboCop stops its
///   `on_send` walk-up at that block boundary, so the multiline `expect(` /
///   `expect_any_instance_of(...).` send is the real offense target. Nitrocop
///   now lets call nodes that own a non-convertible block bypass outer
///   `checked_chain_ranges` suppression so those RSpec chains are checked at the
///   same boundary RuboCop uses.
///
/// ## Fixes applied (2026-04-12)
/// - **Phase 2 comment guard**: backslash continuations with inline comments in
///   the continued lines (for example `attr_reader \` lists with trailing
///   `# DEV(...)` comments) are now skipped. RuboCop will not collapse those
///   comment-bearing expressions onto one line.
/// - **Phase 2 branch-tail string continuations**: backslash string
///   continuations that end directly before `else`/`elsif`/`when`/`rescue`/`end`
///   are skipped without suppressing unrelated nested expressions. This matches
///   RuboCop for string continuations inside `if ... else ... end` assignments
///   while avoiding the broad FN regression from suppressing whole enclosing
///   conditional ranges.
/// - **Multiline interpolated string safety**: interpolated strings that contain
///   literal newlines are now marked unsafe-to-split as whole nodes, except for
///   `%`-newline delimiters like `x = %\n"#{foo}"` where the delimiter newline is
///   not part of the string's effective value. RuboCop treats expressions like
///   `\"<h2>#{\n  call\n}</h2>\"` as unsafe, but still flags newline-delimited
///   percent strings that fit on one line.
/// - **Safe-navigation block-chain precedence**: `Layout/SingleLineBlockChain`
///   only takes precedence for ordinary `.` chains, not `&.` chains. Nitrocop
///   now matches that by excluding safe-navigation callers from the
///   single-line-block precedence collector, so patterns like
///   `registry\n  .find { ... }\n  &.command_class` are correctly reported.
/// - **Phase 2 enclosing-expression guard**: the text fallback now skips
///   backslash groups that are already covered by a larger multiline AST call
///   chain, or by a class-header inheritance span like `class Foo < \ Bar`.
///   RuboCop judges those larger expressions as a whole, so nitrocop must not
///   emit extra inner reports for long operator chains or superclass headers.
///
/// ## Fixes applied (2026-04-17)
/// - **SingleLineBlockChain gating**: `configured_to_not_be_inspected()` now
///   only defers to `Layout/SingleLineBlockChain` when that cop is actually
///   enabled in the resolved config. Previously the precedence check was always
///   active, so repos that disabled `Layout/SingleLineBlockChain` still had
///   multiline chains like `e.select { ... }\n  .join` suppressed here, causing
///   false negatives relative to RuboCop.
/// - **Stabby lambda precedence**: RuboCop's
///   `other_cop_takes_precedence?` also considers single-line stabby lambdas
///   (`-> { ... }`) whose containing send has a dot. Nitrocop only tracked
///   `BlockNode`, so multiline dotted calls like
///   `assoc.has_many ..., -> { ... }, ...` and outer wrappers like
///   `assert_equal(..., obj.call(-> { ... }))` were incorrectly flagged even
///   though `Layout/SingleLineBlockChain` should take precedence.
///
/// ## Fixes applied (2026-04-20)
/// - **Phase 2 ternary predicates**: backslash-continued predicates inside
///   multiline `?:` expressions are now measured against just the predicate
///   span instead of being blocked by the enclosing ternary's unsafe range.
///   The text fallback also anchors those reports at the first non-whitespace
///   column and keeps the `foo \n && bar` / `foo \n || bar` skip disabled only
///   for ternary predicates. This recovers FNs like
///   `o.col_type.nil? \ && ... \ ? ... : ...` without broadening ordinary
///   multiline `&&` / `||` handling.
/// - **Config-sensitive safe-navigation contexts**: corpus examples like
///   `!current_course_user&.\n  email_unsubscriptions...` only reproduce when
///   `Layout/LineLength` is wide enough for the indented chain to collapse.
///   Keep those in config-specific tests rather than the default fixture so
///   shared fixtures do not encode repo-specific line-length settings.
/// - **CRLF line endings**: `too_long()` and the backslash fallback now strip
///   trailing `\r` as well as spaces/tabs. Prism's line slices preserve the
///   carriage return byte in CRLF files, but RuboCop's `processed_source.lines`
///   length check does not treat that byte as visible source content. Without
///   trimming it, expressions that fit exactly at `MaxLineLength` in CRLF files
///   were measured one character too long per line and incorrectly suppressed.
///
/// ## Fixes applied (2026-04-25)
/// - **Safe-navigation chain boundary with trailing blocks**: outer `&.` calls
///   that own a trailing block (for example `receiver&.select { ... }&.values`)
///   still form a safe-navigation boundary for the immediately nested send even
///   though `checked_chain_ranges` truncates the outer range before the block
///   body. Nitrocop now keys that boundary check off the nearest ancestor call
///   start, which recovers FNs like
///   `(@document[...] || {})\n  &.select { ... }\n  &.values` without
///   unsuppressing earlier plain-send segments in the same chain.
/// - **Backslash `if`/`unless` condition anchoring**: the text fallback now
///   reports backslash-continued `if`/`unless` conditions from the condition
///   expression, not from the keyword indentation. This matches RuboCop for
///   cases like `if foo && \` newline `bar`.
/// - **Full-line suitability measurement**: even when RuboCop reports an inner
///   call that starts mid-line (for example the RHS of an assignment), its
///   `too_long?` check still joins the full physical lines in the span. Keep
///   nitrocop aligned with that behavior; slicing the start/end columns causes
///   false positives on long regex-heavy chains that RuboCop accepts.
/// - **Parser unsafe range parity**: `case` pattern matching, `for`, `while`,
///   and `until` nodes are no longer treated as AST unsafe-to-split ranges
///   because RuboCop's `safe_to_split?` does not include those node types.
///   They remain Phase 2 blocking ranges where needed for modifier backslash
///   contexts. The backslash fallback also now anchors `elsif` and leading
///   parenthesized continuation groups at RuboCop's reported expression column.
/// - **Backslash lexical guards**: the text fallback now skips condition
///   headers whose source is only `if`, `elsif`, or `unless` before a trailing
///   backslash, where RuboCop reports no offense, while still allowing a
///   same-line condition ending in `&&` plus a trailing backslash to be flagged.
///   It also treats Ruby's `$\\` global variable as source content rather than
///   a line continuation marker.
///
/// - NOTE: The CLI does not properly enable this preview cop even with `--preview`.
///   Unit tests bypass CLI filtering and work correctly.
///
/// ## Fixes applied (2026-04-26)
/// - **Direct ternary arm split**: RuboCop does not report a backslash on a
///   predicate line when the next physical line is already the leading `?`
///   ternary arm (`predicate \` newline `? then : else`). Nitrocop's text
///   fallback now skips only that direct shape while still reporting longer
///   backslash-continued predicates before a later ternary arm.
/// - **Modifier return/unless and mixed operators**: The text fallback now
///   anchors `return unless condition || \` at the condition expression, and
///   only suppresses `foo \` newline `&& bar` when the first line is not already
///   a boolean operator expression like `foo || bar \`.
///
/// ## Fixes applied (2026-04-27)
/// - **Multistatement class body safety**: Prism models class bodies as a
///   `StatementsNode`, while Parser exposes a multiline `begin` descendant when
///   the body has multiple statements. The unsafe range collector now treats
///   those multistatement class bodies as unsafe so class expressions ending in
///   `.new` are not falsely collapsed, while single-statement class receivers
///   remain reportable like RuboCop.
/// - **Corpus-derived continuation coverage**: command-style split strings,
///   chained assignments, and boolean operator backslash continuations are
///   covered in fixtures while preserving the broad value-only split-string
///   guard needed for RuboCop-accepted hash values, return expressions, and
///   long call arguments.
///
/// ## Fixes applied (2026-09-18)
/// - **Verbatim `to_single_line` port**: `too_long` no longer reconstructs the
///   joined line by trimming each physical line and gluing them with a single
///   space. It now joins the raw lines of the span with `\n` and applies
///   RuboCop's five `to_single_line` substitutions literally (via
///   `regex::bytes`, with the backreference expanded into the two same-quote
///   cases and the `(?=(&)?\.\w)` lookahead emulated by a capture). Two
///   whitespace details were the actual divergence:
///   - trailing whitespace on the **last** line of the span is never followed
///     by a newline, so nothing strips it — `params[...] = {` … `}  ` measures
///     121 chars in RuboCop and 119 in the old reconstruction (scinote-web
///     `repository_rows_service_spec.rb`, LubyRuffy/fofa `ziptest.rb`);
///   - the chain-dot rule `/\n\s*(?=(&)?\.\w)/` consumes only the newline
///     and the *following* indent, so the previous line's trailing padding and
///     its line-continuation backslash both survive into the joined string
///     (xwmx/pandoc-ruby `test_pandoc_ruby.rb`, tamc/excel_to_code).
///
///   Sampled corpus effect (21 repos reproducing the oracle's per-repo counts):
///   FP 39 → 23, FN unchanged.
/// - **Chained assignment**: `check_assignment` no longer consults
///   `part_of_reported_node`. RuboCop's `CheckAssignment` path is
///   `return unless offense?(node); register_offense(node)` — the
///   `!part_of_ignored_node?(node)` guard exists only on `on_send`. So in
///   `@a =\n@b =\n@c = nil` each nested write node that is still multiline is
///   reported, while nitrocop reported only the outermost one. Where the
///   outermost join exceeds `MaxLineLength` (mcorino/wxRuby3
///   `samples/widgets/button.rb:51`) nitrocop previously reported nothing at
///   all, because the outer node was rejected and the inner ones suppressed.
///
///   Sampled corpus effect: FN 77 → 33 (wxRuby3 41 → 0, gdelugre/origami
///   3 → 0, dependabot-core 3 → 0), FP unchanged at 23.
/// - **Binary operator walk-up now reports**: `on_send`'s walk-up
///   (`node = node.parent while node.parent&.send_type? || convertible_block?(node)
///   || node.parent.is_a?(RuboCop::AST::BinaryOperatorNode)`) can land on an
///   `and`/`or` node, and `offense?` then returns `require_backslash?(node)`.
///   nitrocop only ever used this for *suppression*
///   (`inside_binary_op_without_backslash`); the positive case was left to the
///   Phase 2 text scan, which misses operators nested in array elements, hash
///   values, `unless` conditions and parenthesized receivers. Added
///   `visit_and_node`/`visit_or_node` with:
///   - `binary_operator_walks_up()` — the walk-up continues when the parent is
///     a `CallNode` (Prism's `ArgumentsNode` wrapper is transparent only for
///     calls, since `return`/`yield`/`super` also carry one and are not
///     `send_type?`) or another `and`/`or`; in that case the enclosing node is
///     the one RuboCop checks;
///   - `reaches_send()` — the walk-up only traverses send and binary-operator
///     parents, so an operand that is a literal, a variable read or a
///     parenthesized expression (Parser's `begin`) never reaches the operator
///     and cannot trigger it. `( (a && b) \ || (c && d) )` reports the inner
///     `&&`, never the outer `||` (ruby/tk `lib/tk/optionobj.rb:146`);
///   - `operator_line_ends_with_backslash()` — `require_backslash?` verbatim,
///     keyed on the line of `node.loc.operator`, so in
///     `( (a \ && b ) \ || ( c \ && d ) )` only the first `&&` fires.
///
///   Sampled corpus effect: FN 33 → 8, FP unchanged at 23. The eight remaining
///   sampled FNs are all in non-`.rb` or deliberately-unparseable corpus files
///   (`Gemfile`, `*.gemspec`, `bad_syntax.rb`, `not-ruby.rb`), i.e. file
///   discovery/parsing, not cop logic.
/// - **Phase 2 removed**: the text-based backslash-continuation scan is gone.
///   RuboCop only ever registers this cop's offenses from `on_send` /
///   `on_csend` (walked up through sends, convertible blocks and `and`/`or`)
///   and from `CheckAssignment`. A backslash is never itself a reason to
///   report — it only affects `require_backslash?` and the joined length. With
///   the operator walk-up now modelled in the AST phase, Phase 2 had no shape
///   left to cover, and all its remaining output was noise: over a 72-repo
///   sample (~50k offenses, including six repos with >1000 offenses each) it
///   produced 20 offenses RuboCop does not, and zero that the AST phase did
///   not already produce. Deleting it removes ~560 lines of heuristics
///   (group spans, ternary/branch-tail/keyword guards, enclosing-expression
///   checks) whose only purpose was damage control.
///
///   Sampled corpus effect: FP 33 → 13, FN unchanged.
/// - **`%x` executable strings**: Parser models `%x{...}` as an `xstr` whose
///   children are `str` nodes, so `safe_to_split?`'s
///   `each_descendant(:dstr, :str)` sees a newline in the value. Prism has a
///   single `XStringNode`/`InterpolatedXStringNode`, so the unsafe-range
///   collector now checks those explicitly (zhaocai/alfred-workflow,
///   opal `spec/opal/stdlib/native/hash_spec.rb`).
/// - **Heredoc interpolations**: the collector used to stop descending once it
///   pushed a heredoc's range, so strings inside a heredoc's `#{...}` code were
///   never collected. The interpolated code is checked independently (Parser's
///   `begin` around an interpolation stops `on_send`'s walk-up), and its own
///   `safe_to_split?` looks at those strings — `#{matter.gsub(%r!\n!, "\n    ")}`
///   split over three lines is not reportable because `"\n    "` holds a
///   newline (xcatliu/jekyllcn `features/support/helpers.rb`).
///
///   Sampled corpus effect: FP 10 → 5, FN unchanged at 4.
pub struct RedundantLineBreak;

impl Cop for RedundantLineBreak {
    fn name(&self) -> &'static str {
        "Layout/RedundantLineBreak"
    }

    fn default_enabled(&self) -> bool {
        false
    }

    fn check_source(
        &self,
        source: &SourceFile,
        parse_result: &ruby_prism::ParseResult<'_>,
        _code_map: &CodeMap,
        config: &CopConfig,
        diagnostics: &mut Vec<Diagnostic>,
        _corrections: Option<&mut Vec<crate::correction::Correction>>,
    ) {
        let inspect_blocks = config.get_bool("InspectBlocks", false);
        let max_line_length = config.get_usize("MaxLineLength", 120);
        let single_line_block_chain_enabled = config.get_bool("SingleLineBlockChainEnabled", true);

        // Collect comment line numbers (1-indexed) for the comment_within check.
        let comment_lines: HashSet<usize> = parse_result
            .comments()
            .map(|c| {
                let (line, _) = source.offset_to_line_col(c.location().start_offset());
                line
            })
            .collect();

        // Pre-collect ranges of unsafe-to-split constructs:
        // if/unless/case/begin/def nodes, heredocs, and multiline strings.
        let mut unsafe_collector = UnsafeRangeCollector {
            ranges: Vec::new(),
            group_blocking_ranges: Vec::new(),
            ternary_ranges: Vec::new(),
        };
        unsafe_collector.visit(&parse_result.node());
        let unsafe_ranges = unsafe_collector.ranges;

        // Pre-collect block ranges (for InspectBlocks: false check)
        let mut block_collector = BlockRangeCollector {
            ranges: Vec::new(),
            source,
        };
        block_collector.visit(&parse_result.node());
        let block_ranges = block_collector.ranges;

        // Pre-collect single-line block ranges (for Layout/SingleLineBlockChain precedence)
        let mut sl_block_collector = SingleLineBlockCollector {
            ranges: Vec::new(),
            source,
            ancestors: Vec::new(),
        };
        sl_block_collector.visit(&parse_result.node());
        let single_line_block_ranges = sl_block_collector.ranges;

        // Phase 1: AST-based detection (method calls and assignments)
        let mut visitor = RedundantLineBreakVisitor {
            source,
            cop_name: self.name(),
            max_line_length,
            inspect_blocks,
            comment_lines: &comment_lines,
            unsafe_ranges: &unsafe_ranges,
            block_ranges: &block_ranges,
            single_line_block_ranges: &single_line_block_ranges,
            single_line_block_chain_enabled,
            ast_diagnostics: Vec::new(),
            reported_starts: HashSet::new(),
            reported_ranges: Vec::new(),
            checked_chain_ranges: Vec::new(),
            ancestors: Vec::new(),
        };
        visitor.visit(&parse_result.node());

        diagnostics.extend(visitor.ast_diagnostics);
    }
}

/// Collects byte ranges of unsafe-to-split constructs.
///
/// Matches RuboCop's `safe_to_split?` from `CheckSingleLineSuitability`:
///   node.each_descendant(:if, :case, :kwbegin, :any_def).none? &&
///     node.each_descendant(:dstr, :str).none? { |n| n.heredoc? || n.value.include?("\n") } &&
///     node.each_descendant(:begin, :sym).none? { |b| !b.single_line? }
///
/// Parser exposes multiline regexp bodies through descendant `:str` nodes, so
/// RuboCop's `safe_to_split?` implicitly treats multiline regexps as unsafe.
/// Prism represents a plain regexp as a single `RegularExpressionNode`, so
/// nitrocop must explicitly mark those ranges unsafe. Arrays (`%w`, `%i`) are
/// still intentionally left alone because RuboCop does flag those.
struct UnsafeRangeCollector {
    /// (start_offset, end_offset) of nodes that make their parent unsafe to merge.
    ranges: Vec<(usize, usize)>,
    /// Expression ranges that should also suppress Phase 2 backslash groups
    /// when they cover the whole group.
    group_blocking_ranges: Vec<(usize, usize)>,
    /// Ternary (`?:`) IfNode ranges. These stay unsafe for enclosing
    /// expressions, but Phase 2 sometimes needs to judge just the predicate.
    ternary_ranges: Vec<(usize, usize)>,
}

impl<'pr> Visit<'pr> for UnsafeRangeCollector {
    fn visit_if_node(&mut self, node: &ruby_prism::IfNode<'pr>) {
        let loc = node.location();
        self.ranges.push((loc.start_offset(), loc.end_offset()));
        self.group_blocking_ranges
            .push((loc.start_offset(), loc.end_offset()));
        if node.if_keyword_loc().is_none() {
            self.ternary_ranges
                .push((loc.start_offset(), loc.end_offset()));
        }
        // Recurse into children so nested unsafe constructs (strings, regexps,
        // inner ifs) inside the if body are also collected. The if itself is
        // unsafe for its parent, but children may need their own unsafe ranges
        // for inner assignments.
        ruby_prism::visit_if_node(self, node);
    }

    fn visit_unless_node(&mut self, node: &ruby_prism::UnlessNode<'pr>) {
        let loc = node.location();
        self.ranges.push((loc.start_offset(), loc.end_offset()));
        self.group_blocking_ranges
            .push((loc.start_offset(), loc.end_offset()));
        ruby_prism::visit_unless_node(self, node);
    }

    fn visit_case_node(&mut self, node: &ruby_prism::CaseNode<'pr>) {
        let loc = node.location();
        self.ranges.push((loc.start_offset(), loc.end_offset()));
        self.group_blocking_ranges
            .push((loc.start_offset(), loc.end_offset()));
        ruby_prism::visit_case_node(self, node);
    }

    fn visit_case_match_node(&mut self, node: &ruby_prism::CaseMatchNode<'pr>) {
        let loc = node.location();
        self.group_blocking_ranges
            .push((loc.start_offset(), loc.end_offset()));
        ruby_prism::visit_case_match_node(self, node);
    }

    fn visit_begin_node(&mut self, node: &ruby_prism::BeginNode<'pr>) {
        let loc = node.location();
        self.ranges.push((loc.start_offset(), loc.end_offset()));
        self.group_blocking_ranges
            .push((loc.start_offset(), loc.end_offset()));
        ruby_prism::visit_begin_node(self, node);
    }

    fn visit_def_node(&mut self, node: &ruby_prism::DefNode<'pr>) {
        let loc = node.location();
        self.ranges.push((loc.start_offset(), loc.end_offset()));
        // Must recurse: inner assignments need to see unsafe ranges from
        // strings, ifs, etc. nested inside this def body.
        ruby_prism::visit_def_node(self, node);
    }

    fn visit_class_node(&mut self, node: &ruby_prism::ClassNode<'pr>) {
        if class_body_has_multiple_statements(node) {
            let loc = node.location();
            self.ranges.push((loc.start_offset(), loc.end_offset()));
        }
        if let Some(superclass) = node.superclass() {
            let loc = node.location();
            let super_loc = superclass.location();
            self.group_blocking_ranges
                .push((loc.start_offset(), super_loc.end_offset()));
        }
        ruby_prism::visit_class_node(self, node);
    }

    fn visit_string_node(&mut self, node: &ruby_prism::StringNode<'pr>) {
        if let Some(open) = node.opening_loc() {
            if open.as_slice().starts_with(b"<<") {
                let loc = node.location();
                self.ranges.push((loc.start_offset(), loc.end_offset()));
                return;
            }
        }
        // Check the decoded VALUE for newlines, not the source representation.
        // This matches RuboCop's `n.value.include?("\n")` in `safe_to_split?`.
        // Source-level checks miss escape sequences like "\r\n" (FPs) and
        // falsely catch backslash line continuations "foo" \ "bar" (FNs).
        if node.unescaped().contains(&b'\n') {
            let loc = node.location();
            self.ranges.push((loc.start_offset(), loc.end_offset()));
        }
    }

    fn visit_interpolated_string_node(&mut self, node: &ruby_prism::InterpolatedStringNode<'pr>) {
        if let Some(open) = node.opening_loc() {
            if open.as_slice().starts_with(b"<<") {
                let loc = node.location();
                self.ranges.push((loc.start_offset(), loc.end_offset()));
                // Keep descending: a heredoc body can hold `#{...}`
                // interpolations whose code is checked independently (the
                // interpolation is a `begin` node in Parser, which stops
                // `on_send`'s walk-up), and the strings inside that code are
                // `:str` descendants for its own `safe_to_split?`.
                ruby_prism::visit_interpolated_string_node(self, node);
                return;
            }

            // Prism models `%`-newline delimiters like:
            //
            //   x = %
            //   "#{foo}"
            //
            // as a multiline InterpolatedStringNode whose opening token ends
            // with `\n`. RuboCop still treats these as safe-to-split because
            // the delimiter newline is not part of the effective string value.
            if open.as_slice().ends_with(b"\n") {
                ruby_prism::visit_interpolated_string_node(self, node);
                return;
            }
        }
        // Multiline interpolated strings are unsafe in RuboCop even when the
        // embedded send would fit on one line by itself. Treat only LITERAL
        // newlines as unsafe here. Backslash string continuations such as
        // `"foo #{x}" \` newline `"bar #{y}"` should still be checkable.
        if contains_non_continuation_newline(node.location().as_slice()) {
            let loc = node.location();
            self.ranges.push((loc.start_offset(), loc.end_offset()));
        }
        ruby_prism::visit_interpolated_string_node(self, node);
    }

    /// Parser models `%x{...}` as an `xstr` whose children are `str` nodes, so
    /// `safe_to_split?`'s `each_descendant(:dstr, :str)` sees them and a
    /// newline in the value makes the enclosing expression unsafe. Prism has a
    /// single `XStringNode`, so the check has to be made here.
    fn visit_x_string_node(&mut self, node: &ruby_prism::XStringNode<'pr>) {
        if node.opening_loc().as_slice().starts_with(b"<<") || node.unescaped().contains(&b'\n') {
            let loc = node.location();
            self.ranges.push((loc.start_offset(), loc.end_offset()));
        }
    }

    fn visit_interpolated_x_string_node(
        &mut self,
        node: &ruby_prism::InterpolatedXStringNode<'pr>,
    ) {
        if node.opening_loc().as_slice().starts_with(b"<<")
            || contains_non_continuation_newline(node.location().as_slice())
        {
            let loc = node.location();
            self.ranges.push((loc.start_offset(), loc.end_offset()));
        }
        ruby_prism::visit_interpolated_x_string_node(self, node);
    }

    fn visit_symbol_node(&mut self, node: &ruby_prism::SymbolNode<'pr>) {
        // Check decoded value for newlines, matching RuboCop's safe_to_split?.
        if node.unescaped().contains(&b'\n') {
            let loc = node.location();
            self.ranges.push((loc.start_offset(), loc.end_offset()));
        }
    }

    fn visit_interpolated_symbol_node(&mut self, node: &ruby_prism::InterpolatedSymbolNode<'pr>) {
        // Rely on recursion into child StringNode parts for newline detection.
        ruby_prism::visit_interpolated_symbol_node(self, node);
    }

    /// Multiline parenthesized groups `(...)` — maps to `:begin` in Parser AST.
    /// RuboCop's `safe_to_split?` checks
    /// `node.each_descendant(:begin, :sym).none? { |b| !b.single_line? }`.
    fn visit_parentheses_node(&mut self, node: &ruby_prism::ParenthesesNode<'pr>) {
        let content = node.location().as_slice();
        if content.contains(&b'\n') {
            let loc = node.location();
            self.ranges.push((loc.start_offset(), loc.end_offset()));
            self.group_blocking_ranges
                .push((loc.start_offset(), loc.end_offset()));
        }
        // Still recurse into children to find nested unsafe constructs
        ruby_prism::visit_parentheses_node(self, node);
    }

    fn visit_until_node(&mut self, node: &ruby_prism::UntilNode<'pr>) {
        let loc = node.location();
        self.group_blocking_ranges
            .push((loc.start_offset(), loc.end_offset()));
        ruby_prism::visit_until_node(self, node);
    }

    fn visit_while_node(&mut self, node: &ruby_prism::WhileNode<'pr>) {
        let loc = node.location();
        self.group_blocking_ranges
            .push((loc.start_offset(), loc.end_offset()));
        ruby_prism::visit_while_node(self, node);
    }

    fn visit_for_node(&mut self, node: &ruby_prism::ForNode<'pr>) {
        let loc = node.location();
        self.group_blocking_ranges
            .push((loc.start_offset(), loc.end_offset()));
        ruby_prism::visit_for_node(self, node);
    }

    fn visit_regular_expression_node(&mut self, node: &ruby_prism::RegularExpressionNode<'pr>) {
        if node.unescaped().contains(&b'\n') {
            let loc = node.location();
            self.ranges.push((loc.start_offset(), loc.end_offset()));
        }
        ruby_prism::visit_regular_expression_node(self, node);
    }

    fn visit_interpolated_regular_expression_node(
        &mut self,
        node: &ruby_prism::InterpolatedRegularExpressionNode<'pr>,
    ) {
        if node.location().as_slice().contains(&b'\n') {
            let loc = node.location();
            self.ranges.push((loc.start_offset(), loc.end_offset()));
        }
        ruby_prism::visit_interpolated_regular_expression_node(self, node);
    }
}

fn class_body_has_multiple_statements(node: &ruby_prism::ClassNode<'_>) -> bool {
    node.body()
        .and_then(|body| body.as_statements_node())
        .is_some_and(|statements| statements.body().len() > 1)
}

/// Collects byte ranges of block/lambda nodes, tracking whether each is multiline.
struct BlockRangeCollector<'a> {
    /// (start_offset, end_offset, is_multiline)
    ranges: Vec<(usize, usize, bool)>,
    source: &'a SourceFile,
}

impl<'pr> Visit<'pr> for BlockRangeCollector<'_> {
    fn visit_block_node(&mut self, node: &ruby_prism::BlockNode<'pr>) {
        let loc = node.location();
        let (start_line, _) = self.source.offset_to_line_col(loc.start_offset());
        let (end_line, _) = self
            .source
            .offset_to_line_col(loc.end_offset().saturating_sub(1));
        let is_multiline = start_line != end_line;
        self.ranges
            .push((loc.start_offset(), loc.end_offset(), is_multiline));
        ruby_prism::visit_block_node(self, node);
    }

    fn visit_lambda_node(&mut self, node: &ruby_prism::LambdaNode<'pr>) {
        let loc = node.location();
        let (start_line, _) = self.source.offset_to_line_col(loc.start_offset());
        let (end_line, _) = self
            .source
            .offset_to_line_col(loc.end_offset().saturating_sub(1));
        let is_multiline = start_line != end_line;
        self.ranges
            .push((loc.start_offset(), loc.end_offset(), is_multiline));
        ruby_prism::visit_lambda_node(self, node);
    }
}

/// Collects byte ranges of single-line block/lambda nodes whose parent (in Parser AST
/// terms) is a send with a dot.
///
/// Matches RuboCop's `other_cop_takes_precedence?` which checks:
///   `block_node.parent.send_type? && block_node.parent.loc.dot && !block_node.multiline?`
///
/// In Parser AST, `block_node.parent` is the CONTAINING node. For:
///   - `foo.map { ... }.compact` → block parent is `.compact` send (has dot) ✓
///   - `assoc.has_many :x, -> { ... }, ...` → lambda block parent is `.has_many` send ✓
///   - `foo.bar(proc { ... })` → block parent is `.bar` send (has dot) ✓
///   - `x = foo.map { ... }` → block parent is assignment (no dot) ✗
///   - `bar(proc { ... })` → block parent is `bar` send (no dot) ✗
///
/// In Prism, a block is always a child of its "owning" CallNode. The "containing"
/// node in Parser AST terms is found by skipping the owning CallNode and any
/// ArgumentsNode wrapper in the ancestor stack. A `LambdaNode` is already the
/// block wrapper, so only Prism-only argument wrappers are skipped.
struct SingleLineBlockCollector<'a, 'pr> {
    ranges: Vec<(usize, usize)>,
    source: &'a SourceFile,
    ancestors: Vec<ruby_prism::Node<'pr>>,
}

impl<'pr> Visit<'pr> for SingleLineBlockCollector<'_, 'pr> {
    fn visit_branch_node_enter(&mut self, node: ruby_prism::Node<'pr>) {
        self.ancestors.push(node);
    }

    fn visit_branch_node_leave(&mut self) {
        self.ancestors.pop();
    }

    fn visit_leaf_node_enter(&mut self, _node: ruby_prism::Node<'pr>) {}

    fn visit_block_node(&mut self, node: &ruby_prism::BlockNode<'pr>) {
        let loc = node.location();
        let (start_line, _) = self.source.offset_to_line_col(loc.start_offset());
        let (end_line, _) = self
            .source
            .offset_to_line_col(loc.end_offset().saturating_sub(1));
        if start_line == end_line && self.containing_call_has_dot_for_block() {
            self.ranges.push((loc.start_offset(), loc.end_offset()));
        }
        ruby_prism::visit_block_node(self, node);
    }

    fn visit_lambda_node(&mut self, node: &ruby_prism::LambdaNode<'pr>) {
        let loc = node.location();
        let (start_line, _) = self.source.offset_to_line_col(loc.start_offset());
        let (end_line, _) = self
            .source
            .offset_to_line_col(loc.end_offset().saturating_sub(1));
        if start_line == end_line && self.containing_call_has_dot_for_lambda() {
            self.ranges.push((loc.start_offset(), loc.end_offset()));
        }
        ruby_prism::visit_lambda_node(self, node);
    }
}

impl SingleLineBlockCollector<'_, '_> {
    fn call_has_dot(call: &ruby_prism::CallNode<'_>) -> bool {
        call.call_operator_loc()
            .is_some_and(|loc| loc.as_slice() == b".")
    }

    /// Check if the block's "parent" in Parser AST terms is a CallNode with a dot.
    ///
    /// Walk up ancestors, skipping the owning CallNode (immediate parent of the
    /// block in Prism) and any ArgumentsNode wrapper (Prism-only, no Parser
    /// equivalent). The next significant node is the "containing" context.
    fn containing_call_has_dot_for_block(&self) -> bool {
        let len = self.ancestors.len();
        // Need at least 2 ancestors: the BlockNode itself (pushed by
        // visit_branch_node_enter before visit_block_node runs) and
        // the owning CallNode above it.
        if len < 2 {
            return false;
        }
        let mut skipped_owning_call = false;
        // Start at len-2 to skip the current BlockNode at the top of the stack.
        for i in (0..len - 1).rev() {
            let ancestor = &self.ancestors[i];
            // The first CallNode we encounter (nearest) is the "owning" call
            // (e.g., `proc` for `proc { ... }`, or `.map` for `.map { ... }`).
            // Skip it to find the containing context.
            if !skipped_owning_call && ancestor.as_call_node().is_some() {
                skipped_owning_call = true;
                continue;
            }
            // ArgumentsNode is a Prism wrapper with no Parser AST equivalent.
            if ancestor.as_arguments_node().is_some() {
                continue;
            }
            // Found the containing node. Check if it's a CallNode with a dot.
            return ancestor
                .as_call_node()
                .is_some_and(|call| Self::call_has_dot(&call));
        }
        false
    }

    /// Lambda nodes (`-> { ... }`) are already the block wrapper in Prism, so
    /// there is no "owning" CallNode to skip. Only skip Prism-only arguments
    /// wrappers, then check the containing node directly.
    fn containing_call_has_dot_for_lambda(&self) -> bool {
        let len = self.ancestors.len();
        if len < 1 {
            return false;
        }
        for i in (0..len - 1).rev() {
            let ancestor = &self.ancestors[i];
            if ancestor.as_arguments_node().is_some() {
                continue;
            }
            return ancestor
                .as_call_node()
                .is_some_and(|call| Self::call_has_dot(&call));
        }
        false
    }
}

/// AST visitor that finds multiline expressions that could fit on a single line.
struct RedundantLineBreakVisitor<'a, 'pr> {
    source: &'a SourceFile,
    cop_name: &'static str,
    max_line_length: usize,
    inspect_blocks: bool,
    comment_lines: &'a HashSet<usize>,
    unsafe_ranges: &'a [(usize, usize)],
    block_ranges: &'a [(usize, usize, bool)],
    single_line_block_ranges: &'a [(usize, usize)],
    single_line_block_chain_enabled: bool,
    ast_diagnostics: Vec<Diagnostic>,
    reported_starts: HashSet<usize>,
    /// Byte ranges of nodes already reported, to skip descendant checks.
    reported_ranges: Vec<(usize, usize)>,
    /// Byte ranges of outermost call chain nodes that were checked (whether reported or not).
    /// Inner CallNodes within these ranges are skipped to match RuboCop's walk-up behavior.
    checked_chain_ranges: Vec<(usize, usize)>,
    ancestors: Vec<ruby_prism::Node<'pr>>,
}

impl<'a, 'pr> RedundantLineBreakVisitor<'a, 'pr> {
    fn is_multiline(&self, start_offset: usize, end_offset: usize) -> bool {
        let (start_line, _) = self.source.offset_to_line_col(start_offset);
        let (end_line, _) = self
            .source
            .offset_to_line_col(end_offset.saturating_sub(1).max(start_offset));
        start_line != end_line
    }

    /// Check if combining lines of this span would exceed max_line_length.
    ///
    /// Matches RuboCop's `to_single_line` method which merges string
    /// continuations across backslash: `"foo" \ "bar"` → `"foobar"` (same
    /// quotes merged), `"foo" \ 'bar'` → `"foo" + 'bar'` (different quotes).
    fn too_long(&self, start_offset: usize, end_offset: usize) -> bool {
        let (start_line, _) = self.source.offset_to_line_col(start_offset);
        let (end_line, _) = self
            .source
            .offset_to_line_col(end_offset.saturating_sub(1).max(start_offset));

        let lines: Vec<&[u8]> = self.source.lines().collect();
        let mut joined: Vec<u8> = Vec::new();
        for line_num in start_line..=end_line {
            if line_num > lines.len() {
                break;
            }
            if line_num > start_line {
                joined.push(b'\n');
            }
            let mut line = lines[line_num - 1];
            // Prism keeps the CR of a CRLF pair in its line slices; RuboCop's
            // `processed_source.lines` does not expose it as source content.
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            joined.extend_from_slice(line);
        }

        utf8_char_count(&to_single_line(&joined)) > self.max_line_length
    }

    fn comment_within(&self, start_offset: usize, end_offset: usize) -> bool {
        let (start_line, _) = self.source.offset_to_line_col(start_offset);
        let (end_line, _) = self
            .source
            .offset_to_line_col(end_offset.saturating_sub(1).max(start_offset));
        self.comment_lines
            .iter()
            .any(|&line| line >= start_line && line <= end_line)
    }

    /// Check if any unsafe range is contained within (or overlaps) the given span.
    fn contains_unsafe(&self, start_offset: usize, end_offset: usize) -> bool {
        self.unsafe_ranges
            .iter()
            .any(|&(us, ue)| us >= start_offset && ue <= end_offset)
    }

    /// Check if any multiline block range is contained within the given span.
    /// This matches RuboCop's `any_descendant?(node, :any_block, &:multiline?)`.
    fn contains_multiline_block(&self, start_offset: usize, end_offset: usize) -> bool {
        self.block_ranges
            .iter()
            .any(|&(bs, be, multiline)| multiline && bs >= start_offset && be <= end_offset)
    }

    /// Check if any single-line block is contained within the given span.
    fn contains_single_line_block(&self, start_offset: usize, end_offset: usize) -> bool {
        self.single_line_block_ranges
            .iter()
            .any(|&(bs, be)| bs >= start_offset && be <= end_offset)
    }

    fn suitable_as_single_line(&self, start_offset: usize, end_offset: usize) -> bool {
        !self.too_long(start_offset, end_offset)
            && !self.comment_within(start_offset, end_offset)
            && !self.contains_unsafe(start_offset, end_offset)
    }

    fn configured_to_not_be_inspected(&self, start_offset: usize, end_offset: usize) -> bool {
        // Layout/SingleLineBlockChain takes precedence for single-line blocks in chains
        if self.single_line_block_chain_enabled
            && self.contains_single_line_block(start_offset, end_offset)
        {
            return true;
        }
        // When InspectBlocks is false (default), skip expressions containing
        // multiline blocks. This matches RuboCop's:
        //   node.any_block_type? || any_descendant?(node, :any_block, &:multiline?)
        if !self.inspect_blocks && self.contains_multiline_block(start_offset, end_offset) {
            return true;
        }
        false
    }

    /// Check if a byte offset falls within any already-reported node's range.
    fn part_of_reported_node(&self, start_offset: usize, end_offset: usize) -> bool {
        self.reported_ranges
            .iter()
            .any(|&(rs, re)| start_offset >= rs && end_offset <= re)
    }

    /// Check if a node is an inner part of a call chain that was already checked.
    /// This prevents inner CallNodes from being individually checked when the
    /// outermost CallNode in the chain was already visited (and either reported or rejected).
    /// Calls inside block bodies within the chain are NOT suppressed — in RuboCop's
    /// Parser AST, blocks are parent nodes (not part of the send's range), so calls
    /// inside block bodies are checked independently.
    /// Calls nested inside data structures (hashes, arrays, etc.) within arguments
    /// are also NOT suppressed — in RuboCop's `on_send`, the walk-up only follows
    /// parent send_type? nodes, so calls inside hash/array structures don't walk up
    /// and are checked independently.
    fn part_of_checked_chain(&self, start_offset: usize, end_offset: usize) -> bool {
        self.checked_chain_ranges.iter().any(|&(cs, ce)| {
            start_offset >= cs
                && end_offset <= ce
                && (start_offset > cs || end_offset < ce)
                && !self.inside_block_body_within_chain(start_offset, end_offset, cs, ce)
                && !self.immediately_nested_under_safe_navigation_call(cs, ce)
                && !self.is_nested_in_data_structure()
        })
    }

    /// RuboCop's `on_send` walk-up follows plain parent sends until it reaches
    /// the first safe-navigation `csend` parent. Only the call directly below
    /// that `&.` boundary is checked independently; deeper plain-send segments
    /// still walk up to the nearest plain-send ancestor first.
    ///
    /// `checked_chain_ranges` intentionally truncates chains before non-owned
    /// block bodies. A safe-navigation ancestor can therefore extend past the
    /// recorded chain end when it owns a trailing block, so look at the
    /// nearest ancestor call start rather than requiring the full location to
    /// fit inside the range.
    fn immediately_nested_under_safe_navigation_call(
        &self,
        chain_start: usize,
        chain_end: usize,
    ) -> bool {
        if self.ancestors.len() < 2 {
            return false;
        }

        for i in (0..self.ancestors.len() - 1).rev() {
            let ancestor = &self.ancestors[i];
            if ancestor.as_arguments_node().is_some() {
                continue;
            }
            if let Some(call) = ancestor.as_call_node() {
                let loc = call.location();
                if loc.start_offset() < chain_start || loc.start_offset() >= chain_end {
                    return false;
                }
                return Self::is_safe_navigation_call(&call);
            }
            return false;
        }

        false
    }

    /// Returns true if the current call is nested inside a data structure (hash, array,
    /// assoc pair, etc.) relative to its nearest ancestor CallNode. In RuboCop's Parser
    /// AST, such nesting breaks the walk-up in `on_send` because the intermediate nodes
    /// (hash, pair, array) are not `send_type?`. In Prism, ArgumentsNode is the only
    /// intermediate wrapper that doesn't break the walk-up (it has no Parser equivalent).
    fn is_nested_in_data_structure(&self) -> bool {
        // ancestors includes the current node as the last element.
        // Walk up from the second-to-last to find the nearest ancestor CallNode.
        if self.ancestors.len() < 2 {
            return false;
        }
        for i in (0..self.ancestors.len() - 1).rev() {
            let ancestor = &self.ancestors[i];
            // If we reach a CallNode, this node is a direct child (receiver or argument)
            // — not nested in a data structure.
            if ancestor.as_call_node().is_some() {
                return false;
            }
            // ArgumentsNode is Prism's wrapper for arguments; it doesn't exist in Parser
            // AST, so skip it (it doesn't break the walk-up).
            if ancestor.as_arguments_node().is_some() {
                continue;
            }
            // Any other node type (KeywordHashNode, HashNode, ArrayNode, AssocNode,
            // SplatNode, ParenthesesNode, etc.) breaks the walk-up in RuboCop.
            return true;
        }
        false
    }

    /// Returns true if the current call is inside a binary operator node
    /// (`||`/`&&`/`or`/`and`) AND the operator's line does NOT end with `\`.
    ///
    /// In RuboCop, `on_send` walks up from the inner send through parent sends,
    /// convertible blocks, AND `BinaryOperatorNode` parents (OrNode/AndNode).
    /// The walked-up node then undergoes an `operator_keyword?` check: if true,
    /// the offense is gated on `require_backslash?` (the operator line must end
    /// with `\`). Since `operator_keyword?` returns true for both `||`/`or` and
    /// `&&`/`and`, a multiline call inside `foo || bar(...)` is NOT flagged
    /// unless the `||` line ends with `\`.
    ///
    /// Nitrocop's AST visitor doesn't walk up through OrNode/AndNode, so it
    /// would incorrectly flag the inner call. This method detects the situation
    /// and suppresses the offense.
    fn inside_binary_op_without_backslash(&self) -> bool {
        // Walk up ancestors following RuboCop's on_send walk-up rules:
        //   - CallNode (parent send) → continue
        //   - ArgumentsNode (Prism wrapper) → continue
        //   - BlockNode (convertible block) → continue
        //   - OrNode/AndNode → found! check backslash
        //   - Anything else → stop
        if self.ancestors.len() < 2 {
            return false;
        }
        for i in (0..self.ancestors.len() - 1).rev() {
            let ancestor = &self.ancestors[i];

            if let Some(call) = ancestor.as_call_node() {
                if Self::is_safe_navigation_call(&call) {
                    break;
                }
                continue;
            }

            if ancestor.as_arguments_node().is_some() {
                continue;
            }

            // Convertible blocks: in RuboCop, the walk-up goes through blocks
            // whose send is parenthesized or has no args. Be a bit generous and
            // continue through any BlockNode.
            if ancestor.as_block_node().is_some() {
                continue;
            }

            // Found an OrNode or AndNode — check if its operator line ends with `\`.
            let operator_loc = ancestor
                .as_or_node()
                .map(|n| n.operator_loc())
                .or_else(|| ancestor.as_and_node().map(|n| n.operator_loc()));

            if let Some(op_loc) = operator_loc {
                let outer_loc = ancestor.location();
                // RuboCop's `on_send` walks up through Or/And operators. The walked-up
                // node is then checked via `offense?` — which short-circuits on
                // `suitable_as_single_line?` (e.g. `too_long?`) before evaluating
                // `require_backslash?`. If the outer expression doesn't fit on one
                // line, no offense fires. Suppress here to match.
                if !self.suitable_as_single_line(outer_loc.start_offset(), outer_loc.end_offset()) {
                    return true;
                }
                let (op_line, _) = self.source.offset_to_line_col(op_loc.start_offset());
                let lines: Vec<&[u8]> = self.source.lines().collect();
                if op_line > 0 && op_line <= lines.len() {
                    let line = lines[op_line - 1];
                    let trimmed = trim_trailing_whitespace(line);
                    return !trimmed.ends_with(b"\\");
                }
                return true;
            }

            // Any other node type stops the walk-up.
            break;
        }
        false
    }

    /// Returns true if the node at (start, end) is inside a block body that
    /// itself is contained within the checked chain range (cs, ce).
    fn inside_block_body_within_chain(
        &self,
        start: usize,
        end: usize,
        chain_start: usize,
        chain_end: usize,
    ) -> bool {
        self.block_ranges
            .iter()
            .any(|&(bs, be, _)| bs >= chain_start && be <= chain_end && start >= bs && end <= be)
    }

    fn register_offense(&mut self, start_offset: usize, end_offset: usize) {
        let (line, col) = self.source.offset_to_line_col(start_offset);

        if self.reported_starts.contains(&line) {
            return;
        }
        self.reported_starts.insert(line);
        self.reported_ranges.push((start_offset, end_offset));

        self.ast_diagnostics.push(Diagnostic {
            path: self.source.path_str().to_string(),
            location: crate::diagnostic::Location { line, column: col },
            severity: crate::diagnostic::Severity::Convention,
            cop_name: self.cop_name.to_string(),
            message: "Redundant line break detected.".to_string(),
            corrected: false,
        });
    }

    fn receiver_chain_contains_safe_navigation(&self, node: &ruby_prism::CallNode<'pr>) -> bool {
        Self::is_safe_navigation_call(node)
            || node
                .receiver()
                .and_then(|receiver| receiver.as_call_node())
                .is_some_and(|receiver| self.receiver_chain_contains_safe_navigation(&receiver))
    }

    /// RuboCop's `on_send` walk-up:
    ///
    /// ```ruby
    /// node = node.parent while node.parent&.send_type? ||
    ///                          convertible_block?(node) ||
    ///                          node.parent.is_a?(RuboCop::AST::BinaryOperatorNode)
    /// ```
    ///
    /// `BinaryOperatorNode` is included only by `AndNode` and `OrNode`, so a
    /// multiline `&&`/`||`/`and`/`or` expression can itself be the node that is
    /// checked — and then `offense?` takes the `operator_keyword?` branch and
    /// gates on `require_backslash?`. Returns true when the walk-up would
    /// continue past this operator node, in which case the enclosing expression
    /// is the one RuboCop checks.
    fn binary_operator_walks_up(&self) -> bool {
        // `ancestors` has the operator node itself as its last element.
        if self.ancestors.len() < 2 {
            return false;
        }
        let mut idx = self.ancestors.len() - 2;
        // Prism puts call arguments in an `ArgumentsNode`; Parser hangs them
        // directly off the send, so the wrapper is transparent here. It is only
        // transparent for calls — `return a && b`, `yield(a && b)` and `super`
        // also carry an `ArgumentsNode`, and none of those are `send_type?`.
        if self.ancestors[idx].as_arguments_node().is_some() {
            if idx == 0 {
                return false;
            }
            idx -= 1;
            return self.ancestors[idx].as_call_node().is_some();
        }
        let parent = &self.ancestors[idx];
        parent.as_call_node().is_some()
            || parent.as_and_node().is_some()
            || parent.as_or_node().is_some()
    }

    /// Whether an `on_send` callback can actually walk up to this operator node.
    /// The walk-up only traverses send and binary-operator parents, so an
    /// operand that is a literal, a variable read or a parenthesized expression
    /// (Parser's `begin`) never reaches the operator.
    fn reaches_send(node: &ruby_prism::Node<'pr>) -> bool {
        if node.as_call_node().is_some() {
            return true;
        }
        if let Some(and) = node.as_and_node() {
            return Self::reaches_send(&and.left()) || Self::reaches_send(&and.right());
        }
        if let Some(or) = node.as_or_node() {
            return Self::reaches_send(&or.left()) || Self::reaches_send(&or.right());
        }
        false
    }

    /// RuboCop's `require_backslash?`:
    /// `processed_source.lines[node.loc.operator.line - 1].end_with?('\\')`.
    fn operator_line_ends_with_backslash(&self, operator_loc: &ruby_prism::Location<'pr>) -> bool {
        let (op_line, _) = self.source.offset_to_line_col(operator_loc.start_offset());
        let lines: Vec<&[u8]> = self.source.lines().collect();
        if op_line == 0 || op_line > lines.len() {
            return false;
        }
        trim_trailing_whitespace(lines[op_line - 1]).ends_with(b"\\")
    }

    /// Mirrors `on_send` for the case where the walk-up lands on an `and`/`or`
    /// node: `offense?` returns `require_backslash?(node)` for
    /// `operator_keyword?` nodes, short-circuiting on `multiline?` and
    /// `suitable_as_single_line?` first.
    fn check_binary_operator(
        &mut self,
        loc: &ruby_prism::Location<'pr>,
        operator_loc: &ruby_prism::Location<'pr>,
        left: &ruby_prism::Node<'pr>,
        right: &ruby_prism::Node<'pr>,
    ) {
        if self.binary_operator_walks_up() {
            return;
        }
        if !Self::reaches_send(left) && !Self::reaches_send(right) {
            return;
        }
        let start_offset = loc.start_offset();
        let end_offset = loc.end_offset();
        if !self.is_multiline(start_offset, end_offset) {
            return;
        }
        if !self.suitable_as_single_line(start_offset, end_offset) {
            return;
        }
        if !self.operator_line_ends_with_backslash(operator_loc) {
            return;
        }
        if self.part_of_reported_node(start_offset, end_offset) {
            return;
        }
        self.register_offense(start_offset, end_offset);
    }

    fn is_safe_navigation_call(node: &ruby_prism::CallNode<'_>) -> bool {
        node.call_operator_loc()
            .is_some_and(|loc| loc.as_slice() == b"&.")
    }
}

impl<'pr> Visit<'pr> for RedundantLineBreakVisitor<'_, 'pr> {
    fn visit_branch_node_enter(&mut self, node: ruby_prism::Node<'pr>) {
        self.ancestors.push(node);
    }

    fn visit_branch_node_leave(&mut self) {
        self.ancestors.pop();
    }

    fn visit_leaf_node_enter(&mut self, _node: ruby_prism::Node<'pr>) {}

    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
        let loc = node.location();
        let start_offset = loc.start_offset();
        let end_offset = loc.end_offset();

        // In RuboCop's Parser AST, the block is a parent node of the send.
        // In Prism, the CallNode includes the block. For offense checks, use
        // the "send-only" range (excluding the block) when the block is NOT
        // convertible. A block is convertible when the send is parenthesized
        // or has no args — in that case, RuboCop's walk-up merges them.
        let has_non_convertible_block =
            node.block()
                .and_then(|b| b.as_block_node())
                .is_some_and(|_| {
                    // Not convertible: has arguments AND is not parenthesized
                    node.arguments().is_some() && node.opening_loc().is_none()
                });
        let check_end = if has_non_convertible_block {
            node.block()
                .and_then(|b| b.as_block_node())
                .map_or(end_offset, |block| block.location().start_offset())
        } else {
            end_offset
        };

        let skip_for_checked_chain =
            !has_non_convertible_block && self.part_of_checked_chain(start_offset, end_offset);
        let unary_bang_wrapper = node.name().as_slice() == b"!"
            && node.arguments().is_none()
            && node.block().is_none()
            && node.receiver().and_then(|r| r.as_call_node()).is_some();
        let safe_navigation_unary_wrapper = unary_bang_wrapper
            && node
                .receiver()
                .and_then(|receiver| receiver.as_call_node())
                .is_some_and(|receiver| self.receiver_chain_contains_safe_navigation(&receiver));

        if safe_navigation_unary_wrapper {
            ruby_prism::visit_call_node(self, node);
            return;
        }

        if self.is_multiline(start_offset, check_end)
            && !self.part_of_reported_node(start_offset, end_offset)
            && !skip_for_checked_chain
        {
            // RuboCop's `on_send` walks up through parent sends even when the
            // inner send is a direct argument of the outer one, not only when
            // it is the receiver in a method chain. Record every multiline send
            // range so nested direct-argument sends are skipped unless another
            // structural boundary (hash/array/parentheses/block body) breaks
            // the walk-up.
            //
            // Exclude block bodies: in RuboCop, the send node's range does not
            // include the block (the block is a parent node). Calls inside
            // block bodies should be checked independently.
            let effective_end = node
                .block()
                .and_then(|b| b.as_block_node())
                .map_or(end_offset, |block| block.location().start_offset());
            self.checked_chain_ranges
                .push((start_offset, effective_end));

            // Skip index access chains: hash[:foo][:bar]
            let is_index_chain = if node.name().as_slice() == b"[]" {
                node.receiver()
                    .and_then(|r| r.as_call_node())
                    .is_some_and(|r| r.name().as_slice() == b"[]")
            } else {
                false
            };

            // When InspectBlocks is false and this CallNode has a convertible block,
            // the node maps to a block_type in RuboCop's walk-up (on_send walks
            // through convertible blocks, making the outermost node a :block).
            // RuboCop's `node.any_block_type?` returns true → skip.
            // A block is convertible when: parenthesized OR no explicit args.
            let has_convertible_block = !has_non_convertible_block
                && node.block().and_then(|b| b.as_block_node()).is_some();

            if !is_index_chain
                && !self.inside_binary_op_without_backslash()
                && self.suitable_as_single_line(start_offset, check_end)
                && !self.configured_to_not_be_inspected(start_offset, check_end)
                && (!has_convertible_block || self.inspect_blocks)
            {
                self.register_offense(start_offset, check_end);
            }
        }

        ruby_prism::visit_call_node(self, node);
    }

    fn visit_and_node(&mut self, node: &ruby_prism::AndNode<'pr>) {
        self.check_binary_operator(
            &node.location(),
            &node.operator_loc(),
            &node.left(),
            &node.right(),
        );
        ruby_prism::visit_and_node(self, node);
    }

    fn visit_or_node(&mut self, node: &ruby_prism::OrNode<'pr>) {
        self.check_binary_operator(
            &node.location(),
            &node.operator_loc(),
            &node.left(),
            &node.right(),
        );
        ruby_prism::visit_or_node(self, node);
    }

    fn visit_local_variable_write_node(&mut self, node: &ruby_prism::LocalVariableWriteNode<'pr>) {
        let loc = node.location();
        self.check_assignment(loc.start_offset(), loc.end_offset());
        ruby_prism::visit_local_variable_write_node(self, node);
    }

    fn visit_instance_variable_write_node(
        &mut self,
        node: &ruby_prism::InstanceVariableWriteNode<'pr>,
    ) {
        let loc = node.location();
        self.check_assignment(loc.start_offset(), loc.end_offset());
        ruby_prism::visit_instance_variable_write_node(self, node);
    }

    fn visit_class_variable_write_node(&mut self, node: &ruby_prism::ClassVariableWriteNode<'pr>) {
        let loc = node.location();
        self.check_assignment(loc.start_offset(), loc.end_offset());
        ruby_prism::visit_class_variable_write_node(self, node);
    }

    fn visit_global_variable_write_node(
        &mut self,
        node: &ruby_prism::GlobalVariableWriteNode<'pr>,
    ) {
        let loc = node.location();
        self.check_assignment(loc.start_offset(), loc.end_offset());
        ruby_prism::visit_global_variable_write_node(self, node);
    }

    fn visit_constant_write_node(&mut self, node: &ruby_prism::ConstantWriteNode<'pr>) {
        let loc = node.location();
        self.check_assignment(loc.start_offset(), loc.end_offset());
        ruby_prism::visit_constant_write_node(self, node);
    }

    fn visit_constant_path_write_node(&mut self, node: &ruby_prism::ConstantPathWriteNode<'pr>) {
        let loc = node.location();
        self.check_assignment(loc.start_offset(), loc.end_offset());
        ruby_prism::visit_constant_path_write_node(self, node);
    }

    fn visit_multi_write_node(&mut self, node: &ruby_prism::MultiWriteNode<'pr>) {
        let loc = node.location();
        self.check_assignment(loc.start_offset(), loc.end_offset());
        ruby_prism::visit_multi_write_node(self, node);
    }

    fn visit_local_variable_operator_write_node(
        &mut self,
        node: &ruby_prism::LocalVariableOperatorWriteNode<'pr>,
    ) {
        let loc = node.location();
        self.check_assignment(loc.start_offset(), loc.end_offset());
        ruby_prism::visit_local_variable_operator_write_node(self, node);
    }

    fn visit_local_variable_or_write_node(
        &mut self,
        node: &ruby_prism::LocalVariableOrWriteNode<'pr>,
    ) {
        let loc = node.location();
        self.check_assignment(loc.start_offset(), loc.end_offset());
        ruby_prism::visit_local_variable_or_write_node(self, node);
    }

    fn visit_local_variable_and_write_node(
        &mut self,
        node: &ruby_prism::LocalVariableAndWriteNode<'pr>,
    ) {
        let loc = node.location();
        self.check_assignment(loc.start_offset(), loc.end_offset());
        ruby_prism::visit_local_variable_and_write_node(self, node);
    }

    fn visit_instance_variable_operator_write_node(
        &mut self,
        node: &ruby_prism::InstanceVariableOperatorWriteNode<'pr>,
    ) {
        let loc = node.location();
        self.check_assignment(loc.start_offset(), loc.end_offset());
        ruby_prism::visit_instance_variable_operator_write_node(self, node);
    }

    fn visit_instance_variable_or_write_node(
        &mut self,
        node: &ruby_prism::InstanceVariableOrWriteNode<'pr>,
    ) {
        let loc = node.location();
        self.check_assignment(loc.start_offset(), loc.end_offset());
        ruby_prism::visit_instance_variable_or_write_node(self, node);
    }

    fn visit_instance_variable_and_write_node(
        &mut self,
        node: &ruby_prism::InstanceVariableAndWriteNode<'pr>,
    ) {
        let loc = node.location();
        self.check_assignment(loc.start_offset(), loc.end_offset());
        ruby_prism::visit_instance_variable_and_write_node(self, node);
    }

    fn visit_class_variable_operator_write_node(
        &mut self,
        node: &ruby_prism::ClassVariableOperatorWriteNode<'pr>,
    ) {
        let loc = node.location();
        self.check_assignment(loc.start_offset(), loc.end_offset());
        ruby_prism::visit_class_variable_operator_write_node(self, node);
    }

    fn visit_class_variable_or_write_node(
        &mut self,
        node: &ruby_prism::ClassVariableOrWriteNode<'pr>,
    ) {
        let loc = node.location();
        self.check_assignment(loc.start_offset(), loc.end_offset());
        ruby_prism::visit_class_variable_or_write_node(self, node);
    }

    fn visit_class_variable_and_write_node(
        &mut self,
        node: &ruby_prism::ClassVariableAndWriteNode<'pr>,
    ) {
        let loc = node.location();
        self.check_assignment(loc.start_offset(), loc.end_offset());
        ruby_prism::visit_class_variable_and_write_node(self, node);
    }

    fn visit_global_variable_operator_write_node(
        &mut self,
        node: &ruby_prism::GlobalVariableOperatorWriteNode<'pr>,
    ) {
        let loc = node.location();
        self.check_assignment(loc.start_offset(), loc.end_offset());
        ruby_prism::visit_global_variable_operator_write_node(self, node);
    }

    fn visit_global_variable_or_write_node(
        &mut self,
        node: &ruby_prism::GlobalVariableOrWriteNode<'pr>,
    ) {
        let loc = node.location();
        self.check_assignment(loc.start_offset(), loc.end_offset());
        ruby_prism::visit_global_variable_or_write_node(self, node);
    }

    fn visit_global_variable_and_write_node(
        &mut self,
        node: &ruby_prism::GlobalVariableAndWriteNode<'pr>,
    ) {
        let loc = node.location();
        self.check_assignment(loc.start_offset(), loc.end_offset());
        ruby_prism::visit_global_variable_and_write_node(self, node);
    }

    fn visit_constant_operator_write_node(
        &mut self,
        node: &ruby_prism::ConstantOperatorWriteNode<'pr>,
    ) {
        let loc = node.location();
        self.check_assignment(loc.start_offset(), loc.end_offset());
        ruby_prism::visit_constant_operator_write_node(self, node);
    }

    fn visit_constant_or_write_node(&mut self, node: &ruby_prism::ConstantOrWriteNode<'pr>) {
        let loc = node.location();
        self.check_assignment(loc.start_offset(), loc.end_offset());
        ruby_prism::visit_constant_or_write_node(self, node);
    }

    fn visit_constant_and_write_node(&mut self, node: &ruby_prism::ConstantAndWriteNode<'pr>) {
        let loc = node.location();
        self.check_assignment(loc.start_offset(), loc.end_offset());
        ruby_prism::visit_constant_and_write_node(self, node);
    }

    fn visit_constant_path_operator_write_node(
        &mut self,
        node: &ruby_prism::ConstantPathOperatorWriteNode<'pr>,
    ) {
        let loc = node.location();
        self.check_assignment(loc.start_offset(), loc.end_offset());
        ruby_prism::visit_constant_path_operator_write_node(self, node);
    }

    fn visit_constant_path_or_write_node(
        &mut self,
        node: &ruby_prism::ConstantPathOrWriteNode<'pr>,
    ) {
        let loc = node.location();
        self.check_assignment(loc.start_offset(), loc.end_offset());
        ruby_prism::visit_constant_path_or_write_node(self, node);
    }

    fn visit_constant_path_and_write_node(
        &mut self,
        node: &ruby_prism::ConstantPathAndWriteNode<'pr>,
    ) {
        let loc = node.location();
        self.check_assignment(loc.start_offset(), loc.end_offset());
        ruby_prism::visit_constant_path_and_write_node(self, node);
    }

    fn visit_call_operator_write_node(&mut self, node: &ruby_prism::CallOperatorWriteNode<'pr>) {
        let loc = node.location();
        self.check_assignment(loc.start_offset(), loc.end_offset());
        ruby_prism::visit_call_operator_write_node(self, node);
    }

    fn visit_call_or_write_node(&mut self, node: &ruby_prism::CallOrWriteNode<'pr>) {
        let loc = node.location();
        self.check_assignment(loc.start_offset(), loc.end_offset());
        ruby_prism::visit_call_or_write_node(self, node);
    }

    fn visit_call_and_write_node(&mut self, node: &ruby_prism::CallAndWriteNode<'pr>) {
        let loc = node.location();
        self.check_assignment(loc.start_offset(), loc.end_offset());
        ruby_prism::visit_call_and_write_node(self, node);
    }

    fn visit_index_operator_write_node(&mut self, node: &ruby_prism::IndexOperatorWriteNode<'pr>) {
        let loc = node.location();
        self.check_assignment(loc.start_offset(), loc.end_offset());
        ruby_prism::visit_index_operator_write_node(self, node);
    }

    fn visit_index_or_write_node(&mut self, node: &ruby_prism::IndexOrWriteNode<'pr>) {
        let loc = node.location();
        self.check_assignment(loc.start_offset(), loc.end_offset());
        ruby_prism::visit_index_or_write_node(self, node);
    }

    fn visit_index_and_write_node(&mut self, node: &ruby_prism::IndexAndWriteNode<'pr>) {
        let loc = node.location();
        self.check_assignment(loc.start_offset(), loc.end_offset());
        ruby_prism::visit_index_and_write_node(self, node);
    }
}

impl RedundantLineBreakVisitor<'_, '_> {
    /// Mirrors RuboCop's `check_assignment`:
    ///
    /// ```ruby
    /// def check_assignment(node, _rhs)
    ///   return unless offense?(node)
    ///
    ///   register_offense(node)
    /// end
    /// ```
    ///
    /// Note the absence of a `part_of_ignored_node?` guard — only `on_send`
    /// has one. Chained assignments (`@a =\n@b =\n@c = nil`) are therefore
    /// reported once per nested write node, not once for the outermost one.
    fn check_assignment(&mut self, start_offset: usize, end_offset: usize) {
        if !self.is_multiline(start_offset, end_offset) {
            return;
        }
        if !self.suitable_as_single_line(start_offset, end_offset) {
            return;
        }
        if self.configured_to_not_be_inspected(start_offset, end_offset) {
            return;
        }
        self.register_offense(start_offset, end_offset);
    }
}

/// Whether `bytes` contains a newline that is not a backslash line continuation.
fn contains_non_continuation_newline(bytes: &[u8]) -> bool {
    for (i, &b) in bytes.iter().enumerate() {
        if b != b'\n' {
            continue;
        }

        let mut j = i;
        while j > 0 && (bytes[j - 1] == b' ' || bytes[j - 1] == b'\t') {
            j -= 1;
        }

        if j == 0 || bytes[j - 1] != b'\\' {
            return true;
        }
    }
    false
}

fn trim_trailing_whitespace(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    while end > 0 && (line[end - 1] == b' ' || line[end - 1] == b'\t' || line[end - 1] == b'\r') {
        end -= 1;
    }
    &line[..end]
}

/// Count the number of Unicode characters (code points) in a UTF-8 byte slice.
/// RuboCop measures line length in characters, not bytes. For multi-byte UTF-8
/// (e.g. CJK characters), byte length > char length, causing FNs when using
/// byte length.
/// Ruby's `\s` character class: `[ \t\r\n\f\v]`.
const RUBY_WS: &str = r"[ \t\r\n\x0B\x0C]";

/// Faithful port of RuboCop's `CheckSingleLineSuitability#to_single_line`:
///
/// ```ruby
/// source
///   .gsub(/" *\\\n\s*'/, %q(" + '))  # Double quote, backslash, then single quote
///   .gsub(/' *\\\n\s*"/, %q(' + "))  # Single quote, backslash, then double quote
///   .gsub(/(["']) *\\\n\s*\1/, '')   # Double or single quote, backslash, same quote
///   .gsub(/\n\s*(?=(&)?\.\w)/, '')   # Method chaining, including `&.`
///   .gsub(/\s*\\?\n\s*/, ' ')        # Any other line break, with or without backslash
/// ```
///
/// Reproducing these substitutions verbatim matters: the two whitespace-sensitive
/// details are that the chain-dot rule (4th) consumes only the newline and the
/// *following* indentation — leaving the previous line's trailing padding and
/// line-continuation backslash in place — and that trailing whitespace on the
/// last line of the span is never followed by a newline, so nothing strips it.
/// Both inflate RuboCop's measured length relative to a naive
/// trim-and-join-with-one-space reconstruction.
fn to_single_line(source: &[u8]) -> Vec<u8> {
    // The backreference in RuboCop's third pattern is expanded into the two
    // concrete quote characters; patterns 1 and 2 have already consumed the
    // mixed-quote cases, so the two passes cannot overlap.
    static RE_DQ_SQ: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(&format!(r#""\x20*\\\n{RUBY_WS}*'"#)).unwrap());
    static RE_SQ_DQ: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(&format!(r#"'\x20*\\\n{RUBY_WS}*""#)).unwrap());
    static RE_DQ_DQ: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(&format!(r#""\x20*\\\n{RUBY_WS}*""#)).unwrap());
    static RE_SQ_SQ: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(&format!(r#"'\x20*\\\n{RUBY_WS}*'"#)).unwrap());
    // `(?=(&)?\.\w)` is emulated by capturing the lookahead text and putting it
    // back; the regex crate has no lookaround. `\w` is Ruby's ASCII `\w`.
    static RE_CHAIN_DOT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(&format!(r"\n{RUBY_WS}*(&?\.[A-Za-z0-9_])")).unwrap());
    static RE_ANY_BREAK: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(&format!(r"{RUBY_WS}*\\?\n{RUBY_WS}*")).unwrap());

    let s = RE_DQ_SQ.replace_all(source, &b"\" + '"[..]);
    let s = RE_SQ_DQ.replace_all(&s, &b"' + \""[..]);
    let s = RE_DQ_DQ.replace_all(&s, &b""[..]);
    let s = RE_SQ_SQ.replace_all(&s, &b""[..]);
    let s = RE_CHAIN_DOT.replace_all(&s, &b"$1"[..]);
    RE_ANY_BREAK.replace_all(&s, &b" "[..]).into_owned()
}

fn utf8_char_count(bytes: &[u8]) -> usize {
    // UTF-8 continuation bytes match the pattern 10xxxxxx (0x80..0xBF).
    // Every other byte is the start of a new character.
    bytes.iter().filter(|&&b| (b & 0xC0) != 0x80).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    crate::cop_fixture_tests!(RedundantLineBreak, "cops/layout/redundant_line_break");
    crate::cop_variant_fixture_tests!(
        RedundantLineBreak,
        "cops/layout/redundant_line_break",
        max_line_length_130
    );

    #[test]
    fn safe_navigation_chain_with_trailing_operator_uses_exact_joined_length() {
        let source = b"!current_course_user&.\n  email_unsubscriptions&.\n  where(course_settings_email_id: email_setting_enabled(component, setting).id)&.exists?\n";
        let config = CopConfig {
            options: HashMap::from([(
                "MaxLineLength".to_string(),
                serde_yml::Value::Number(serde_yml::Number::from(132)),
            )]),
            ..CopConfig::default()
        };

        let diagnostics =
            crate::testutil::run_cop_full_with_config(&RedundantLineBreak, source, config);

        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].location.line, 1);
        assert_eq!(diagnostics[0].location.column, 1);
    }

    #[test]
    fn safe_navigation_chain_inside_method_body_uses_configured_line_length() {
        let source = b"module Course::Forum::ControllerHelper\n  def email_setting_enabled(component, setting)\n    current_course.email_enabled(component, setting)\n  end\n\n  def email_subscription_enabled_current_course_user(component, setting)\n    !current_course_user&.\n      email_unsubscriptions&.\n      where(course_settings_email_id: email_setting_enabled(component, setting).id)&.exists?\n  end\nend\n";
        let config = CopConfig {
            options: HashMap::from([(
                "MaxLineLength".to_string(),
                serde_yml::Value::Number(serde_yml::Number::from(140)),
            )]),
            ..CopConfig::default()
        };

        let diagnostics =
            crate::testutil::run_cop_full_with_config(&RedundantLineBreak, source, config);

        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].location.line, 7);
        assert_eq!(diagnostics[0].location.column, 5);
    }

    #[test]
    fn reports_single_line_block_chains_when_single_line_block_chain_is_disabled() {
        let source = b"e.select { |i| i.cond? }\n  .join\n";
        let config = CopConfig {
            options: HashMap::from([(
                "SingleLineBlockChainEnabled".to_string(),
                serde_yml::Value::Bool(false),
            )]),
            ..CopConfig::default()
        };

        let diagnostics =
            crate::testutil::run_cop_full_with_config(&RedundantLineBreak, source, config);

        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].location.line, 1);
        assert_eq!(diagnostics[0].location.column, 0);
    }

    #[test]
    fn skips_single_line_block_chains_when_single_line_block_chain_is_enabled() {
        let source = b"e.select { |i| i.cond? }\n  .join\n";
        let config = CopConfig {
            options: HashMap::from([(
                "SingleLineBlockChainEnabled".to_string(),
                serde_yml::Value::Bool(true),
            )]),
            ..CopConfig::default()
        };

        let diagnostics =
            crate::testutil::run_cop_full_with_config(&RedundantLineBreak, source, config);

        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn stabby_lambda_arguments_respect_single_line_block_chain_config() {
        let source = b"foo.bar :x,\n  -> { baz }\n";

        let disabled = CopConfig {
            options: HashMap::from([(
                "SingleLineBlockChainEnabled".to_string(),
                serde_yml::Value::Bool(false),
            )]),
            ..CopConfig::default()
        };
        let diagnostics =
            crate::testutil::run_cop_full_with_config(&RedundantLineBreak, source, disabled);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].location.line, 1);
        assert_eq!(diagnostics[0].location.column, 0);

        let enabled = CopConfig {
            options: HashMap::from([(
                "SingleLineBlockChainEnabled".to_string(),
                serde_yml::Value::Bool(true),
            )]),
            ..CopConfig::default()
        };
        let diagnostics =
            crate::testutil::run_cop_full_with_config(&RedundantLineBreak, source, enabled);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn reports_multiline_call_inside_singleton_method_body() {
        let source = b"module WinRM\n  module PSRP\n    class MessageFactory\n      class << self\n        def session_capability_message(runspace_pool_id)\n          Message.new(\n            runspace_pool_id,\n            Message::MESSAGE_TYPES[:session_capability],\n            render('session_capability')\n          )\n        end\n      end\n    end\n  end\nend\n";

        let diagnostics = crate::testutil::run_cop_full_with_config(
            &RedundantLineBreak,
            source,
            CopConfig::default(),
        );

        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].location.line, 6);
        assert_eq!(diagnostics[0].location.column, 10);
    }

    #[test]
    fn reports_multiline_rspec_chain_inside_block_body() {
        let source = b"describe WinRM::PSRP::ReceiveResponseReader do\n  before do\n    allow(transport).to receive(:send_request).and_return(\n      REXML::Document.new(test_data_xml_template.result(binding))\n    )\n  end\nend\n";

        let diagnostics = crate::testutil::run_cop_full_with_config(
            &RedundantLineBreak,
            source,
            CopConfig::default(),
        );

        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].location.line, 3);
        assert_eq!(diagnostics[0].location.column, 4);
    }

    #[test]
    fn reports_constant_path_chain_inside_block_body() {
        let source = b"module SidekiqServerExpectations\n  def expect_in_sidekiq_server\n    expect_in_fork do\n      Datadog::Tracing::Contrib::Sidekiq::Patcher\n        .instance_variable_get(:@patch_only_once)\n        &.send(:reset_ran_once_state_for_tests)\n    end\n  end\nend\n";

        let diagnostics = crate::testutil::run_cop_full_with_config(
            &RedundantLineBreak,
            source,
            CopConfig::default(),
        );

        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].location.line, 4);
        assert_eq!(diagnostics[0].location.column, 6);
    }

    #[test]
    fn reports_assignment_rhs_chain_when_full_assignment_is_too_long() {
        let source = b"module PublishingApi::PayloadBuilder\n  class ConfigurableDocumentLinks\n    def self.organisations(item)\n      primary_publishing_organisation = item.edition_organisations.select(&:lead?)\n        .min_by(&:lead_ordering)\n        &.organisation&.content_id\n    end\n  end\nend\n";

        let diagnostics = crate::testutil::run_cop_full_with_config(
            &RedundantLineBreak,
            source,
            CopConfig::default(),
        );

        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].location.line, 4);
        assert_eq!(diagnostics[0].location.column, 40);
    }

    #[test]
    fn reports_safe_navigation_chain_with_single_line_block() {
        let source = b"class SchemaValidator\n  def presence_validation_properties\n    (@document[\"schema\"][\"validations\"] || {})\n      &.select { |key, _| key == \"presence\" }\n      &.values\n      &.flat_map { |validator| validator[\"attributes\"] }\n  end\nend\n";

        let diagnostics = crate::testutil::run_cop_full_with_config(
            &RedundantLineBreak,
            source,
            CopConfig::default(),
        );

        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].location.line, 3);
        assert_eq!(diagnostics[0].location.column, 4);
    }

    #[test]
    fn reports_safe_navigation_chain_with_numbered_block() {
        let source = b"class StripePayoutProcessor\n  def self.instantly_payable_amount_cents_on_stripe(user)\n    balance.try(:instant_available)\n      &.first\n      &.try(:net_available)\n      &.find { _1[\"destination\"] == active_bank_account.stripe_bank_account_id }\n      &.[](\"amount\") || 0\n  end\nend\n";

        let diagnostics = crate::testutil::run_cop_full_with_config(
            &RedundantLineBreak,
            source,
            CopConfig::default(),
        );

        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].location.line, 3);
        assert_eq!(diagnostics[0].location.column, 4);
    }

    #[test]
    fn reports_backslash_continued_if_condition() {
        let source = b"class Helper\n  def self.setup(options)\n    if options[:username] && options[:server_ip] && \\\n      (options[:password] || options[:password_base64])\n      creds = options\n    end\n  end\nend\n";

        let diagnostics = crate::testutil::run_cop_full_with_config(
            &RedundantLineBreak,
            source,
            CopConfig::default(),
        );

        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].location.line, 3);
        assert_eq!(diagnostics[0].location.column, 7);
    }

    #[test]
    fn reports_multiline_command_call_with_old_hash_rocket_keywords() {
        let source = b"class CommentsController < ApplicationController\n  def edit\n    if !((comment = find_comment) && comment.is_editable_by_user?(@user))\n      return render :text => \"can't find comment\", :status => 400\n    end\n\n    render :partial => \"commentbox\", :layout => false,\n      :content_type => \"text/html\", :locals => { :comment => comment }\n  end\nend\n";

        let diagnostics = crate::testutil::run_cop_full_with_config(
            &RedundantLineBreak,
            source,
            CopConfig::default(),
        );

        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].location.line, 7);
        assert_eq!(diagnostics[0].location.column, 4);
    }

    #[test]
    fn old_hash_rocket_command_does_not_suppress_later_multiline_calls() {
        let source = b"class CommentsController < ApplicationController\n  def edit\n    render :partial => \"commentbox\", :layout => false,\n      :content_type => \"text/html\", :locals => { :comment => comment }\n  end\nend\n\nclass StripePayoutProcessor\n  def self.instantly_payable_amount_cents_on_stripe(user)\n    balance.try(:instant_available)\n      &.first\n      &.try(:net_available)\n      &.find { _1[\"destination\"] == active_bank_account.stripe_bank_account_id }\n      &.[](\"amount\") || 0\n  end\nend\n";

        let diagnostics = crate::testutil::run_cop_full_with_config(
            &RedundantLineBreak,
            source,
            CopConfig::default(),
        );

        assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
        assert_eq!(diagnostics[0].location.line, 3);
        assert_eq!(diagnostics[0].location.column, 4);
        assert_eq!(diagnostics[1].location.line, 10);
        assert_eq!(diagnostics[1].location.column, 4);
    }

    #[test]
    fn reports_crlf_multiline_calls_that_fit_exactly_at_max_line_length() {
        let source = b"class CommentsController\r\n  def edit\r\n    render :partial => \"commentbox\", :layout => false,\r\n      :content_type => \"text/html\", :locals => { :comment => comment }\r\n  end\r\n\r\n  def upvote\r\n    begin\r\n      Vote.vote_thusly_on_story_or_comment_for_user_because(1, comment.story_id,\r\n        comment.id, @user.id, params[:reason])\r\n    rescue\r\n      nil\r\n    end\r\n  end\r\nend\r\n";

        let diagnostics = crate::testutil::run_cop_full_with_config(
            &RedundantLineBreak,
            source,
            CopConfig::default(),
        );

        assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
        assert_eq!(diagnostics[0].location.line, 3);
        assert_eq!(diagnostics[0].location.column, 4);
        assert_eq!(diagnostics[1].location.line, 9);
        assert_eq!(diagnostics[1].location.column, 6);
    }

    #[test]
    fn reports_backslash_continued_ternary_predicate() {
        let source = b"module ArelExtensions\n  module Visitors\n    class Arel::Visitors::MySQL\n      def visit_ArelExtensions_Nodes_Format o, collector\n        first = o.expressions[0]\n        type =\n          o.col_type.nil? \\\n            && (first.respond_to?(:return_type) && !first&.return_type.nil?) \\\n          ? first&.return_type \\\n          : o.col_type\n      end\n    end\n  end\nend\n";

        let diagnostics = crate::testutil::run_cop_full_with_config(
            &RedundantLineBreak,
            source,
            CopConfig::default(),
        );

        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].location.line, 7);
        assert_eq!(diagnostics[0].location.column, 10);
    }
}
