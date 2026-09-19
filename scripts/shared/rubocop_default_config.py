#!/usr/bin/env python3
"""Read a single cop's block out of a vendored `config/default.yml`.

RuboCop's `config/default.yml` embeds `!ruby/regexp` (and occasionally other
`!ruby/*`) YAML tags that PyYAML's `SafeLoader` refuses to parse. This mirrors
the workaround already used by `docs/planning/census.py` / the gap-analysis
tooling: a `SafeLoader` subclass that treats any `!ruby/*` tag as a plain
scalar/sequence/mapping instead of raising.
"""

from __future__ import annotations

import re
from typing import Any

import yaml


class _RubyTagTolerantLoader(yaml.SafeLoader):
    """A SafeLoader that ignores `!ruby/*` tags instead of raising."""


def _construct_ruby_tag(loader: yaml.SafeLoader, tag_suffix: str, node: yaml.Node) -> Any:
    if isinstance(node, yaml.ScalarNode):
        return loader.construct_scalar(node)
    if isinstance(node, yaml.SequenceNode):
        return loader.construct_sequence(node)
    if isinstance(node, yaml.MappingNode):
        return loader.construct_mapping(node)
    return None


_RubyTagTolerantLoader.add_multi_constructor("!ruby/", _construct_ruby_tag)


def load_default_yml(text: str) -> dict:
    """Parse a full `config/default.yml` document, tolerating `!ruby/*` tags."""
    return yaml.load(text, Loader=_RubyTagTolerantLoader) or {}


def extract_cop_block_text(default_yml_text: str, cop_name: str) -> str | None:
    """Return the raw YAML text of the `<cop_name>:` top-level block, or None."""
    lines = default_yml_text.split("\n")
    out: list[str] = []
    capturing = False
    header_re = re.compile(rf"^{re.escape(cop_name)}:\s*$")
    next_top_level_re = re.compile(r"^[A-Za-z]")
    for line in lines:
        if not capturing:
            if header_re.match(line):
                capturing = True
                out.append(line)
            continue
        if next_top_level_re.match(line):
            break
        out.append(line)
    if not out:
        return None
    return "\n".join(out)


def load_cop_config(default_yml_text: str, cop_name: str) -> dict | None:
    """Parse just the named cop's config block into a dict, or None if absent."""
    block = extract_cop_block_text(default_yml_text, cop_name)
    if block is None:
        return None
    doc = load_default_yml(block)
    return doc.get(cop_name)
