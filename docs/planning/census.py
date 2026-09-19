#!/usr/bin/env python3
"""Census of RuboCop core + plugin cop source files.

Surveys vendor/rubocop{,-rails,-rspec,-performance,-rake,-factory_bot,-rspec_rails}
to measure LOC, node-matcher usage, on_* hooks, mixins, autocorrect, source-text
access, config usage, and a rough declarative-ness bucket (A/B/C) per cop file.
Also builds a frequency table of NodePattern syntax features used across all
def_node_matcher / def_node_search patterns.

Outputs (written next to this script):
  cops.csv                - one row per cop
  nodepattern_features.csv - one row per NodePattern feature
  census_summary.json     - aggregate tables consumed by rubocop-census.md
"""

from __future__ import annotations

import csv
import json
import re
from dataclasses import dataclass, field
from pathlib import Path

REPO_ROOT = Path("/Users/pnomolos/Projects/nitrocop")
OUT_DIR = Path(
    "/private/tmp/claude-501/-Users-pnomolos-Projects-nitrocop/"
    "b366f2a3-5de1-4f15-a5e8-1b8ffbf964b5/scratchpad/census"
)

# gem -> (vendor dir name, list of (dept_label, subdir-relative-to-lib/rubocop/cop))
GEMS: dict[str, tuple[str, list[tuple[str, str]]]] = {
    "rubocop": (
        "rubocop",
        [
            ("Bundler", "bundler"),
            ("Gemspec", "gemspec"),
            ("Layout", "layout"),
            ("Lint", "lint"),
            ("Metrics", "metrics"),
            ("Migration", "migration"),
            ("Naming", "naming"),
            ("Security", "security"),
            ("Style", "style"),
            ("InternalAffairs", "internal_affairs"),
        ],
    ),
    "rubocop-rails": ("rubocop-rails", [("Rails", "rails")]),
    "rubocop-rspec": ("rubocop-rspec", [("RSpec", "rspec")]),
    "rubocop-performance": ("rubocop-performance", [("Performance", "performance")]),
    "rubocop-rake": ("rubocop-rake", [("Rake", "rake")]),
    "rubocop-factory_bot": ("rubocop-factory_bot", [("FactoryBot", "factory_bot")]),
    "rubocop-rspec_rails": ("rubocop-rspec_rails", [("RSpecRails", "rspec_rails")]),
}

# Directories that hold infrastructure, not concrete cops, even though they
# sit alongside department directories (rubocop core only: mixin/utils/etc.
# already excluded by only walking the department subdirs listed above).
NON_COP_CLASS_NAMES = {"Base", "Cop"}

CLASS_DEF_RE = re.compile(
    r"^\s*class\s+(?P<name>[A-Z]\w*)\s*<\s*(?:::)?(?:RuboCop::(?:Cop|RSpec|RSpecRails|FactoryBot)::)?"
    r"(?:RSpec::|RSpecRails::|FactoryBot::)?(?P<parent>Base|Cop)\b"
)
NODE_MATCHER_RE = re.compile(r"\bdef_node_(matcher|search)\b")
HOOK_DEF_RE = re.compile(r"^\s*def\s+(on_[a-z0-9_]+|after_[a-z0-9_]+)\b")
HOOK_ALIAS_RE = re.compile(
    r"\balias(?:_method)?\s+:?(?P<a>on_[a-z0-9_]+)\s*,?\s+:?(?P<b>on_[a-z0-9_]+)"
)
INCLUDE_EXTEND_RE = re.compile(r"^\s*(include|extend|prepend)\s+([A-Z][\w:]*)")
CONFIG_RE = re.compile(r"\bcop_config\s*[\[\.]")
SOURCE_TEXT_MARKERS = (
    "processed_source",
    ".comments",
    ".tokens",
    "each_token",
    "each_comment",
    "source_range",
    "each_line",
)
AUTOCORRECT_MARKERS = ("AutoCorrector", "corrector.", "def autocorrect")

# --- NodePattern extraction --------------------------------------------------

# def_node_matcher(:name, <<~PATTERN ... PATTERN)   (heredoc form)
HEREDOC_MATCHER_RE = re.compile(
    r"def_node_(?:matcher|search)\(?\s*:[\w?!=]+\s*,\s*<<[-~]?(?P<tag>['\"]?)(?P<delim>\w+)(?P=tag)\s*\n"
    r"(?P<body>.*?)\n\s*(?P=delim)",
    re.DOTALL,
)

# def_node_matcher :name, '...'   or   def_node_matcher :name, "..."  (single-line/string form)
STRING_MATCHER_RE = re.compile(
    r"def_node_(?:matcher|search)\(?\s*:[\w?!=]+\s*,\s*"
    r"(?P<q>['\"])(?P<body>(?:\\.|(?!(?P=q)).)*)(?P=q)"
)

