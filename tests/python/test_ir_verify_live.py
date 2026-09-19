#!/usr/bin/env python3
"""Opt-in live end-to-end test: the whole `ir_verify.py` chain against real
RuboCop 1.91.0, on `Style/TimeNow` — the one pilot cop already shipped and
embedded, so `embedded_freshness` and `cargo_test` have something real to
check against.

Skipped by default (never runs in CI, never runs under plain `pytest`). Set
`IR_VERIFY_LIVE=1` and `IR_VERIFY_RUBOCOP_GEM_DIR=/path/to/scratch/gem/dir`
(from `mise exec -- gem install rubocop -v 1.91.0 --install-dir <dir> --no-document`)
to run it:

    mise exec -- gem install rubocop -v 1.91.0 --install-dir /tmp/rubocop-1.91.0 --no-document
    IR_VERIFY_LIVE=1 IR_VERIFY_RUBOCOP_GEM_DIR=/tmp/rubocop-1.91.0 \\
        uv run pytest tests/python/test_ir_verify_live.py -v -s

Also needs: `vendor/rubocop` checked out at (or fetchable to) `v1.91.0`
(`git -C vendor/rubocop fetch --tags && git -C vendor/rubocop checkout v1.91.0`),
a release nitrocop binary built from the current tree (`cargo build --release`),
and `mise` with a Ruby installed (`mise.toml` pins one).
"""
from __future__ import annotations

import importlib.util
import os
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
sys.path.insert(0, str(ROOT / "scripts" / "workflows"))

_spec = importlib.util.spec_from_file_location("ir_verify", ROOT / "scripts" / "ir_verify.py")
ir_verify = importlib.util.module_from_spec(_spec)
sys.modules["ir_verify"] = ir_verify
_spec.loader.exec_module(ir_verify)


pytestmark = pytest.mark.skipif(
    os.environ.get("IR_VERIFY_LIVE") != "1",
    reason="opt-in live test — set IR_VERIFY_LIVE=1 (see module docstring)",
)


def test_style_time_now_full_verify_chain_against_real_rubocop():
    gem_dir = os.environ.get("IR_VERIFY_RUBOCOP_GEM_DIR")
    if not gem_dir:
        pytest.fail("IR_VERIFY_RUBOCOP_GEM_DIR must be set when IR_VERIFY_LIVE=1")
    gem_dir_path = Path(gem_dir)
    assert (gem_dir_path / "bin" / "rubocop").is_file(), f"no rubocop binary under {gem_dir}"

    yml_path = ROOT / "src" / "resources" / "ir" / "style" / "time_now.cop.yml"
    assert yml_path.is_file()

    binary = ir_verify.resolve_nitrocop_binary(None)
    assert Path(binary).is_file(), (
        f"no nitrocop binary at {binary} — run `cargo build --release` first"
    )

    rubocop_root = ROOT / "vendor" / "rubocop"
    if not (rubocop_root / "lib" / "rubocop" / "cop" / "style" / "time_now.rb").is_file():
        subprocess.run(["git", "-C", str(rubocop_root), "fetch", "--tags", "--quiet"], check=False)
        subprocess.run(["git", "-C", str(rubocop_root), "checkout", "v1.91.0", "--quiet"], check=False)
    if not (rubocop_root / "lib" / "rubocop" / "cop" / "style" / "time_now.rb").is_file():
        pytest.skip("could not materialize vendor/rubocop at v1.91.0 (no network access?)")

    class Args:
        pass

    args = Args()
    args.cop = "Style/TimeNow"
    args.cop_yml = yml_path
    args.rubocop_root = rubocop_root
    args.plugin_root = []
    args.spec = None
    args.rubocop_gem_dir = gem_dir_path
    args.nitrocop_bin = binary
    args.fixtures_dir = ROOT / "tests" / "fixtures" / "cops"
    args.target_ruby_versions = None
    args.autocorrect_loop_limit = 10
    args.min_no_offense_lines = 5
    args.skip_differential = False
    args.skip_cargo_test = False
    args.debug_build = False
    args.allow_unwired = False

    report = ir_verify.run_verify(args)
    print(report.to_markdown())

    by_name = {c.name: c for c in report.checks}
    assert by_name["validate_ir"].ok
    assert by_name["matcher_byte_equality"].ok
    assert by_name["embedded_freshness"].ok, by_name["embedded_freshness"].messages
    assert by_name["differential"].ok, by_name["differential"].messages
    assert by_name["autocorrect_convergence"].ok, by_name["autocorrect_convergence"].messages
    assert by_name["no_offense_silent"].ok, by_name["no_offense_silent"].messages
    assert by_name["cargo_test"].ok, by_name["cargo_test"].messages
    assert report.passed
