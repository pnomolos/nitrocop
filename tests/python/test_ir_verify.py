#!/usr/bin/env python3
"""Tests for scripts/ir_verify.py (Cop IR Stage 5: verify — the gate).

No real `rubocop`/`nitrocop` invocations here: the RuboCop/nitrocop
invocation layer (`run_real_rubocop_json`, `run_nitrocop_json`) is monkeypatched
throughout, per the task's "the RuboCop-invocation layer mocked" requirement.
An opt-in live end-to-end test lives in `test_ir_verify_live.py`
(`IR_VERIFY_LIVE=1`).
"""
from __future__ import annotations

import importlib.util
import sys
import textwrap
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "ir_verify.py"
sys.path.insert(0, str(ROOT / "scripts"))
sys.path.insert(0, str(ROOT / "scripts" / "workflows"))

_spec = importlib.util.spec_from_file_location("ir_verify", SCRIPT)
ir_verify = importlib.util.module_from_spec(_spec)
sys.modules["ir_verify"] = ir_verify
_spec.loader.exec_module(ir_verify)


# --- cop_mod_name / fixture_dir_for -----------------------------------------


def test_cop_mod_name():
    assert ir_verify.cop_mod_name("Style/TimeNow") == "style_time_now"
    assert ir_verify.cop_mod_name("Lint/DataDefineOverride") == "lint_data_define_override"


def test_fixture_dir_for(tmp_path):
    result = ir_verify.fixture_dir_for("Style/TimeNow", tmp_path)
    assert result == tmp_path / "style" / "time_now"


# --- (b) matcher byte-equality ------------------------------------------------

TIME_NOW_YAML = """\
schema: 1
cop: "Style/TimeNow"
autocorrect: safe
matchers:
  time_new:
    pattern: |
      (call (const {nil? cbase} :Time) :new)
hooks:
  - on: [send, csend]
    match: time_new
    offense:
      location: node
      message: "Prefer `Time.now` over `Time.new` to retrieve the current time."
"""

TIME_NOW_SOURCE = textwrap.dedent(
    '''\
    module RuboCop
      module Cop
        module Style
          class TimeNow < Base
            extend AutoCorrector

            MSG = "Prefer `Time.now` over `Time.new` to retrieve the current time."
            RESTRICT_ON_SEND = %i[new].freeze

            def_node_matcher :time_new?, <<~PATTERN
              (call (const {nil? cbase} :Time) :new)
            PATTERN

            def on_send(node)
              return unless time_new?(node)

              add_offense(node) do |corrector|
                corrector.replace(node.loc.selector.join(node.source_range.end), "now")
              end
            end
            alias on_csend on_send
          end
        end
      end
    end
    '''
)


def _write_upstream_source(tmp_path: Path) -> Path:
    src_dir = tmp_path / "lib" / "rubocop" / "cop" / "style"
    src_dir.mkdir(parents=True)
    path = src_dir / "time_now.rb"
    path.write_text(TIME_NOW_SOURCE)
    return tmp_path


def test_check_matcher_byte_equality_passes_for_verbatim_matcher(tmp_path):
    import yaml

    root = _write_upstream_source(tmp_path)
    doc = yaml.safe_load(TIME_NOW_YAML)
    result = ir_verify.check_matcher_byte_equality(doc, TIME_NOW_YAML, "Style/TimeNow", [root])
    assert result.ok
    assert result.hard_fail


def test_check_matcher_byte_equality_catches_mutated_pattern(tmp_path):
    import yaml

    root = _write_upstream_source(tmp_path)
    mutated_yaml = TIME_NOW_YAML.replace(":Time) :new)", ":Time) :mutated)")
    doc = yaml.safe_load(mutated_yaml)
    result = ir_verify.check_matcher_byte_equality(doc, mutated_yaml, "Style/TimeNow", [root])
    assert not result.ok
    assert result.hard_fail
    assert any("differs from upstream" in m for m in result.messages)


def test_check_matcher_byte_equality_allows_deviation_comment(tmp_path):
    import yaml

    root = _write_upstream_source(tmp_path)
    mutated_yaml = TIME_NOW_YAML.replace(
        "  time_new:\n    pattern: |",
        "  time_new:\n    # deviation: testing\n    pattern: |",
    ).replace(":Time) :new)", ":Time) :mutated)")
    doc = yaml.safe_load(mutated_yaml)
    result = ir_verify.check_matcher_byte_equality(doc, mutated_yaml, "Style/TimeNow", [root])
    assert result.ok
    assert any("deviation" in m for m in result.messages)


