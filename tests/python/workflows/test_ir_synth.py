#!/usr/bin/env python3
"""Tests for scripts/workflows/ir_synth.py (Cop IR Stage 3: synthesize).

No live API calls — `synthesize()` is exercised with a fake Anthropic client
whose `.messages.create` returns pre-recorded response text, per AGENTS.md's
"No `Mutex` ... " style of "record what you can, mock the network" testing
already used by `test_batch_dispatch.py` et al.

Deliberately hermetic: tests build a synthetic extraction record shaped like
`ir_extract.py`'s real output for `Style/TimeNow` (checked once against a
`v1.91.0` checkout of `vendor/rubocop` — see
`tests/python/workflows/test_ir_extract_upstream_pilot.py` for that
verification) rather than re-extracting from the vendored submodule, which is
pinned to an older RuboCop release (AGENTS.md: "Dual-Platform Development" /
the corpus bundle pin) and does not contain `Style/TimeNow`'s source at all.
Depending on the live checkout here would make most of this file silently
skip in ordinary CI runs.
"""
from __future__ import annotations

import importlib.util
import json
import re
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[3]
SCRIPT = ROOT / "scripts" / "workflows" / "ir_synth.py"
sys.path.insert(0, str(ROOT / "scripts"))
sys.path.insert(0, str(SCRIPT.parent))

_extract_spec = importlib.util.spec_from_file_location("ir_extract", SCRIPT.parent / "ir_extract.py")
ir_extract = importlib.util.module_from_spec(_extract_spec)
sys.modules["ir_extract"] = ir_extract
_extract_spec.loader.exec_module(ir_extract)

_synth_spec = importlib.util.spec_from_file_location("ir_synth", SCRIPT)
ir_synth = importlib.util.module_from_spec(_synth_spec)
sys.modules["ir_synth"] = ir_synth
_synth_spec.loader.exec_module(ir_synth)

PREDICATES_RS = ROOT / "src" / "node_pattern" / "predicates.rs"


def _time_now_record() -> dict:
    """A synthetic extraction record shaped exactly like
    `ir_extract.extract_one("Style/TimeNow", ...)`'s real output against
    `vendor/rubocop` @ `v1.91.0` (field-for-field, values taken from a real
    run against that tag)."""
    return {
        "cop": "Style/TimeNow",
        "source_path": "vendor/rubocop/lib/rubocop/cop/style/time_now.rb",
        "gem_root": "vendor/rubocop",
        "loc": 24,
        "code_loc": 12,
        "matchers": [
            {
                "name": "time_new?",
                "kind": "matcher",
                "pattern": "(call (const {nil? cbase} :Time) :new)",
                "n_captures": 0,
            }
        ],
        "restrict_on_send": ["new"],
        "messages": {"MSG": "Prefer `Time.now` over `Time.new` to retrieve the current time."},
        "unsupported_message_formats": [],
        "constants": {},
        "non_map_constants": {},
        "unparsed_constants": [],
        "hooks": ["on_csend", "on_send"],
        "mixins": ["AutoCorrector"],
        "uses_source_text": False,
        "extends_autocorrector": True,
        "cop_config_keys": [],
        "helper_predicates": [],
        "config": {
            "enabled": "pending",
            "version_added": "1.90",
            "version_changed": None,
            "safe": True,
            "safe_autocorrect": None,
            "options": {},
        },
        "autocorrect_mode": "safe",
        "body_after_matchers": (
            "module RuboCop\n  module Cop\n    module Style\n      class TimeNow < Base\n"
            "        extend AutoCorrector\n\n        MSG = 'Prefer `Time.now` over `Time.new` "
            "to retrieve the current time.'\n        RESTRICT_ON_SEND = %i[new].freeze\n\n"
            "        def on_send(node)\n          return unless time_new?(node)\n\n"
            "          add_offense(node) do |corrector|\n"
            "            corrector.replace(node.loc.selector.join(node.source_range.end), 'now')\n"
            "          end\n        end\n        alias on_csend on_send\n      end\n    end\n  end\nend\n"
        ),
    }


# --- known predicate whitelist stays in sync with the Rust registry --------


