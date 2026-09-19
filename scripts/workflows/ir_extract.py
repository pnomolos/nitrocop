#!/usr/bin/env python3
"""Cop IR translation pipeline — Stage 1: extract.

Deterministically extracts everything statically knowable from a RuboCop cop's
Ruby source and its `config/default.yml` entry: `def_node_matcher`/
`def_node_search` patterns (verbatim), `%CONST`/`RESTRICT_ON_SEND` constants,
`MSG*` message constants (with `%<x>s` -> `%{x}` conversion), `on_*` hooks,
mixins, `cop_config` keys, the config/default.yml entry, whether the cop
extends `AutoCorrector`, and `#helper?` names referenced by patterns that are
not local matchers (candidates for `predicates:`/builtins).

No LLM. See docs/planning/04-cop-ir-design.md §5 Stage 1 (on the
`planning/program-status` branch) for the design this implements.

Usage:
    python3 scripts/workflows/ir_extract.py Style/HashExcept
    python3 scripts/workflows/ir_extract.py Style/TimeNow RSpec/MatchWithSimpleRegex \\
        --plugin-root vendor/rubocop-rspec --out-dir build/ir

Output (per cop): an extraction record (JSON) and a partial `<snake>.cop.yml`
skeleton. Both are always produced; `--out-dir` additionally writes them to
`<out-dir>/<Dept>/<snake_name>/{extract.json,skeleton.cop.yml}`. Without
`--out-dir`, the extraction record (which embeds the skeleton YAML text under
`"skeleton_yaml"`) is printed to stdout as JSON.

The skeleton is intentionally never a finished cop: `hooks[].when` and
`hooks[].offense` (message/location/correct) are left for Stage 3 (synthesis,
`ir_synth.py`, not implemented here) to fill in. `--validate-skeleton` checks
the parts of the skeleton that *are* filled in (meta, config, matchers,
constants, predicates, and each hook's `on:`/`match:`) against
`scripts/shared/ir_schema.json`, using a relaxed copy of the schema that does
not require the still-TODO `offense` key — a full (strict) validation of an
extract-only skeleton is expected to fail, by design.
"""

from __future__ import annotations

import argparse
import copy
import json
import re
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
SCRIPTS_ROOT = SCRIPT_DIR.parent
sys.path.insert(0, str(SCRIPTS_ROOT))

from shared import rubocop_default_config as rdc  # noqa: E402
from shared import ruby_source as rs  # noqa: E402

PROJECT_ROOT = SCRIPTS_ROOT.parent
IR_SCHEMA_PATH = SCRIPTS_ROOT / "shared" / "ir_schema.json"

# config/default.yml keys that are metadata, not a declared `config:` option.
_CONFIG_META_KEYS = {
    "Description",
    "StyleGuide",
    "Reference",
    "Enabled",
    "Safe",
    "SafeAutoCorrect",
    "VersionAdded",
    "VersionChanged",
    "AutoCorrect",
    "Severity",
    "Include",
    "Exclude",
    "Changelog",
    "Deprecated",
    "Autocorrect",  # some cops spell it lowercase-c historically
}

_MSG_CONST_RE = re.compile(r"^\s*(MSG(?:_[A-Z0-9_]+)?)\s*=")
_TOP_LEVEL_CONST_RE = re.compile(r"^\s{0,8}([A-Z][A-Z0-9_]*)\s*=\s*(.+)$")
_FORMAT_SPEC_RE = re.compile(r"%<([A-Za-z_][A-Za-z0-9_]*)>([a-zA-Z])")
_HELPER_CALL_RE = re.compile(r"#([A-Za-z_][A-Za-z0-9_]*[?!]?)")


class ExtractError(Exception):
    """Raised when a cop's source file or config entry cannot be located."""


def find_cop_source(cop: str, roots: list[Path]) -> tuple[Path, Path]:
    """Return (source_path, gem_root) for `cop` by searching `roots` in order."""
    dept, _, name = cop.partition("/")
    if not dept or not name:
        raise ExtractError(f"cop name must be 'Dept/Name', got {cop!r}")
    dept_dir = rs.department_to_dir(dept)
    snake_name = rs.camel_to_snake(name)
    tried = []
    for root in roots:
        candidate = root / "lib" / "rubocop" / "cop" / dept_dir / f"{snake_name}.rb"
        tried.append(candidate)
        if candidate.is_file():
            return candidate, root
    raise ExtractError(
        f"could not find source for {cop} (dept_dir={dept_dir}, snake={snake_name}); tried:\n"
        + "\n".join(f"  {p}" for p in tried)
    )


