#!/usr/bin/env python3
"""Golden tests for scripts/workflows/ir_extract.py (Cop IR Stage 1: extract).

Covers 5 vendored cops of deliberately different shapes:
  - Style/HashExcept       — 0 matchers, mixin-delegated (HashSubset), unsafe autocorrect
  - Style/ClassCheck       — 0 matchers, MSG with %<x>s placeholders, EnforcedStyle enum config
  - Style/NegatedIf        — 0 matchers, EnforcedStyle enum with 3 values
  - Lint/DuplicateMethods  — 6 matchers, RESTRICT_ON_SEND, large (bucket C shape)
  - RSpec/ExpectChange     — 2 matchers, 2 MSG constants, EnforcedStyle enum, plugin gem
"""
from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
SCRIPT = ROOT / "scripts" / "workflows" / "ir_extract.py"
sys.path.insert(0, str(ROOT / "scripts"))
sys.path.insert(0, str(SCRIPT.parent))

_spec = importlib.util.spec_from_file_location("ir_extract", SCRIPT)
assert _spec and _spec.loader
ir_extract = importlib.util.module_from_spec(_spec)
sys.modules["ir_extract"] = ir_extract
_spec.loader.exec_module(ir_extract)

RUBOCOP_ROOT = ROOT / "vendor" / "rubocop"
RSPEC_ROOT = ROOT / "vendor" / "rubocop-rspec"


def extract(cop: str, extra_roots: list[Path] | None = None) -> dict:
    roots = [RUBOCOP_ROOT, *(extra_roots or [])]
    source_path, gem_root = ir_extract.find_cop_source(cop, roots)
    return ir_extract.build_extraction_record(cop, source_path, gem_root)


def test_hash_except_shape():
    record = extract("Style/HashExcept")
    assert record["cop"] == "Style/HashExcept"
    assert record["matchers"] == []
    assert set(record["mixins"]) == {"AutoCorrector", "HashSubset", "TargetRubyVersion"}
    assert record["extends_autocorrector"] is True
    # Safe: false, no explicit SafeAutoCorrect -> autocorrect defaults to unsafe.
    assert record["config"]["safe"] is False
    assert record["autocorrect_mode"] == "unsafe"
    assert record["config"]["enabled"] == "pending"
    assert record["config"]["version_added"] == "1.7"
    assert record["hooks"] == []  # behavior lives entirely in the HashSubset mixin


def test_class_check_shape():
    record = extract("Style/ClassCheck")
    assert record["matchers"] == []
    assert set(record["hooks"]) == {"on_send", "on_csend"}
    assert record["restrict_on_send"] == ["is_a?", "kind_of?"]
    assert record["messages"] == {
        "MSG": "Prefer `Object#%{prefer}` over `Object#%{current}`."
    }
    assert record["unsupported_message_formats"] == []
    options = record["config"]["options"]
    assert options["EnforcedStyle"] == {
        "type": "enum",
        "values": ["is_a?", "kind_of?"],
        "default": "is_a?",
    }
    assert record["config"]["enabled"] is True
    assert record["autocorrect_mode"] == "safe"  # AutoCorrector, Safe defaults true
    assert "EnforcedStyle" in record["cop_config_keys"]


def test_negated_if_shape():
    record = extract("Style/NegatedIf")
    assert record["matchers"] == []
    assert record["hooks"] == ["on_if"]
    options = record["config"]["options"]
    assert options["EnforcedStyle"]["type"] == "enum"
    assert options["EnforcedStyle"]["values"] == ["both", "prefix", "postfix"]
    assert options["EnforcedStyle"]["default"] == "both"
    assert record["config"]["version_added"] == "0.20"


def test_duplicate_methods_shape():
    record = extract("Lint/DuplicateMethods")
    names = {m["name"] for m in record["matchers"]}
    assert names == {
        "method_alias?",
        "alias_method?",
        "delegate_method?",
        "delegator?",
        "delegators?",
        "sym_name",
    }
    # False-positive guard: on_delegate/on_attr are cop-private helpers, not
    # real Parser-gem AST hooks, and must not appear in `hooks`.
    assert record["hooks"] == sorted({"on_send", "on_alias", "on_def", "on_defs"})
    assert set(record["restrict_on_send"]) == {
        "alias_method",
        "attr_reader",
        "attr_writer",
        "attr_accessor",
        "attr",
        "delegate",
        "def_delegator",
        "def_instance_delegator",
        "def_delegators",
        "def_instance_delegators",
    }
    assert record["messages"]["MSG"] == "Method `%{method}` is defined at both %{defined} and %{current}."
    assert record["mixins"] == []
    assert record["extends_autocorrector"] is False
    assert record["autocorrect_mode"] == "none"
    assert record["code_loc"] > 80  # confirmed bucket-C shape


def test_expect_change_shape():
    record = extract("RSpec/ExpectChange", extra_roots=[RSPEC_ROOT])
    names = {m["name"] for m in record["matchers"]}
    assert names == {"expect_change_with_arguments", "expect_change_with_block"}
    assert record["restrict_on_send"] == ["change"]
    assert record["messages"] == {
        "MSG_BLOCK": "Prefer `change(%{obj}, :%{attr})`.",
        "MSG_CALL": "Prefer `change { %{obj}.%{attr} }`.",
    }
    options = record["config"]["options"]
    assert options["EnforcedStyle"]["values"] == ["method_call", "block"]
    assert options["EnforcedStyle"]["default"] == "method_call"
    # SafeAutoCorrect: false in config/default.yml -> unsafe autocorrect.
    assert record["config"]["safe_autocorrect"] is False
    assert record["autocorrect_mode"] == "unsafe"


def test_skeleton_validates_for_all_five():
    import json

    schema = json.loads((ROOT / "scripts" / "shared" / "ir_schema.json").read_text())
    for cop, roots in [
        ("Style/HashExcept", []),
        ("Style/ClassCheck", []),
        ("Style/NegatedIf", []),
        ("Lint/DuplicateMethods", []),
        ("RSpec/ExpectChange", [RSPEC_ROOT]),
    ]:
        record = extract(cop, roots)
        doc = ir_extract.load_skeleton_doc(record["skeleton_yaml"])
        errors = ir_extract.validate_skeleton(doc, schema)
        assert errors == [], f"{cop}: {errors}"
        assert doc["cop"] == cop
        assert doc["schema"] == 1


def test_missing_cop_raises_extract_error():
    import pytest

    with pytest.raises(ir_extract.ExtractError):
        extract("Style/ThisCopDoesNotExist")
