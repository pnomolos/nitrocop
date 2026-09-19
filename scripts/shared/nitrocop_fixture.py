#!/usr/bin/env python3
"""A Python re-implementation of nitrocop's own fixture format parser.

Mirrors `src/testutil.rs::parse_fixture` / `try_parse_annotation` /
`try_parse_expect_annotation` / `try_parse_filename_directive` field-for-field
(not a wrapper around the Rust code — there is no Python binding for it, and
building the `nitrocop` binary is out of scope for the pure-Python IR
translation pipeline). Used to round-trip-validate fixtures this pipeline
generates: if this parser (kept deliberately faithful to testutil.rs) can
recover the expected offenses from a generated fixture, `cargo test`'s
`assert_cop_offenses_full` almost certainly can too.

Kept here (not duplicated) so both `scripts/spec_to_fixture.py`'s tests and
any future caller share one implementation.
"""

from __future__ import annotations

from dataclasses import dataclass, field


@dataclass
class ExpectedOffense:
    line: int
    column: int
    cop_name: str
    message: str


@dataclass
class ParsedFixture:
    source: str
    expected: list[ExpectedOffense] = field(default_factory=list)
    filename: str | None = None


def _try_parse_annotation(line: str) -> tuple[int, str, str] | None:
    """Mirrors `try_parse_annotation` in src/testutil.rs. Returns (column, cop_name, message)."""
    trimmed = line.lstrip(" \t")
    if not trimmed.startswith("^"):
        return None
    caret_count = 0
    while caret_count < len(trimmed) and trimmed[caret_count] == "^":
        caret_count += 1
    after_carets = trimmed[caret_count:]
    if not after_carets.startswith(" "):
        return None
    rest = after_carets[1:].rstrip()
    colon_space = rest.find(": ")
    if colon_space == -1:
        return None
    cop_name = rest[:colon_space]
    message = rest[colon_space + 2 :]
    if "/" not in cop_name:
        return None
    column = len(line) - len(trimmed)
    return column, cop_name, message


def _try_parse_filename_directive(line: str) -> str | None:
    prefix = "# nitrocop-filename: "
    if line.startswith(prefix):
        return line[len(prefix) :].rstrip()
    return None


def _try_parse_expect_annotation(line: str) -> ExpectedOffense | None:
    prefix = "# nitrocop-expect: "
    if not line.startswith(prefix):
        return None
    rest = line[len(prefix) :]
    space_idx = rest.find(" ")
    if space_idx == -1:
        return None
    loc_part = rest[:space_idx]
    colon_idx = loc_part.find(":")
    if colon_idx == -1:
        return None
    try:
        line_num = int(loc_part[:colon_idx])
        column = int(loc_part[colon_idx + 1 :])
    except ValueError:
        return None
    after_loc = rest[space_idx + 1 :].rstrip()
    colon_space = after_loc.find(": ")
    if colon_space == -1:
        return None
    cop_name = after_loc[:colon_space]
    message = after_loc[colon_space + 2 :]
    if "/" not in cop_name:
        return None
    return ExpectedOffense(line=line_num, column=column, cop_name=cop_name, message=message)


def parse_fixture(raw: str) -> ParsedFixture:
    """Faithful port of `src/testutil.rs::parse_fixture`."""
    elements = raw.split("\n")
    source_lines: list[str] = []
    expected: list[ExpectedOffense] = []
    filename: str | None = None

    start_idx = 0
    if elements:
        maybe_name = _try_parse_filename_directive(elements[0])
        if maybe_name is not None:
            filename = maybe_name
            start_idx = 1

    for raw_idx in range(start_idx, len(elements)):
        element = elements[raw_idx]
        expect = _try_parse_expect_annotation(element)
        if expect is not None:
            expected.append(expect)
            continue

        annotation = _try_parse_annotation(element)
        if annotation is not None:
            if not source_lines:
                raise ValueError(
                    f"Annotation on raw line {raw_idx + 1} appears before any source line: {element!r}"
                )
            column, cop_name, message = annotation
            source_line_number = len(source_lines)
            expected.append(
                ExpectedOffense(line=source_line_number, column=column, cop_name=cop_name, message=message)
            )
        else:
            source_lines.append(element)

    return ParsedFixture(source="\n".join(source_lines), expected=expected, filename=filename)


def parse_variant_fixture(raw: str) -> tuple[dict, str]:
    """Mirrors `src/testutil.rs::parse_variant_fixture`."""
    lines = raw.split("\n")
    first_line = lines[0] if lines else ""
    prefix = "# nitrocop-config: "
    if not first_line.startswith(prefix):
        raise ValueError(f"Variant fixture must start with '# nitrocop-config: ...' but got: {first_line!r}")
    config_str = first_line[len(prefix) :]
    options = {}
    for pair in config_str.split(", "):
        pair = pair.strip()
        if ": " in pair:
            key, value = pair.split(": ", 1)
            options[key.strip()] = value.strip()
    rest = raw[len(first_line) :]
    if rest.startswith("\n"):
        rest = rest[1:]
    return options, rest