def test_known_predicates_matches_rust_registry():
    """Guards against KNOWN_PREDICATES silently drifting from
    src/node_pattern/predicates.rs, since there is no `--list-ir-predicates`
    flag yet to query it live (see module docstring)."""
    if not PREDICATES_RS.is_file():
        pytest.skip("src/node_pattern/predicates.rs not found")
    text = PREDICATES_RS.read_text()
    names = set(re.findall(r'name:\s*"([^"]+)"', text))
    # KNOWN_PREDICATES additionally carries "type?", which docs/COP_IR.md
    # documents as compiled directly rather than a registry entry.
    assert names <= ir_synth.KNOWN_PREDICATES
    assert names, "regex found no predicates — has predicates.rs's format changed?"


# --- normalize_matchers -----------------------------------------------------


def test_normalize_matchers_synthesizes_capture_names():
    record = {
        "config": {"options": {}},
        "constants": {},
        "matchers": [
            {"name": "redundant_minmax_by_block?", "pattern": "(block $(call _ _) (args (arg $_x)) (lvar _x))", "n_captures": 2, "captures": []},
        ],
    }
    normalized = ir_synth.normalize_matchers(record)
    assert len(normalized) == 1
    m = normalized[0]
    assert m.key == "redundant_minmax_by_block"
    assert m.captures == ["capture_1", "capture_2"]
    assert m.synthetic_captures is True


def test_normalize_matchers_preserves_given_captures():
    record = {
        "config": {"options": {}},
        "constants": {},
        "matchers": [
            {"name": "time_new?", "pattern": "(call (const {nil? cbase} :Time) :new)", "n_captures": 0, "captures": []},
        ],
    }
    normalized = ir_synth.normalize_matchers(record)
    assert normalized[0].captures == []
    assert normalized[0].synthetic_captures is False


def test_normalize_matchers_derives_params_from_declared_constants():
    record = {
        "config": {"options": {}},
        "constants": {"KIND_METHODS": {"is_a?": True}},
        "matchers": [
            {
                "name": "kind_check?",
                "pattern": "(send (lvar _) %KIND_METHODS _)",
                "n_captures": 0,
                "captures": [],
            }
        ],
    }
    normalized = ir_synth.normalize_matchers(record)
    assert normalized[0].params == ["KIND_METHODS"]


# --- schema construction forbids matchers/config/constants ------------------


def test_synth_output_schema_forbids_matchers_key():
    import jsonschema

    schema = ir_synth.build_synth_output_schema(ir_synth.load_ir_schema())
    validator = jsonschema.Draft202012Validator(schema)
    doc = {
        "hooks": [{"on": ["send"], "match": "x", "offense": {"location": "node", "message": "m"}}],
        "matchers": {"evil": {"pattern": "(send)"}},
    }
    errors = list(validator.iter_errors(doc))
    assert errors
    assert any("matchers" in e.message for e in errors)


def test_synth_output_schema_forbids_config_and_constants_keys():
    import jsonschema

    schema = ir_synth.build_synth_output_schema(ir_synth.load_ir_schema())
    validator = jsonschema.Draft202012Validator(schema)
    base_hooks = [{"on": ["send"], "match": "x", "offense": {"location": "node", "message": "m"}}]
    for bad_key in ("config", "constants", "predicates", "meta", "cop"):
        doc = {"hooks": base_hooks, bad_key: {}}
        errors = list(validator.iter_errors(doc))
        assert errors, f"expected {bad_key!r} to be rejected"


# --- validate_synth_doc whitelist checks -------------------------------------


@pytest.fixture
def time_now_context():
    record = _time_now_record()
    matchers = ir_synth.normalize_matchers(record)
    schema = ir_synth.build_synth_output_schema(ir_synth.load_ir_schema())
    return record, matchers, schema


def test_validate_synth_doc_accepts_faithful_translation(time_now_context):
    record, matchers, schema = time_now_context
    doc = {
        "hooks": [
            {
                "on": ["send", "csend"],
                "match": "time_new",
                "offense": {
                    "location": "node",
                    "message": "Prefer `Time.now` over `Time.new` to retrieve the current time.",
                    "correct": [
                        {
                            "op": "replace",
                            "range": {"start": "node.selector.start", "stop": "node.expression.stop"},
                            "text": "now",
                        }
                    ],
                },
            }
        ]
    }
    errors = ir_synth.validate_synth_doc(doc, schema, matchers, record)
    assert errors == []


