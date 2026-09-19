# nitrocop cop gap analysis

Method: `config/default.yml` parsed from each `vendor/<gem>` checkout with a
YAML loader that strips `!ruby/*` tags (needed for `vendor/rubocop`, which
embeds `!ruby/regexp` values). Cop keys = top-level `Department/Name` keys,
excluding `AllCops`. "Implemented" = exact string match against
`./target/release/nitrocop --list-cops` (built from HEAD, release profile).
Upstream "latest" config pulled via `git show <tag>:config/default.yml`
after `git fetch --tags` — **no working tree was checked out or modified**.

## 1. Vendored version vs. latest release

| Gem | Vendored tag | Vendored ver | Latest on rubygems.org | Behind? | Vendored cops | Latest-tag cops | New cops upstream |
|---|---|---|---|---|---:|---:|---:|
| rubocop | v1.84.2 | 1.84.2 | 1.91.0 | yes | 593 | 613 | 21 added, 1 removed |
| rubocop-rails | v2.34.3 | 2.34.3 | 2.37.0 | yes | 148 | 148 | 0 |
| rubocop-rspec | v3.9.0 | 3.9.0 | 3.10.2 | yes | 114 | 116 | 2 added, 0 removed |
| rubocop-performance | v1.26.1 | 1.26.1 | 1.27.0 | yes | 52 | 52 | 0 |
| rubocop-rake | v0.7.1 | 0.7.1 | 0.7.1 | no | 5 | — | — |
| rubocop-factory_bot | v2.28.0 | 2.28.0 | 2.28.0 | no | 11 | — | — |
| rubocop-rspec_rails | v2.32.0 | 2.32.0 | 2.32.0 | no | 8 | — | — |

All vendored submodules are pinned exactly to their release tag (`git describe --tags`
returns the tag with no `-N-g<sha>` suffix for every gem — no drift from tag to HEAD).

