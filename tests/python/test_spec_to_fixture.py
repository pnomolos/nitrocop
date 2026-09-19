#!/usr/bin/env python3
"""Golden tests for scripts/spec_to_fixture.py (Cop IR Stage 4: spec -> fixture).

Covers 5 real vendored RuboCop specs of deliberately different shapes:
  - Style/HashExcept       — deeply nested contexts, shared_examples +
                             it_behaves_like, AllCops-driven config variants,
                             :unsupported_on => :prism contexts (all skipped)
  - Style/ClassCheck       — context + let(:cop_config) EnforcedStyle variants,
                             every example has a paired expect_correction
  - Style/NegatedIf        — custom subject(:cop) override (whole file skipped)
  - Lint/DuplicateMethods  — shared_examples + it_behaves_like (skipped), plus
                             real top-level examples with custom `file`
                             arguments (-> offense/<scenario>.rb)
  - RSpec/ExpectChange     — let(:cop_config) referencing a nested
                             let(:enforced_style) (one-level indirection)

Golden output lives under tests/python/testdata/spec_to_fixture/<slug>/ (kept
well away from tests/fixtures/cops/ — 4 of these 5 cops already have real,
Rust-wired hand-written fixtures there for their *existing* implementations;
this script's golden output is a from-spec re-derivation for the IR pipeline
and must never overwrite those).

Also round-trips every golden fixture through a from-scratch Python port of
`src/testutil.rs`'s own fixture parser (scripts/shared/nitrocop_fixture.py)
to confirm the annotation format, `# nitrocop-config:` directives, and
`# nitrocop-filename:` directives are all byte-for-byte what nitrocop's Rust
test harness expects.
"""
from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "spec_to_fixture.py"
GOLDEN_ROOT = Path(__file__).resolve().parent / "testdata" / "spec_to_fixture"
sys.path.insert(0, str(ROOT / "scripts"))

_spec = importlib.util.spec_from_file_location("spec_to_fixture", SCRIPT)
assert _spec and _spec.loader
s2f = importlib.util.module_from_spec(_spec)
sys.modules["spec_to_fixture"] = s2f
_spec.loader.exec_module(s2f)

from shared import nitrocop_fixture as nf  # noqa: E402

CASES = [
    ("vendor/rubocop/spec/rubocop/cop/style/hash_except_spec.rb", "style_hash_except", "Style/HashExcept"),
    ("vendor/rubocop/spec/rubocop/cop/style/class_check_spec.rb", "style_class_check", "Style/ClassCheck"),
    ("vendor/rubocop/spec/rubocop/cop/style/negated_if_spec.rb", "style_negated_if", "Style/NegatedIf"),
    (
        "vendor/rubocop/spec/rubocop/cop/lint/duplicate_methods_spec.rb",
        "lint_duplicate_methods",
        "Lint/DuplicateMethods",
    ),
    (
        "vendor/rubocop-rspec/spec/rubocop/cop/rspec/expect_change_spec.rb",
        "rspec_expect_change",
        "RSpec/ExpectChange",
    ),
]


def _convert(spec_rel_path: str, out_dir: Path):
    text = (ROOT / spec_rel_path).read_text(encoding="utf-8")
    examples, skips = s2f.convert_spec(text)
    cop_name = s2f.find_cop_name(text)
    fixtures = s2f.assemble_fixtures(examples)
    fixture_dir = out_dir / s2f.fixture_dir_for(cop_name)
    written = s2f.write_fixtures(fixtures, fixture_dir, dry_run=False)
    return cop_name, examples, skips, written


def _golden_files(slug: str) -> dict[str, str]:
    base = GOLDEN_ROOT / slug
    result = {}
    for path in base.rglob("*"):
        if path.is_file() and path.name != "CONVERSION_LOG.txt":
            result[str(path.relative_to(base))] = path.read_text()
    return result


def test_golden_dataset_exists():
    for _spec_path, slug, _cop in CASES:
        assert (GOLDEN_ROOT / slug).is_dir(), f"missing golden dir for {slug}"


def test_regenerated_output_matches_golden(tmp_path):
    for spec_path, slug, cop_name in CASES:
        out_dir = tmp_path / slug
        got_cop, _examples, _skips, _written = _convert(spec_path, out_dir)
        assert got_cop == cop_name

        golden = _golden_files(slug)
        actual = {}
        for path in out_dir.rglob("*"):
            if path.is_file():
                actual[str(path.relative_to(out_dir))] = path.read_text()

        assert set(actual) == set(golden), f"{slug}: file set differs"
        for rel, expected_text in golden.items():
            assert actual[rel] == expected_text, f"{slug}/{rel} content differs"