def find_default_yml(gem_root: Path) -> Path | None:
    candidate = gem_root / "config" / "default.yml"
    return candidate if candidate.is_file() else None


def convert_message_format(msg: str) -> tuple[str, bool]:
    """Convert `%<name>s` -> `%{name}` and `%%` -> `%`.

    Returns (converted, fully_mechanical) — `fully_mechanical` is False when a
    `%<name>` placeholder used flags/width/precision this mechanical converter
    does not attempt (design risk #2: "anything else is a needs_human flag").
    """
    fully_mechanical = True
    # Flag any %<name>... occurrence with extra format flags (not just a bare
    # single conversion letter) as non-mechanical, without altering it.
    for m in re.finditer(r"%<[A-Za-z_][A-Za-z0-9_]*>([^a-zA-Z%]*)([a-zA-Z])", msg):
        if m.group(1):  # width/precision/flags present
            fully_mechanical = False
    converted = _FORMAT_SPEC_RE.sub(lambda m: "%{" + m.group(1) + "}", msg)
    converted = converted.replace("%%", "%")
    return converted, fully_mechanical


def strip_symbol(value: str) -> str:
    return value[1:] if isinstance(value, str) and value.startswith(":") else value


def find_top_level_constants(source: str) -> dict[str, str]:
    """Return {CONST_NAME: raw_rhs_text} for top-level `CONST = ...` assignments.

    Deliberately shallow (indentation <= 8 columns) so method-local `CONST =`
    inside a `def` body (rare, but happens) is not mistaken for a class-level
    constant table.
    """
    names: dict[str, str] = {}
    for line in source.splitlines():
        m = _TOP_LEVEL_CONST_RE.match(line)
        if m and not m.group(1).startswith("MSG"):
            names.setdefault(m.group(1), None)
    result = {}
    for name in names:
        rhs = rs.find_constant_assignment(source, name)
        if rhs is not None:
            result[name] = rhs
    return result


def find_message_constants(source: str) -> dict[str, str]:
    names = []
    for line in source.splitlines():
        m = _MSG_CONST_RE.match(line)
        if m:
            names.append(m.group(1))
    result = {}
    for name in names:
        rhs = rs.find_constant_assignment(source, name)
        if rhs is None:
            continue
        try:
            literal = rs.parse_ruby_literal(rhs)
        except rs.RubyLiteralError:
            continue
        if isinstance(literal, str):
            result[name] = literal
    return result


def derive_config_options(cop_config: dict) -> dict:
    """Turn a parsed config/default.yml cop block into IR `config:` declarations."""
    enforced_keys = [k for k in cop_config if k.startswith("Enforced")]
    supported_keys_consumed = set()
    options: dict = {}
    for key in enforced_keys:
        suffix = key[len("Enforced") :]
        # RuboCop's convention pluralizes "Style" when deriving the Supported*
        # counterpart: EnforcedStyle -> SupportedStyles,
        # EnforcedStyleForFoo -> SupportedStylesForFoo. Try the literal
        # suffix first, then the pluralized-Style form, and use whichever
        # actually resolves to a list in this cop's config.
        candidates = [suffix]
        if "Style" in suffix:
            candidates.append(suffix.replace("Style", "Styles", 1))
        for candidate_suffix in candidates:
            supported_key = "Supported" + candidate_suffix
            supported = cop_config.get(supported_key)
            if isinstance(supported, list):
                options[key] = {
                    "type": "enum",
                    "values": [str(v) for v in supported],
                    "default": cop_config[key],
                }
                supported_keys_consumed.add(supported_key)
                break

    for key, value in cop_config.items():
        if key in _CONFIG_META_KEYS or key in supported_keys_consumed or key in options:
            continue
        if key.startswith("Supported"):
            # An orphaned SupportedX with no matching EnforcedX; not a real option.
            continue
        options[key] = _infer_option_type(value)
    return options


def _infer_option_type(value) -> dict:
    if isinstance(value, bool):
        return {"type": "bool", "default": value}
    if isinstance(value, int):
        return {"type": "int", "default": value}
    if isinstance(value, float):
        return {"type": "float", "default": value}
    if isinstance(value, list):
        return {"type": "string_array", "default": [str(v) for v in value]}
    if isinstance(value, dict):
        return {"type": "string_map", "default": {str(k): str(v) for k, v in value.items()}}
    return {"type": "string", "default": "" if value is None else str(value)}