def test_check_matcher_byte_equality_whitespace_only_is_soft(tmp_path):
    import yaml

    root = _write_upstream_source(tmp_path)
    # Trailing whitespace on the pattern line differs, tokens are identical —
    # a real byte difference (so YAML's block-scalar dedent doesn't erase it
    # before this check ever sees it), but not a token-level one.
    reindented = TIME_NOW_YAML.replace(
        "      (call (const {nil? cbase} :Time) :new)",
        "      (call (const {nil? cbase} :Time) :new)   ",
    )
    doc = yaml.safe_load(reindented)
    result = ir_verify.check_matcher_byte_equality(doc, reindented, "Style/TimeNow", [root])
    assert result.ok  # whitespace-only difference does not hard-fail
    assert any("whitespace" in m for m in result.messages)


def test_check_matcher_byte_equality_skips_when_no_upstream_source(tmp_path):
    import yaml

    doc = yaml.safe_load(TIME_NOW_YAML)
    result = ir_verify.check_matcher_byte_equality(doc, TIME_NOW_YAML, "Style/DoesNotExist", [tmp_path])
    assert not result.ok
    assert not result.hard_fail  # informational — cannot verify, not "verified and wrong"


# --- offense-set parsing / diffing -------------------------------------------


def test_rubocop_offense_set_parses_and_converts_columns_and_severity():
    data = {
        "files": [
            {
                "path": "/tmp/a.rb",
                "offenses": [
                    {
                        "severity": "convention",
                        "message": "Prefer `Time.now`.",
                        "location": {"line": 1, "column": 1, "length": 8},
                    }
                ],
            }
        ]
    }
    result = ir_verify._rubocop_offense_set(data)
    assert result == frozenset({(1, 0, "Prefer `Time.now`.", "C")})


def test_nitrocop_offense_set_parses():
    data = {"offenses": [{"line": 1, "column": 0, "message": "Prefer `Time.now`.", "severity": "C"}]}
    result = ir_verify._nitrocop_offense_set(data)
    assert result == frozenset({(1, 0, "Prefer `Time.now`.", "C")})


def test_uses_it_or_numbered_params_detection():
    assert ir_verify._uses_it_or_numbered_params("arr.map { it.to_s }")
    assert ir_verify._uses_it_or_numbered_params("arr.map { _1.to_s }")
    assert not ir_verify._uses_it_or_numbered_params("arr.map { |x| x.to_s }")


# --- (c) check_differential set-diff logic on canned JSON --------------------

MINI_SPEC = textwrap.dedent(
    """\
    # frozen_string_literal: true

    RSpec.describe RuboCop::Cop::Style::TimeNow, :config do
      it 'registers an offense' do
        expect_offense(<<~RUBY)
          Time.new
          ^^^^^^^^ Prefer `Time.now` over `Time.new` to retrieve the current time.
        RUBY

        expect_correction(<<~RUBY)
          Time.now
        RUBY
      end
    end
    """
)


def test_check_differential_passes_when_sets_match(monkeypatch):
    matching = {
        "files": [
            {
                "offenses": [
                    {
                        "severity": "convention",
                        "message": "Prefer `Time.now` over `Time.new` to retrieve the current time.",
                        "location": {"line": 1, "column": 1, "length": 8},
                    }
                ]
            }
        ]
    }
    matching_nitrocop = {
        "offenses": [
            {
                "line": 1,
                "column": 0,
                "message": "Prefer `Time.now` over `Time.new` to retrieve the current time.",
                "severity": "C",
            }
        ]
    }
    monkeypatch.setattr(ir_verify, "run_real_rubocop_json", lambda *a, **k: dict(matching))
    monkeypatch.setattr(ir_verify, "run_nitrocop_json", lambda *a, **k: dict(matching_nitrocop))

    result = ir_verify.check_differential(
        "Style/TimeNow", Path("/fake/gem/dir"), "fake-nitrocop", MINI_SPEC, [4.0, 3.4]
    )
    assert result.ok
    assert result.hard_fail
    assert result.details["examples_checked"] == 1


def test_check_differential_hard_fails_on_persistent_mismatch(monkeypatch):
    rb_data = {
        "files": [
            {
                "offenses": [
                    {
                        "severity": "convention",
                        "message": "Prefer `Time.now` over `Time.new` to retrieve the current time.",
                        "location": {"line": 1, "column": 1, "length": 8},
                    }
                ]
            }
        ]
    }
    nc_data = {"offenses": []}  # nitrocop found nothing at every version
    monkeypatch.setattr(ir_verify, "run_real_rubocop_json", lambda *a, **k: dict(rb_data))
    monkeypatch.setattr(ir_verify, "run_nitrocop_json", lambda *a, **k: dict(nc_data))

    result = ir_verify.check_differential(
        "Style/TimeNow", Path("/fake/gem/dir"), "fake-nitrocop", MINI_SPEC, [4.0, 3.4]
    )
    assert not result.ok
    assert result.hard_fail
    assert any("rubocop-only" in m for m in result.messages)


