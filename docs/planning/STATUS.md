# Program status (fork: pnomolos/nitrocop)

Planning branch, not for upstream. Read this first when resuming.

## Goals
1. Parity with RuboCop core. 2. Parity with common plugins. 3. Translation layer (RuboCop cop -> nitrocop IR). 4. Runtime user-extension format on the same IR.

## Conventions
- PRs upstreamable, atomic, stacked with explicit `--base`; owner reviews and merges. Never auto-merge.
- Corpus oracle runs on the fork (Actions enabled 2026-09-19). `check_cop.py` needs a *successful* run; PR #1 gates the GH App step so fork runs succeed.
- Budget gate: `~/.claude/hooks/usage_check.sh 80` between chunks.

## Findings so far (2026-09-19)
- Gap vs pinned vendor versions: **0 unimplemented** (920/920). See 01-gap-analysis.md.
- Last upstream oracle (2026-04-26): 99.99% match, 5 diverging cops (Layout/MultilineMethodCallIndentation, Layout/MultilineOperationIndentation, Layout/RedundantLineBreak, Lint/RedundantCopDisableDirective, Lint/UselessAssignment), 33 cops with no corpus data.
- Upstream drift: rubocop 1.84.2 -> 1.91.0 (+21 cops), rubocop-rspec 3.9.0 -> 3.10.2 (+2), rails/performance behavior-only bumps. 23 net-new cops, none needing external inputs.
- src/node_pattern is an unwired spike: predicates/params/`^`/`` ` `` stubbed true, `<>` unsupported. See 02-nitrocop-internals.md.
- Cop census: A 12% / B 70% / C 18% declarativeness; on_send+on_csend ~580 hook registrations; RangeHelp in 257/957 cops. See 03-rubocop-cop-census.md.

## Workstreams
| # | Stream | State |
|---|--------|-------|
| W0 | Corpus oracle baseline on fork (run 35413735947, success). 99.99%, 1,047 FP / 716 FN, 6 diverging cops. See 06-oracle-baseline-2026-09-19.md | done |
| W1 | Vendor bump to latest: PR #2 (bump only, config_audit red on 8 new options) + stacked PR implementing the 8 options (branch vendor/new-cop-options) | PRs open |
| W2 | Fix diverging cops: Lint/RedundantCopDisableDirective (740/203) in progress on branch fix/lint-redundant-cop-disable-directive; then Layout/MultilineMethodCallIndentation, Style/MethodCallWithArgsParentheses (omit_parentheses variant), Layout/RedundantLineBreak, Layout/MultilineOperationIndentation, Lint/UselessAssignment, Layout/HashAlignment (separator variant) | #8, #10 open; All six baseline diverging cops have fix PRs (#8, #10, #17, #18, #20, #22). W2 paused until #2/#6 merge and the oracle re-runs on main. Deferred until after the bump because upstream changed them in 1.89-1.91: Style/MethodCallWithArgsParentheses omit_parentheses (reparse verification), HashAlignment tail |
| W3 | Cop IR design (docs/planning/04-cop-ir-design.md) | drafted, awaiting owner review |
| W4 | node_pattern completion PRs | done: #3 → #4 → #5 → #7 → #11 → #13 (752/991 vendored patterns resolve on builtins; 991/991 parse) |
| W5 | #9 schema+loader open; Expr compiler/evaluator in progress (branch ir/expr, includes merge of np/predicate-resolution); #14 IrCop + #16 TimeNow open; 8-cop pilot batch in progress (branch ir/pilot-batch); #21 discovery open | in progress |
| W7 | Post-merge follow-ups: shared tab-aware `indentation_of` + dedup; `Severity:` config parity; HashAlignment 1.91 semantics; MethodCallWithArgsParentheses omit_parentheses reparse rule | blocked on merges |
| W6 | ir_extract.py / ir_classify.py / spec_to_fixture.py #15 open; ir_synth.py + ir_verify.py + CI job in progress (branch ir/translator-synth-verify, base #23) | in progress |

## Open PRs
- #1 ci: skip corpus-oracle PR step without GH App secrets
- #3 np: real captures (base main)
- #5 np: `#helper(args)`/`%param` lexing + builtin predicate registry (base #4)
- #7 np: `#helper`/`pred?`/`%param` resolution (base #5); 747/991 vendored patterns resolve on builtins alone
- #8 fix: Lint/RedundantCopDisableDirective (base main) — sample FP 437→3, FN 121→111; remaining FN is cluster 4 below
- #6 vendor: 8 new cop config options (base #2), makes the bump stack green
- #11 np: walker ancestor stack, `^`/`` ` ``/`%0`, ancestor predicates (base #7)
- #13 np: `?`/`*`/`+` repetition operators (base #11). np stack complete: #3→#4→#5→#7→#11→#13. Known mapping gap worth its own PR: Prism `StatementsNode` wrapper where Parser has a bare body statement (breaks `(def _ (args) $(...))`-shaped patterns); parameterless `def` has no `ParametersNode` so `(args)` cannot match
- #12 ir: Expr compiler + evaluator (base #9, includes merge of np/predicate-resolution)
- #14 ir: IrCopRunner + registry + `ir_cop_fixture_tests!` (base #12, includes merge of np/repetition); ~13 µs/file upper-bound overhead
- #16 ir: first translated cop Style/TimeNow (base #14); engine fix: exact child arity for every sequence
- #24 np: body/`(args)`/`rescue` mapping fidelity (base #13); 4 commits are cherry-picks from #19/#23 (already-applied on rebase), 2 new. **After merge, ir/* branches must re-run pilot fixture tests: body slots, parameterless blocks, keyword-less BeginNode, variable targets changed shape.**
- #21 ir: user-cop discovery `.nitrocop/cops/**/*.cop.yml` + `AllCops.CustomCopPaths`, `(custom)` classification, fail-closed, cache key (base #16)
- #19 ir: pilot batch 1/2 (FileOpen, DataDefineOverride, RedundantMinMaxBy, PredicateWithKind) (base #16)
- #23 ir: pilot batch 2/2 (TallyMethod, SelectByKind, RedundantStructKeywordInit) (base #19). 7/8 landed; 8 engine additions incl. `_name` unification, `%TABLE` via `constants:`, `min_target_ruby:`, sibling access. Blocked on vocabulary: RSpec/MatchWithSimpleRegex (regex→string transform), Style/MapJoin (needs `.last_line` / loc-part line access in expressions)
- #15 ir: ir_extract.py / ir_classify.py / spec_to_fixture.py (base #12); pilot buckets A=2 B=10 C=11 (5 of C are ProjectIndexHelp)
- #9 ir: schema + loader + `--validate-ir` (base main)
- #22 fix: Lint/UselessAssignment (base main) — VariableForce engine gains `RescueModifierNode` branch handling; two RuboCop quirks replicated (loop-shape structural equality, declaration-order chained-assignment suppression)
- #20 fix: Layout/MultilineOperationIndentation (base main) — line-by-line port; 97-repo sample 19/39 → 0/0 (aligned), 49/173 → 0/0 (indented), zero message mismatches
- #18 fix: Layout/RedundantLineBreak (base main) — 66-repo sample 41/81 → 0/4; residue is invalid-Ruby files Parser error-recovers and Prism does not (parse/discovery divergence, not cop logic); deleted ~560 lines of text-based backslash heuristics
- #17 fix: Layout/MultilineMethodCallIndentation (base main) — 30-repo sample default 89/88 → 0/0, indented 59/85 → 2/3, irtr 10/107 → 0/4; open: `check_hash_pair_indented_style` path
- #10 fix: Layout/HashAlignment separator variant (base main) — two structural causes; note it still replicates the 1.84.2 clobber-abort quirk that 1.91.0 removes, revisit after the bump
- #4 np: `<>` unordered + mapping expansion (base #3)
- #2 vendor: bump rubocop 1.91.0 / rails 2.37.0 / rspec 3.10.2 / performance 1.27.0 / ast 1.50.0 (see 05-vendor-bump-scope.md; 280 implemented cops have upstream behavior changes; oracle re-run on main after merge measures drift)