def derive_autocorrect_mode(extends_autocorrector: bool, cop_config: dict) -> str:
    if not extends_autocorrector:
        return "none"
    safe = cop_config.get("Safe", True)
    safe_autocorrect = cop_config.get("SafeAutoCorrect", safe)
    return "safe" if safe_autocorrect else "unsafe"


def derive_enabled_default(cop_config: dict):
    enabled = cop_config.get("Enabled", "pending")
    if enabled in (True, False, "pending"):
        return enabled
    return "pending"


def find_helper_predicates(matchers: list[rs.ExtractedPattern]) -> list[str]:
    local_names = set()
    for m in matchers:
        local_names.add(m.method_name.rstrip("?!"))
    helpers: set[str] = set()
    for m in matchers:
        for helper_m in _HELPER_CALL_RE.finditer(m.pattern):
            name = helper_m.group(1)
            if name.rstrip("?!") not in local_names:
                helpers.add(name)
    return sorted(helpers)


def build_extraction_record(cop: str, source_path: Path, gem_root: Path) -> dict:
    source = source_path.read_text(encoding="utf-8")
    matchers_raw = rs.extract_patterns(source)
    stripped = rs.strip_matcher_defs(source)

    mixins = sorted(rs.find_mixins(source))
    hooks = sorted(rs.find_hooks(source))
    extends_autocorrector = "AutoCorrector" in mixins and _extends_marker(source, "AutoCorrector")

    restrict_rhs = rs.find_constant_assignment(source, "RESTRICT_ON_SEND")
    restrict_on_send: list[str] = []
    if restrict_rhs is not None:
        try:
            parsed = rs.parse_ruby_literal(restrict_rhs)
            if isinstance(parsed, list):
                restrict_on_send = [strip_symbol(v) for v in parsed]
        except rs.RubyLiteralError:
            pass

    raw_messages = find_message_constants(source)
    messages = {}
    unsupported_message_formats = []
    for name, raw in raw_messages.items():
        converted, mechanical = convert_message_format(raw)
        messages[name] = converted
        if not mechanical:
            unsupported_message_formats.append(name)

    raw_constants = find_top_level_constants(source)
    constants: dict = {}
    non_map_constants: dict = {}
    unparsed_constants: list[str] = []
    for name, rhs in raw_constants.items():
        if name == "RESTRICT_ON_SEND":
            continue
        try:
            literal = rs.parse_ruby_literal(rhs)
        except rs.RubyLiteralError:
            unparsed_constants.append(name)
            continue
        if isinstance(literal, dict):
            constants[name] = literal
        else:
            non_map_constants[name] = literal

    cop_config_keys = sorted(rs.find_cop_config_keys(source))

    default_yml_path = find_default_yml(gem_root)
    cop_config: dict = {}
    if default_yml_path is not None:
        text = default_yml_path.read_text(encoding="utf-8")
        cop_config = rdc.load_cop_config(text, cop) or {}

    config_options = derive_config_options(cop_config)
    cop_config_keys = sorted(set(cop_config_keys) | set(config_options.keys()))

    matchers = [
        {
            "name": m.method_name,
            "kind": m.kind.value,
            "pattern": m.pattern,
            "n_captures": m.n_captures,
        }
        for m in matchers_raw
    ]

    record = {
        "cop": cop,
        "source_path": str(source_path.relative_to(PROJECT_ROOT))
        if source_path.is_relative_to(PROJECT_ROOT)
        else str(source_path),
        "gem_root": str(gem_root.relative_to(PROJECT_ROOT))
        if gem_root.is_relative_to(PROJECT_ROOT)
        else str(gem_root),
        "loc": len(source.splitlines()),
        "code_loc": rs.code_lines(stripped),
        "matchers": matchers,
        "restrict_on_send": restrict_on_send,
        "messages": messages,
        "unsupported_message_formats": unsupported_message_formats,
        "constants": constants,
        "non_map_constants": non_map_constants,
        "unparsed_constants": unparsed_constants,
        "hooks": hooks,
        "mixins": mixins,
        "uses_source_text": rs.uses_source_text(source),
        "extends_autocorrector": extends_autocorrector,
        "cop_config_keys": cop_config_keys,
        "helper_predicates": find_helper_predicates(matchers_raw),
        "config": {
            "enabled": derive_enabled_default(cop_config),
            "version_added": cop_config.get("VersionAdded"),
            "version_changed": cop_config.get("VersionChanged"),
            "safe": cop_config.get("Safe", True),
            "safe_autocorrect": cop_config.get("SafeAutoCorrect"),
            "options": config_options,
        },
        "autocorrect_mode": derive_autocorrect_mode(extends_autocorrector, cop_config),
        "body_after_matchers": stripped,
    }
    record["skeleton_yaml"] = render_skeleton_yaml(record)
    return record


