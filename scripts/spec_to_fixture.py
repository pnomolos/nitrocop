#!/usr/bin/env python3
"""Cop IR translation pipeline — Stage 4: RuboCop spec -> nitrocop fixture.

Converts a RuboCop cop spec file's `expect_offense` / `expect_no_offenses` /
`expect_correction` blocks into nitrocop fixtures under
`tests/fixtures/cops/<dept>/<snake>/`, using the conventions documented in
`src/testutil.rs` and `scripts/generate_fixture.py` (verified against real
fixtures under `tests/fixtures/cops/`, not guessed):

- `^^^^ message` annotation lines are rewritten to `^^^^ Dept/Name: message`
  (`scripts/generate_fixture.py`'s convention; `src/testutil.rs::try_parse_annotation`
  requires the `Department/CopName: ` prefix nitrocop's own fixtures use, but
  RuboCop's own `expect_offense` DSL does not).
- `expect_no_offenses` bodies go to `no_offense.rb`.
- `expect_offense` + a following `expect_correction` in the same example pairs
  the (annotation-stripped) source with the corrected source for
  `cop_autocorrect_fixture_tests!` (`corrected.rb`).
- `context 'when EnforcedStyle is X' do / let(:cop_config) { {...} }` (or
  `let(:config)`, including one nested `let` indirection level, e.g.
  rubocop-rspec's `let(:cop_config) { { 'EnforcedStyle' => enforced_style } }`
  + `let(:enforced_style) { 'x' }`) becomes an `offense.<variant>.rb` /
  `no_offense.<variant>.rb` pair with a `# nitrocop-config:` directive
  (`src/testutil.rs::parse_variant_fixture`).
- `expect_offense(<<~RUBY, 'some/path.rb')` (a plain string second argument,
  RuboCop's own `file` parameter — see `vendor/rubocop/lib/rubocop/rspec/expect_offense.rb`)
  becomes a `# nitrocop-filename:` directive. Because that directive is only
  valid on a fixture's first line, and a single `offense.rb`/`no_offense.rb`
  aggregates many examples, an example with an *explicit* filename is instead
  written to its own `offense/<slug>.rb` scenario file (see AGENTS.md
  "Fixture Rules": "an `offense/` scenario directory ... for cops that fire
  once per file or do not fit `^` annotations" — a per-file-identity
  assertion like `Lint/DuplicateMethods`'s falls in exactly that category).

Constructs this script deliberately does **not** convert (skipped, and
reported, rather than guessed at — see AGENTS.md "Skip and report constructs
you cannot convert ... rather than emitting wrong fixtures"):

- `it_behaves_like`/`shared_examples`/`include_examples` and any block whose
  parameters are interpolated into the heredoc body (e.g. `%w[...].each do
  |type| ... "#{type}" ...`) — the body is only known once the block runs.
- `expect_offense(<<~RUBY, key: value)` keyword-argument interpolation
  (RuboCop's `%{key}`/`^{key}`/`_{key}` template markers) — would need to
  replicate `RuboCop::RSpec::ExpectOffense#format_offense`'s string
  substitution, which this script does not attempt.
- A custom `subject(:cop) { described_class.new(config) }` override instead
  of the standard `:config` shared-context idiom — there is no reliable
  static way to know what config it builds without evaluating Ruby.
  (`spec/rubocop/cop/style/negated_if_spec.rb` is exactly this shape.)
- More than one `expect_offense`/`expect_no_offenses` call in a single `it`
  example — ambiguous which annotations belong to which snippet.
- A message annotation using RuboCop's `[...]` abbreviation (fuzzy match) or
  the `^{}` blank-line marker — nitrocop's fixture format requires an exact
  message and a real caret range.
- `let(:config)` built from `AllCops:`/another cop's settings — nitrocop's
  `# nitrocop-config:` directive only carries the cop's own `config:` keys
  (`CopConfig.options`), so an `AllCops`-shaped override has no fixture
  representation.

Usage:
    python3 scripts/spec_to_fixture.py vendor/rubocop/spec/rubocop/cop/style/class_check_spec.rb
    python3 scripts/spec_to_fixture.py path/to/spec.rb --out-dir tests/fixtures/cops --dry-run
"""

