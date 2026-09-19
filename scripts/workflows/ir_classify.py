#!/usr/bin/env python3
"""Cop IR translation pipeline — Stage 2: classify.

Buckets cops into A / B / C per docs/planning/04-cop-ir-design.md §5 Stage 2
(on the `planning/program-status` branch), using the Stage 1 extraction
record (`ir_extract.py`) as input:

    C (stays hand-written Rust) if any of:
      - code_loc > 80
      - mixins intersect a disqualifying set (RangeHelp, Alignment,
        SurroundingSpace, EndKeywordAlignment, Heredoc, PercentLiteral,
        ProjectIndexHelp, IndexedMethodArity)
      - defines on_new_investigation (file-level state machine)
      - references processed_source/tokens/comments directly
    A (matcher + guards only, cheapest to translate) if n_matchers >= 1 and
      code_loc <= 25 and no C-disqualifier fired.
    B (matcher + non-trivial glue) otherwise.

Every cop gets a printed, auditable rationale (which rule fired, and why);
this is deliberately mechanical, matching docs/planning/census.py's original
A/B/C heuristic plus the design's hard disqualifiers — it is not meant to
replace a human judgment call on a borderline cop, just to make that judgment
call auditable and reproducible.

Usage:
    python3 scripts/workflows/ir_classify.py Style/HashExcept Style/ClassCheck
    python3 scripts/workflows/ir_classify.py --cops-file cops.txt --csv out.csv
"""

from __future__ import annotations

import argparse
import csv
import json
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
SCRIPTS_ROOT = SCRIPT_DIR.parent
sys.path.insert(0, str(SCRIPTS_ROOT))
sys.path.insert(0, str(SCRIPT_DIR))

import ir_extract  # noqa: E402
from shared import ruby_source as rs  # noqa: E402

PROJECT_ROOT = SCRIPTS_ROOT.parent

# Mixins that, if present, keep a cop in hand-written Rust even if it has
# matchers and low LOC — each needs layout/token-stream/cross-file machinery
# the IR's `Expr`/`Matcher` layer explicitly does not attempt (design §2.3).
# RangeHelp is handled separately (see RANGE_HELP_STITCHING_METHODS below):
# the design's own rule is "RangeHelp (when used for multi-token stitching)",
# not RangeHelp's mere presence.
DISQUALIFYING_MIXINS = {
    "Alignment",
    "SurroundingSpace",
    "EndKeywordAlignment",
    "Heredoc",
    "PercentLiteral",
    "ProjectIndexHelp",
    "IndexedMethodArity",
}

# RangeHelp methods that scan the token/comment stream to stitch a range
# together (surrounding whitespace/commas, whole-line ranges, comment
# attachment) — these need real token-stream machinery the IR does not have.
# `range_between`/`source_range` (a plain two-offset range, or the node's own
# range) are trivially expressible via the IR's `{start:, stop:}` anchors
# (design §1.5's `Style/RedundantMinMaxBy` example does exactly this while
# still `include RangeHelp`), so including RangeHelp and calling only those
# does not disqualify a cop on its own.
RANGE_HELP_STITCHING_METHODS = (
    "range_with_surrounding_space",
    "range_with_surrounding_comma",
    "range_by_whole_lines",
    "contents_range",
    "arguments_range",
    "range_with_comments_and_lines",
    "range_with_comments(",
    "column_offset_between",
    "effective_column",
)


def _uses_range_help_stitching(source: str) -> bool:
    return any(marker in source for marker in RANGE_HELP_STITCHING_METHODS)

BUCKET_A = "A"
BUCKET_B = "B"
BUCKET_C = "C"