def _extends_marker(source: str, marker: str) -> bool:
    return bool(re.search(rf"^\s*extend\s+{re.escape(marker)}\b", source, re.MULTILINE))


# --- skeleton rendering -------------------------------------------------


_YAML_AMBIGUOUS_SCALARS = {"true", "false", "null", "~", "yes", "no", "on", "off"}
_YAML_NUMERIC_RE = re.compile(r"[-+]?(\d+\.?\d*|\.\d+)([eE][-+]?\d+)?")


def _yaml_str(value: str) -> str:
    """Render a Python string as a YAML flow scalar, forcing quotes whenever
    leaving it bare would let YAML re-interpret it as a bool/null/number
    instead of the string it actually is (e.g. a `version_added: "1.7"`), or
    would break a plain scalar inside a flow collection (`?`/`!` are legal in
    a method name like `is_a?` but not always safe unquoted inside `[...]`)."""
    if value == "":
        return '""'
    simple = re.fullmatch(r"[A-Za-z0-9_./#*-]+", value)
    looks_ambiguous = (
        value.lower() in _YAML_AMBIGUOUS_SCALARS or _YAML_NUMERIC_RE.fullmatch(value)
    )
    if simple and not looks_ambiguous:
        return value
    escaped = value.replace("\\", "\\\\").replace('"', '\\"')
    return f'"{escaped}"'


def _indent_block(text: str, indent: str) -> str:
    lines = text.split("\n")
    return "\n".join(f"{indent}{line}" if line else indent.rstrip() for line in lines)