from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

from shared import ruby_source as rs  # noqa: E402

PROJECT_ROOT = SCRIPT_DIR.parent


class SpecConversionError(Exception):
    pass


# --- block-scope scanning ---------------------------------------------------


@dataclass
class Frame:
    kind: str  # root, describe, context, shared, it, let, subject, subject_cop, before, block
    start_line: int
    let_name: str | None = None
    dynamic: bool = False  # contents are parameterized by a block arg / shared example
    skip: bool = False
    skip_reason: str | None = None
    lets: dict[str, str] = field(default_factory=dict)  # name -> raw Ruby RHS text
    body_lines: list[str] = field(default_factory=list)  # only used for let/subject frames
    has_custom_subject_cop: bool = False


@dataclass
class Call:
    kind: str  # offense, no_offense, correction, no_corrections
    heredoc: rs.Heredoc
    trailing_args: str  # raw text after the heredoc's closing delimiter, before ')'
    frames: list[Frame]  # snapshot of the frame stack at the call site (outer -> inner)


_DO_BLOCK_OPEN_RE = re.compile(r"(?:^|\s)do(?:\s*\|[^|]*\|)?\s*$")
_DESCRIBE_RE = re.compile(r"^(?:RSpec\.describe|describe)\b(.*)$")
_CONTEXT_RE = re.compile(r"^context\b(.*)$")
_SHARED_RE = re.compile(r"^(?:shared_examples|shared_context)\b(.*)$")
_IT_RE = re.compile(r"^(?:it|example|specify)\b(.*)$")
_BEFORE_RE = re.compile(r"^(?:before|after|around)\b(.*)$")
_LET_DO_RE = re.compile(r"^let\(:([A-Za-z_][A-Za-z0-9_!?]*)\)\s*(.*)$")
_SUBJECT_DO_RE = re.compile(r"^subject(?=[\s(])(?:\(:([A-Za-z_][A-Za-z0-9_]*)\))?(.*)$")
_SINGLE_LINE_LET_RE = re.compile(r"^let\(:([A-Za-z_][A-Za-z0-9_!?]*)\)\s*\{(.*)$")
_UNSUPPORTED_TAG_RE = re.compile(r"unsupported_on:\s*:prism")


def _extract_balanced_braces(text: str) -> tuple[str, str] | None:
    """Given text starting just after an opening `{`, return (inner, rest_after_close)."""
    depth = 1
    quote = None
    i = 0
    while i < len(text):
        c = text[i]
        if quote:
            if c == "\\":
                i += 2
                continue
            if c == quote:
                quote = None
        elif c in "'\"":
            quote = c
        elif c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return text[:i], text[i + 1 :]
        i += 1
    return None


def _classify_opener(stripped: str) -> Frame | None:
    if not _DO_BLOCK_OPEN_RE.search(stripped):
        return None

    m = _LET_DO_RE.match(stripped)
    if m:
        return Frame(kind="let", start_line=-1, let_name=m.group(1))

    m = _SUBJECT_DO_RE.match(stripped)
    if m:
        name = m.group(1)
        return Frame(kind="subject_cop" if name == "cop" else "subject", start_line=-1)

    m = _SHARED_RE.match(stripped)
    if m:
        return Frame(kind="shared", start_line=-1, dynamic=True)

    m = _DESCRIBE_RE.match(stripped) or _CONTEXT_RE.match(stripped)
    if m:
        kind = "describe" if stripped.startswith(("RSpec.describe", "describe")) else "context"
        skip = bool(_UNSUPPORTED_TAG_RE.search(m.group(1)))
        return Frame(
            kind=kind,
            start_line=-1,
            skip=skip,
            skip_reason="tagged unsupported_on: :prism (parser-gem-only behavior)" if skip else None,
        )

    m = _IT_RE.match(stripped)
    if m:
        return Frame(kind="it", start_line=-1)

    m = _BEFORE_RE.match(stripped)
    if m:
        return Frame(kind="before", start_line=-1)

    # Any other `... do` / `... do |x|` opener (`.each do |x|`, `N.times do`, a
    # bare `if ... do`, etc.) — contents are potentially parameterized/dynamic,
    # so treated conservatively as unsupported.
    return Frame(kind="block", start_line=-1, dynamic=True)


