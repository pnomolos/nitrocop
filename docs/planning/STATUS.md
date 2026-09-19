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
| W2 | Fix diverging cops: Lint/RedundantCopDisableDirective (740/203) in progress on branch fix/lint-redundant-cop-disable-directive; then Layout/MultilineMethodCallIndentation, Style/MethodCallWithArgsParentheses (omit_parentheses variant), Layout/RedundantLineBreak, Layout/MultilineOperationIndentation, Lint/UselessAssignment, Layout/HashAlignment (separator variant) | #8 open; Layout/HashAlignment separator variant in progress (branch fix/layout-hash-alignment-separator) |
| W3 | Cop IR design (docs/planning/04-cop-ir-design.md) | drafted, awaiting owner review |
| W4 | node_pattern completion PRs | #3 → #4 → #5 → #7 open; ancestors/`^`/`` ` `` + repetition operators in progress (branches np/ancestors, np/repetition) |
| W5 | IR schema + loader (branch ir/schema-and-loader) in progress; then Expr compiler, IrCop + registry, first translated cop | in progress |
| W6 | Translator scripts + pilot on 23 new cops | blocked on W1, W5 |

## Open PRs
- #1 ci: skip corpus-oracle PR step without GH App secrets
- #3 np: real captures (base main)
- #5 np: `#helper(args)`/`%param` lexing + builtin predicate registry (base #4)
- #7 np: `#helper`/`pred?`/`%param` resolution (base #5); 747/991 vendored patterns resolve on builtins alone
- #8 fix: Lint/RedundantCopDisableDirective (base main) — sample FP 437→3, FN 121→111; remaining FN is cluster 4 below
- #6 vendor: 8 new cop config options (base #2), makes the bump stack green
- #4 np: `<>` unordered + mapping expansion (base #3)
- #2 vendor: bump rubocop 1.91.0 / rails 2.37.0 / rspec 3.10.2 / performance 1.27.0 / ast 1.50.0 (see 05-vendor-bump-scope.md; 280 implemented cops have upstream behavior changes; oracle re-run on main after merge measures drift)

## Owner decisions needed
- **Config-relative Include/Exclude (PR #8 cluster 4, 110 FN + 2 FP):** RuboCop absolutizes cop-level Include/Exclude against the config file's directory; the oracle passes a temp-dir config so RuboCop never runs include-gated cops there. Options: (a) match RuboCop in `CopFilterSet` (see docs/investigations/investigation-target-dir-relativization.md), or (b) teach the oracle's include-gated pass to re-derive Lint/RedundantCopDisableDirective. Do NOT ignore include-gated cops when marking directives used.
- **ProjectIndexHelp** (5 new cops need a cross-file symbol index): build the index infra, or leave those 5 unimplemented.

## Sequencing notes
- After #2 merges: dispatch corpus oracle on main, then triage drift (W2). Three cops replicate RuboCop 1.84 crashes that upstream fixed: Layout/HashAlignment, Layout/IndentationWidth, Lint/LiteralAsCondition.
- 5 of the 23 new cops need a cross-file symbol index (ProjectIndexHelp): Lint/ArgumentMismatch, DeprecatedReference, NameTypo, SuperArgumentMismatch, UnusedPrivateMethod. Needs an owner decision; not IR work.