def render_skeleton_yaml(record: dict) -> str:
    """Render a partial `.cop.yml` skeleton for `record`.

    `meta`, `config`, `matchers`, and `constants` are filled in for real.
    `hooks[].on` and `hooks[].match` are filled with a best-effort guess;
    `hooks[].when` and `hooks[].offense` are deliberately omitted (rather than
    filled with placeholder text that would slip past schema validation) —
    Stage 3 (`ir_synth.py`) or a human must add them before this cop can load.
    """
    lines: list[str] = []
    lines.append("# GENERATED SKELETON — NOT A FINISHED COP.")
    lines.append("# Produced by scripts/workflows/ir_extract.py (Stage 1: extract).")
    lines.append(
        "# hooks[].when and hooks[].offense are intentionally omitted below; "
        "the IR loader"
    )
    lines.append(
        "# requires `offense` on every hook, so this document does not "
        "validate as a finished"
    )
    lines.append("# cop until Stage 3 (synthesis) or a human fills them in.")
    lines.append("schema: 1")
    lines.append(f"cop: {_yaml_str(record['cop'])}")
    cfg = record["config"]
    if cfg.get("version_added"):
        lines.append(f"version_added: {_yaml_str(str(cfg['version_added']))}")
    enabled = cfg["enabled"]
    enabled_str = str(enabled).lower() if isinstance(enabled, bool) else enabled
    lines.append(f"enabled_default: {enabled_str}")
    lines.append("tier: preview")
    lines.append(f"autocorrect: {record['autocorrect_mode']}")
    if record["restrict_on_send"]:
        rest = ", ".join(_yaml_str(m) for m in record["restrict_on_send"])
        lines.append(f"restrict_on_send: [{rest}]")

    options = cfg.get("options") or {}
    if options:
        lines.append("config:")
        for key, decl in sorted(options.items()):
            if decl["type"] == "enum":
                values = ", ".join(_yaml_str(v) for v in decl["values"])
                lines.append(
                    f"  {key}: {{ type: enum, values: [{values}], "
                    f"default: {_yaml_str(str(decl['default']))} }}"
                )
            elif decl["type"] == "string_array":
                values = ", ".join(_yaml_str(v) for v in decl["default"])
                lines.append(f"  {key}: {{ type: string_array, default: [{values}] }}")
            elif decl["type"] == "string_map":
                if not decl["default"]:
                    lines.append(f"  {key}: {{ type: string_map, default: {{}} }}")
                else:
                    pairs = ", ".join(
                        f"{_yaml_str(k)}: {_yaml_str(v)}" for k, v in decl["default"].items()
                    )
                    lines.append(f"  {key}: {{ type: string_map, default: {{ {pairs} }} }}")
            elif decl["type"] == "bool":
                lines.append(f"  {key}: {{ type: bool, default: {str(decl['default']).lower()} }}")
            elif decl["type"] in ("int", "float"):
                lines.append(f"  {key}: {{ type: {decl['type']}, default: {decl['default']} }}")
            else:
                lines.append(f"  {key}: {{ type: string, default: {_yaml_str(decl['default'])} }}")

    if record["constants"]:
        lines.append("constants:")
        for name, table in sorted(record["constants"].items()):
            lines.append(f"  {_const_key(name)}:")
            for k, v in table.items():
                lines.append(f"    {_yaml_str(str(k))}: {_yaml_str(str(v))}")

    if record["non_map_constants"]:
        lines.append("# non-map constants found (not representable under `constants:`,")
        lines.append("# schema requires a map of maps) — inline these into pattern/param text:")
        for name, value in sorted(record["non_map_constants"].items()):
            lines.append(f"#   {name} = {value!r}")

    if record["matchers"]:
        lines.append("matchers:")
        for m in record["matchers"]:
            lines.append(f"  {_matcher_key(m['name'])}:")
            lines.append("    pattern: |")
            lines.append(_indent_block(m["pattern"], "      "))
            lines.append("    captures: []  # TODO: name the $ captures in occurrence order")

    hook_types = sorted({h[3:] for h in record["hooks"] if h.startswith("on_")})
    if hook_types and record["matchers"]:
        matcher_names = [_matcher_key(m["name"]) for m in record["matchers"]]
        if len(matcher_names) == 1:
            match_expr = matcher_names[0]
        else:
            match_expr = "{ any_of: [" + ", ".join(matcher_names) + "] }  # TODO: narrow per hook"
        lines.append("hooks:")
        for on_type in hook_types:
            lines.append(f"  - on: [{on_type}]")
            lines.append(f"    match: {match_expr}")
            lines.append("    # TODO(stage3): when:, bind:, offense: {location, message, correct}")
    elif record["hooks"] or record["matchers"]:
        lines.append("# TODO(stage3): no confident (hook, matcher) pairing found;")
        lines.append(f"# hooks seen locally: {sorted(record['hooks'])}")
        lines.append(f"# matchers seen: {[m['name'] for m in record['matchers']]}")
    else:
        lines.append(
            "# TODO: no local on_* hooks or matchers found — behavior likely lives in a "
        )
        lines.append(f"# mixin ({record['mixins']}); classify before drafting hooks.")

    return "\n".join(lines) + "\n"


_NAME_SANITIZE_RE = re.compile(r"[^A-Za-z0-9_]")


def _matcher_key(method_name: str) -> str:
    name = method_name.rstrip("?!")
    name = _NAME_SANITIZE_RE.sub("_", name)
    if name and name[0].isdigit():
        name = f"m_{name}"
    return name or "matcher"


def _const_key(name: str) -> str:
    return name.lower()


# --- schema validation ---------------------------------------------------


def relaxed_schema(schema: dict) -> dict:
    """Return a copy of the IR JSON Schema that does not require `hooks[].offense`
    or the top-level `hooks` key — exactly the parts a Stage-1 skeleton omits.
    """
    relaxed = copy.deepcopy(schema)
    relaxed["required"] = [k for k in relaxed["required"] if k != "hooks"]
    hook_def = relaxed["$defs"]["hook"]
    hook_def["required"] = [k for k in hook_def["required"] if k in ("on", "match")]
    return relaxed


def validate_skeleton(doc: dict, schema: dict) -> list[str]:
    try:
        import jsonschema
    except ImportError as exc:  # pragma: no cover - dependency declared in pyproject
        raise RuntimeError(
            "jsonschema is required for --validate-skeleton (uv sync should provide it)"
        ) from exc

    relaxed = relaxed_schema(schema)
    validator = jsonschema.Draft202012Validator(relaxed)
    return [f"{'.'.join(str(p) for p in e.path)}: {e.message}" for e in validator.iter_errors(doc)]


