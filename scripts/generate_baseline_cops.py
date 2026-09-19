#!/usr/bin/env python3
from __future__ import annotations

"""Regenerate src/resources/baseline_cops.json from vendored config/default.yml files.

`baseline_cops.json` is a flat `{cop_name: default_enabled}` map embedded into the
nitrocop binary (see `src/rules.rs::load_baseline_cops`). It powers the `--rules`
command's `in_baseline`/`default_enabled` columns, which distinguish "known
RuboCop cop nitrocop hasn't implemented yet" from "not a real cop at all".

The value for each cop is the *raw* `Enabled:` setting from the vendor gem's own
`config/default.yml` — NOT the corpus oracle's baseline_rubocop.yml overrides
(those exist only to force opt-in cops on for corpus testing, and must not leak
into this file). `Enabled: pending` resolves to `true` (RuboCop's `NewCops: enable`
convention, matching how nitrocop's own baseline_rubocop.yml treats pending cops).
This mapping was reverse-engineered from the pre-existing (hand-maintained,
generator-less) baseline_cops.json — see docs/agent notes on the rubocop 1.91
vendor bump for the verification method.

Usage:
    python3 scripts/generate_baseline_cops.py            # regenerate the file
    python3 scripts/generate_baseline_cops.py --check     # verify it's in sync (CI/tests)
"""

import argparse
import json
import sys
from pathlib import Path

import yaml

PROJECT_ROOT = Path(__file__).resolve().parent.parent
OUTPUT_PATH = PROJECT_ROOT / "src" / "resources" / "baseline_cops.json"

# Vendor gems whose cops are covered by the embedded baseline. rubocop-rake and
# rubocop-ast are intentionally excluded: rake cops aren't part of this baseline
# (see bench/corpus/baseline_rubocop.yml, which also omits it from src/resources/
# baseline.json's version map), and rubocop-ast ships no cops at all.
VENDOR_CONFIGS = [
    PROJECT_ROOT / "vendor" / "rubocop" / "config" / "default.yml",
    PROJECT_ROOT / "vendor" / "rubocop-rails" / "config" / "default.yml",
    PROJECT_ROOT / "vendor" / "rubocop-performance" / "config" / "default.yml",
    PROJECT_ROOT / "vendor" / "rubocop-rspec" / "config" / "default.yml",
    PROJECT_ROOT / "vendor" / "rubocop-rspec_rails" / "config" / "default.yml",
    PROJECT_ROOT / "vendor" / "rubocop-factory_bot" / "config" / "default.yml",
]


class _RubyYamlLoader(yaml.SafeLoader):
    """YAML loader that handles Ruby-specific tags like !ruby/regexp."""


_RubyYamlLoader.add_constructor(
    "!ruby/regexp", lambda loader, node: loader.construct_scalar(node)
)


def load_cop_enabled_states(config_path: Path) -> dict[str, bool]:
    """Parse one vendor config/default.yml into {cop_name: default_enabled}.

    Top-level keys without a "/" (e.g. `AllCops`) are not cops and are skipped.
    `Enabled: pending` resolves to `True`; missing `Enabled:` defaults to `True`
    (RuboCop treats an absent key as enabled), matching every cop actually
    present in vendor configs today (all of them set `Enabled:` explicitly).
    """
    with open(config_path) as f:
        data = yaml.load(f, Loader=_RubyYamlLoader)

    cops: dict[str, bool] = {}
    for name, settings in data.items():
        if "/" not in name or not isinstance(settings, dict):
            continue
        enabled = settings.get("Enabled", True)
        cops[name] = True if enabled == "pending" else bool(enabled)
    return cops


def compute_baseline_cops() -> dict[str, bool]:
    baseline: dict[str, bool] = {}
    for config_path in VENDOR_CONFIGS:
        if not config_path.exists():
            raise FileNotFoundError(
                f"vendor config not found: {config_path} "
                "(run `git submodule update --init` first)"
            )
        baseline.update(load_cop_enabled_states(config_path))
    return dict(sorted(baseline.items()))


def render(baseline: dict[str, bool]) -> str:
    return json.dumps(baseline, indent=2, sort_keys=True) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="verify src/resources/baseline_cops.json is in sync; exit 1 if not (no write)",
    )
    args = parser.parse_args()

    baseline = compute_baseline_cops()
    rendered = render(baseline)

    if args.check:
        current = OUTPUT_PATH.read_text() if OUTPUT_PATH.exists() else ""
        if current == rendered:
            print(f"{OUTPUT_PATH.relative_to(PROJECT_ROOT)} is in sync.")
            return 0

        current_cops = json.loads(current) if current else {}
        added = sorted(set(baseline) - set(current_cops))
        removed = sorted(set(current_cops) - set(baseline))
        changed = sorted(
            k
            for k in baseline
            if k in current_cops and baseline[k] != current_cops[k]
        )
        print(
            f"{OUTPUT_PATH.relative_to(PROJECT_ROOT)} is out of sync with vendor "
            "config/default.yml files.",
            file=sys.stderr,
        )
        if added:
            print(f"  added ({len(added)}): {added}", file=sys.stderr)
        if removed:
            print(f"  removed ({len(removed)}): {removed}", file=sys.stderr)
        if changed:
            print(f"  changed default_enabled ({len(changed)}): {changed}", file=sys.stderr)
        print(
            "Run `python3 scripts/generate_baseline_cops.py` to regenerate.",
            file=sys.stderr,
        )
        return 1

    OUTPUT_PATH.write_text(rendered)
    print(f"Wrote {len(baseline)} cops to {OUTPUT_PATH.relative_to(PROJECT_ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