## Cross-cutting findings
- Review pass on #8/#22 found and fixed 5 defects (3 in `each_already_disabled` port: append-order adjacency, duplicate diagnostics, inline `N..N` adjacency; 2 in the loop-shape quirk: whitespace normalization broader than AST equality, op-assign reads). Pre-existing gaps noted on the PRs: short-name qualification is registry-wide not plugin-aware (9 ambiguous short names), department/`all` interactions in RCDD, `AllowCopDirectives` line length, `retry`-as-loop. Review of #17/#18/#20 in progress.
- Test hygiene: `default_args()` in tests/integration.rs leaves the result cache on while other tests unset `NITROCOP_CACHE_DIR`; a failing run poisons `~/.cache/nitrocop`. Clear it before full runs; small fix PR pending.
- Tab-indented files: `shared::util::indentation_of` counts spaces only; RuboCop's `indentation` uses `/\S/`. Bit three Layout cops (#17, #20, HashAlignment-adjacent). Audit in 07-tab-indentation-audit.md: fix `src/cop/shared/util.rs::indentation_of` in place (count `\t`), delete 5 cop-local reimplementations (pure dedup, corpus-neutral), then per-cop gated PRs for the 3 remaining spaces-only decision sites. **Do after #17/#20 merge** (they added local helpers that the dedup removes).
- `gen_repo_config.py` writes to a fixed `/tmp/nitrocop_corpus_configs/corpus_config_<repo>.yml`; running `run_nitrocop.py` between generating a variant overlay and invoking RuboCop silently reverts the variant.
- Local runs must be wrapped in `mise exec --` or `bundle info --path` plugin lookups fail silently and create phantom divergence.