def scan_spec(text: str) -> tuple[list[Frame], list[Call]]:
    """Scan a spec file into a (root-to-leaf) frame tree and a flat call list."""
    lines = text.split("\n")
    root = Frame(kind="root", start_line=0)
    stack: list[Frame] = [root]
    calls: list[Call] = []

    i = 0
    n = len(lines)
    while i < n:
        raw_line = lines[i]
        stripped = raw_line.strip()

        if "<<" in raw_line and any(
            f"{name}(" in raw_line
            for name in ("expect_offense", "expect_no_offenses", "expect_correction")
        ):
            heredoc = rs.find_heredoc(lines, i, 0)
            if heredoc is not None:
                if "expect_offense(" in raw_line:
                    kind = "offense"
                elif "expect_no_offenses(" in raw_line:
                    kind = "no_offense"
                else:
                    kind = "correction"
                # RuboCop specs write extra call args on the SAME physical
                # line as the heredoc marker (`expect_offense(<<~RUBY, 'x.rb')`),
                # not after the terminator — Ruby heredoc terminators must be
                # alone on their line.
                trailing_args = heredoc.opener_tail.strip()
                calls.append(Call(kind, heredoc, trailing_args, list(stack)))
                i = heredoc.end_line + 1
                continue

        m = _SINGLE_LINE_LET_RE.match(stripped)
        if m:
            balanced = _extract_balanced_braces(stripped[m.end(1) + len(") {") :])
            if balanced is not None:
                stack[-1].lets[m.group(1)] = balanced[0].strip()
            i += 1
            continue

        if stripped == "end":
            if len(stack) > 1:
                popped = stack.pop()
                if popped.kind in ("let", "subject", "subject_cop") and popped.let_name:
                    stack[-1].lets[popped.let_name] = "\n".join(popped.body_lines).strip()
                if popped.kind == "subject_cop":
                    # `subject(:cop)` is a SIBLING declaration, not an ancestor
                    # of the `it` blocks it affects — RSpec's `subject`/`let`
                    # are visible throughout the whole enclosing example
                    # group regardless of textual position. Mark the parent
                    # frame (not `call.frames`, which won't include this
                    # already-closed frame) so every call under that parent
                    # scope is recognized as using a custom subject override.
                    stack[-1].has_custom_subject_cop = True
            i += 1
            continue

        opener = _classify_opener(stripped)
        if opener is not None:
            opener.start_line = i
            stack.append(opener)
            i += 1
            continue

        if stripped == "expect_no_corrections":
            calls.append(Call("no_corrections", None, "", list(stack)))
            i += 1
            continue

        top = stack[-1]
        if top.kind in ("let", "subject", "subject_cop"):
            top.body_lines.append(raw_line)

        i += 1

    return [root], calls


# --- cop_config resolution ---------------------------------------------------

_IDENT_RE = re.compile(r"\b[a-z_][a-zA-Z0-9_]*\b")


def _lookup_let(name: str, frames: list[Frame]) -> str | None:
    for frame in reversed(frames):
        if name in frame.lets:
            return frame.lets[name]
    return None


def _resolve_let_text(text: str, frames: list[Frame], depth: int = 5) -> str:
    """Substitute bare identifiers that name a `let` in `frames` into `text`.

    Handles the common one-level indirection RuboCop-rspec specs use, e.g.
    `let(:cop_config) { { 'EnforcedStyle' => enforced_style } }` +
    `let(:enforced_style) { 'method_call' }`.
    """
    for _ in range(depth):
        changed = False

        def repl(m: re.Match) -> str:
            nonlocal changed
            name = m.group(0)
            value = _lookup_let(name, frames)
            if value is None:
                return name
            changed = True
            return f"({value})"

        new_text = _IDENT_RE.sub(repl, text)
        if not changed:
            return new_text
        text = new_text
    return text