NP_FEATURES: list[tuple[str, re.Pattern]] = [
    ("capture $", re.compile(r"\$")),
    ("rest ...", re.compile(r"\.\.\.")),
    ("unordered <>", re.compile(r"<[^<>]*>")),
    ("union {}", re.compile(r"\{")),
    ("intersection []", re.compile(r"\[")),
    ("predicate #method?", re.compile(r"#[A-Za-z_][\w?!]*")),
    ("param %1/%name", re.compile(r"%[\w]+")),
    ("parent ^", re.compile(r"\^")),
    ("descend `", re.compile(r"`")),
    ("wildcard _", re.compile(r"(?<![\w?!])_(?![\w?!])")),
    ("nil?", re.compile(r"\bnil\?")),
    ("negation !", re.compile(r"!(?!=)")),
    ("type predicate ?", re.compile(r"\b[a-z][a-z0-9_]*\?")),
    ("literal sym :x", re.compile(r":[A-Za-z_][\w]*")),
    ("literal str", re.compile(r'"[^"]*"|\'[^\']*\'')),
    ("literal int", re.compile(r"\(int\b")),
]


@dataclass
class CopRecord:
    gem: str
    dept: str
    name: str
    path: str
    loc: int
    code_loc: int
    n_matchers: int
    hooks: list[str] = field(default_factory=list)
    mixins: list[str] = field(default_factory=list)
    autocorrect: bool = False
    uses_source_text: bool = False
    uses_config: bool = False
    bucket: str = ""


def strip_matcher_defs(text: str) -> tuple[str, list[str]]:
    """Remove def_node_matcher/def_node_search definitions from text, return
    (remaining_text, list_of_pattern_bodies)."""
    patterns: list[str] = []

    def _collect_heredoc(m: re.Match) -> str:
        patterns.append(m.group("body"))
        return ""

    text2 = HEREDOC_MATCHER_RE.sub(_collect_heredoc, text)

    def _collect_string(m: re.Match) -> str:
        patterns.append(m.group("body"))
        return ""

    text3 = STRING_MATCHER_RE.sub(_collect_string, text2)
    return text3, patterns


def code_lines(text: str) -> int:
    n = 0
    for line in text.splitlines():
        s = line.strip()
        if not s:
            continue
        if s.startswith("#"):
            continue
        n += 1
    return n


def find_cop_files(gem_key: str) -> list[tuple[str, Path]]:
    """Return list of (dept_label, file_path) for concrete cop files."""
    gem_dir, depts = GEMS[gem_key]
    results = []
    for dept_label, subdir in depts:
        base = REPO_ROOT / "vendor" / gem_dir / "lib" / "rubocop" / "cop" / subdir
        if not base.is_dir():
            continue
        for f in sorted(base.glob("*.rb")):
            results.append((dept_label, f))
    return results


def analyze_file(gem_key: str, dept: str, path: Path) -> CopRecord | None:
    raw = path.read_text(encoding="utf-8", errors="replace")

    # Determine the cop class name (first class inheriting Base/Cop-ish parent).
    cop_name = None
    for line in raw.splitlines():
        m = CLASS_DEF_RE.match(line)
        if m and m.group("name") not in NON_COP_CLASS_NAMES:
            cop_name = m.group("name")
            break
    if cop_name is None:
        return None  # not a concrete cop file (abstract base, aggregator, etc.)

    loc = len(raw.splitlines())

    body_wo_matchers, patterns = strip_matcher_defs(raw)
    n_matchers = len(NODE_MATCHER_RE.findall(raw))

    hooks: set[str] = set()
    for line in raw.splitlines():
        hm = HOOK_DEF_RE.match(line)
        if hm:
            hooks.add(hm.group(1))
    for am in HOOK_ALIAS_RE.finditer(raw):
        hooks.add(am.group("a"))

    mixins: set[str] = set()
    for line in raw.splitlines():
        im = INCLUDE_EXTEND_RE.match(line)
        if im:
            mixins.add(im.group(2))

    autocorrect = any(marker in raw for marker in AUTOCORRECT_MARKERS)
    uses_source_text = any(marker in raw for marker in SOURCE_TEXT_MARKERS)
    uses_config = bool(CONFIG_RE.search(raw))

    code_loc = code_lines(body_wo_matchers)

    if n_matchers >= 1:
        bucket = "A" if code_loc <= 25 else ("B" if code_loc <= 80 else "C")
    else:
        bucket = "B" if code_loc <= 80 else "C"

    rec = CopRecord(
        gem=gem_key,
        dept=dept,
        name=cop_name,
        path=str(path.relative_to(REPO_ROOT)),
        loc=loc,
        code_loc=code_loc,
        n_matchers=n_matchers,
        hooks=sorted(hooks),
        mixins=sorted(mixins),
        autocorrect=autocorrect,
        uses_source_text=uses_source_text,
        uses_config=uses_config,
        bucket=bucket,
    )
    return rec, patterns