_YAML_LOADER_CACHE: list = []


def _yaml_loader_class():
    """A PyYAML SafeLoader whose bool resolver matches the Rust `serde_yml`
    side (`true`/`false` only), not PyYAML's default YAML-1.1 resolver (which
    also treats bare `on`/`off`/`yes`/`no` as booleans — exactly the "on" hook
    key every `.cop.yml` uses). Real `.cop.yml` fixtures
    (`tests/fixtures/ir/valid/time_now.cop.yml`) already rely on `on:` NOT
    being coerced to a boolean, so this loader is what makes Python-side
    validation agree with the real loader instead of a PyYAML-only footgun.
    """
    import yaml

    if _YAML_LOADER_CACHE:
        return _YAML_LOADER_CACHE[0]

    class _StrictBoolLoader(yaml.SafeLoader):
        pass

    _StrictBoolLoader.yaml_implicit_resolvers = {
        first_char: [r for r in resolvers if r[0] != "tag:yaml.org,2002:bool"]
        for first_char, resolvers in yaml.SafeLoader.yaml_implicit_resolvers.items()
    }
    _StrictBoolLoader.add_implicit_resolver(
        "tag:yaml.org,2002:bool",
        re.compile(r"^(?:true|True|TRUE|false|False|FALSE)$"),
        list("tTfF"),
    )
    _YAML_LOADER_CACHE.append(_StrictBoolLoader)
    return _StrictBoolLoader


def load_skeleton_doc(skeleton_yaml: str) -> dict:
    import yaml

    return yaml.load(skeleton_yaml, Loader=_yaml_loader_class())


# --- CLI -------------------------------------------------------------------


def extract_one(cop: str, roots: list[Path]) -> dict:
    source_path, gem_root = find_cop_source(cop, roots)
    return build_extraction_record(cop, source_path, gem_root)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("cops", nargs="+", help="Cop name(s), e.g. Style/HashExcept")
    parser.add_argument(
        "--rubocop-root",
        type=Path,
        default=PROJECT_ROOT / "vendor" / "rubocop",
        help="Root of the vendored rubocop gem checkout (default: vendor/rubocop)",
    )
    parser.add_argument(
        "--plugin-root",
        type=Path,
        action="append",
        default=[],
        help="Additional gem root to search (repeatable), e.g. vendor/rubocop-rspec",
    )
    parser.add_argument("--out-dir", type=Path, default=None, help="Write extract.json + skeleton.cop.yml per cop")
    parser.add_argument(
        "--validate-skeleton",
        action="store_true",
        help="Validate the filled-in parts of each skeleton against ir_schema.json",
    )
    args = parser.parse_args(argv)

    roots = [args.rubocop_root, *args.plugin_root]
    schema = json.loads(IR_SCHEMA_PATH.read_text()) if args.validate_skeleton else None

    records = []
    had_error = False
    for cop in args.cops:
        try:
            record = extract_one(cop, roots)
        except ExtractError as exc:
            print(f"ERROR: {exc}", file=sys.stderr)
            had_error = True
            continue

        if args.validate_skeleton:
            doc = load_skeleton_doc(record["skeleton_yaml"])
            errors = validate_skeleton(doc, schema)
            record["skeleton_validation_errors"] = errors
            if errors:
                print(f"WARNING: {cop} skeleton has schema issues:", file=sys.stderr)
                for e in errors:
                    print(f"  {e}", file=sys.stderr)

        records.append(record)

        if args.out_dir:
            dept, _, name = cop.partition("/")
            out_dir = args.out_dir / dept / rs.camel_to_snake(name)
            out_dir.mkdir(parents=True, exist_ok=True)
            (out_dir / "extract.json").write_text(json.dumps(record, indent=2, sort_keys=True) + "\n")
            (out_dir / "skeleton.cop.yml").write_text(record["skeleton_yaml"])
            print(f"wrote {out_dir}/{{extract.json,skeleton.cop.yml}}", file=sys.stderr)

    if not args.out_dir:
        output = records[0] if len(records) == 1 else records
        print(json.dumps(output, indent=2, sort_keys=True))

    return 1 if had_error else 0


if __name__ == "__main__":
    raise SystemExit(main())
