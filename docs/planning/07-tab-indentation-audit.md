# Tab-indentation column-counting audit

Root cause pattern: RuboCop's parser-gem `node.loc.column` / `processed_source.line_indentation` /
`Heredoc#indent_level` / `Layout::Alignment#offset`+`#display_column` all count each **character**
of leading whitespace as one column (tab == 1), because they operate on `line[/^\s*/]` or a
character-index `column`. Nitrocop's `SourceFile::offset_to_line_col` (`src/parse/source.rs:55-66`)
matches that (counts non-UTF8-continuation bytes). But the original shared helper
`src/cop/shared/util.rs:548 indentation_of()` counts **bytes == b' '** only, so on tab-indented
lines it returns 0 (or an undercount) while `offset_to_line_col` on the same line returns the
correct tab-inclusive column. Any cop that compares the two produces bogus deltas.

## 1. Callers of `indentation_of` and other leading-whitespace helpers

### Category (a) — already tab-aware (cop-local fixes exist)

| Cop / file | Helper | Semantics |
|---|---|---|
| `first_argument_indentation.rs:489 leading_whitespace_count` | spaces+tabs, 1 byte each | matches `offset_to_line_col` |
| `array_alignment.rs:10 first_non_whitespace_column` | spaces+tabs | doc comment cites `/\S/` explicitly |
| `first_array_element_indentation.rs:9 first_non_whitespace_column` | spaces+tabs | same helper duplicated |
| `first_hash_element_indentation.rs:10 leading_whitespace_columns` | spaces+tabs | duplicated again |
| `closing_parenthesis_indentation.rs:13 leading_whitespace_columns` | spaces+tabs | duplicated again |
| `multiline_operation_indentation.rs:676 leading_whitespace_len_with_tabs`, used at 778,847,848,877 | spaces+tabs | **but not used everywhere in this file — see (b1) below** |
| `argument_alignment.rs:264-269` inline `take_while(b==' '\|\|b=='\t')`, plus `display_column`/`display_width` (339-425) | full Unicode-width aware (handles wide/emoji/combining too), tabs fall into default width-1 branch | closest to RuboCop's actual `Alignment#display_column` (`Unicode::DisplayWidth.of`), strictly more correct than the other cops' plain char-count |
| `assignment_indentation.rs:116,169` inline `b==' '\|\|b=='\t'` | spaces+tabs | fixed per its doc comment (30 FP root cause) |
| `line_length.rs` (see tests `leading_tabs_count_toward_line_length`, `shallow_leading_tabs_count_toward_line_length`) | **expands** each leading tab to `IndentationWidth` columns | correctly replicates RuboCop's *one* intentionally-different tab rule (see §2) |

Five near-identical helpers (`leading_whitespace_count` / `first_non_whitespace_column` ×2 /
`leading_whitespace_columns` ×2 / `leading_whitespace_len_with_tabs`) were independently
reinvented across five files — exactly the duplication AGENTS.md's shared-infra section warns
against, and the migration plan below collapses them.

### Category (b1) — spaces-only AND gates the fire/no-fire decision (real FP/FN risk)

| Cop / file:line | Value computed | How it gates the offense |
|---|---|---|
| `layout/multiline_operation_indentation.rs:945` `indentation_of(left_line_bytes)` → `left_indent` | feeds `expected_indent = left_indent + width` (line 971) | `is_ok = right_col == expected_indent` (line 979) directly decides whether the diagnostic at line 984 fires |
| `layout/multiline_method_call_indentation.rs:486` `indentation_of(base_line_bytes)` → `base_indent` in `expected_indented()` | `expected = base_indent + width + kw_extra` | consumed by `if rhs_col != expected` at line 466, which pushes the diagnostic |
| `layout/closing_heredoc_indentation.rs:208-216` `line_indent()` (local, spaces-only; not the shared `indentation_of`, but the same bug pattern) | `opening_line_indent`, `closing_line_indent`, `arg_indent` | `if opening_line_indent == closing_line_indent { return }` (line 84) and the `argument_indent` compare (line 91) directly suppress/fire the offense |

`multiline_operation_indentation.rs` is the most notable: it already has the correct helper
(`leading_whitespace_len_with_tabs`, category a) used four times in the *same file*, but
`check_binary_node` (the hot path for `&&`/`||`/binary-operator continuation indentation) still
imports and uses the old spaces-only `indentation_of` from `shared::util` at line 945 — an
incomplete migration within one file, not just a missing cross-file fix.

