#!/usr/bin/env python3
"""Extract + classify the 23 net-new upstream cops from docs/planning/01-gap-analysis.md §4
(on the `planning/program-status` branch) against real rubocop v1.91.0 / rubocop-rspec
v3.10.2 sources materialized from git — not the older vendored submodule pin.

This is deliberately a *bump-independent* check: it does not touch `vendor/`,
does not require the submodules to be bumped, and works whether or not this
branch has landed the rubocop 1.91.0 bump the design doc calls a prerequisite
"handled elsewhere". It fetches tags into the existing vendor/rubocop(-rspec)
git checkouts (no working tree changes — `git show <tag>:<path>`, exactly
docs/planning/01-gap-analysis.md's own method) and materializes just the 23
cop sources + each gem's config/default.yml into a temp root.

Asserts all 23 extract without error and prints each one's A/B/C bucket.
"""
from __future__ import annotations

import importlib.util
import subprocess
import sys
from pathlib import Path

import pytest

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

RUBOCOP_TAG = "v1.91.0"
RSPEC_TAG = "v3.10.2"

# docs/planning/01-gap-analysis.md §4: 21 net-new rubocop cops + 2 rubocop-rspec cops.
PILOT_COPS = [
    ("Lint/ArgumentMismatch", "lint/argument_mismatch"),
    ("Lint/DataDefineOverride", "lint/data_define_override"),
    ("Lint/DeprecatedReference", "lint/deprecated_reference"),
    ("Lint/MisplacedMagicComment", "lint/misplaced_magic_comment"),
    ("Lint/NameTypo", "lint/name_typo"),
    ("Lint/SuperArgumentMismatch", "lint/super_argument_mismatch"),
    ("Lint/UnreachablePatternBranch", "lint/unreachable_pattern_branch"),
    ("Lint/UnusedPrivateMethod", "lint/unused_private_method"),
    ("Style/DirectiveScope", "style/directive_scope"),
    ("Style/FileOpen", "style/file_open"),
    ("Style/MapJoin", "style/map_join"),
    ("Style/OneClassPerFile", "style/one_class_per_file"),
    ("Style/PartitionInsteadOfDoubleSelect", "style/partition_instead_of_double_select"),
    ("Style/PredicateWithKind", "style/predicate_with_kind"),
    ("Style/ReduceToHash", "style/reduce_to_hash"),
    ("Style/RedundantMinMaxBy", "style/redundant_min_max_by"),
    ("Style/RedundantStructKeywordInit", "style/redundant_struct_keyword_init"),
    ("Style/SelectByKind", "style/select_by_kind"),
    ("Style/SelectByRange", "style/select_by_range"),
    ("Style/TallyMethod", "style/tally_method"),
    ("Style/TimeNow", "style/time_now"),
]
PILOT_RSPEC_COPS = [
    ("RSpec/DiscardedMatcher", "rspec/discarded_matcher"),
    ("RSpec/MatchWithSimpleRegex", "rspec/match_with_simple_regex"),
]


def _git_show(repo: Path, tag: str, path: str) -> bytes | None:
    result = subprocess.run(
        ["git", "-C", str(repo), "show", f"{tag}:{path}"], capture_output=True
    )
    return result.stdout if result.returncode == 0 else None


def _tag_exists(repo: Path, tag: str) -> bool:
    result = subprocess.run(
        ["git", "-C", str(repo), "rev-parse", "--verify", "--quiet", f"{tag}^{{commit}}"],
        capture_output=True,
    )
    return result.returncode == 0


@pytest.fixture(scope="module")
def pilot_root(tmp_path_factory) -> Path:
    rubocop_repo = ROOT / "vendor" / "rubocop"
    rspec_repo = ROOT / "vendor" / "rubocop-rspec"

    for repo, tag in [(rubocop_repo, RUBOCOP_TAG), (rspec_repo, RSPEC_TAG)]:
        if not _tag_exists(repo, tag):
            subprocess.run(["git", "-C", str(repo), "fetch", "--tags", "--quiet"], check=False)
        if not _tag_exists(repo, tag):
            pytest.skip(f"{tag} not fetchable in {repo} (no network access?)")

    root = tmp_path_factory.mktemp("ir23")
    for gem, repo, tag, cops in [
        ("rubocop", rubocop_repo, RUBOCOP_TAG, PILOT_COPS),
        ("rubocop-rspec", rspec_repo, RSPEC_TAG, PILOT_RSPEC_COPS),
    ]:
        for _cop, snake in cops:
            data = _git_show(repo, tag, f"lib/rubocop/cop/{snake}.rb")
            assert data is not None, f"missing {gem}@{tag}:{snake}"
            dest = root / gem / "lib" / "rubocop" / "cop" / f"{snake}.rb"
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_bytes(data)
        config_data = _git_show(repo, tag, "config/default.yml")
        assert config_data is not None
        config_dest = root / gem / "config" / "default.yml"
        config_dest.parent.mkdir(parents=True, exist_ok=True)
        config_dest.write_bytes(config_data)

    return root