def test_validate_synth_doc_rejects_matchers_key(time_now_context):
    """The task's explicit requirement: reject a response that touches `matchers:`."""
    record, matchers, schema = time_now_context
    doc = {
        "hooks": [{"on": ["send"], "match": "time_new", "offense": {"location": "node", "message": "m"}}],
        "matchers": {"time_new": {"pattern": "(send)"}},
    }
    errors = ir_synth.validate_synth_doc(doc, schema, matchers, record)
    assert errors
    assert any("matchers" in e for e in errors)


def test_validate_synth_doc_rejects_unknown_matcher(time_now_context):
    record, matchers, schema = time_now_context
    doc = {"hooks": [{"on": ["send"], "match": "no_such_matcher", "offense": {"location": "node", "message": "m"}}]}
    errors = ir_synth.validate_synth_doc(doc, schema, matchers, record)
    assert any("unknown matcher" in e for e in errors)


def test_validate_synth_doc_rejects_unknown_predicate(time_now_context):
    record, matchers, schema = time_now_context
    doc = {
        "hooks": [
            {
                "on": ["send"],
                "match": "time_new",
                "when": {"pred": ["node", "not_a_real_predicate?"]},
                "offense": {"location": "node", "message": "m"},
            }
        ]
    }
    errors = ir_synth.validate_synth_doc(doc, schema, matchers, record)
    assert any("unknown predicate" in e for e in errors)


def test_validate_synth_doc_rejects_bare_percent(time_now_context):
    record, matchers, schema = time_now_context
    doc = {
        "hooks": [
            {"on": ["send"], "match": "time_new", "offense": {"location": "node", "message": "100% sure"}}
        ]
    }
    errors = ir_synth.validate_synth_doc(doc, schema, matchers, record)
    assert any("bare '%'" in e for e in errors)


def test_validate_synth_doc_rejects_unresolved_placeholder(time_now_context):
    record, matchers, schema = time_now_context
    doc = {
        "hooks": [
            {
                "on": ["send"],
                "match": "time_new",
                "offense": {"location": "node", "message": "Use %{nonexistent}"},
            }
        ]
    }
    errors = ir_synth.validate_synth_doc(doc, schema, matchers, record)
    assert any("unresolved placeholder" in e for e in errors)


# --- render_cop_yaml is byte-faithful on matchers ---------------------------


def test_render_cop_yaml_keeps_matcher_pattern_verbatim(time_now_context):
    record, matchers, schema = time_now_context
    doc = {
        "hooks": [
            {
                "on": ["send", "csend"],
                "match": "time_new",
                "offense": {"location": "node", "message": "msg"},
            }
        ]
    }
    yaml_text = ir_synth.render_cop_yaml(record, matchers, doc)
    import yaml

    parsed = yaml.safe_load(yaml_text)
    original_pattern = record["matchers"][0]["pattern"]
    rendered_pattern = parsed["matchers"]["time_new"]["pattern"]
    assert rendered_pattern.strip("\n") == original_pattern.strip("\n")


def test_assert_verbatim_sections_passes_for_untouched_matchers(time_now_context):
    record, matchers, _schema = time_now_context
    ir_synth.assert_verbatim_sections(record, matchers)  # must not raise


def test_assert_verbatim_sections_raises_if_pattern_mutated(time_now_context):
    record, matchers, _schema = time_now_context
    matchers[0].pattern = "(send :mutated)"
    with pytest.raises(ir_synth.SynthError):
        ir_synth.assert_verbatim_sections(record, matchers)


# --- synthesize() end to end with a fake client -----------------------------


class _FakeTextBlock:
    def __init__(self, text: str):
        self.type = "text"
        self.text = text


class _FakeResponse:
    def __init__(self, text: str):
        self.content = [_FakeTextBlock(text)]


class _FakeMessages:
    def __init__(self, replies: list[str]):
        self._replies = list(replies)
        self.calls = []

    def create(self, **kwargs):
        self.calls.append(kwargs)
        text = self._replies.pop(0)
        return _FakeResponse(text)


class _FakeClient:
    def __init__(self, replies: list[str]):
        self.messages = _FakeMessages(replies)