def test_check_differential_reports_it_block_diff_as_informational(monkeypatch):
    spec_text = textwrap.dedent(
        """\
        # frozen_string_literal: true

        RSpec.describe RuboCop::Cop::Style::RedundantMinMaxBy, :config do
          it 'registers an offense for max_by { it }' do
            expect_offense(<<~RUBY)
              arr.max_by { it }
              ^^^^^^^^^^^^^^^^^ Use `max` instead of `max_by { it }`.
            RUBY
          end
        end
        """
    )
    match = {
        "files": [
            {
                "offenses": [
                    {
                        "severity": "convention",
                        "message": "Use `max` instead of `max_by { it }`.",
                        "location": {"line": 1, "column": 1, "length": 17},
                    }
                ]
            }
        ]
    }
    no_match_nitrocop = {"offenses": []}
    match_nitrocop = {
        "offenses": [
            {"line": 1, "column": 0, "message": "Use `max` instead of `max_by { it }`.", "severity": "C"}
        ]
    }

    calls = {"n": 0}

    def fake_nitrocop(nitrocop_bin, cop, source, target_ruby, extra_config=None, extra_args=None):
        # Fails to match at the lower (default) version, matches at 3.4 —
        # simulating nitrocop's parser gap for `it`-blocks at older targets.
        calls["n"] += 1
        return dict(match_nitrocop) if target_ruby == 3.4 else dict(no_match_nitrocop)

    monkeypatch.setattr(ir_verify, "run_real_rubocop_json", lambda *a, **k: dict(match))
    monkeypatch.setattr(ir_verify, "run_nitrocop_json", fake_nitrocop)

    result = ir_verify.check_differential(
        "Style/RedundantMinMaxBy", Path("/fake/gem/dir"), "fake-nitrocop", spec_text, [3.0, 3.4]
    )
    assert result.ok  # informational-only diff at the lower version must not hard-fail
    assert any("informational only" in m for m in result.messages)


def test_check_differential_no_spec_is_informational():
    result = ir_verify.check_differential("Style/TimeNow", Path("/x"), "nitrocop", None, [3.4])
    assert not result.ok
    assert not result.hard_fail


# --- (d) cargo test -----------------------------------------------------------


class _FakeCompletedProcess:
    def __init__(self, returncode, stdout):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = ""


def test_check_cargo_test_detects_unwired_cop(monkeypatch):
    monkeypatch.setattr(
        ir_verify.subprocess, "run",
        lambda *a, **k: _FakeCompletedProcess(0, "running 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored\n"),
    )
    result = ir_verify.check_cargo_test("Style/NoSuchCop", release=True, allow_unwired=False)
    assert not result.ok
    assert result.hard_fail
    assert "not yet wired" in result.messages[0]


def test_check_cargo_test_allow_unwired_downgrades_to_pass(monkeypatch):
    monkeypatch.setattr(
        ir_verify.subprocess, "run",
        lambda *a, **k: _FakeCompletedProcess(0, "test result: ok. 0 passed; 0 failed; 0 ignored\n"),
    )
    result = ir_verify.check_cargo_test("Style/NoSuchCop", release=True, allow_unwired=True)
    assert result.ok


def test_check_cargo_test_passes_when_tests_pass(monkeypatch):
    monkeypatch.setattr(
        ir_verify.subprocess, "run",
        lambda *a, **k: _FakeCompletedProcess(0, "test result: ok. 4 passed; 0 failed; 0 ignored\n"),
    )
    result = ir_verify.check_cargo_test("Style/TimeNow", release=True, allow_unwired=False)
    assert result.ok


def test_check_cargo_test_fails_when_tests_fail(monkeypatch):
    monkeypatch.setattr(
        ir_verify.subprocess, "run",
        lambda *a, **k: _FakeCompletedProcess(101, "test result: FAILED. 3 passed; 1 failed; 0 ignored\n"),
    )
    result = ir_verify.check_cargo_test("Style/TimeNow", release=True, allow_unwired=False)
    assert not result.ok


# --- (f) no_offense silence ----------------------------------------------


def test_check_no_offense_silent_passes_when_clean(tmp_path, monkeypatch):
    fixtures_dir = tmp_path / "style" / "time_now"
    fixtures_dir.mkdir(parents=True)
    (fixtures_dir / "no_offense.rb").write_text("Time.now\nTime.new(2024)\n")
    monkeypatch.setattr(ir_verify, "run_real_rubocop_json", lambda *a, **k: {"files": [{"offenses": []}]})
    result = ir_verify.check_no_offense_silent("Style/TimeNow", fixtures_dir, Path("/fake"), 3.4)
    assert result.ok


