#!/usr/bin/env python3
"""Tests for scripts/workflows/ir_classify.py (Cop IR Stage 2: classify)."""
from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
EXTRACT_SCRIPT = ROOT / "scripts" / "workflows" / "ir_extract.py"
CLASSIFY_SCRIPT = ROOT / "scripts" / "workflows" / "ir_classify.py"
sys.path.insert(0, str(ROOT / "scripts"))
sys.path.insert(0, str(EXTRACT_SCRIPT.parent))


def _load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


ir_extract = _load("ir_extract", EXTRACT_SCRIPT)
ir_classify = _load("ir_classify", CLASSIFY_SCRIPT)

RUBOCOP_ROOT = ROOT / "vendor" / "rubocop"
RSPEC_ROOT = ROOT / "vendor" / "rubocop-rspec"


def classify(cop: str, extra_roots: list[Path] | None = None):
    return ir_classify.classify_one(cop, [RUBOCOP_ROOT, *(extra_roots or [])])


def test_hash_except_is_bucket_b_no_local_hooks():
    # 0 local matchers (behavior lives in the HashSubset mixin) -> not bucket A.
    result = classify("Style/HashExcept")
    assert result["bucket"] == "B"
    assert result["n_matchers"] == 0


def test_class_check_and_negated_if_are_bucket_b():
    for cop in ("Style/ClassCheck", "Style/NegatedIf"):
        result = classify(cop)
        assert result["bucket"] == "B", result
        assert result["n_matchers"] == 0


def test_duplicate_methods_is_bucket_c_for_size():
    result = classify("Lint/DuplicateMethods")
    assert result["bucket"] == "C"
    reasons = " ".join(result["reasons"])
    assert "code_loc" in reasons
    assert result["code_loc"] > 80


def test_expect_change_is_bucket_b():
    result = classify("RSpec/ExpectChange", extra_roots=[RSPEC_ROOT])
    assert result["bucket"] == "B"
    assert result["n_matchers"] == 2


def test_range_help_trivial_use_does_not_disqualify():
    # RedundantMinMaxBy `include`s RangeHelp but only calls the trivial
    # range_between(...) (a two-offset range, not token-stream stitching) —
    # design §1.5 uses exactly this cop as its own worked IR example, so
    # RangeHelp's mere presence must not force bucket C.
    source = (
        "class Foo < Base\n"
        "  include RangeHelp\n"
        "  def bar(node)\n"
        "    range_between(node.loc.selector.begin_pos, node.loc.end.end_pos)\n"
        "  end\n"
        "end\n"
    )
    record = {"code_loc": 10, "mixins": ["RangeHelp"], "hooks": [], "matchers": [{"name": "x"}]}
    bucket, reasons = ir_classify.classify_record(record, source)
    assert bucket == "A", reasons


def test_range_help_stitching_use_disqualifies():
    source = (
        "class Foo < Base\n"
        "  include RangeHelp\n"
        "  def bar(node)\n"
        "    range_with_surrounding_space(node.source_range)\n"
        "  end\n"
        "end\n"
    )
    record = {"code_loc": 10, "mixins": ["RangeHelp"], "hooks": [], "matchers": [{"name": "x"}]}
    bucket, reasons = ir_classify.classify_record(record, source)
    assert bucket == "C"
    assert any("RangeHelp" in r for r in reasons)


def test_source_range_accessor_alone_is_not_a_file_level_marker():
    # `node.source_range` is an ordinary per-node accessor (design's own
    # Style/TimeNow example calls it), not evidence of reading the file-level
    # token/comment stream — must not trip the hard disqualifier on its own.
    source = "def on_send(node)\n  corrector.replace(node.source_range, 'x')\nend\n"
    record = {"code_loc": 5, "mixins": [], "hooks": [], "matchers": [{"name": "x"}]}
    bucket, reasons = ir_classify.classify_record(record, source)
    assert bucket == "A", reasons


def test_project_index_help_mixin_forces_bucket_c():
    source = "class Foo < Base\n  include ProjectIndexHelp\nend\n"
    record = {"code_loc": 5, "mixins": ["ProjectIndexHelp"], "hooks": [], "matchers": []}
    bucket, reasons = ir_classify.classify_record(record, source)
    assert bucket == "C"


def test_write_csv_round_trip(tmp_path):
    result = classify("Style/ClassCheck")
    out = tmp_path / "out.csv"
    ir_classify.write_csv([result], out)
    text = out.read_text()
    assert "Style/ClassCheck" in text
    assert text.splitlines()[0].split(",")[0] == "cop"