def main() -> None:
    all_records: list[CopRecord] = []
    all_patterns: list[tuple[str, str]] = []  # (cop_name, pattern_body)

    for gem_key in GEMS:
        for dept, path in find_cop_files(gem_key):
            result = analyze_file(gem_key, dept, path)
            if result is None:
                continue
            rec, patterns = result
            all_records.append(rec)
            for p in patterns:
                all_patterns.append((rec.name, p))

    # --- Write cops.csv ---
    csv_path = OUT_DIR / "cops.csv"
    with csv_path.open("w", newline="") as fh:
        w = csv.writer(fh)
        w.writerow(
            [
                "gem",
                "dept",
                "name",
                "loc",
                "n_matchers",
                "hooks",
                "mixins",
                "autocorrect",
                "uses_source_text",
                "uses_config",
                "bucket",
                "path",
            ]
        )
        for r in sorted(all_records, key=lambda r: (r.gem, r.dept, r.name)):
            w.writerow(
                [
                    r.gem,
                    r.dept,
                    r.name,
                    r.loc,
                    r.n_matchers,
                    "|".join(r.hooks),
                    "|".join(r.mixins),
                    r.autocorrect,
                    r.uses_source_text,
                    r.uses_config,
                    r.bucket,
                    r.path,
                ]
            )

    # --- NodePattern feature frequency ---
    feature_pattern_counts = {name: 0 for name, _ in NP_FEATURES}
    feature_occurrence_counts = {name: 0 for name, _ in NP_FEATURES}
    for _cop, pat in all_patterns:
        for fname, regex in NP_FEATURES:
            matches = regex.findall(pat)
            if matches:
                feature_pattern_counts[fname] += 1
                feature_occurrence_counts[fname] += len(matches)

    np_csv_path = OUT_DIR / "nodepattern_features.csv"
    with np_csv_path.open("w", newline="") as fh:
        w = csv.writer(fh)
        w.writerow(["feature", "patterns_using_it", "total_occurrences", "pct_of_patterns"])
        total_patterns = len(all_patterns) or 1
        for fname, _ in NP_FEATURES:
            pc = feature_pattern_counts[fname]
            oc = feature_occurrence_counts[fname]
            w.writerow([fname, pc, oc, round(100 * pc / total_patterns, 1)])

    # --- Aggregate summary ---
    summary: dict = {
        "total_cops": len(all_records),
        "total_patterns": len(all_patterns),
        "per_gem": {},
        "bucket_totals": {"A": 0, "B": 0, "C": 0},
        "mixin_freq": {},
        "hook_freq": {},
        "largest_core_30": [],
    }

    for r in all_records:
        summary["bucket_totals"][r.bucket] += 1
        g = summary["per_gem"].setdefault(
            r.gem,
            {"total": 0, "A": 0, "B": 0, "C": 0, "autocorrect": 0, "uses_source_text": 0, "uses_config": 0},
        )
        g["total"] += 1
        g[r.bucket] += 1
        if r.autocorrect:
            g["autocorrect"] += 1
        if r.uses_source_text:
            g["uses_source_text"] += 1
        if r.uses_config:
            g["uses_config"] += 1
        for m in r.mixins:
            summary["mixin_freq"][m] = summary["mixin_freq"].get(m, 0) + 1
        for h in r.hooks:
            summary["hook_freq"][h] = summary["hook_freq"].get(h, 0) + 1

    core = [r for r in all_records if r.gem == "rubocop"]
    core_sorted = sorted(core, key=lambda r: -r.loc)[:30]
    summary["largest_core_30"] = [
        {"dept": r.dept, "name": r.name, "loc": r.loc, "bucket": r.bucket, "path": r.path}
        for r in core_sorted
    ]

    summary["mixin_freq"] = dict(
        sorted(summary["mixin_freq"].items(), key=lambda kv: -kv[1])
    )
    summary["hook_freq"] = dict(
        sorted(summary["hook_freq"].items(), key=lambda kv: -kv[1])
    )

    with (OUT_DIR / "census_summary.json").open("w") as fh:
        json.dump(summary, fh, indent=2)

    print(f"Analyzed {len(all_records)} cop files across {len(GEMS)} gems.")
    print(f"Extracted {len(all_patterns)} NodePattern definitions.")
    print(f"Bucket totals: {summary['bucket_totals']}")


if __name__ == "__main__":
    main()