def test_check_no_offense_silent_fails_when_rubocop_finds_an_offense(tmp_path, monkeypatch):
    fixtures_dir = tmp_path / "style" / "time_now"
    fixtures_dir.mkdir(parents=True)
    (fixtures_dir / "no_offense.rb").write_text("Time.new\n")
    data = {
        "files": [
            {
                "offenses": [
                    {
                        "severity": "convention",
                        "message": "boom",
                        "location": {"line": 1, "column": 1, "length": 8},
                    }
                ]
            }
        ]
    }
    monkeypatch.setattr(ir_verify, "run_real_rubocop_json", lambda *a, **k: dict(data))
    result = ir_verify.check_no_offense_silent("Style/TimeNow", fixtures_dir, Path("/fake"), 3.4)
    assert not result.ok
    assert result.hard_fail


def test_check_no_offense_silent_informational_when_no_fixtures(tmp_path):
    fixtures_dir = tmp_path / "style" / "time_now"
    fixtures_dir.mkdir(parents=True)
    result = ir_verify.check_no_offense_silent("Style/TimeNow", fixtures_dir, Path("/fake"), 3.4)
    assert not result.ok
    assert not result.hard_fail


# --- (g) fixture coverage floor -----------------------------------------------


def test_check_fixture_coverage_passes_above_floor(tmp_path):
    (tmp_path).mkdir(exist_ok=True)
    (tmp_path / "no_offense.rb").write_text("\n".join(f"line_{i}" for i in range(6)))
    result = ir_verify.check_fixture_coverage(tmp_path, min_lines=5)
    assert result.ok


def test_check_fixture_coverage_fails_below_floor(tmp_path):
    (tmp_path / "no_offense.rb").write_text("only_one_line\n")
    result = ir_verify.check_fixture_coverage(tmp_path, min_lines=5)
    assert not result.ok
    assert result.hard_fail


def test_check_fixture_coverage_fails_when_missing():
    result = ir_verify.check_fixture_coverage(Path("/definitely/does/not/exist"), min_lines=5)
    assert not result.ok
    assert result.hard_fail


# --- embedded_freshness --------------------------------------------------


def test_embedded_freshness_fails_when_not_embedded(tmp_path, monkeypatch):
    monkeypatch.setattr(ir_verify, "embedded_source_path", lambda cop: tmp_path / "nope.cop.yml")
    candidate = tmp_path / "candidate.cop.yml"
    candidate.write_text("schema: 1\n")
    result = ir_verify.check_embedded_freshness("Style/Fake", candidate, "fake-bin")
    assert not result.ok
    assert result.hard_fail
    assert "no embedded document" in result.messages[0]


def test_embedded_freshness_fails_when_content_differs(tmp_path, monkeypatch):
    embedded = tmp_path / "embedded.cop.yml"
    embedded.write_text("schema: 1\ncop: X\n")
    monkeypatch.setattr(ir_verify, "embedded_source_path", lambda cop: embedded)
    candidate = tmp_path / "candidate.cop.yml"
    candidate.write_text("schema: 1\ncop: Y\n")
    result = ir_verify.check_embedded_freshness("Style/Fake", candidate, "fake-bin")
    assert not result.ok
    assert result.hard_fail


def test_embedded_freshness_passes_for_identical_content(tmp_path, monkeypatch):
    embedded = tmp_path / "embedded.cop.yml"
    embedded.write_text("schema: 1\ncop: X\n")
    monkeypatch.setattr(ir_verify, "embedded_source_path", lambda cop: embedded)
    result = ir_verify.check_embedded_freshness("Style/Fake", embedded, "/no/such/binary")
    assert result.ok


# --- VerifyReport -------------------------------------------------------------


def test_verify_report_passed_ignores_informational_failures():
    report = ir_verify.VerifyReport(cop="Style/TimeNow", yml_path="x.yml")
    report.checks.append(ir_verify.CheckResult("hard_ok", True, hard_fail=True))
    report.checks.append(ir_verify.CheckResult("info_fail", False, hard_fail=False))
    assert report.passed


def test_verify_report_passed_false_on_hard_failure():
    report = ir_verify.VerifyReport(cop="Style/TimeNow", yml_path="x.yml")
    report.checks.append(ir_verify.CheckResult("hard_fail", False, hard_fail=True))
    assert not report.passed


def test_verify_report_to_markdown_contains_table():
    report = ir_verify.VerifyReport(cop="Style/TimeNow", yml_path="x.yml")
    report.checks.append(ir_verify.CheckResult("validate_ir", True, hard_fail=True))
    md = report.to_markdown()
    assert "Style/TimeNow" in md
    assert "validate_ir" in md
    assert "PASS" in md