class UnresolvedConfig(Exception):
    def __init__(self, reason: str):
        super().__init__(reason)
        self.reason = reason


def resolve_cop_config(frames: list[Frame], cop_name: str) -> dict:
    """Resolve the effective `cop_config` dict visible at `frames` (innermost last).

    Returns {} if no `cop_config`/`config` `let` is in scope (the RuboCop
    default config applies). Raises UnresolvedConfig for a `let(:config)`
    shaped as `AllCops:`/another cop's settings, which nitrocop's
    `# nitrocop-config:` directive cannot represent (it only carries the
    cop's own `config:` options), or for a `let` body this script's Ruby
    literal parser cannot parse.
    """
    raw = _lookup_let("cop_config", frames)
    source_key = "cop_config"
    if raw is None:
        raw = _lookup_let("config", frames)
        source_key = "config"
    if raw is None:
        return {}

    resolved_text = _resolve_let_text(raw, frames)
    resolved_text = resolved_text.strip()

    m = re.fullmatch(r"RuboCop::Config\.new\((.*)\)", resolved_text, re.DOTALL)
    if m:
        resolved_text = m.group(1).strip()
        # `Foo.new('a' => 1)` is Ruby's implicit-hash-as-last-argument sugar
        # for `Foo.new({'a' => 1})` — re-add the braces our hash parser needs.
        if not resolved_text.startswith("{"):
            resolved_text = "{" + resolved_text + "}"

    try:
        parsed = rs.parse_ruby_literal(resolved_text)
    except rs.RubyLiteralError as exc:
        raise UnresolvedConfig(f"could not parse let(:{source_key}) body: {exc}") from exc

    if not isinstance(parsed, dict):
        raise UnresolvedConfig(f"let(:{source_key}) did not resolve to a hash: {resolved_text!r}")

    if source_key == "config":
        if cop_name in parsed:
            parsed = parsed[cop_name]
        elif not _looks_like_flat_cop_config(parsed):
            raise UnresolvedConfig(
                f"let(:config) sets {sorted(parsed.keys())}, not this cop's own options — "
                "not representable via # nitrocop-config: (cop-scoped only)"
            )

    if not _looks_like_flat_cop_config(parsed):
        raise UnresolvedConfig(f"config does not look like a flat option map: {parsed!r}")

    return parsed


def _looks_like_flat_cop_config(d: dict) -> bool:
    if not isinstance(d, dict):
        return False
    for k in d:
        if not isinstance(k, str) or k == "AllCops" or "/" in k:
            return False
    return True


# --- annotation conversion ---------------------------------------------------


@dataclass
class AnnotationParse:
    indent: str
    carets: str
    message: str


def _try_parse_rubocop_annotation(line: str) -> AnnotationParse | str | None:
    """Parse a RuboCop `expect_offense` caret annotation line.

    Returns an AnnotationParse, the sentinel string "BLANK_MARKER" for the
    unsupported `^{}` blank-line marker, or None if `line` is not an
    annotation line at all.
    """
    stripped = line.lstrip(" ")
    indent = line[: len(line) - len(stripped)]
    if not stripped.startswith("^"):
        return None
    n = 0
    while n < len(stripped) and stripped[n] == "^":
        n += 1
    rest = stripped[n:]
    if rest.startswith("{"):
        return "BLANK_MARKER"
    if not rest.startswith(" "):
        return None
    return AnnotationParse(indent=indent, carets=stripped[:n], message=rest[1:].rstrip())


def convert_offense_body(dedented_body: str, cop_name: str) -> tuple[str | None, str | None]:
    """Rewrite `^^^ message` -> `^^^ Dept/Name: message`.

    Returns (converted_body, None) on success, or (None, reason) if the body
    uses a construct this script does not support.
    """
    out_lines = []
    for line in dedented_body.split("\n"):
        parsed = _try_parse_rubocop_annotation(line)
        if parsed is None:
            out_lines.append(line)
            continue
        if parsed == "BLANK_MARKER":
            return None, "uses RuboCop's `^{}` blank-line offense marker (no nitrocop equivalent)"
        if "[...]" in parsed.message:
            return None, "message uses RuboCop's `[...]` abbreviation (fuzzy match unsupported)"
        out_lines.append(f"{parsed.indent}{parsed.carets} {cop_name}: {parsed.message}")
    return "\n".join(out_lines), None


