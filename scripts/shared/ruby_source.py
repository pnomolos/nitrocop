#!/usr/bin/env python3
"""Shared Ruby-source parsing heuristics for the Cop IR translation pipeline.

Ported (and unified) from two independent, already-proven implementations in
this repo:

- `src/node_pattern/extract.rs` (`extract_patterns` / `cop_name_from_path`) —
  a line-based, heredoc-aware scanner for `def_node_matcher`/`def_node_search`
  that also records the bound method name.
- `docs/planning/census.py` (on the `planning/program-status` branch) — regex
  feature-detection for `on_*` hooks, `include`/`extend`/`prepend` mixins,
  `cop_config` usage, autocorrect/source-text markers, and a regex-based
  (name-discarding) matcher-body stripper used to compute "code LOC excluding
  matcher bodies".

Both scripts/workflows/ir_extract.py and scripts/spec_to_fixture.py build on
this module rather than re-implementing Ruby-source scanning.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from enum import Enum

# --- name <-> path conversions ----------------------------------------------

# Department (PascalCase, as it appears in "Dept/Name") -> the directory name
# under lib/rubocop/cop/ in the owning gem. Mirrors census.py's GEMS table;
# camel_to_snake() alone gets this wrong for acronym-shaped departments
# (RSpec -> "r_spec" instead of "rspec"), so known departments are looked up
# directly and anything unrecognized falls back to the generic algorithm.
DEPT_TO_DIR: dict[str, str] = {
    "Bundler": "bundler",
    "Gemspec": "gemspec",
    "Layout": "layout",
    "Lint": "lint",
    "Metrics": "metrics",
    "Migration": "migration",
    "Naming": "naming",
    "Security": "security",
    "Style": "style",
    "InternalAffairs": "internal_affairs",
    "Rails": "rails",
    "RSpec": "rspec",
    "Performance": "performance",
    "Rake": "rake",
    "FactoryBot": "factory_bot",
    "RSpecRails": "rspec_rails",
}

_CAMEL_RE_1 = re.compile(r"(.)([A-Z][a-z]+)")
_CAMEL_RE_2 = re.compile(r"([a-z0-9])([A-Z])")


def camel_to_snake(name: str) -> str:
    """Convert a PascalCase/camelCase identifier to snake_case.

    Good enough for RuboCop cop names (verified against all 5 golden-test
    cops and the 23 upstream pilot cops in docs/planning/01-gap-analysis.md
    §4); it is not acronym-aware, so a department name should be looked up in
    `DEPT_TO_DIR` first.
    """
    s1 = _CAMEL_RE_1.sub(r"\1_\2", name)
    s2 = _CAMEL_RE_2.sub(r"\1_\2", s1)
    return s2.lower()


def department_to_dir(dept: str) -> str:
    """Map a `Dept` (as in `Dept/Name`) to its lib/rubocop/cop/ subdirectory."""
    return DEPT_TO_DIR.get(dept, camel_to_snake(dept))


def snake_to_pascal(s: str) -> str:
    """Inverse of department_to_dir/camel_to_snake for known acronyms.

    Ported from census.py's `snake_to_pascal`.
    """
    if s == "rspec":
        return "RSpec"
    if s == "rspec_rails":
        return "RSpecRails"
    return "".join(word[:1].upper() + word[1:] for word in s.split("_") if word)


def cop_name_from_snake_path(dept_dir: str, snake_name: str) -> str:
    """Build "Dept/Name" from a department directory name and snake_case cop name."""
    dept = next((d for d, dir_ in DEPT_TO_DIR.items() if dir_ == dept_dir), None)
    if dept is None:
        dept = snake_to_pascal(dept_dir)
    return f"{dept}/{snake_to_pascal(snake_name)}"


# --- def_node_matcher / def_node_search extraction --------------------------
# Ported line-for-line from src/node_pattern/extract.rs, which is already
# proven against every vendored cop file (used by the Rust `--validate-ir`
# / pattern-DB tooling). Kept behaviorally identical on purpose so
# `ir_extract.py`'s notion of "the patterns in this file" never disagrees
# with the Rust side.


class PatternKind(Enum):
    MATCHER = "matcher"
    SEARCH = "search"


@dataclass
class ExtractedPattern:
    kind: PatternKind
    method_name: str
    pattern: str
    n_captures: int = field(init=False)

    def __post_init__(self) -> None:
        self.n_captures = self.pattern.count("$")


def _parse_method_name_and_rest(rest: str) -> tuple[str, str] | None:
    rest = rest.strip()
    if rest.startswith(":"):
        rest2 = rest[1:]
        comma_pos = rest2.find(",")
        if comma_pos == -1:
            return None
        method_name = rest2[:comma_pos].strip()
        after_comma = rest2[comma_pos + 1 :].strip()
        return method_name, after_comma
    if rest.startswith("'") or rest.startswith('"'):
        quote = rest[0]
        inner = rest[1:]
        end = inner.find(quote)
        if end == -1:
            return None
        method_name = inner[:end].strip()
        after_name = inner[end + 1 :].strip()
        if not after_name.startswith(","):
            return None
        after_comma = after_name[1:].strip()
        return method_name, after_comma
    return None


def _strip_heredoc_trailing_comment(delimiter: str) -> str:
    hash_pos = delimiter.find("#")
    if hash_pos != -1:
        return delimiter[:hash_pos].strip()
    return delimiter


def extract_patterns(source: str) -> list[ExtractedPattern]:
    """Extract `def_node_matcher`/`def_node_search` definitions from Ruby source.

    Handles both the heredoc form (`def_node_matcher :name, <<~PATTERN ... PATTERN`)
    and the inline string form (`def_node_matcher :name, '...'`).
    """
    results: list[ExtractedPattern] = []
    lines = source.split("\n")
    i = 0
    n = len(lines)
    prefixes = (
        ("def_node_matcher", PatternKind.MATCHER),
        ("def_node_search", PatternKind.SEARCH),
    )
    while i < n:
        trimmed = lines[i].strip()
        for prefix, kind in prefixes:
            if not trimmed.startswith(prefix):
                continue
            rest = trimmed[len(prefix) :]
            parsed = _parse_method_name_and_rest(rest)
            if parsed is None:
                continue
            method_name, after_comma = parsed
            if after_comma.startswith("<<~") or after_comma.startswith("<<-"):
                raw_delim = _strip_heredoc_trailing_comment(after_comma[3:])
                delimiter = raw_delim.strip().strip("'").strip('"')
                pattern_lines: list[str] = []
                i += 1
                while i < n:
                    line = lines[i].strip()
                    if line == delimiter:
                        break
                    pattern_lines.append(line)
                    i += 1
                pattern = "\n".join(pattern_lines)
                results.append(ExtractedPattern(kind, method_name, pattern))
            elif after_comma.startswith("'") or after_comma.startswith('"'):
                quote = after_comma[0]
                inner = after_comma[1:]
                end = inner.rfind(quote)
                if end != -1:
                    pattern = inner[:end]
                    results.append(ExtractedPattern(kind, method_name, pattern))
            break
        i += 1
    return results


# --- census.py-style regex feature detection ---------------------------------
# Ported from docs/planning/census.py (planning/program-status branch).

# Every Parser-gem node type RuboCop's Commissioner can dispatch on (from
# `parser`'s `Parser::Meta::NODE_TYPES`, pinned as of parser 3.3.x), plus the
# framework's own non-node callbacks. `def on_<x>`/`def after_<x>` is only a
# *real* AST hook when `<x>` is in this set — RuboCop cops are otherwise free
# to name private helper methods `on_something`/`after_something` (e.g.
# `Lint/DuplicateMethods#on_delegate`, `#on_attr`), which the framework never
# invokes as callbacks since "delegate"/"attr" are not node types. Filtering
# on this set avoids treating those as false-positive hooks.
_NODE_TYPE_HOOK_NAMES = frozenset(
    """
    true false nil int float str dstr
    sym dsym xstr regopt regexp array splat
    pair kwsplat hash irange erange self
    lvar ivar cvar gvar const defined? lvasgn
    ivasgn cvasgn gvasgn casgn mlhs masgn
    op_asgn and_asgn ensure rescue arg_expr
    or_asgn back_ref nth_ref
    match_with_lvasgn match_current_line
    module class sclass def defs undef alias args
    cbase arg optarg restarg blockarg block_pass kwarg kwoptarg
    kwrestarg kwnilarg send csend super zsuper yield block
    and not or if when case while until while_post
    until_post for break next redo return resbody
    kwbegin begin retry preexe postexe iflipflop eflipflop
    shadowarg complex rational __FILE__ __LINE__ __ENCODING__
    ident lambda indexasgn index procarg0
    restarg_expr blockarg_expr
    objc_kwarg objc_restarg objc_varargs
    numargs numblock forward_args forwarded_args forward_arg
    case_match in_match in_pattern
    match_var pin match_alt match_as match_rest
    array_pattern match_with_trailing_comma array_pattern_with_tail
    hash_pattern const_pattern if_guard unless_guard match_nil_pattern
    empty_else find_pattern kwargs
    match_pattern_p match_pattern
    forwarded_restarg forwarded_kwrestarg
    itarg itblock
    numblock itblock any_block
    """.split()
)
# RuboCop::Cop::Base's own lifecycle callbacks (src: `lib/rubocop/cop/base.rb`),
# not tied to any single node type.
_FRAMEWORK_HOOK_NAMES = frozenset(
    {"new_investigation", "investigation_end"}
)
_ALL_HOOK_NAMES = _NODE_TYPE_HOOK_NAMES | _FRAMEWORK_HOOK_NAMES

HOOK_DEF_RE = re.compile(r"^\s*def\s+(on_[a-z0-9_?!]+|after_[a-z0-9_?!]+)\b")
HOOK_ALIAS_RE = re.compile(r"\balias(?:_method)?\s+:?(?P<a>on_[a-z0-9_?!]+)\s*,?\s+:?(?P<b>on_[a-z0-9_?!]+)")


def _is_real_hook(name: str) -> bool:
    for prefix in ("on_", "after_"):
        if name.startswith(prefix):
            return name[len(prefix) :] in _ALL_HOOK_NAMES
    return False
INCLUDE_EXTEND_RE = re.compile(r"^\s*(include|extend|prepend)\s+([A-Z][\w:]*)")
COP_CONFIG_KEY_RE = re.compile(
    r"cop_config(?:\.fetch)?\s*[\[\.]\s*['\"]?(?P<key>[A-Za-z_][A-Za-z0-9_]*)['\"]?\s*[\],)]"
)
CONFIG_USAGE_RE = re.compile(r"\bcop_config\s*[\[\.]")
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

# A stricter subset of SOURCE_TEXT_MARKERS for the Stage-2 classifier's hard
# disqualifier (docs/planning/04-cop-ir-design.md §5 "references
# processed_source/tokens/comments"). Deliberately excludes "source_range":
# `node.source_range` is an ordinary per-node AST accessor nearly every
# autocorrecting cop calls (e.g. `Style/TimeNow`'s
# `node.loc.selector.join(node.source_range.end)`), not a sign the cop reads
# the file-level token/comment stream — using the broader SOURCE_TEXT_MARKERS
# set as a hard gate misclassifies most bucket-A/B autocorrect cops as C.
FILE_LEVEL_SOURCE_MARKERS = (
    "processed_source",
    ".comments",
    ".tokens",
    "each_token",
    "each_comment",
    "each_line",
)


def uses_file_level_source_text(source: str) -> bool:
    return any(marker in source for marker in FILE_LEVEL_SOURCE_MARKERS)

# def_node_matcher/def_node_search removal for LOC/body purposes only (name
# discarded) — used to compute "code LOC excluding matcher bodies", exactly
# as census.py's cops.csv `code_loc` column does.
_HEREDOC_MATCHER_RE = re.compile(
    r"def_node_(?:matcher|search)\(?\s*:[\w?!=]+\s*,\s*<<[-~]?(?P<tag>['\"]?)(?P<delim>\w+)(?P=tag)\s*\n"
    r"(?P<body>.*?)\n\s*(?P=delim)",
    re.DOTALL,
)
_STRING_MATCHER_RE = re.compile(
    r"def_node_(?:matcher|search)\(?\s*:[\w?!=]+\s*,\s*"
    r"(?P<q>['\"])(?P<body>(?:\\.|(?!(?P=q)).)*)(?P=q)"
)


def strip_matcher_defs(text: str) -> str:
    """Remove def_node_matcher/def_node_search bodies from Ruby source text."""
    text = _HEREDOC_MATCHER_RE.sub("", text)
    text = _STRING_MATCHER_RE.sub("", text)
    return text


def code_lines(text: str) -> int:
    """Count non-blank, non-comment-only lines."""
    n = 0
    for line in text.splitlines():
        s = line.strip()
        if not s or s.startswith("#"):
            continue
        n += 1
    return n


def find_hooks(source: str) -> set[str]:
    """Find real `on_*`/`after_*` AST-dispatch hooks defined or aliased in `source`.

    Filters out cop-private helper methods that merely start with `on_`/`after_`
    but do not name a real Parser-gem node type or framework callback (see
    `_ALL_HOOK_NAMES`).
    """
    hooks: set[str] = set()
    for line in source.splitlines():
        m = HOOK_DEF_RE.match(line)
        if m and _is_real_hook(m.group(1)):
            hooks.add(m.group(1))
    for m in HOOK_ALIAS_RE.finditer(source):
        if _is_real_hook(m.group("a")):
            hooks.add(m.group("a"))
    return hooks


def find_mixins(source: str) -> set[str]:
    mixins: set[str] = set()
    for line in source.splitlines():
        m = INCLUDE_EXTEND_RE.match(line)
        if m:
            mixins.add(m.group(2))
    return mixins


def find_cop_config_keys(source: str) -> set[str]:
    return {m.group("key") for m in COP_CONFIG_KEY_RE.finditer(source)}


def uses_source_text(source: str) -> bool:
    return any(marker in source for marker in SOURCE_TEXT_MARKERS)


def uses_autocorrect(source: str) -> bool:
    return any(marker in source for marker in AUTOCORRECT_MARKERS)


# --- Ruby literal parsing (a strict subset) ----------------------------------
# Understands what actually shows up in RuboCop `CONST = ... .freeze` /
# `RESTRICT_ON_SEND = ...` / `MSG = '...'` statements: %i[]/%w[]/%I[]/%W[]
# percent literals, [...]  arrays, {...} hashes (both `key: value` and
# `'key' => value` forms), string/symbol/int/float/bool/nil literals.
# Anything outside this subset raises ValueError so callers can fail closed
# (skip / flag) instead of silently emitting a wrong constant.

_PERCENT_LITERAL_RE = re.compile(r"^%([iIwW])\[(.*)\]$", re.DOTALL)


class RubyLiteralError(ValueError):
    """Raised when `parse_ruby_literal` encounters unsupported Ruby syntax."""


def _strip_freeze(text: str) -> str:
    text = text.strip()
    if text.endswith(".freeze"):
        text = text[: -len(".freeze")].strip()
    return text


def _unwrap_parens(text: str) -> str:
    """Strip one layer of enclosing `(...)` if it wraps the whole string."""
    if not (text.startswith("(") and text.endswith(")")):
        return text
    depth = 0
    for i, c in enumerate(text):
        if c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
            if depth == 0 and i != len(text) - 1:
                return text  # the opening '(' closes before the end — not a full wrap
    return text[1:-1].strip()


def parse_ruby_literal(text: str):
    """Parse a (small, strict) subset of Ruby literal syntax into a Python value."""
    text = _strip_freeze(text)
    text = text.strip()
    text = _unwrap_parens(text)
    if not text:
        raise RubyLiteralError("empty literal")

    m = _PERCENT_LITERAL_RE.match(text)
    if m:
        kind, inner = m.group(1), m.group(2)
        words = inner.split()
        if kind in ("i", "I"):
            return [f":{w}" if not w.startswith(":") else w for w in words]
        return list(words)

    if text.startswith("[") and text.endswith("]"):
        return [parse_ruby_literal(p) for p in _split_top_level(text[1:-1], ",")]

    if text.startswith("{") and text.endswith("}"):
        inner = text[1:-1].strip()
        result: dict = {}
        if not inner:
            return result
        for pair in _split_top_level(inner, ","):
            pair = pair.strip()
            if not pair:
                continue
            if "=>" in pair:
                key_txt, _, val_txt = pair.partition("=>")
            elif ":" in pair:
                key_txt, _, val_txt = pair.partition(":")
                key_txt = key_txt.strip()
                if key_txt.startswith(('"', "'")):
                    pass
                else:
                    key_txt = f":{key_txt}"
            else:
                raise RubyLiteralError(f"unrecognized hash pair: {pair!r}")
            key = parse_ruby_literal(key_txt.strip())
            value = parse_ruby_literal(val_txt.strip())
            key_s = key[1:] if isinstance(key, str) and key.startswith(":") else key
            result[key_s] = value
        return result

    if text.startswith(":"):
        return text  # keep the leading ':' — callers treat symbols as strings prefixed with ':'

    if (text.startswith("'") and text.endswith("'")) or (
        text.startswith('"') and text.endswith('"')
    ):
        return text[1:-1]

    if text in ("true", "false"):
        return text == "true"
    if text == "nil":
        return None

    try:
        if re.fullmatch(r"-?\d+", text):
            return int(text)
        if re.fullmatch(r"-?\d+\.\d+", text):
            return float(text)
    except ValueError:
        pass

    raise RubyLiteralError(f"unsupported Ruby literal: {text!r}")


def _split_top_level(text: str, sep: str) -> list[str]:
    """Split `text` on `sep`, ignoring separators nested inside (), [], {}, quotes."""
    parts: list[str] = []
    depth = 0
    quote: str | None = None
    current: list[str] = []
    i = 0
    while i < len(text):
        c = text[i]
        if quote:
            current.append(c)
            if c == "\\" and i + 1 < len(text):
                current.append(text[i + 1])
                i += 2
                continue
            if c == quote:
                quote = None
            i += 1
            continue
        if c in "'\"":
            quote = c
            current.append(c)
        elif c in "([{":
            depth += 1
            current.append(c)
        elif c in ")]}":
            depth -= 1
            current.append(c)
        elif c == sep and depth == 0:
            parts.append("".join(current))
            current = []
        else:
            current.append(c)
        i += 1
    if current:
        parts.append("".join(current))
    return parts


def find_constant_assignment(source: str, name: str) -> str | None:
    """Find `NAME = <rhs>` (possibly spanning multiple lines) and return the RHS text.

    Only matches simple `CONST = <literal>` forms at the start of a (stripped)
    line — good enough for RuboCop's `FOO = {...}.freeze` / `%i[...]` style
    module-level constants.
    """
    pattern = re.compile(rf"^\s*{re.escape(name)}\s*=\s*(.+)$")
    lines = source.split("\n")
    for i, line in enumerate(lines):
        m = pattern.match(line)
        if not m:
            continue
        rhs_lines = [m.group(1)]
        depth = _bracket_delta(m.group(1))
        j = i
        while depth > 0 and j + 1 < len(lines):
            j += 1
            rhs_lines.append(lines[j])
            depth += _bracket_delta(lines[j])
        return "\n".join(rhs_lines).strip()
    return None


def _bracket_delta(text: str) -> int:
    # Ignore brackets inside string/symbol literals for this rough scan.
    depth = 0
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
        elif c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
        i += 1
    return depth


# --- heredoc scanning (generic; used by spec_to_fixture.py) -----------------


@dataclass
class Heredoc:
    start_line: int  # 0-indexed line containing the opening marker
    end_line: int  # 0-indexed line containing the closing delimiter
    delimiter: str
    squiggly: bool
    body_lines: list[str]  # raw lines, NOT dedented
    opener_tail: str = ""  # text after the heredoc marker on the opening line

    @property
    def dedented_body(self) -> str:
        if not self.squiggly:
            return "\n".join(self.body_lines)
        indents = [
            len(line) - len(line.lstrip(" "))
            for line in self.body_lines
            if line.strip() != ""
        ]
        strip = min(indents) if indents else 0
        return "\n".join(line[strip:] if len(line) >= strip else line for line in self.body_lines)


_HEREDOC_OPEN_RE = re.compile(r"<<([~-]?)(['\"]?)([A-Za-z_][A-Za-z0-9_]*)\2")


def find_heredoc(lines: list[str], from_line: int, from_col: int) -> Heredoc | None:
    """Find the next heredoc marker at or after (from_line, from_col) and return it.

    Scans a single line (`lines[from_line][from_col:]`) for a `<<~FOO` /
    `<<-FOO` / `<<FOO` opener, then consumes subsequent lines up to (and
    including) the terminator line. Returns None if no opener is found on
    that line.
    """
    m = _HEREDOC_OPEN_RE.search(lines[from_line], from_col)
    if not m:
        return None
    squiggly = m.group(1) == "~"
    delimiter = m.group(3)
    opener_tail = lines[from_line][m.end() :]
    body: list[str] = []
    i = from_line + 1
    while i < len(lines):
        if lines[i].strip() == delimiter:
            return Heredoc(from_line, i, delimiter, squiggly, body, opener_tail)
        body.append(lines[i])
        i += 1
    # Unterminated heredoc — treat everything to EOF as the body.
    return Heredoc(from_line, len(lines) - 1, delimiter, squiggly, body, opener_tail)
