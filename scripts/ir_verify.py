#!/usr/bin/env python3
"""Cop IR translation pipeline — Stage 5: verify. THE GATE.

Given a cop name and a finished `.cop.yml` document (produced by
`scripts/workflows/ir_synth.py`, Stage 3, or hand-written/hand-edited), runs
every mechanical check design `docs/planning/04-cop-ir-design.md` §5 Stage 5
calls for, in order:

  (a) `nitrocop --validate-ir` — schema + resolution + DAG check.
  (b) matcher byte-equality against the upstream `def_node_matcher`/
      `def_node_search` text (modulo a documented leading `^`; any other
      deviation must carry a `# deviation:` comment in the YAML or this
      fails).
  (c) a differential run: harvest example inputs from the upstream RuboCop
      spec (reusing `scripts/spec_to_fixture.py`'s parser, but taking
      annotations from real `rubocop --format json` output, not from the
      spec's own — asserted-message text in a spec can be wrong; PR #23's
      friction log item 11 documents exactly that), at two
      `TargetRubyVersion` values, comparing offense SETS (line, column,
      message, severity) rather than counts. A diff that disappears at
      `TargetRubyVersion: 3.4` (Ruby's `it`-block syntax) is reported as
      informational, not a hard failure.
  (d) `cargo test --release --lib -- cop::ir::embedded::<mod>` on the fixture
      macro this cop is wired into.
  (e) an `-A` convergence check: loop `nitrocop -A` to a fixed point and
      compare the final file to real RuboCop's own `-A` output.
  (f) every `no_offense*.rb` fixture must be silent under real RuboCop.
  (g) a fixture coverage floor (>= 5 non-empty lines in `no_offense.rb`),
      reported before the Rust test step.

Exits non-zero if any HARD check fails. Always writes a machine-readable JSON
report and a Markdown summary; `--report-dir` controls where (default
`build/ir/<Dept>/<snake>/verify/`, gitignored per design §5's artifacts
table).

This is a *public* CLI (not CI-gated like `scripts/check_cop.py`, which is
explicitly reserved for the corpus oracle) — run it locally while iterating
on a translated cop.

Checks (c)/(e)/(f) need real RuboCop at the cop's pinned upstream version,
which is usually newer than the corpus bundle's pinned 1.84.2
(`bench/corpus/vendor/bundle`) — install it once into a scratch gem dir and
pass `--rubocop-gem-dir`:

    mise exec -- gem install rubocop -v 1.91.0 --install-dir /tmp/rubocop-1.91.0 --no-document

Usage:
    python3 scripts/ir_verify.py Style/TimeNow src/resources/ir/style/time_now.cop.yml \\
        --rubocop-gem-dir /tmp/rubocop-1.91.0 --rubocop-root vendor/rubocop

    # Fast mechanical-only loop while iterating (skips (c)/(e)/(f), which need
    # the scratch gem dir and network-fetched upstream source):
    python3 scripts/ir_verify.py Style/TimeNow build/ir/Style/time_now/synth.cop.yml \\
        --skip-differential
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))
sys.path.insert(0, str(SCRIPT_DIR / "workflows"))

import ir_extract  # noqa: E402
import spec_to_fixture  # noqa: E402
from shared import nitrocop_fixture  # noqa: E402
from shared import ruby_source as rs  # noqa: E402

PROJECT_ROOT = SCRIPT_DIR.parent
CORPUS_BASELINE = PROJECT_ROOT / "bench" / "corpus" / "baseline_rubocop.yml"

SEVERITY_LETTERS = {"convention": "C", "warning": "W", "error": "E", "fatal": "F"}


# --- result plumbing -----------------------------------------------------


@dataclass
class CheckResult:
    name: str
    ok: bool
    hard_fail: bool  # False => a failure here is informational only
    messages: list[str] = field(default_factory=list)
    details: dict = field(default_factory=dict)

    def to_dict(self) -> dict:
        return {
            "name": self.name,
            "ok": self.ok,
            "hard_fail": self.hard_fail,
            "messages": self.messages,
            "details": self.details,
        }


@dataclass
class VerifyReport:
    cop: str
    yml_path: str
    checks: list[CheckResult] = field(default_factory=list)

    @property
    def passed(self) -> bool:
        return all(c.ok for c in self.checks if c.hard_fail)

    def to_dict(self) -> dict:
        return {
            "cop": self.cop,
            "yml_path": self.yml_path,
            "passed": self.passed,
            "checks": [c.to_dict() for c in self.checks],
        }

    def to_markdown(self) -> str:
        lines = [f"# ir_verify report — {self.cop}", "", f"YAML: `{self.yml_path}`", ""]
        lines.append("**Overall: " + ("PASS" if self.passed else "FAIL") + "**")
        lines.append("")
        lines.append("| Check | Result | Gate |")
        lines.append("|---|---|---|")
        for c in self.checks:
            status = "OK" if c.ok else ("INFO" if not c.hard_fail else "FAIL")
            gate = "hard" if c.hard_fail else "info"
            lines.append(f"| {c.name} | {status} | {gate} |")
        lines.append("")
        for c in self.checks:
            if c.messages:
                lines.append(f"## {c.name}")
                for m in c.messages:
                    lines.append(f"- {m}")
                lines.append("")
        return "\n".join(lines)


# --- misc helpers -----------------------------------------------------------


def cop_mod_name(cop: str) -> str:
    dept, _, name = cop.partition("/")
    return f"{dept.lower()}_{rs.camel_to_snake(name)}"


def fixture_dir_for(cop: str, fixtures_root: Path) -> Path:
    dept, _, name = cop.partition("/")
    return fixtures_root / rs.department_to_dir(dept) / rs.camel_to_snake(name)


def guess_spec_path(cop: str, rubocop_root: Path, plugin_roots: list[Path]) -> Path | None:
    dept, _, name = cop.partition("/")
    dept_dir = rs.department_to_dir(dept)
    snake = rs.camel_to_snake(name)
    for root in [rubocop_root, *plugin_roots]:
        candidate = root / "spec" / "rubocop" / "cop" / dept_dir / f"{snake}_spec.rb"
        if candidate.is_file():
            return candidate
    return None


def resolve_nitrocop_binary(explicit: str | None) -> str:
    if explicit:
        return explicit
    env_bin = os.environ.get("NITROCOP_BIN")
    if env_bin:
        return env_bin
    cargo_target = os.environ.get("CARGO_TARGET_DIR", "target")
    for profile in ("release", "debug"):
        candidate = PROJECT_ROOT / cargo_target / profile / "nitrocop"
        if candidate.exists():
            return str(candidate)
    return "nitrocop"


def project_default_target_ruby() -> float:
    if CORPUS_BASELINE.is_file():
        m = re.search(r"TargetRubyVersion:\s*([0-9.]+)", CORPUS_BASELINE.read_text())
        if m:
            return float(m.group(1))
    return 3.4


# --- (a) validate-ir ---------------------------------------------------------


def embedded_source_path(cop: str) -> Path:
    """The canonical `src/resources/ir/<dept>/<snake>.cop.yml` path — the ONLY
    location `nitrocop`'s runtime actually reads a cop's hooks from
    (`src/cop/ir/embedded.rs`'s `include_str!` table). Cop discovery of
    arbitrary `.nitrocop/cops/**` documents (design §4.1 items 2-4) has not
    landed yet, so this is not a convention, it is the only path that exists.
    """
    dept, _, name = cop.partition("/")
    return PROJECT_ROOT / "src" / "resources" / "ir" / rs.department_to_dir(dept) / f"{rs.camel_to_snake(name)}.cop.yml"


def check_embedded_freshness(cop: str, yml_path: Path, nitrocop_bin: str) -> CheckResult:
    """`nitrocop --validate-ir` accepts an arbitrary path, but every OTHER
    check in this script runs the compiled `nitrocop` binary end to end, and
    that binary only ever executes the cop hooks it was compiled with —
    `include_str!`'d from `embedded_source_path(cop)` at build time. Pointing
    this script at a candidate document that is not (yet) that file, or is
    but was edited after the last build, means the differential/-A/
    no_offense checks below would silently exercise stale or unrelated
    behavior and still look green.

    This is the single biggest thing a "translate a brand-new cop end to
    end" run needs a human for: promote the candidate YAML to
    `src/resources/ir/<dept>/<snake>.cop.yml`, wire it into
    `src/cop/ir/embedded.rs` (`FILES` + an `ir_cop_fixture_tests!` line), and
    rebuild — there is no dynamic-loading path yet.
    """
    embedded = embedded_source_path(cop)
    binary_path = Path(nitrocop_bin)
    if not embedded.is_file():
        return CheckResult(
            "embedded_freshness", False, hard_fail=True,
            messages=[
                f"{cop} has no embedded document at {embedded} yet. The nitrocop binary's runtime "
                "only executes cops wired into src/cop/ir/embedded.rs — differential/-A-convergence/"
                "no_offense-silence checks below cannot exercise a document that isn't embedded. "
                "Copy the candidate YAML there, add it to embedded.rs's FILES + an "
                "ir_cop_fixture_tests! line, rebuild, then re-run (or pass --skip-differential for a "
                "pre-embedding mechanical-only pass: validate-ir + matcher byte-equality + fixture coverage)."
            ],
        )
    if embedded.read_bytes() != yml_path.read_bytes():
        return CheckResult(
            "embedded_freshness", False, hard_fail=True,
            messages=[
                f"{yml_path} differs from the embedded copy at {embedded} that the nitrocop binary "
                "was actually built from. Copy your candidate over the embedded file (or pass the "
                "embedded path directly) and rebuild before trusting differential/-A/no_offense "
                "results — right now those checks would be exercising whatever is currently embedded, "
                "not the document you gave this script."
            ],
        )
    if binary_path.is_file() and binary_path.stat().st_mtime < embedded.stat().st_mtime:
        return CheckResult(
            "embedded_freshness", False, hard_fail=False,
            messages=[
                f"{nitrocop_bin} is older than {embedded} — rebuild (`cargo build --release`) to make "
                "sure the differential/-A/no_offense checks reflect this document's current content."
            ],
        )
    return CheckResult("embedded_freshness", True, hard_fail=True, messages=[])


def check_validate_ir(nitrocop_bin: str, yml_path: Path) -> CheckResult:
    proc = subprocess.run(
        [nitrocop_bin, "--validate-ir", str(yml_path)], capture_output=True, text=True
    )
    ok = proc.returncode == 0
    msgs = [] if ok else [proc.stdout.strip() or proc.stderr.strip() or f"exit {proc.returncode}"]
    return CheckResult(
        "validate_ir", ok, hard_fail=True, messages=msgs,
        details={"returncode": proc.returncode, "stdout": proc.stdout, "stderr": proc.stderr},
    )


# --- (b) matcher byte equality -----------------------------------------------

_LEADING_CARET_RE = re.compile(r"^(\^+)")


def _normalize_pattern_whitespace(pattern: str) -> str:
    lines = [line.strip() for line in pattern.strip("\n").splitlines()]
    return "\n".join(line for line in lines if line)


def _matcher_block_text(raw_yaml: str, matcher_key: str) -> str:
    """Return the raw YAML text spanning one matcher's block (from its `name:`
    line to the next matcher/top-level key), so a `# deviation:` comment
    inside it can be detected without a full YAML-with-comments parser."""
    lines = raw_yaml.split("\n")
    out = []
    capturing = False
    header_re = re.compile(rf"^\s{{2}}{re.escape(matcher_key)}:\s*$")
    next_key_re = re.compile(r"^\s{2}\S.*:\s*$")
    for line in lines:
        if not capturing:
            if header_re.match(line):
                capturing = True
                out.append(line)
            continue
        if next_key_re.match(line) and not header_re.match(line):
            break
        if re.match(r"^(matchers|hooks|predicates|config|constants):\s*$", line):
            break
        out.append(line)
    return "\n".join(out)


def check_matcher_byte_equality(
    doc: dict, raw_yaml: str, cop: str, roots: list[Path]
) -> CheckResult:
    matchers = doc.get("matchers") or {}
    if not matchers:
        return CheckResult("matcher_byte_equality", True, hard_fail=True, messages=["no matchers declared"])

    try:
        source_path, _gem_root = ir_extract.find_cop_source(cop, roots)
    except ir_extract.ExtractError as exc:
        return CheckResult(
            "matcher_byte_equality", False, hard_fail=False,
            messages=[f"could not locate upstream source to compare against: {exc}"],
        )
    source = source_path.read_text(encoding="utf-8")
    upstream = {
        ir_extract._matcher_key(p.method_name): p.pattern for p in rs.extract_patterns(source)
    }

    messages = []
    hard_ok = True
    for name, spec in matchers.items():
        pattern = spec["pattern"] if isinstance(spec, dict) else spec
        stripped = _LEADING_CARET_RE.sub("", pattern.strip("\n"))
        upstream_pattern = upstream.get(name)
        if upstream_pattern is None:
            messages.append(
                f"{name}: no 1:1 upstream matcher method found by name (composed/renamed "
                "matcher) — not verified against upstream text"
            )
            continue
        if stripped == upstream_pattern.strip("\n"):
            continue
        if _normalize_pattern_whitespace(stripped) == _normalize_pattern_whitespace(upstream_pattern):
            messages.append(f"{name}: matches upstream modulo whitespace/indentation only")
            continue
        block = _matcher_block_text(raw_yaml, name)
        if "# deviation:" in block:
            messages.append(f"{name}: differs from upstream, but carries a '# deviation:' comment")
            continue
        hard_ok = False
        messages.append(
            f"{name}: pattern text differs from upstream and has no '# deviation:' comment.\n"
            f"    upstream: {upstream_pattern!r}\n"
            f"    document: {stripped!r}"
        )

    return CheckResult("matcher_byte_equality", hard_ok, hard_fail=True, messages=messages)


# --- rubocop / nitrocop single-file invocation -------------------------------


def _write_target_config(
    path: Path, cop: str, target_ruby: float, extra_config: dict | None = None
) -> None:
    lines = [
        "AllCops:",
        "  DisabledByDefault: true",
        f"  TargetRubyVersion: {target_ruby}",
        f"{cop}:",
        "  Enabled: true",
    ]
    for k, v in (extra_config or {}).items():
        lines.append(f"  {k}: {v}")
    path.write_text("\n".join(lines) + "\n")


def _mise_prefix() -> list[str]:
    """Wrap invocations in `mise exec --` per AGENTS.md, but only where `mise`
    actually manages the Ruby toolchain (local dev). CI installs Ruby via
    `ruby/setup-ruby@v1` from `.tool-versions` and has no `mise` binary at
    all — there, the gem's own `#!/usr/bin/env ruby` shebang already resolves
    to the right interpreter."""
    import shutil

    return ["mise", "exec", "--"] if shutil.which("mise") else []


def run_real_rubocop_json(
    gem_dir: Path,
    cop: str,
    source: str,
    target_ruby: float,
    extra_config: dict | None = None,
    extra_args: list[str] | None = None,
) -> dict:
    """Run real RuboCop (installed at `gem_dir`) on `source`, through
    `mise exec` where available (AGENTS.md: "wrap RuboCop in `mise exec --`"),
    directly otherwise (see `_mise_prefix`). Returns the parsed top-level
    JSON."""
    with tempfile.TemporaryDirectory() as td:
        tdp = Path(td)
        rb_path = tdp / "input.rb"
        rb_path.write_text(source)
        cfg_path = tdp / "config.yml"
        _write_target_config(cfg_path, cop, target_ruby, extra_config)

        env = os.environ.copy()
        env["GEM_HOME"] = str(gem_dir)
        env["GEM_PATH"] = str(gem_dir)
        cmd = _mise_prefix() + [
            str(gem_dir / "bin" / "rubocop"),
            "--only", cop, "--config", str(cfg_path), "--format", "json",
        ] + (extra_args or []) + [str(rb_path)]
        proc = subprocess.run(cmd, capture_output=True, text=True, env=env, timeout=60)
        try:
            data = json.loads(proc.stdout)
        except json.JSONDecodeError:
            data = {"files": [], "_error": proc.stderr, "_stdout": proc.stdout}
        data["_corrected_source"] = rb_path.read_text() if extra_args and "-A" in extra_args else None
        return data


def run_nitrocop_json(
    nitrocop_bin: str,
    cop: str,
    source: str,
    target_ruby: float,
    extra_config: dict | None = None,
    extra_args: list[str] | None = None,
) -> dict:
    with tempfile.TemporaryDirectory() as td:
        tdp = Path(td)
        rb_path = tdp / "input.rb"
        rb_path.write_text(source)
        cfg_path = tdp / "config.yml"
        _write_target_config(cfg_path, cop, target_ruby, extra_config)

        cmd = [
            nitrocop_bin, "--preview", "--format", "json", "--no-cache",
            "--config", str(cfg_path), "--only", cop,
        ] + (extra_args or []) + [str(rb_path)]
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=60)
        try:
            data = json.loads(proc.stdout)
        except json.JSONDecodeError:
            data = {"offenses": [], "_error": proc.stderr, "_stdout": proc.stdout}
        data["_corrected_source"] = rb_path.read_text() if extra_args else None
        return data


def _rubocop_offense_set(data: dict) -> frozenset:
    out = set()
    for f in data.get("files", []):
        for o in f.get("offenses", []):
            loc = o["location"]
            sev = o.get("severity", "").lower()
            out.add((loc["line"], loc["column"] - 1, o["message"], SEVERITY_LETTERS.get(sev, sev)))
    return frozenset(out)


def _nitrocop_offense_set(data: dict) -> frozenset:
    out = set()
    for o in data.get("offenses", []):
        out.add((o["line"], o["column"], o["message"], o.get("severity", "")))
    return frozenset(out)


def _uses_it_or_numbered_params(source: str) -> bool:
    return bool(re.search(r"\{\s*it\b", source) or re.search(r"\bit\b\s*\}", source)) or "_1" in source


# --- (c) differential ---------------------------------------------------------


def harvest_examples(spec_text: str, cop: str) -> tuple[list, list]:
    examples, skips = spec_to_fixture.convert_spec(spec_text, cop_name=cop)
    return examples, skips


def check_differential(
    cop: str,
    gem_dir: Path,
    nitrocop_bin: str,
    spec_text: str | None,
    target_ruby_versions: list[float],
) -> CheckResult:
    if spec_text is None:
        return CheckResult(
            "differential", False, hard_fail=False,
            messages=["no upstream spec found — harvested-input differential skipped"],
        )
    try:
        examples, skips = harvest_examples(spec_text, cop)
    except spec_to_fixture.SpecConversionError as exc:
        return CheckResult("differential", False, hard_fail=False, messages=[f"could not parse spec: {exc}"])

    if not examples:
        return CheckResult(
            "differential", False, hard_fail=False,
            messages=[f"spec converted 0 examples ({len(skips)} skipped) — nothing to differential-check"],
        )

    versions = sorted(target_ruby_versions)
    hard_ok = True
    messages = []
    per_example = []

    for ex in examples:
        raw_source = spec_to_fixture.strip_annotations(ex.body)
        if not raw_source.strip():
            continue
        diffs_by_version = {}
        for tv in versions:
            rb_data = run_real_rubocop_json(gem_dir, cop, raw_source, tv, ex.config)
            nc_data = run_nitrocop_json(nitrocop_bin, cop, raw_source, tv, ex.config)
            rb_set = _rubocop_offense_set(rb_data)
            nc_set = _nitrocop_offense_set(nc_data)
            diffs_by_version[tv] = (rb_set - nc_set, nc_set - rb_set)

        top_version = versions[-1]
        top_missing, top_extra = diffs_by_version[top_version]
        if top_missing or top_extra:
            hard_ok = False
            messages.append(
                f"line {ex.source_line} @ TargetRubyVersion {top_version}: "
                f"rubocop-only={sorted(top_missing)} nitrocop-only={sorted(top_extra)}"
            )
        else:
            for tv in versions[:-1]:
                missing, extra = diffs_by_version[tv]
                if (missing or extra) and _uses_it_or_numbered_params(raw_source):
                    messages.append(
                        f"line {ex.source_line} @ TargetRubyVersion {tv}: informational only "
                        f"(matches at {top_version}; likely `it`/numbered-param parser-gem gap) "
                        f"rubocop-only={sorted(missing)} nitrocop-only={sorted(extra)}"
                    )
                elif missing or extra:
                    hard_ok = False
                    messages.append(
                        f"line {ex.source_line} @ TargetRubyVersion {tv}: "
                        f"rubocop-only={sorted(missing)} nitrocop-only={sorted(extra)}"
                    )
        per_example.append({"line": ex.source_line, "diffs": {str(k): [sorted(v[0]), sorted(v[1])] for k, v in diffs_by_version.items()}})

    if skips:
        messages.append(f"{len(skips)} spec example(s) could not be harvested (see spec_to_fixture skip reasons)")

    return CheckResult(
        "differential", hard_ok, hard_fail=True, messages=messages,
        details={"examples_checked": len(examples), "skipped": len(skips), "per_example": per_example},
    )


# --- (d) cargo test -----------------------------------------------------------


def check_cargo_test(cop: str, release: bool, allow_unwired: bool) -> CheckResult:
    mod = cop_mod_name(cop)
    cmd = ["cargo", "test"] + (["--release"] if release else []) + ["--lib", "--", f"cop::ir::embedded::{mod}"]
    proc = subprocess.run(cmd, capture_output=True, text=True, cwd=str(PROJECT_ROOT), timeout=600)
    output = proc.stdout + proc.stderr
    m = re.search(r"(\d+) passed; (\d+) failed", output)
    if m and int(m.group(1)) == 0 and int(m.group(2)) == 0:
        msg = (
            f"no tests matched `cop::ir::embedded::{mod}` — this cop is not yet wired into "
            "src/cop/ir/embedded.rs (FILES + the ir_cop_fixture_tests! line). Wire it in, then re-run."
        )
        return CheckResult("cargo_test", allow_unwired, hard_fail=True, messages=[msg])
    ok = proc.returncode == 0
    return CheckResult(
        "cargo_test", ok, hard_fail=True,
        messages=[] if ok else [output[-4000:]],
        details={"returncode": proc.returncode, "mod": mod},
    )


# --- (e) autocorrect convergence ---------------------------------------------


def _loop_nitrocop_autocorrect(
    nitrocop_bin: str, cop: str, source: str, target_ruby: float, extra_config: dict | None, max_iterations: int
) -> tuple[str, int]:
    current = source
    for i in range(max_iterations):
        data = run_nitrocop_json(nitrocop_bin, cop, current, target_ruby, extra_config, extra_args=["-A"])
        corrected = data.get("_corrected_source")
        if corrected is None or corrected == current:
            return current, i
        current = corrected
    return current, max_iterations


def check_autocorrect_convergence(
    cop: str,
    autocorrect_mode: str,
    fixtures_dir: Path,
    nitrocop_bin: str,
    gem_dir: Path | None,
    target_ruby: float,
    max_iterations: int,
) -> CheckResult:
    if autocorrect_mode == "none":
        return CheckResult("autocorrect_convergence", True, hard_fail=False, messages=["autocorrect: none — nothing to check"])

    pairs = []
    for offense_path in sorted(fixtures_dir.glob("offense*.rb")):
        suffix = offense_path.name[len("offense"):]  # "" or ".variant.rb"
        corrected_path = fixtures_dir / f"corrected{suffix}"
        if corrected_path.is_file():
            pairs.append((offense_path, corrected_path))

    if not pairs:
        return CheckResult(
            "autocorrect_convergence", False, hard_fail=False,
            messages=["no offense.rb/corrected.rb pair found under fixtures dir — nothing to converge-check"],
        )

    messages = []
    hard_ok = True
    for offense_path, corrected_path in pairs:
        parsed = nitrocop_fixture.parse_fixture(offense_path.read_text())
        expected_final = corrected_path.read_text()
        config = {}
        final, iterations = _loop_nitrocop_autocorrect(
            nitrocop_bin, cop, parsed.source, target_ruby, config, max_iterations
        )
        if final.rstrip("\n") != expected_final.rstrip("\n"):
            hard_ok = False
            messages.append(
                f"{offense_path.name}: nitrocop -A did not converge to {corrected_path.name} "
                f"after {iterations} iteration(s)"
            )
            continue
        if iterations >= max_iterations:
            hard_ok = False
            messages.append(f"{offense_path.name}: did not reach a fixed point within {max_iterations} iterations")
            continue

        if gem_dir is not None:
            rb_data = run_real_rubocop_json(gem_dir, cop, parsed.source, target_ruby, config, extra_args=["-A"])
            rb_final = rb_data.get("_corrected_source")
            if rb_final is not None and rb_final.rstrip("\n") != final.rstrip("\n"):
                hard_ok = False
                messages.append(
                    f"{offense_path.name}: nitrocop -A output differs from real RuboCop -A output"
                )

    return CheckResult("autocorrect_convergence", hard_ok, hard_fail=True, messages=messages)


# --- (f) no_offense silence ---------------------------------------------------


def check_no_offense_silent(
    cop: str, fixtures_dir: Path, gem_dir: Path, target_ruby: float
) -> CheckResult:
    paths = sorted(fixtures_dir.glob("no_offense*.rb"))
    if not paths:
        return CheckResult("no_offense_silent", False, hard_fail=False, messages=["no no_offense*.rb fixtures found"])

    messages = []
    ok = True
    for path in paths:
        raw = path.read_text()
        config = {}
        if raw.startswith("# nitrocop-config:"):
            config, raw = nitrocop_fixture.parse_variant_fixture(raw)
        data = run_real_rubocop_json(gem_dir, cop, raw, target_ruby, config)
        offenses = _rubocop_offense_set(data)
        if offenses:
            ok = False
            messages.append(f"{path.name}: real RuboCop reports {len(offenses)} offense(s): {sorted(offenses)}")
    return CheckResult("no_offense_silent", ok, hard_fail=True, messages=messages)


# --- (g) fixture coverage floor ----------------------------------------------


def check_fixture_coverage(fixtures_dir: Path, min_lines: int) -> CheckResult:
    no_offense = fixtures_dir / "no_offense.rb"
    if not no_offense.is_file():
        return CheckResult(
            "fixture_coverage", False, hard_fail=True,
            messages=[f"{no_offense} does not exist"],
        )
    non_empty = [ln for ln in no_offense.read_text().splitlines() if ln.strip()]
    ok = len(non_empty) >= min_lines
    return CheckResult(
        "fixture_coverage", ok, hard_fail=True,
        messages=[] if ok else [f"no_offense.rb has {len(non_empty)} non-empty line(s), floor is {min_lines}"],
        details={"non_empty_lines": len(non_empty)},
    )


# --- orchestration ------------------------------------------------------------


def run_verify(args) -> VerifyReport:
    yml_path = args.cop_yml
    raw_yaml = yml_path.read_text()
    import yaml

    doc = yaml.safe_load(raw_yaml)
    cop = args.cop
    roots = [args.rubocop_root, *args.plugin_root]
    fixtures_dir = fixture_dir_for(cop, args.fixtures_dir)
    nitrocop_bin = resolve_nitrocop_binary(args.nitrocop_bin)
    report = VerifyReport(cop=cop, yml_path=str(yml_path))

    report.checks.append(check_validate_ir(nitrocop_bin, yml_path))
    report.checks.append(check_matcher_byte_equality(doc, raw_yaml, cop, roots))
    report.checks.append(check_fixture_coverage(fixtures_dir, args.min_no_offense_lines))

    if args.skip_differential:
        report.checks.append(
            CheckResult("differential", True, hard_fail=False, messages=["--skip-differential"])
        )
        report.checks.append(
            CheckResult("autocorrect_convergence", True, hard_fail=False, messages=["--skip-differential"])
        )
        report.checks.append(
            CheckResult("no_offense_silent", True, hard_fail=False, messages=["--skip-differential"])
        )
    else:
        freshness = check_embedded_freshness(cop, yml_path, nitrocop_bin)
        report.checks.append(freshness)
        if not freshness.ok and freshness.hard_fail:
            msg = "skipped — see embedded_freshness"
            report.checks.append(CheckResult("differential", False, hard_fail=True, messages=[msg]))
            report.checks.append(CheckResult("autocorrect_convergence", False, hard_fail=True, messages=[msg]))
            report.checks.append(CheckResult("no_offense_silent", False, hard_fail=True, messages=[msg]))
        elif args.rubocop_gem_dir is None:
            msg = "--rubocop-gem-dir not given; install the pinned upstream rubocop into a scratch dir and pass it"
            report.checks.append(CheckResult("differential", False, hard_fail=True, messages=[msg]))
            report.checks.append(CheckResult("autocorrect_convergence", False, hard_fail=True, messages=[msg]))
            report.checks.append(CheckResult("no_offense_silent", False, hard_fail=True, messages=[msg]))
        else:
            spec_path = args.spec or guess_spec_path(cop, args.rubocop_root, args.plugin_root)
            spec_text = spec_path.read_text(encoding="utf-8") if spec_path and spec_path.is_file() else None
            versions = args.target_ruby_versions or [project_default_target_ruby(), 3.4]
            report.checks.append(
                check_differential(cop, args.rubocop_gem_dir, nitrocop_bin, spec_text, versions)
            )
            report.checks.append(
                check_autocorrect_convergence(
                    cop, doc.get("autocorrect", "none"), fixtures_dir, nitrocop_bin,
                    args.rubocop_gem_dir, versions[0] if versions else 3.4, args.autocorrect_loop_limit,
                )
            )
            report.checks.append(
                check_no_offense_silent(cop, fixtures_dir, args.rubocop_gem_dir, versions[0] if versions else 3.4)
            )

    if args.skip_cargo_test:
        report.checks.append(CheckResult("cargo_test", True, hard_fail=False, messages=["--skip-cargo-test"]))
    else:
        report.checks.append(check_cargo_test(cop, release=not args.debug_build, allow_unwired=args.allow_unwired))

    return report


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("cop", help="Cop name, e.g. Style/TimeNow")
    parser.add_argument("cop_yml", type=Path, help="Path to the finished .cop.yml document")
    parser.add_argument("--rubocop-root", type=Path, default=PROJECT_ROOT / "vendor" / "rubocop")
    parser.add_argument("--plugin-root", type=Path, action="append", default=[])
    parser.add_argument("--spec", type=Path, default=None, help="Override the guessed spec file path")
    parser.add_argument(
        "--rubocop-gem-dir", type=Path, default=None,
        help="Scratch --install-dir of `gem install rubocop -v <pinned>` at the cop's upstream version",
    )
    parser.add_argument("--nitrocop-bin", default=None)
    parser.add_argument("--fixtures-dir", type=Path, default=PROJECT_ROOT / "tests" / "fixtures" / "cops")
    parser.add_argument("--report-dir", type=Path, default=None)
    parser.add_argument("--target-ruby-versions", type=float, nargs="*", default=None)
    parser.add_argument("--autocorrect-loop-limit", type=int, default=10)
    parser.add_argument("--min-no-offense-lines", type=int, default=5)
    parser.add_argument("--skip-differential", action="store_true", help="Skip (c)/(e)/(f) — mechanical checks only")
    parser.add_argument("--skip-cargo-test", action="store_true")
    parser.add_argument("--debug-build", action="store_true", help="Use `cargo test` (debug) instead of --release")
    parser.add_argument(
        "--allow-unwired", action="store_true",
        help="Treat 'cop not wired into embedded.rs' as a pass rather than a hard failure",
    )
    args = parser.parse_args(argv)

    report = run_verify(args)

    dept, _, name = args.cop.partition("/")
    report_dir = args.report_dir or PROJECT_ROOT / "build" / "ir" / dept / rs.camel_to_snake(name) / "verify"
    report_dir.mkdir(parents=True, exist_ok=True)
    (report_dir / "report.json").write_text(json.dumps(report.to_dict(), indent=2) + "\n")
    (report_dir / "report.md").write_text(report.to_markdown())

    print(report.to_markdown())
    print(f"\nFull report: {report_dir}/report.json")

    return 0 if report.passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