def strip_annotations(dedented_body: str) -> str:
    out_lines = []
    for line in dedented_body.split("\n"):
        if _try_parse_rubocop_annotation(line) is not None:
            continue
        out_lines.append(line)
    return "\n".join(out_lines)


_UNESCAPED_INTERP_RE = re.compile(r"(?<!\\)#[{@$]")


def unescape_heredoc_body(raw_body: str) -> str:
    return (
        raw_body.replace("\\#{", "#{").replace("\\#@", "#@").replace("\\#$", "#$")
    )


def has_unresolved_interpolation(raw_body: str) -> bool:
    return bool(_UNESCAPED_INTERP_RE.search(raw_body))


# --- trailing-args (filename) parsing ----------------------------------------


@dataclass
class TrailingArgs:
    filename: str | None = None
    unsupported_reason: str | None = None


def parse_trailing_args(trailing_args: str) -> TrailingArgs:
    """Parse the text right after a heredoc's closing delimiter, up to `)`.

    Supported: nothing (just `)`), or a single quoted string (RuboCop's
    `file` positional parameter) optionally followed by `)`.  Anything else
    (keyword arguments used for `%{}`/`^{}`/`_{}` template substitution) is
    reported as unsupported.
    """
    text = trailing_args.strip()
    if text.startswith(")"):
        return TrailingArgs()
    if not text.startswith(","):
        return TrailingArgs(unsupported_reason=f"unrecognized call continuation: {text!r}")
    text = text[1:].strip()
    m = re.match(r"^(['\"])((?:\\.|(?!\1).)*)\1\s*\)", text)
    if m:
        return TrailingArgs(filename=m.group(2))
    return TrailingArgs(unsupported_reason=f"unsupported expect_offense argument(s): {text!r}")


# --- top-level conversion -----------------------------------------------------


@dataclass
class ConvertedExample:
    kind: str  # offense | no_offense
    body: str  # annotation-converted (offense) or verbatim (no_offense), unescaped
    filename: str | None
    config: dict
    corrected_body: str | None  # None if unknown/undetermined
    source_line: int


@dataclass
class SkipReport:
    line: int
    reason: str


def find_cop_name(text: str) -> str | None:
    m = re.search(
        r"RuboCop::Cop::([A-Za-z0-9]+)::([A-Za-z0-9]+)\b",
        text,
    )
    if not m:
        return None
    dept, name = m.group(1), m.group(2)
    return f"{dept}/{name}"


