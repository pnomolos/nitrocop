#!/usr/bin/env python3
"""Tests for generate_baseline_cops.py.

Guards against src/resources/baseline_cops.json drifting out of sync with the
vendored config/default.yml files after a gem version bump (see AGENTS.md's
`update-gem` skill and docs of the rubocop 1.91 vendor bump).
"""

import json
import subprocess
import sys
from pathlib import Path

PROJECT_ROOT = Path(__file__).parents[2]
SCRIPT = PROJECT_ROOT / "scripts" / "generate_baseline_cops.py"
BASELINE_COPS = PROJECT_ROOT / "src" / "resources" / "baseline_cops.json"


def run(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(SCRIPT), *args],
        capture_output=True,
        text=True,
        cwd=PROJECT_ROOT,
    )


def test_baseline_cops_json_in_sync_with_vendor_configs():
    """The checked-in baseline_cops.json must match what the generator computes.

    If this fails after bumping a vendor submodule, run:
        python3 scripts/generate_baseline_cops.py
    and commit the result.
    """
    result = run("--check")
    assert result.returncode == 0, (
        "src/resources/baseline_cops.json is out of sync with vendor "
        f"config/default.yml files:\n{result.stdout}{result.stderr}"
    )


def test_baseline_cops_json_shape():
    """Sanity-check the on-disk file's shape (flat {cop_name: bool}, sorted, valid)."""
    data = json.loads(BASELINE_COPS.read_text())
    assert isinstance(data, dict)
    assert len(data) > 500  # rubocop alone ships hundreds of cops
    assert list(data.keys()) == sorted(data.keys())
    for name, enabled in data.items():
        assert "/" in name, f"cop name missing department separator: {name}"
        assert isinstance(enabled, bool), f"{name} value is not a bool: {enabled!r}"


def test_generated_output_is_deterministic():
    """Running the generator twice (without --check) produces byte-identical output."""
    import tempfile

    with tempfile.TemporaryDirectory() as tmp:
        out1 = Path(tmp) / "run1.json"
        out2 = Path(tmp) / "run2.json"

        sys.path.insert(0, str(PROJECT_ROOT / "scripts"))
        try:
            import importlib

            gen = importlib.import_module("generate_baseline_cops")
            baseline = gen.compute_baseline_cops()
            out1.write_text(gen.render(baseline))
            out2.write_text(gen.render(gen.compute_baseline_cops()))
        finally:
            sys.path.pop(0)
            sys.modules.pop("generate_baseline_cops", None)

        assert out1.read_text() == out2.read_text()
