---
name: update-gem
description: Checklist of files to update when adding or updating a rubocop plugin gem
allowed-tools: Read, Grep, Glob
---

When adding a new rubocop plugin gem or updating an existing gem version, the following files must be updated. Use this checklist to ensure nothing is missed.

## Files to update

### 1. `bench/corpus/Gemfile`
Pin the gem version (e.g. `gem "rubocop-rake", "0.7.1"`).

### 2. `bench/corpus/diff_results.py`
Add or update the gem version in the `"baseline"` dictionary inside `write_corpus_results()`.

### 3. `bench/corpus/baseline_rubocop.yml`
Add the gem to the `plugins:` list. If any cops are `Enabled: false` in the gem's vendor default config, add explicit `Enabled: true` overrides (see existing examples for RSpec, Rails, etc.). Cops that default to `Enabled: true` or `Enabled: pending` don't need overrides — `NewCops: enable` handles pending cops.

### 4. `bench/corpus/update_readme.py`
Add an entry to the `GEMS` list with `key`, `url`, and `departments`.

### 5. `scripts/dispatch_cops.py`
Add the department to `DEPT_TO_VENDOR` (maps department name to `vendor/<gem-dir>`). If the department name doesn't snake_case cleanly (e.g. `FactoryBot` -> `factory_bot`, `RSpecRails` -> `rspec_rails`), also add it to `DEPT_TO_DIR` and `DEPT_TO_SRC_DIR`.

### 6. `.github/workflows/batch-dispatch.yml`
Add the department to the `department` choice options list (keep alphabetical order).

### 7. Vendor submodule
Add the vendor submodule pinned to the release tag: `git submodule add <repo-url> vendor/<gem-name>`.

### 8. Tier gating (no action needed)
New cops default to `preview` tier via `src/resources/tiers.json`. Promotion to `stable` happens automatically via the corpus oracle workflow — do not manually edit tiers.json. The corpus oracle uses `--preview` so preview-tier cops are always tested in CI, but users running nitrocop locally without `--preview` won't see them.

### 9. `src/resources/baseline_cops.json`
Regenerate with `python3 scripts/generate_baseline_cops.py` (verify first with `--check`). This flat `{cop_name: default_enabled}` map is read at runtime by `src/rules.rs::load_baseline_cops` to power the `--rules`/`--migrate`/doctor "known cop, not yet implemented" (`unimplemented`) vs. "not a real cop" (`outside_baseline`) distinction — it must include every cop from every vendored gem's `config/default.yml` (rubocop, rubocop-rails, rubocop-performance, rubocop-rspec, rubocop-rspec_rails, rubocop-factory_bot; rubocop-rake and rubocop-ast are intentionally excluded — see the script's docstring), with `Enabled: pending` resolved to `true`. `tests/python/test_generate_baseline_cops.py` fails CI if this drifts out of sync with the vendor submodules, so it's required, not optional, on every version bump — including a plain patch bump, since cop defaults can change without a new cop being added. If a cop is removed upstream (superseded by another cop, e.g. `Style/DoubleCopDisableDirective` -> `Lint/CopDirectiveSyntax` in rubocop 1.91), it drops out of this file automatically; also override that cop's Rust `default_enabled()` to `false` and document the removal in a `///` doc comment, per AGENTS.md's rule for vendor-disabled cops.

### 10. `bench/synthetic/Gemfile` and `bench/synthetic/Gemfile.lock`
Not auto-synced by `bench/update_rubocop_deps.rb` (that only touches ephemeral `bench/repos/*` checkouts) and easy to miss because it's not covered by `bench/corpus/`. Hand-edit the pinned gem versions in `bench/synthetic/Gemfile` to match `bench/corpus/Gemfile`, then regenerate the lockfile: `cd bench/synthetic && mise exec -- bundle install` (Ruby version from `mise.toml`; no `BUNDLE_PATH` override needed here, unlike `bench/corpus`).

## Reference

See PR #1353 (rubocop-rake support) for an example of adding a new plugin gem, though note it missed several of the items in this checklist (fixed in a follow-up commit).

## Verification

After making changes, run:
```bash
uv run ruff check bench/corpus/diff_results.py scripts/dispatch_cops.py bench/corpus/update_readme.py scripts/generate_baseline_cops.py
uv run python3 scripts/generate_baseline_cops.py --check   # or run it for real and git diff the result
uv run pytest tests/python/test_generate_baseline_cops.py --tb=short
```