def convert_spec(text: str, cop_name: str | None = None) -> tuple[list[ConvertedExample], list[SkipReport]]:
    if cop_name is None:
        cop_name = find_cop_name(text)
    if cop_name is None:
        raise SpecConversionError("could not determine cop name (no RuboCop::Cop::Dept::Name found)")

    _, calls = scan_spec(text)
    skips: list[SkipReport] = []
    examples: list[ConvertedExample] = []

    # Group calls by their innermost `it` frame (identity) to detect
    # multiple expect_offense/no_offenses calls per example, and to pair a
    # trailing expect_correction/expect_no_corrections with its offense.
    by_it: dict[int, list[Call]] = {}
    order: list[int] = []
    for call in calls:
        it_frames = [f for f in call.frames if f.kind == "it"]
        it_frame = it_frames[-1] if it_frames else None
        key = id(it_frame) if it_frame is not None else id(call)
        if key not in by_it:
            by_it[key] = []
            order.append(key)
        by_it[key].append(call)

    for key in order:
        group = by_it[key]
        primary = group[0]
        line = primary.heredoc.start_line + 1 if primary.heredoc else primary.frames[-1].start_line + 1

        dynamic_frames = [f for f in primary.frames if f.kind in ("shared", "block")]
        if dynamic_frames:
            skips.append(SkipReport(line, "inside shared_examples/it_behaves_like or a parameterized block"))
            continue
        if any(f.kind == "subject_cop" or f.has_custom_subject_cop for f in primary.frames):
            skips.append(SkipReport(line, "custom subject(:cop) override (not the :config idiom)"))
            continue
        skip_frame = next((f for f in primary.frames if f.skip), None)
        if skip_frame is not None:
            skips.append(SkipReport(line, skip_frame.skip_reason or "tagged skip"))
            continue

        primary_calls = [c for c in group if c.kind in ("offense", "no_offense")]
        if len(primary_calls) > 1:
            skips.append(
                SkipReport(line, "multiple expect_offense/expect_no_offenses calls in one example")
            )
            continue
        if not primary_calls:
            continue
        call = primary_calls[0]

        trailing = parse_trailing_args(call.trailing_args)
        if trailing.unsupported_reason:
            skips.append(SkipReport(line, trailing.unsupported_reason))
            continue

        raw_body = call.heredoc.dedented_body
        if has_unresolved_interpolation(raw_body):
            skips.append(SkipReport(line, "heredoc contains unresolved #{}/#@/#$ interpolation"))
            continue
        body = unescape_heredoc_body(raw_body)

        try:
            config = resolve_cop_config(call.frames, cop_name)
        except UnresolvedConfig as exc:
            skips.append(SkipReport(line, str(exc)))
            continue

        corrected_body: str | None = None
        correction_calls = [c for c in group if c.kind == "correction"]
        no_correction_calls = [c for c in group if c.kind == "no_corrections"]
        if call.kind == "offense":
            if correction_calls:
                if len(correction_calls) > 1:
                    skips.append(SkipReport(line, "multiple expect_correction calls in one example"))
                    continue
                craw = correction_calls[0].heredoc.dedented_body
                if has_unresolved_interpolation(craw):
                    skips.append(
                        SkipReport(line, "expect_correction heredoc has unresolved interpolation")
                    )
                    continue
                corrected_body = unescape_heredoc_body(craw)
            elif no_correction_calls:
                corrected_body = strip_annotations(body)

        if call.kind == "offense":
            converted, reason = convert_offense_body(body, cop_name)
            if reason:
                skips.append(SkipReport(line, reason))
                continue
            examples.append(
                ConvertedExample("offense", converted, trailing.filename, config, corrected_body, line)
            )
        else:
            examples.append(
                ConvertedExample("no_offense", body, trailing.filename, config, None, line)
            )

    return examples, skips


# --- fixture assembly ---------------------------------------------------------


def _variant_slug(config: dict) -> str:
    parts = []
    for key in sorted(config):
        value = str(config[key])
        value = re.sub(r"[^A-Za-z0-9]+", "_", value).strip("_").lower()
        parts.append(value)
    slug = "_".join(parts) or "variant"
    if slug[0].isdigit():
        slug = f"v_{slug}"
    return slug


def _config_directive(config: dict) -> str:
    pairs = ", ".join(f"{k}: {config[k]}" for k in sorted(config))
    return f"# nitrocop-config: {pairs}"


@dataclass
class FixtureSet:
    # variant_key (None for base) -> joined offense.rb text
    offense: dict[str | None, str]
    no_offense: dict[str | None, str]
    corrected: dict[str | None, str]
    # scenario name -> (offense body incl. optional filename directive, corrected body or None)
    scenarios: dict[str, tuple[str, str | None]]
    variant_configs: dict[str, dict]  # slug -> config dict, for reporting