## IR vocabulary gaps found by the pilot (design revisions)
- Expressions cannot read loc parts (`node.loc.dot.line`) or `last_line`; anchors can. Fuse the vocabularies.
- No string transforms (MatchWithSimpleRegex needs a Regexp::Parser-equivalent) — keep such cops in Rust.
- `matchers:` compile in name order; a helper union named before its operands is unresolvable (RedundantStructKeywordInit). Consider two-pass declaration.
- `constants:` needs a list-valued form; `in:` cannot take a `string_array` config value.
- Extractor: merge `on_send`+`on_csend` into one hook, do not blanket `any_of` matchers, handle `%i[].to_set.freeze`, carry `Enabled`/`minimum_target_ruby_version`/department severity into the skeleton.
- Verify: fixtures generated from real RuboCop JSON output are trustworthy; spec text is not (upstream spec asserted a wrong message once). Run differentials at default and 3.4 target Ruby; compare sets; check `-A` convergence, not single pass.

## Owner decisions needed
- **Custom cop config location (PR #21 finding):** RuboCop 1.84 exits 2 on any unknown cop section (`Custom/Foo: {Enabled: true}`) in `.rubocop.yml`, but only warns on unknown `AllCops` params. Teams running both tools can carry `AllCops.CustomCopPaths` but cannot configure custom cops there. Options: (a) accept the limitation (documented in docs/CUSTOM_COPS.md), (b) add a nitrocop-only overlay file (`.nitrocop.yml`, contrary to upstream PLAN.md's initial stance), (c) per-cop config inside the `.cop.yml` document itself. Recommendation: (b) as a thin overlay merged after `.rubocop.yml`.
- **`Severity:` from `.rubocop.yml` is ignored by every hand-written cop** (parsed, never read). Wired for IR cops only in #21. A repo-wide fix moves corpus numbers and needs its own PR + oracle gate.
- **Config-relative Include/Exclude (PR #8 cluster 4, 110 FN + 2 FP):** RuboCop absolutizes cop-level Include/Exclude against the config file's directory; the oracle passes a temp-dir config so RuboCop never runs include-gated cops there. Options: (a) match RuboCop in `CopFilterSet` (see docs/investigations/investigation-target-dir-relativization.md), or (b) teach the oracle's include-gated pass to re-derive Lint/RedundantCopDisableDirective. Do NOT ignore include-gated cops when marking directives used.
- **ProjectIndexHelp** (5 new cops need a cross-file symbol index): build the index infra, or leave those 5 unimplemented.

## Merge order for the IR stream
main ← #3 ← #4 ← #5 ← #7 ← #11 ← #13 ← #24 (np) ; main ← #9 ← #12 (merges #7) ← #14 (merges #13) ← #16 ← pilot ; #15 (Python) on #12. Merging the np stack first flattens the merge commits in #12/#14.

## Sequencing notes
- After #2 merges: dispatch corpus oracle on main, then triage drift (W2). Three cops replicate RuboCop 1.84 crashes that upstream fixed: Layout/HashAlignment, Layout/IndentationWidth, Lint/LiteralAsCondition.
- 5 of the 23 new cops need a cross-file symbol index (ProjectIndexHelp): Lint/ArgumentMismatch, DeprecatedReference, NameTypo, SuperArgumentMismatch, UnusedPrivateMethod. Needs an owner decision; not IR work.