_GOOD_TIME_NOW_RESPONSE = json.dumps(
    {
        "hooks": [
            {
                "on": ["send", "csend"],
                "match": "time_new",
                "offense": {
                    "location": "node",
                    "message": "Prefer `Time.now` over `Time.new` to retrieve the current time.",
                    "correct": [
                        {
                            "op": "replace",
                            "range": {"start": "node.selector.start", "stop": "node.expression.stop"},
                            "text": "now",
                        }
                    ],
                },
            }
        ]
    }
)


def test_synthesize_merges_a_recorded_response_correctly():
    record = _time_now_record()
    client = _FakeClient([_GOOD_TIME_NOW_RESPONSE])
    result = ir_synth.synthesize(
        record, model="claude-opus-5", max_retries=0, max_tokens=4096, spec_text=None, client=client
    )
    assert result.ok, result.errors
    assert result.attempts == 1
    assert "time_new" in result.yaml_text
    assert "Prefer `Time.now`" in result.yaml_text
    assert "matchers:" in result.yaml_text
    # The merged document must load through the real IR schema, not just this
    # script's own subset check.
    import yaml

    parsed = yaml.safe_load(result.yaml_text)
    assert parsed["cop"] == "Style/TimeNow"
    assert parsed["hooks"][0]["match"] == "time_new"


def test_synthesize_rejects_a_response_that_touches_matchers():
    """The task's explicit requirement, exercised through the full
    synthesize() orchestration (not just validate_synth_doc directly): a
    response that rewrites `matchers:` must never be merged into the output,
    and after exhausting retries the cop is reported as failed (needs_human),
    never silently accepted."""
    record = _time_now_record()
    bad_response = json.dumps(
        {
            "hooks": [
                {"on": ["send"], "match": "time_new", "offense": {"location": "node", "message": "m"}}
            ],
            "matchers": {"time_new": {"pattern": "(send :something_else)"}},
        }
    )
    client = _FakeClient([bad_response, bad_response])  # retry gets the same bad answer
    result = ir_synth.synthesize(
        record, model="claude-opus-5", max_retries=1, max_tokens=4096, spec_text=None, client=client
    )
    assert not result.ok
    assert result.yaml_text is None
    assert any("matchers" in e for e in result.errors)
    # Two attempts were made (initial + one retry), proving the retry loop ran
    # and still refused to accept the tainted response.
    assert len(client.messages.calls) == 2


def test_synthesize_recovers_after_one_invalid_attempt():
    record = _time_now_record()
    bad_response = json.dumps(
        {"hooks": [{"on": ["send"], "match": "no_such_matcher", "offense": {"location": "node", "message": "m"}}]}
    )
    client = _FakeClient([bad_response, _GOOD_TIME_NOW_RESPONSE])
    result = ir_synth.synthesize(
        record, model="claude-opus-5", max_retries=2, max_tokens=4096, spec_text=None, client=client
    )
    assert result.ok
    assert result.attempts == 2


def test_synthesize_handles_markdown_fenced_json():
    record = _time_now_record()
    fenced = "```json\n" + _GOOD_TIME_NOW_RESPONSE + "\n```"
    client = _FakeClient([fenced])
    result = ir_synth.synthesize(
        record, model="claude-opus-5", max_retries=0, max_tokens=4096, spec_text=None, client=client
    )
    assert result.ok


# --- resolve_api_client: no key -> clear error ------------------------------


def test_resolve_api_client_raises_without_credentials(monkeypatch):
    monkeypatch.delenv("ANTHROPIC_API_KEY", raising=False)
    monkeypatch.delenv("ANTHROPIC_AUTH_TOKEN", raising=False)
    with pytest.raises(ir_synth.SynthError, match="No Anthropic API credential"):
        ir_synth.resolve_api_client()


def test_main_exits_2_without_credentials(monkeypatch, capsys):
    # main() checks for API credentials before touching vendor/rubocop at
    # all, so this needs no real extraction source on disk.
    monkeypatch.delenv("ANTHROPIC_API_KEY", raising=False)
    monkeypatch.delenv("ANTHROPIC_AUTH_TOKEN", raising=False)
    exit_code = ir_synth.main(["Style/TimeNow"])
    assert exit_code == 2
    captured = capsys.readouterr()
    assert "No Anthropic API credential" in captured.err