### Category (b2) — spaces-only, column-sensitive, but does NOT gate fire/no-fire (cosmetic column or autocorrect-only; invisible to the line-keyed corpus oracle)

| Cop / file:line | Used for | Why invisible to corpus FP/FN |
|---|---|---|
| `style/class_and_module_children.rs:294 count_leading_spaces_at_offset` → `last_child_indent` (line 260) | feeds `column_delta` for the `unindent` **autocorrector** only (mirrors RuboCop's `unindent`/`spaces_size`) | offense detection doesn't depend on it; only `-a`/`-A` output on tab-indented nested `class Foo::Bar` bodies would be corrupted |
| `rspec/empty_line_after_hook.rs:135`, `rspec/empty_line_after_example_group.rs:183`, `rspec/empty_line_after_final_let.rs:182` — all `line.iter().take_while(b==' ').count()` → `report_col` | column of an *already-decided* "add empty line after X" offense | the missing-blank-line check that decides to fire doesn't touch this value at all |
| `layout/multiline_method_call_indentation.rs:751 indentation_of(chain_line_bytes)` → `chain_indent` in `indented_message()` | only builds the "(not N)" number in the message string for an offense whose *firing* was already decided by the (b1) bug at line 486 | message text, not line/cop keying |

The corpus oracle's `fp_examples`/`fn_examples` keys are `"repo: path/to/file.rb:line"`
(`bench/corpus/diagnose_corpus.py:66-78`, and every by_cop example in the artifact) — no column
component. So category-(b2) bugs cannot show up as corpus FP/FN by construction; they only
matter for (1) exact-column diffing tools if any exist downstream, and (2) autocorrect byte-output
correctness, neither of which the standard `check_cop.py` counts exercise. This is a distinct
reason for "no corpus evidence" from category-(b1) cops that simply happened not to hit a
tab-indented file in the sample.

### Category (c) — spaces-only but irrelevant (not indentation/column arithmetic)

Broad grep across `src/cop/` for `take_while .. b==' '`, `trim_start_matches(' ')`,
`while .. == b' '` turned up ~40 more hits (`space_around_operators.rs`, `space_inside_*`,
`redundant_parentheses.rs`, `multiline_if_then.rs`, `multiline_when_then.rs`, `not.rs`,
`negated_if.rs`, `unless_else.rs`, `naming/inclusive_language.rs`, `rails/where_not.rs`,
`gemspec/dependency_version.rs`, `symbol_conversion.rs`, `symbol_array.rs`, `copyright.rs`). All
of these trim/detect a single literal space adjacent to a token on the same physical line
(spacing rules, string/regex content, value parsing) — none compare a line's leading-indent
column against another line's. `layout/indentation_style.rs` is the cop that *defines* tabs vs
spaces style itself; it intentionally does raw indent-string comparison, not column arithmetic,
so it is unaffected by (and does not need) the tab-aware column helper.

## 2. RuboCop-side helpers and the one intentional tab-width exception

| Nitrocop cop | RuboCop mixin/method used upstream | vendor path |
|---|---|---|
| FirstArgumentIndentation, FirstArrayElementIndentation, FirstHashElementIndentation, ArgumentAlignment, ArrayAlignment, ClosingParenthesisIndentation, MultilineMethodCallIndentation, MultilineOperationIndentation, ClassAndModuleChildren | `Alignment#indentation`/`#offset`/`#display_column` | `vendor/rubocop/lib/rubocop/cop/mixin/alignment.rb:7-58` |
| MultilineElementIndentation-based cops (First*ElementIndentation) | `MultilineElementIndentation#check_indentation`, `configured_indentation_width` | `vendor/rubocop/lib/rubocop/cop/mixin/multiline_element_indentation.rb:30,81` |
| ClosingHeredocIndentation | `Heredoc#indent_level` — `str.lines.map { |l| l[/^\s*/] }.min_by(&:size).size` | `vendor/rubocop/lib/rubocop/cop/mixin/heredoc.rb:23-26` |
| (generic) | `ProcessedSource#line_indentation` | `vendor/rubocop/lib/rubocop/ext/processed_source.rb:187` |

`Alignment#display_column` is notably **not** a plain char count — it's
`Unicode::DisplayWidth.of(line[0, range.column])`, i.e. grapheme/wide-char aware. Nitrocop's
`argument_alignment.rs` (category a) is the only cop that actually replicates this; the other
"tab-aware" cop-local helpers (`leading_whitespace_columns` etc.) only count 1-per-char, which is
byte/char-count equivalent to `offset_to_line_col`, not full `Unicode::DisplayWidth`. In practice
this divergence is unreachable — real indentation never contains wide/emoji runes — so it's a
correctness footnote, not a corpus risk.

**Intentional tab-width exception found:** `Layout/LineLength` is the *only* cop in RuboCop that
does not treat tabs as 1 column. Its `check_line`/`highlight_start`
(`vendor/rubocop/lib/rubocop/cop/layout/line_length.rb:1-260`) expands each leading tab to
`Layout/IndentationWidth`'s `Width` for the purposes of the *length* count (with a documented
known bug: "TODO ... getting a correct highlighting range when tabs are used ... doesn't work
currently" at line 250-252). Nitrocop's `line_length.rs` already replicates this via two dedicated
tests. `Layout/TrailingWhitespace` and `Layout/IndentationStyle`/`Layout/IndentationConsistency`
have no tab-width option (`grep TabWidth` over `config/default.yml` returns nothing) — they only
compare indentation strings directly or check trailing bytes, so tabs vs spaces there is a style
question, not a column-arithmetic one.

## 3. Corpus oracle evidence (from the supplied `corpus-results.json`)

| Cop | fp | fn | match_rate | Live tab evidence found? |
|---|---|---|---|---|
| `Layout/MultilineOperationIndentation` | 19 | 39 | 0.9987 | **Yes — strong.** `jjyg__metasm__a70271c` (1274/1490 tab-indented lines) alone accounts for 20 of the 39 FN, all "Use 2 (not 0) spaces for indenting..." at `metasm/debug.rb:1111`, `decompile.rb:968`, `elf_encode.rb:180-183`, `wasm.rb:396-398`, etc. `dhanasingh__redmine_wktime__17bf010` (2339/2676 tab lines) contributes 2 more FN at `wktime_controller.rb:884,1322`. `brav0hax__smbexec__a54fc14` (958 tab lines) contributes 1 FN at `cachedump.rb:298`. **≥23 of 39 FN (59%) are directly attributable to this one (b1) bug.** Verified `hanami/hanami` and `sorah/mamiya` examples with the same "(not 0)" message text are *not* tab-indented — they're a separate wrong-base-line bug, so not every "(not 0)" is a tab hit; fetched source to disambiguate case by case. |
| `Layout/MultilineMethodCallIndentation` | 216 | 192 | 0.9899 | **Sampled, not confirmed.** Of the ~100 fp + 100 fn examples returned, 44 use the "(not N) spaces for indentation of a chained method call" message path that goes through the buggy `expected_indented()`. Fetched 5 of those source files (`Pistos/diakonos`, `alphagov/whitehall`, `capistrano/sshkit`, `louismullie/treat`, `librariesio/libraries.io`) — **none were tab-indented**; all 5 are actually a *different*, larger bug (wrong chain-root/base-line selection, e.g. `content.map { |c| ... }.compact` picking the wrong anchor line entirely, giving indent 0). The (b1) tab bug here is real (same code shape as the fixed multiline_operation_indentation case) but is currently masked/dwarfed by that unrelated bug in this corpus sample — did not find a positive tab hit in the ~140 examples inspected. |
| `Layout/ClosingHeredocIndentation` | 0 | 0 | 1.0 (perfect) | **None found — unverified.** No FP/FN at all in the current baseline, so no examples to mine. The (b1) bug is real by code inspection (spaces-only `line_indent` gates the `==` fire check) but tab-indented heredocs with a *mismatched* opening/closing indent apparently didn't occur in this corpus snapshot, or existing tab-indented heredocs happen to have matching (both-0) indents on both delimiters, which masks the bug. Needs a synthetic fixture, not a corpus gate, to pin down. |
| `Style/ClassAndModuleChildren`, `RSpec/EmptyLineAfterHook`, `RSpec/EmptyLineAfterExampleGroup`, `RSpec/EmptyLineAfterFinalLet` | 0/0 each | — | 1.0 each | **Structurally unverifiable via corpus** — these are category (b2): the oracle keys offenses by `file:line` only (no column), so even a real tab-column bug here can never surface as corpus FP/FN. Confirmed by inspecting `bench/corpus/diagnose_corpus.py:66-78`'s `repo_id: path:line` key format and the `by_cop[].fp_examples[].loc` shape in the artifact (no column field anywhere). |
| `Layout/FirstArgumentIndentation`, `FirstArrayElementIndentation`, `FirstHashElementIndentation`, `ArrayAlignment`, `ArgumentAlignment`, `ClosingParenthesisIndentation`, `ParameterAlignment`, `AssignmentIndentation` | 0/0 each | — | 1.0 each | Already fixed (category a); perfect match confirms the fix held with no regressions. |

## 4. Recommendation

### Shared helper

Add to `src/cop/shared/util.rs`, replacing `indentation_of` call sites (keep `indentation_of`
itself only if something still legitimately wants spaces-only counting — a full-repo grep found
no such legitimate caller, so it can likely be deleted once migrated):

```rust
/// Column of the first non-whitespace byte on a line, counting each byte
/// (space or tab) as one column — matches RuboCop's `line[/^\s*/].size`,
/// `node.loc.column`, and `ProcessedSource#line_indentation`, all of which
/// operate on parser-gem character columns rather than terminal display
/// width. Equivalent to Ruby's `source_line =~ /\S/`.
pub fn indentation_of(line: &[u8]) -> usize {
    line.iter()
        .take_while(|&&b| b == b' ' || b == b'\t')
        .count()
}
```

Keep the name `indentation_of` (minimize call-site churn) rather than introducing yet another
name — the five duplicated cop-local fns (`leading_whitespace_count`,
`first_non_whitespace_column` ×2, `leading_whitespace_columns` ×2,
`leading_whitespace_len_with_tabs`) should be deleted and their call sites repointed at
`shared::util::indentation_of` once it's fixed in place. This is a pure behavior-preserving
dedup for the cops already in category (a) — no corpus risk, since they compute the identical
value today.

Do **not** attempt to fold `argument_alignment.rs`'s `display_width`/`display_column` (Unicode
grapheme width) into this helper — it's solving a different, unrelated problem (wide/emoji chars
in argument text) and is already the most correct implementation on file; leave it cop-local or
promote it to shared separately if another cop needs Unicode-width alignment.

### Migration plan (per-PR, corpus-gated)

1. **PR A — fix `src/cop/shared/util.rs::indentation_of` in place** (add `|| b == b'\t'`), then
   repoint every category-(a) cop's local duplicate helper at it and delete the duplicates
   (`first_argument_indentation.rs`, `array_alignment.rs`, `first_array_element_indentation.rs`,
   `first_hash_element_indentation.rs`, `closing_parenthesis_indentation.rs`). Since these cops
   are all at 1.0 corpus match today and the replacement is byte-identical logic, gate with
   `check_cop.py --rerun` per cop as a no-op confirmation (expect exactly 0 delta), not a
   from-scratch investigation. Low risk, pure cleanup — matches AGENTS.md's shared-infra reuse
   rule.
2. **PR B — `Layout/MultilineOperationIndentation`**: swap the stray `indentation_of` import at
   `multiline_operation_indentation.rs:945` for the file's own already-correct
   `leading_whitespace_len_with_tabs` (or, after PR A, the fixed shared `indentation_of`). This
   is the highest-value fix in this audit — confirmed ≥23 live FN. Gate with
   `check_cop.py Layout/MultilineOperationIndentation --rerun --verbose`, expect ~20+ FN drop
   concentrated in `jjyg__metasm`, `dhanasingh__redmine_wktime`, `brav0hax__smbexec`. Add fixture
   cases (see below) before touching the fn body per AGENTS.md TDD rule.
3. **PR C — `Layout/MultilineMethodCallIndentation`**: fix `indentation_of` at line 486 (and 751
   for message-text parity) the same way. Expect this PR to be *noisy* against the corpus gate
   because 216 fp / 192 fn are dominated by the unrelated chain-root/base-line bug found in
   `diakonos`/`whitehall`/`sshkit`/`treat`/`libraries.io` — don't let that block or get conflated
   with the tab fix; land the tab fix and confirm via `--rerun` that it doesn't *regress* any of
   the currently-passing subset, rather than expecting full conformance. Consider splitting the
   base-line bug into its own separate, non-tab PR first so this PR's diff is legible.
4. **PR D — `Layout/ClosingHeredocIndentation`**: fix local `line_indent()` (line 208-216).
   Zero corpus evidence exists either way, so this PR is TDD-only: add a tab-indented fixture
   (below), confirm no corpus regression via `check_cop.py --rerun` (expect 0/0 unchanged), and
   land on fixture-correctness alone, documenting in the cop's `///` doc comment that this was
   corpus-unverified per AGENTS.md's investigation-documentation rule.
5. **PR E (optional, lower priority) — category (b2) cosmetic/autocorrect fixes**:
   `class_and_module_children.rs:294`, and the three RSpec `empty_line_after_*` report-column
   sites. These cannot move any corpus number by construction (line-keyed oracle), so gate by
   targeted fixture only (assert exact `column` in a tab-indented offense/no_offense fixture) and
   by manually running `-A` autocorrect over a tab-indented fixture for `class_and_module_children`
   to confirm the corrector no longer miscomputes `column_delta`.

### Tests

- Fixture convention: add `# nitrocop-filename:` is not needed; a plain `.rb` fixture with real
  `\t` bytes works today — confirmed no formatter/editorconfig mangles tabs in
  `tests/fixtures/cops/**`: repo has no `.editorconfig`, and `cargo fmt`/`rustfmt` only touches
  `.rs` files, not `tests/fixtures/**/*.rb`. `rustfmt.toml`/`.editorconfig` search turned up
  nothing that would rewrite fixture file content.
- For each PR B/C/D, add both an `offense.rb` (or `offense/` scenario dir per AGENTS.md fixture
  rules) and a `no_offense.rb` case with literal tabs, e.g. reproduce the minimal shape from
  `metasm/debug.rb:1111` (`\tif s = @symbols[addr] ? addr : @symbols_len.keys.find {...}` /
  continuation line) for `multiline_operation_indentation`, and the `cachedump.rb:298`
  `\t\tcredentials <<\n\t\t[` shape as a second, independent tab-only case.
  Use `reduce_mismatch.py Layout/MultilineOperationIndentation jjyg__metasm__a70271c metasm/debug.rb:1111` (CI-guarded/local per AGENTS.md — run without `CI` set, it's not the `check_cop.py`
  script) to get a minimized reproducer rather than hand-copying the full file.
- Do not add a corpus-only regression test for `ClosingHeredocIndentation` — there being no
  corpus evidence means the fixture is the only verification; note that explicitly in the cop's
  doc comment so a future agent doesn't waste time hunting for corpus examples that don't exist.

## Key file:line index

- `src/cop/shared/util.rs:548` — `indentation_of` (the bug)
- `src/parse/source.rs:55` — `offset_to_line_col` (the char-counting ground truth it should match)
- `src/cop/layout/multiline_operation_indentation.rs:9,945` — live (b1) bug, confirmed corpus hits
- `src/cop/layout/multiline_operation_indentation.rs:676,778,847,848,877` — the file's own correct helper, unused at line 945
- `src/cop/layout/multiline_method_call_indentation.rs:4,486,751` — live (b1)+(b2) bug, sampled but unconfirmed
- `src/cop/layout/closing_heredoc_indentation.rs:80,83,152,208-217` — live (b1) bug, zero corpus evidence
- `src/cop/style/class_and_module_children.rs:260,294` — (b2), autocorrect-only
- `src/cop/rspec/empty_line_after_hook.rs:135`, `empty_line_after_example_group.rs:183`, `empty_line_after_final_let.rs:182` — (b2), report-column-only
- `vendor/rubocop/lib/rubocop/cop/mixin/alignment.rb:7-58` — `display_column`/`indentation` ground truth
- `vendor/rubocop/lib/rubocop/cop/mixin/heredoc.rb:23-26` — `indent_level` ground truth
- `vendor/rubocop/lib/rubocop/cop/layout/line_length.rb:10-11,250-252` — the one intentional tab-width exception (already correctly handled in `src/cop/layout/line_length.rs`)