Total vendored cop *definitions* across all 7 gems: 593+148+114+52+5+11+8 = **931**.
11 of those names are defined in two gem configs simultaneously (an extension
gem re-declaring/re-tuning a core `rubocop` cop, or `rubocop-rspec` overriding
`Metrics/BlockLength`'s config) — see §2 — so the count of *unique* cop names
across all gems is 931 − 11 = **920**.

## 2. Implemented vs. vendored (pinned versions) — the headline number

```
./target/release/nitrocop --list-cops | wc -l   ->  920
```

This is an **exact match** to the 920 unique vendored cop names. Per gem:

| Gem | Vendored cops | Implemented | Unimplemented |
|---|---:|---:|---:|
| rubocop | 593 | 593 | **0** |
| rubocop-rails | 148 | 148 | **0** |
| rubocop-rspec | 114 | 114 | **0** |
| rubocop-performance | 52 | 52 | **0** |
| rubocop-rake | 5 | 5 | **0** |
| rubocop-factory_bot | 11 | 11 | **0** |
| rubocop-rspec_rails | 8 | 8 | **0** |

**Zero unimplemented cops against the vendored gem versions.** nitrocop has
100% nominal coverage of every cop declared in every pinned vendored
`config/default.yml`. `gap/unimplemented.json` is correctly all-empty arrays —
this is not a bug in the extraction, it's confirmed by two independent
sources agreeing exactly (920 == 920).

Cross-check against `src/resources/tiers.json`: `default_tier` is `"preview"`,
and `overrides` contains **910** entries (all mapped to `"stable"`), not 905
as assumed in the task brief — the actual current count is 910. Every
override key exists in `--list-cops` output (0 stale overrides). The 10
implemented cops with *no* override — i.e. still `preview` tier by the
`default_tier` fallback — are:

```
Layout/MultilineMethodCallIndentation   Lint/NumberedParameterAssignment
Layout/MultilineOperationIndentation    Lint/RedundantCopDisableDirective
Layout/RedundantLineBreak               Lint/UselessAssignment
Lint/ItWithoutArgumentsInBlock          Lint/UselessElseWithoutRescue
Lint/NonDeterministicRequireOrder       Security/YAMLLoad
```

Five of those ten are RuboCop cops nitrocop implements as intentional no-ops
(each has a `///` doc comment explaining why): `Lint/ItWithoutArgumentsInBlock`,
`Lint/NonDeterministicRequireOrder`, `Lint/NumberedParameterAssignment`,
`Lint/UselessElseWithoutRescue`, `Security/YAMLLoad` — all obsolete on modern
Ruby (these are also `registry.rs`'s documented "5 no-ops", matching the
`// 915 supported + 5 no-ops` comment and `assert_eq!(reg.len(), 915 + 5)` in
`src/cop/registry.rs:91-92`). The other five preview-tier cops
(`Layout/MultilineMethodCallIndentation`, `Layout/MultilineOperationIndentation`,
`Layout/RedundantLineBreak`, `Lint/RedundantCopDisableDirective`,
`Lint/UselessAssignment`) are exactly the cops flagged as diverging in
`docs/corpus.md` (§5 below) — consistent with them still being gated behind
`--preview` rather than promoted to `stable`.

### The 11 duplicate cop names (why 931 vendored defs = 920 unique names)

| Cop | Declared in |
|---|---|
| Lint/NumberConversion | rubocop, rubocop-rails |
| Lint/RedundantSafeNavigation | rubocop, rubocop-rails |
| Lint/SafeNavigationChain | rubocop, rubocop-rails |
| Lint/UselessAccessModifier | rubocop, rubocop-rails |
| Lint/UselessMethodDefinition | rubocop, rubocop-rails |
| Metrics/BlockLength | rubocop, rubocop-rspec |
| Style/AndOr | rubocop, rubocop-rails |
| Style/CollectionCompact | rubocop, rubocop-rails |
| Style/FormatStringToken | rubocop, rubocop-rails |
| Style/InvertibleUnlessCondition | rubocop, rubocop-rails |
| Style/SymbolProc | rubocop, rubocop-rails |

(rubocop-rails re-declares these to widen/narrow their `Include`/`Exclude` for
Rails-flavored files; rubocop-rspec does the same for `Metrics/BlockLength`
scoped to spec blocks.)

## 3. Registry mechanism (how `--list-cops` was produced)

- `src/cop/registry.rs`: `CopRegistry::default_registry()` calls
  `<dept>::register_all(&mut registry)` for 15 department modules
  (`bundler, factory_bot, gemspec, layout, migration, lint, metrics, naming,
  performance, rails, rake, rspec, rspec_rails, security, style`).
- `Cop::name()` (`src/cop/mod.rs:275`) is the trait method each cop struct
  implements to return its canonical `"Department/Name"` string; the registry
  indexes on that string (`registry.rs:41-46`).
- The binary exposes this via `--list-cops` (`src/cli.rs:75`, wired in
  `src/lib.rs:226-227`), which needs no config file — it just dumps
  `registry.names()`. This is more reliable than grepping cop struct
  attributes because it reflects exactly what's registered at runtime,
  including anything registered conditionally.
- `cargo test` in `src/cop/registry.rs::default_registry_has_cops` already
  hard-asserts `reg.len() == 915 + 5 == 920`, matching `--list-cops` output
  exactly — no discrepancy between the compiled-in assertion and the CLI.

## 4. Upstream drift: cops that exist upstream but not in the vendored tag

These are **not** "unimplemented" in the strict sense the task defines (they
aren't in the pinned `vendor/` config), but they are the actual forward-looking
gap once the vendored gems get bumped. None of the 23 cops below are
implemented in nitrocop today (checked against `--list-cops`); none require
external inputs (no `schema.rb`/`Gemfile.lock`/filesystem dependence — the
only "external" reads found were `minimum_target_ruby_version` guards, which
nitrocop already resolves from `TargetRubyVersion` config, not a new input
category).

### rubocop v1.84.2 → v1.91.0 (+21 / −1)

| Cop | Enabled (latest) | VersionAdded | LOC (Ruby impl) | External input? |
|---|---|---|---:|---|
| Lint/ArgumentMismatch | pending | 1.90 | 99 | no |
| Lint/DataDefineOverride | pending | 1.85 | 63 | no |
| Lint/DeprecatedReference | pending | 1.89 | 194 | no |
| Lint/MisplacedMagicComment | pending | 1.91 | 185 | no |
| Lint/NameTypo | pending | 1.89 | 270 | no |
| Lint/SuperArgumentMismatch | pending | 1.90 | 179 | no |
| Lint/UnreachablePatternBranch | pending | 1.85 | 113 | no |
| Lint/UnusedPrivateMethod | false | 1.89 | 234 | no |
| Style/DirectiveScope | pending | 1.90 | 304 | no |
| Style/FileOpen | pending | 1.85 | 84 | no |
| Style/MapJoin | pending | 1.85 | 123 | no |
| Style/OneClassPerFile | pending | 1.85 | 115 | no |
| Style/PartitionInsteadOfDoubleSelect | pending | 1.85 | 270 | no |
| Style/PredicateWithKind | pending | 1.85 | 84 | no |
| Style/ReduceToHash | pending | 1.85 | 200 | no |
| Style/RedundantMinMaxBy | pending | 1.85 | 93 | no |
| Style/RedundantStructKeywordInit | false | 1.85 | 133 | no |
| Style/SelectByKind | pending | 1.85 | 158 | no |
| Style/SelectByRange | pending | 1.85 | 197 | no |
| Style/TallyMethod | pending | 1.85 | 181 | no |
| Style/TimeNow | pending | 1.90 | 41 | no |

Removed: `Style/DoubleCopDisableDirective` — folded into `Lint/CopDirectiveSyntax`
(rubocop commit `96687647e`, "Move double-directive checking into
`Lint/CopDirectiveSyntax`"). `Lint/CopDirectiveSyntax` is already both
vendored and implemented in nitrocop, so this removal creates no gap.

### rubocop-rspec v3.9.0 → v3.10.2 (+2 / −0)

| Cop | Enabled (latest) | VersionAdded | LOC (Ruby impl) | External input? |
|---|---|---|---:|---|
| RSpec/DiscardedMatcher | pending | 3.10 | 113 | no |
| RSpec/MatchWithSimpleRegex | pending | 3.10 | 92 | no |

### rubocop-rails v2.34.3 → v2.37.0, rubocop-performance v1.26.1 → v1.27.0

No cop-count change (0 added, 0 removed) despite the version bump — those
releases were bugfix/behavior-only.

**Net upstream gap if all four drifting gems were bumped to latest: 23 net
new cop names to implement (21 rubocop + 2 rubocop-rspec), 0 requiring
external inputs, ranging 41–304 LOC in the reference Ruby implementation
(median ~158 LOC).**

## 5. Corpus oracle baseline (docs/corpus.md, generated 2026-04-26 — stale per AGENTS.md)

| Metric | Value |
|---|---:|
| Repos | 5,587 |
| Repos with 100% match | 3,750 |
| Files inspected | 590,633 |
| Offenses compared | 28,374,438 |
| Matches | 28,372,427 |
| FP (nitrocop extra) | 1,370 |
| FN (nitrocop missing) | 641 |
| Registered cops (at generation time) | 915 |
| Cops with exact match | 877 |
| Cops with divergence | 5 |
| Cops with no corpus data | 33 |
| Match rate (default config) | 99.99% |
| Match rate (all variants) | 99.98% |

Diverging cops (default config), with match %:

| Cop | Matches | FP | FN | Match % |
|---|---:|---:|---:|---:|
| Lint/RedundantCopDisableDirective | 2,106 | 951 | 210 | 64.4% |
| Layout/MultilineMethodCallIndentation (default) | 40,431 | 252 | 210 | 98.8% |
| Layout/RedundantLineBreak | 276,201 | 111 | 102 | 99.9% |
| Layout/MultilineOperationIndentation (default) | 47,040 | 22 | 96 | 99.7% |
| Lint/UselessAssignment | 27,211 | 34 | 23 | 99.7% |

All 5 corpus-diverging cops are exactly the 5 non-no-op preview-tier cops
identified in §2 — the corpus oracle and the tier gating agree on which cops
are not yet "done."

## Bottom line

- **Cop-name coverage against pinned vendor versions: 920/920 (100%), 0 gap.**
- The only real backlog is upstream version drift: bumping `rubocop` to
  1.91.0 and `rubocop-rspec` to 3.10.2 would introduce 23 net-new cop names
  (`rubocop-rails`, `rubocop-performance`, `rubocop-rake`,
  `rubocop-factory_bot`, `rubocop-rspec_rails` are either already current or
  added no cops in their latest release).
- The remaining quality gap is not "unimplemented cops" but the 5
  corpus-diverging, preview-tier cops in `docs/corpus.md` — matches
  `fix-department`/`repair-variant`/`triage` skill territory, not new-cop
  work.

## Files in this directory

- `gap-analysis.md` — this file.
- `unimplemented.json` — `{gem: []}` for all 7 gems (confirmed empty; see §2).
- `implemented.txt` — 920 cop names, one per line, sorted, from `--list-cops`.
- `vendored_<gem>.json`, `latest_<gem>.json` — raw per-cop metadata dumps used
  to build the tables above (intermediate work files, not part of the
  requested deliverable set, kept for audit trail).