def assemble_fixtures(examples: list[ConvertedExample]) -> FixtureSet:
    # variant slug (or None) -> list[ConvertedExample] (excluding filename'd offense examples)
    groups: dict[str | None, list[ConvertedExample]] = {}
    variant_configs: dict[str, dict] = {}
    scenarios: dict[str, tuple[str, str | None]] = {}
    scenario_seq = 0

    for ex in examples:
        slug = None
        if ex.config:
            slug = _variant_slug(ex.config)
            variant_configs[slug] = ex.config

        if ex.kind == "offense" and ex.filename:
            scenario_seq += 1
            name = f"{slug + '_' if slug else ''}scenario_{scenario_seq}"
            body = f"# nitrocop-filename: {ex.filename}\n{ex.body}\n"
            corrected = f"# nitrocop-filename: {ex.filename}\n{ex.corrected_body}\n" if ex.corrected_body is not None else None
            scenarios[name] = (body, corrected)
            continue

        groups.setdefault(slug, []).append(ex)

    offense_out: dict[str | None, str] = {}
    no_offense_out: dict[str | None, str] = {}
    corrected_out: dict[str | None, str] = {}

    for slug, group_examples in groups.items():
        offense_bodies = [e.body for e in group_examples if e.kind == "offense"]
        no_offense_bodies = [e.body for e in group_examples if e.kind == "no_offense"]

        if offense_bodies:
            offense_out[slug] = "\n\n".join(offense_bodies) + "\n"
            corrected_bodies = [e.corrected_body for e in group_examples if e.kind == "offense"]
            if all(c is not None for c in corrected_bodies):
                corrected_out[slug] = "\n\n".join(corrected_bodies) + "\n"
        if no_offense_bodies:
            no_offense_out[slug] = "\n\n".join(no_offense_bodies) + "\n"

    for slug, body in list(offense_out.items()):
        if slug is not None:
            offense_out[slug] = _config_directive(variant_configs[slug]) + "\n" + body
    for slug, body in list(no_offense_out.items()):
        if slug is not None:
            no_offense_out[slug] = _config_directive(variant_configs[slug]) + "\n" + body
    for slug, body in list(corrected_out.items()):
        if slug is not None:
            corrected_out[slug] = _config_directive(variant_configs[slug]) + "\n" + body

    return FixtureSet(offense_out, no_offense_out, corrected_out, scenarios, variant_configs)


def fixture_dir_for(cop_name: str) -> Path:
    dept, _, name = cop_name.partition("/")
    return Path(rs.department_to_dir(dept)) / rs.camel_to_snake(name)


def write_fixtures(fixtures: FixtureSet, out_dir: Path, dry_run: bool) -> list[str]:
    written = []

    def _write(path: Path, content: str):
        written.append(str(path))
        if not dry_run:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content)

    for slug, body in fixtures.offense.items():
        name = "offense.rb" if slug is None else f"offense.{slug}.rb"
        _write(out_dir / name, body)
    for slug, body in fixtures.no_offense.items():
        name = "no_offense.rb" if slug is None else f"no_offense.{slug}.rb"
        _write(out_dir / name, body)
    for slug, body in fixtures.corrected.items():
        name = "corrected.rb" if slug is None else f"corrected.{slug}.rb"
        _write(out_dir / name, body)
    for scenario_name, (body, corrected) in fixtures.scenarios.items():
        _write(out_dir / "offense" / f"{scenario_name}.rb", body)
        if corrected is not None:
            _write(out_dir / "corrected" / f"{scenario_name}.rb", corrected)

    return written


# --- CLI ----------------------------------------------------------------------


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("spec", type=Path, help="Path to a RuboCop cop _spec.rb file")
    parser.add_argument(
        "--out-dir",
        type=Path,
        default=PROJECT_ROOT / "tests" / "fixtures" / "cops",
        help="Fixture root (default: tests/fixtures/cops)",
    )
    parser.add_argument("--cop", default=None, help="Override the detected cop name (Dept/Name)")
    parser.add_argument("--dry-run", action="store_true", help="Print what would be written, write nothing")
    args = parser.parse_args(argv)

    text = args.spec.read_text(encoding="utf-8")
    try:
        examples, skips = convert_spec(text, cop_name=args.cop)
    except SpecConversionError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 1

    cop_name = args.cop or find_cop_name(text)
    fixtures = assemble_fixtures(examples)
    fixture_dir = args.out_dir / fixture_dir_for(cop_name)

    written = write_fixtures(fixtures, fixture_dir, args.dry_run)

    verb = "would write" if args.dry_run else "wrote"
    print(f"{cop_name}: {len(examples)} example(s) converted, {len(skips)} skipped")
    for path in written:
        print(f"  {verb}: {path}")
    if skips:
        print("Skipped:")
        for s in skips:
            print(f"  spec line {s.line}: {s.reason}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