# Cross-checked against docs/planning/04-cop-ir-design.md §6.1's own measured
# LOC/matcher-count table (all values below matched exactly during
# development of this pipeline).
EXPECTED_LOC_AND_MATCHERS = {
    "Lint/ArgumentMismatch": (41, 0),
    "Lint/DataDefineOverride": (31, 1),
    "Lint/DeprecatedReference": (104, 0),
    "Lint/MisplacedMagicComment": (97, 0),
    "Lint/NameTypo": (146, 0),
    "Lint/SuperArgumentMismatch": (82, 0),
    "Lint/UnreachablePatternBranch": (39, 0),
    "Lint/UnusedPrivateMethod": (113, 0),
    "Style/DirectiveScope": (180, 0),
    "Style/FileOpen": (25, 1),
    "Style/MapJoin": (46, 4),
    "Style/OneClassPerFile": (40, 0),
    "Style/PartitionInsteadOfDoubleSelect": (171, 1),
    "Style/PredicateWithKind": (37, 2),
    "Style/ReduceToHash": (105, 2),
    "Style/RedundantMinMaxBy": (51, 3),
    "Style/RedundantStructKeywordInit": (64, 4),
    "Style/SelectByKind": (71, 5),
    "Style/SelectByRange": (85, 5),
    "Style/TallyMethod": (57, 4),
    "Style/TimeNow": (18, 1),
    "RSpec/DiscardedMatcher": (66, 0),
    "RSpec/MatchWithSimpleRegex": (50, 1),
}


def test_all_23_pilot_cops_extract_without_error(pilot_root):
    rubocop_root = pilot_root / "rubocop"
    rspec_root = pilot_root / "rubocop-rspec"

    results = {}
    for cop, _snake in PILOT_COPS:
        record = ir_extract.build_extraction_record(
            cop, *ir_extract.find_cop_source(cop, [rubocop_root])
        )
        results[cop] = record
    for cop, _snake in PILOT_RSPEC_COPS:
        record = ir_extract.build_extraction_record(
            cop, *ir_extract.find_cop_source(cop, [rubocop_root, rspec_root])
        )
        results[cop] = record

    assert len(results) == 23
    for cop, (expected_loc, expected_matchers) in EXPECTED_LOC_AND_MATCHERS.items():
        record = results[cop]
        assert record["code_loc"] == expected_loc, f"{cop}: code_loc"
        assert len(record["matchers"]) == expected_matchers, f"{cop}: n_matchers"


def test_all_23_pilot_cops_classify_and_print_buckets(pilot_root, capsys):
    rubocop_root = pilot_root / "rubocop"
    rspec_root = pilot_root / "rubocop-rspec"

    buckets = {}
    for cop, _snake in PILOT_COPS:
        result = ir_classify.classify_one(cop, [rubocop_root])
        buckets[cop] = result["bucket"]
    for cop, _snake in PILOT_RSPEC_COPS:
        result = ir_classify.classify_one(cop, [rubocop_root, rspec_root])
        buckets[cop] = result["bucket"]

    print("\n23-cop pilot bucket table:")
    for cop in sorted(buckets):
        print(f"  {cop}: {buckets[cop]}")

    assert set(buckets) == {c for c, _ in PILOT_COPS} | {c for c, _ in PILOT_RSPEC_COPS}
    assert all(b in ("A", "B", "C") for b in buckets.values())

    # The 5 ProjectIndexHelp-blocked cops must land in C via the
    # ProjectIndexHelp/IndexedMethodArity mixin disqualifier, with no
    # cop-specific special-casing in the classifier.
    for cop in (
        "Lint/ArgumentMismatch",
        "Lint/DeprecatedReference",
        "Lint/NameTypo",
        "Lint/SuperArgumentMismatch",
        "Lint/UnusedPrivateMethod",
    ):
        assert buckets[cop] == "C", cop

    # design §6.1: TimeNow and FileOpen are both <=25 code_loc bucket-A cops.
    assert buckets["Style/TimeNow"] == "A"
    assert buckets["Style/FileOpen"] == "A"