def test_negated_if_is_entirely_skipped():
    text = (ROOT / "vendor/rubocop/spec/rubocop/cop/style/negated_if_spec.rb").read_text()
    examples, skips = s2f.convert_spec(text)
    assert examples == []
    assert len(skips) == 12
    assert all("custom subject(:cop)" in s.reason for s in skips)


def test_duplicate_methods_skips_shared_examples_and_splits_scenarios():
    text = (ROOT / "vendor/rubocop/spec/rubocop/cop/lint/duplicate_methods_spec.rb").read_text()
    examples, skips = s2f.convert_spec(text)
    assert len(skips) == 64
    assert all("shared_examples" in s.reason for s in skips)
    # 6 `offense` examples with a custom filename -> 6 offense/<scenario>.rb
    # files (see assemble_fixtures); a 7th, `no_offense` with a filename, is
    # folded into the aggregate no_offense.rb instead (dropping the filename
    # — see spec_to_fixture.py's module docstring on why no_offense scenarios
    # aren't split the way offense ones are).
    filenamed_offense = [e for e in examples if e.filename and e.kind == "offense"]
    assert len(filenamed_offense) == 6
    # 'src.rb' examples exist in the spec too, but only inside the
    # `%w[class module].each do |type|` block (dynamic, skipped above).
    assert {e.filename for e in filenamed_offense} == {"toplevel.rb", "test.rb"}
    assert sum(1 for e in examples if e.filename and e.kind == "no_offense") == 1


def test_hash_except_skips_allcops_variants_and_prism_tag():
    text = (ROOT / "vendor/rubocop/spec/rubocop/cop/style/hash_except_spec.rb").read_text()
    examples, skips = s2f.convert_spec(text)
    reasons = [s.reason for s in skips]
    assert any("AllCops" in r for r in reasons)
    assert any("unsupported_on: :prism" in r for r in reasons)
    assert len(examples) > 0  # the non-shared, non-AllCops-variant examples still convert


def test_expect_change_resolves_nested_let_indirection():
    text = (
        ROOT / "vendor/rubocop-rspec/spec/rubocop/cop/rspec/expect_change_spec.rb"
    ).read_text()
    examples, skips = s2f.convert_spec(text)
    assert skips == []
    configs = {tuple(sorted(e.config.items())) for e in examples}
    assert (("EnforcedStyle", "method_call"),) in configs
    assert (("EnforcedStyle", "block"),) in configs


def test_class_check_every_example_has_a_correction():
    text = (ROOT / "vendor/rubocop/spec/rubocop/cop/style/class_check_spec.rb").read_text()
    examples, skips = s2f.convert_spec(text)
    assert skips == []
    assert len(examples) == 4
    assert all(e.corrected_body is not None for e in examples)


# --- round-trip through the nitrocop fixture parser --------------------------


def test_golden_fixtures_round_trip_through_nitrocop_parser():
    for _spec_path, slug, cop_name in CASES:
        for rel, text in _golden_files(slug).items():
            name = Path(rel).name
            if name.startswith("offense.") or name == "offense.rb" or Path(rel).parent.name == "offense":
                parsed = nf.parse_fixture(text)
                assert parsed.expected, f"{slug}/{rel}: expected at least one offense"
                for offense in parsed.expected:
                    assert offense.cop_name == cop_name, f"{slug}/{rel}: {offense}"
                    assert offense.message  # never empty
                # The annotation-stripped source must still be non-empty Ruby.
                assert parsed.source.strip()
            elif name.startswith("no_offense.") or name == "no_offense.rb":
                parsed = nf.parse_fixture(text)
                assert parsed.expected == []
            elif name.startswith("corrected."):
                # corrected.<variant>.rb carries the same # nitrocop-config:
                # directive as its offense.<variant>.rb sibling (no macro
                # consumes it yet — see the hand-off notes) — must still
                # parse cleanly as a variant fixture.
                if "# nitrocop-config:" in text.splitlines()[0]:
                    config, source = nf.parse_variant_fixture(text)
                    assert config
                    assert source.strip()


def test_variant_directive_round_trips_to_the_right_config():
    golden = _golden_files("style_class_check")
    is_a = golden["style/class_check/offense.is_a.rb"]
    config, source = nf.parse_variant_fixture(is_a)
    assert config == {"EnforcedStyle": "is_a?"}
    assert "kind_of?" in source

    kind_of = golden["style/class_check/offense.kind_of.rb"]
    config2, source2 = nf.parse_variant_fixture(kind_of)
    assert config2 == {"EnforcedStyle": "kind_of?"}
    assert "is_a?" in source2


def test_scenario_fixture_carries_its_own_filename_directive():
    golden = _golden_files("lint_duplicate_methods")
    scenario = golden["lint/duplicate_methods/offense/scenario_1.rb"]
    parsed = nf.parse_fixture(scenario)
    assert parsed.filename is not None
    assert parsed.expected
    for offense in parsed.expected:
        assert parsed.filename in offense.message