def classify_record(record: dict, source: str) -> tuple[str, list[str]]:
    """Return (bucket, reasons) for an extraction record (see `ir_extract.py`).

    `source` is the cop's raw Ruby source — needed (in addition to the
    extraction record) to tell trivial RangeHelp/source_range use apart from
    genuine token-stream stitching; see RANGE_HELP_STITCHING_METHODS and
    `rs.uses_file_level_source_text` above.
    """
    reasons: list[str] = []
    code_loc = record["code_loc"]
    mixins = set(record["mixins"])
    hooks = set(record["hooks"])

    disqualifying_mixins = sorted(mixins & DISQUALIFYING_MIXINS)
    if "RangeHelp" in mixins and _uses_range_help_stitching(source):
        disqualifying_mixins.append("RangeHelp (multi-token stitching)")
        disqualifying_mixins.sort()

    if code_loc > 80:
        reasons.append(f"code_loc {code_loc} > 80 (design §5 Stage 2 hard cap)")
    if disqualifying_mixins:
        reasons.append(f"disqualifying mixin(s): {', '.join(disqualifying_mixins)}")
    if "on_new_investigation" in hooks:
        reasons.append("defines on_new_investigation (file-level state machine, design §2.3)")
    if rs.uses_file_level_source_text(source):
        reasons.append("references processed_source/tokens/comments (design §2.3)")

    if reasons:
        return BUCKET_C, reasons

    n_matchers = len(record["matchers"])
    if n_matchers >= 1 and code_loc <= 25:
        return BUCKET_A, [
            f"{n_matchers} matcher(s), code_loc {code_loc} <= 25, no C-disqualifier"
        ]
    return BUCKET_B, [
        f"n_matchers={n_matchers}, code_loc={code_loc} — matcher+glue, not cheap enough for A"
    ]


def classify_one(cop: str, roots: list[Path]) -> dict:
    """Extract `cop` and classify it. Returns a result dict with rationale."""
    source_path, gem_root = ir_extract.find_cop_source(cop, roots)
    record = ir_extract.build_extraction_record(cop, source_path, gem_root)
    source = source_path.read_text(encoding="utf-8")
    bucket, reasons = classify_record(record, source)
    return {
        "cop": cop,
        "bucket": bucket,
        "reasons": reasons,
        "code_loc": record["code_loc"],
        "n_matchers": len(record["matchers"]),
        "mixins": record["mixins"],
        "hooks": record["hooks"],
        "extends_autocorrector": record["extends_autocorrector"],
    }


def write_csv(results: list[dict], path: Path) -> None:
    with path.open("w", newline="") as fh:
        writer = csv.writer(fh)
        writer.writerow(
            ["cop", "bucket", "code_loc", "n_matchers", "mixins", "hooks", "reasons"]
        )
        for r in results:
            writer.writerow(
                [
                    r["cop"],
                    r["bucket"],
                    r["code_loc"],
                    r["n_matchers"],
                    "|".join(r["mixins"]),
                    "|".join(r["hooks"]),
                    " ; ".join(r["reasons"]),
                ]
            )


def print_rationale(result: dict) -> None:
    print(f"{result['cop']}: bucket {result['bucket']}")
    for reason in result["reasons"]:
        print(f"    - {reason}")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("cops", nargs="*", help="Cop name(s), e.g. Style/HashExcept")
    parser.add_argument(
        "--cops-file", type=Path, default=None, help="File with one cop name per line (batch mode)"
    )
    parser.add_argument(
        "--rubocop-root", type=Path, default=PROJECT_ROOT / "vendor" / "rubocop"
    )
    parser.add_argument("--plugin-root", type=Path, action="append", default=[])
    parser.add_argument("--csv", type=Path, default=None, help="Write results as CSV to this path")
    parser.add_argument("--json", type=Path, default=None, help="Write results as JSON to this path")
    parser.add_argument("--quiet", action="store_true", help="Suppress per-cop rationale on stdout")
    args = parser.parse_args(argv)

    cops = list(args.cops)
    if args.cops_file:
        cops.extend(
            line.strip()
            for line in args.cops_file.read_text().splitlines()
            if line.strip() and not line.strip().startswith("#")
        )
    if not cops:
        parser.error("no cops given (pass cop names or --cops-file)")

    roots = [args.rubocop_root, *args.plugin_root]
    results = []
    had_error = False
    for cop in cops:
        try:
            result = classify_one(cop, roots)
        except ir_extract.ExtractError as exc:
            print(f"ERROR: {exc}", file=sys.stderr)
            had_error = True
            continue
        results.append(result)
        if not args.quiet:
            print_rationale(result)

    totals = {BUCKET_A: 0, BUCKET_B: 0, BUCKET_C: 0}
    for r in results:
        totals[r["bucket"]] += 1
    if not args.quiet:
        print(f"\nTotals: A={totals[BUCKET_A]} B={totals[BUCKET_B]} C={totals[BUCKET_C]}")

    if args.csv:
        write_csv(results, args.csv)
        print(f"wrote {args.csv}", file=sys.stderr)
    if args.json:
        args.json.write_text(json.dumps(results, indent=2, sort_keys=True) + "\n")
        print(f"wrote {args.json}", file=sys.stderr)

    return 1 if had_error else 0


if __name__ == "__main__":
    raise SystemExit(main())
