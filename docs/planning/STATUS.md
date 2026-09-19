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
| W0 | Corpus oracle baseline on fork (run 35413735947, branch ci/corpus-oracle-optional-app-token, PR #1) | running |
| W1 | Vendor bump to latest (scope doc pending) | scoping |
| W2 | Fix remaining diverging cops from fresh oracle | blocked on W0 |
| W3 | Cop IR design (docs/planning/04-cop-ir-design.md) | drafting |
| W4 | node_pattern completion PRs | blocked on W3 |
| W5 | IR loader/interpreter MVP | blocked on W4 |
| W6 | Translator scripts + pilot on 23 new cops | blocked on W1, W5 |

## Open PRs
- #1 ci: skip corpus-oracle PR step without GH App secrets
