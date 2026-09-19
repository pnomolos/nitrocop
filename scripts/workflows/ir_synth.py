#!/usr/bin/env python3
"""Cop IR translation pipeline — Stage 3: synthesize.

Offline only. No LLM in the binary — this script is the *only* place the
translation pipeline calls a model. Given a cop's extraction record (Stage 1,
`ir_extract.py`), the upstream Ruby source, and optionally the cop's RuboCop
spec, it asks an Anthropic model to fill in exactly the parts of a `.cop.yml`
document that cannot be copied verbatim: `hooks[].when`, `hooks[].bind`,
`hooks[].offense`, and — only when mechanically derivable from the source —
`severity`, `enabled_default`, `min_target_ruby`, `autocorrect`.

Per design (`docs/planning/04-cop-ir-design.md` §5 Stage 3, on
`planning/program-status`): "`matchers`, `config`, `constants`, and metadata
are copied verbatim from `extract.json` by the script, never by the model."
This script enforces that in three independent ways, deliberately redundant:

1. The model's JSON output schema has no `matchers`/`config`/`constants`/
   `predicates`/`meta` properties at all (`additionalProperties: false`), so a
   compliant response cannot contain them.
2. If the model ignores the schema and emits one of those keys anyway, the
   merge step raises rather than merging it.
3. After merging, `matchers`/`config`/`constants` in the final document are
   asserted byte-equal (as parsed values, not YAML source text) to what
   `ir_extract.py`'s own record produced for this cop — see
   `assert_verbatim_sections`.

See `docs/COP_IR.md` "Pipeline" section for the end-to-end CLI walkthrough,
and PR #19 / #23's "Pipeline friction log" sections (this repo's translation
history) for the rules embedded in `SYSTEM_PROMPT_RULES` below — they are
quoted close to verbatim from those documents' "Translating an upstream cop"
findings, because that is the one thing this script cannot get wrong without
regenerating garbage cops.

Usage:
    export ANTHROPIC_API_KEY=...
    python3 scripts/workflows/ir_synth.py Style/TimeNow
    python3 scripts/workflows/ir_synth.py Style/RedundantMinMaxBy \\
        --spec vendor/rubocop/spec/rubocop/cop/style/redundant_min_max_by_spec.rb
    python3 scripts/workflows/ir_synth.py Style/TimeNow --dry-run   # print the prompt, no API call
    python3 scripts/workflows/ir_synth.py Style/TimeNow --model claude-opus-5 --max-retries 2

Output: `build/ir/<Dept>/<snake>/{synth.cop.yml,synth.raw.txt,synth.report.json}`
(gitignored, per design §5's artifacts table). `synth.raw.txt` is the exact
text of the model's last response, kept for audit regardless of whether
validation succeeded. Nothing under `src/resources/ir/` is written by this
script — promoting a synthesized document to a shipped cop is a human
decision (review the YAML, move it, wire it into `embedded.rs`, add fixtures).
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from dataclasses import dataclass
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
SCRIPTS_ROOT = SCRIPT_DIR.parent
sys.path.insert(0, str(SCRIPT_DIR))
sys.path.insert(0, str(SCRIPTS_ROOT))

import ir_extract  # noqa: E402
from shared import ruby_source as rs  # noqa: E402

PROJECT_ROOT = SCRIPTS_ROOT.parent
IR_SCHEMA_PATH = SCRIPTS_ROOT / "shared" / "ir_schema.json"
FEWSHOT_DIR = PROJECT_ROOT / "src" / "resources" / "ir"

DEFAULT_MODEL = "claude-opus-5"
DEFAULT_MAX_RETRIES = 3
DEFAULT_MAX_TOKENS = 8000

# The NodePattern builtin predicate registry (`src/node_pattern/predicates.rs`,
# ~80 entries) plus the four spellings `docs/COP_IR.md` says are compiled
# directly rather than registered (`type?`/`<t>_type?`/`root?`/`value_used?`).
# `nitrocop --list-ir-predicates` (design §2.2) would make this a live query;
# it does not exist yet in this codebase, so the list is vendored here and
# re-checked by `tests/python/workflows/test_ir_synth.py` against a grep of
# `src/node_pattern/predicates.rs` so it cannot silently drift out of date.
KNOWN_PREDICATES = frozenset(
    {
        "access_modifier?", "argument?", "arguments?", "arithmetic_operation?",
        "assignment_method?", "assignment_or_similar?", "assignment?",
        "bang_method?", "bare_access_modifier?", "basic_conditional?",
        "basic_literal?", "binary_operation?", "blank?", "block_argument?",
        "block_literal?", "braces?", "camel_case_method?", "chained?",
        "command?", "comparison_method?", "composite_literal?", "conditional?",
        "const_receiver?", "def_modifier?", "dot?", "double_colon?",
        "empty_source?", "empty?", "enumerable_method?", "enumerator_method?",
        "equal?", "equals_asgn?", "falsey_literal?", "global_const?",
        "guard_clause?", "immutable_literal?", "implicit_call?", "keyword?",
        "lambda_literal?", "lambda_or_proc?", "lambda?", "literal?",
        "loop_keyword?", "macro?", "method?", "modifier_form?", "multiline?",
        "mutable_literal?", "negation_method?", "negative?",
        "non_bare_access_modifier?", "nonmutating_array_method?",
        "nonmutating_binary_operator_method?", "nonmutating_hash_method?",
        "nonmutating_operator_method?", "nonmutating_string_method?",
        "nonmutating_unary_operator_method?", "operator_keyword?",
        "operator_method?", "parent?", "parenthesized_call?", "positive?",
        "post_condition_loop?", "predicate_method?", "prefix_bang?",
        "prefix_not?", "proc?", "receiver?", "recursive_basic_literal?",
        "recursive_literal?", "reference?", "root?", "safe_navigation?",
        "self_receiver?", "setter_method?", "shorthand_asgn?",
        "single_line?", "special_keyword?", "special_modifier?",
        "truthy_literal?", "unary_operation?", "value_omission?",
        "value_used?", "variable?", "zero?",
        # Compiled directly, not registry entries (docs/COP_IR.md "pred: names"):
        "type?",
    }
)
# `<t>_type?` is a family, not a fixed name; matched separately.
_TYPE_SUFFIX_RE = re.compile(r"^[a-z][a-z0-9_]*_type\?$")


class SynthError(Exception):
    """Raised for user-facing failures (missing API key, bad cop name, etc.)."""


# --- system prompt ----------------------------------------------------------

SYSTEM_PROMPT_RULES = """\
You are translating one RuboCop cop into nitrocop's cop IR (schema v1), a
declarative YAML format. nitrocop is a Rust reimplementation of RuboCop that
must match RuboCop's output byte-for-byte, including quirks — any deviation
is treated as a regression by the project's corpus oracle.

You will be given an EXTRACTION RECORD (JSON) produced by a deterministic,
non-LLM extractor, plus the cop's upstream Ruby source with matcher method
bodies stripped out. The extraction record's `matchers`, `config`,
`constants`, and `predicates` sections are ALREADY FINAL — copied verbatim
from upstream. You do not repeat, rewrite, or "correct" them. Your entire
job is to produce a `hooks` array, plus (only when the source makes them
mechanically obvious) `severity`, `enabled_default`, `min_target_ruby`, and
`autocorrect`.

Respond with EXACTLY ONE JSON object and nothing else — no markdown code
fences, no prose before or after. If a previous attempt is shown to you with
validation errors, fix those errors and resend the complete corrected JSON
object (not a diff).

## Hard rules (violating any of these fails automatic verification)

1. **`match:` may only reference matcher names given to you.** Never invent a
   NodePattern string. If you need a compound condition, use
   `{ any_of: [name1, name2] }` / `{ all_of: [...] }` over the given names —
   never write a new `pattern:`.

2. **`when:`/`bind:`/`offense:` expressions may only reference:**
   - `node`, `parent`, a capture name from the matcher you matched (as
     `$name`, given to you per matcher);
   - `cfg.<Key>` for a declared config key, `consts.<Table>` for a declared
     constant table — always exactly two segments;
   - `pred: [<expr>, "<name>", ...]` where `<name>` is one of the predicate
     names given to you (an unknown predicate name fails validation);
   - the operators and attributes documented in the schema excerpt you are
     given (`all`, `any`, `not`, `eq`, `ne`, `lt`/`le`/`gt`/`ge`, `in`, `if`,
     `lit`, `lookup`, `attr`, `matches`, `regex`, `any_of`/`all_of`/`none_of`,
     `count`, and the attribute table).
   Do not invent new operators, attributes, or predicate names.

3. **Two dispatch inversions recur in almost every cop.** Recognize them:

   a. **`on_send` + `node.block_node`** (upstream hooks the call, then walks
      down into its block). Prism has ONE node for a call carrying a block —
      there is no separate "send" level to hook. The IR hook goes on
      `[block]` (and `[numblock]`, `[itblock]` if the upstream cop also
      handles numbered/`it` params) instead of `[send, csend]`. Inside that
      hook, `node.method_name` IS the predicate/selector upstream reached via
      the send, and `node.selector` is that selector's `loc`. Upstream's
      `RESTRICT_ON_SEND` then has no `send` hook to filter, so re-express it
      as `{ in: [node.method_name, [":method1", ":method2", ...]] }` inside
      `when:`. Example (`Style/PredicateWithKind`, from this codebase):
      upstream hooks `on_send` on `any?`/`all?`/`none?`/`one?` and reaches the
      block via `node.block_node`; the IR hook is `on: [block]` with
      `when: { in: [node.method_name, [":any?", ":all?", ":none?", ":one?"]] }`.

   b. **`on_send` + a loop over children, calling `add_offense` once per
      child** (e.g. once per member of `Data.define(...)`, once per hash pair).
      A single IR hook reports once — there is no "for each" construct. Invert
      the dispatch: put the hook on the CHILD's node type, and have the
      matcher ASCEND to the enclosing call with a leading `^` (or `^^` for
      grandparent, etc.) rather than descending from it. `^` walks
      Parser-visible ancestry, so a splatted or hash-nested argument correctly
      fails to match — exactly the set upstream's loop would have skipped.
      Example (`Lint/DataDefineOverride`): upstream hooks `on_send` on
      `Data.define` and loops `node.arguments`; the IR matcher is
      `^(send (const {nil? cbase} :Data) :define ...)` with the hook on
      `[sym]`/`[str]` (whatever node types can appear as a member name).

   c. **The converse of (a) bites too.** A `send` hook DOES fire on a call
      that carries a literal block (Prism has one node for both), where
      upstream's `on_send` — running on the Parser gem — only ever sees the
      bare send with no block, because the block wraps it one level up. If a
      guard reads `node.parent` (or otherwise depends on what a Parser-visible
      caller would see as the immediate parent), and upstream would never have
      reached this call in its block form at all, add an explicit
      `not: { pred: [node, "block_literal?"] }` term and say in a YAML
      comment (in the `docs:`/inline comment, not in this JSON — see rule 8)
      that this term has no upstream counterpart. Do not silently omit it or
      silently over-match.

4. **One hook per correction-range branch.** If upstream's autocorrect logic
   is an `if`/`elsif`/`else` choosing between different ranges to edit (not
   just different guard conditions with the same edit), there is no `correct:`
   construct that branches — write one hook per branch, each carrying the
   `when:` guard upstream used to choose that branch, sharing the same `on:`
   and `match:`. If one of upstream's branches is provably unreachable given
   the matcher's own constraints (e.g. a size check the matcher's arity already
   guarantees), omit that hook rather than writing dead code, and rely on the
   normal review process to catch you if you're wrong about "provably".

5. **Drop a guard the matcher already implies — but leave a comment saying
   so.** If upstream has a Ruby-level guard clause that the NodePattern
   itself already enforces (e.g. `return unless node.arguments.size == 2`
   when the matcher's own arity is exactly 2), do not re-encode it in
   `when:`. State in your `notes` field (see Output format) that you dropped
   it and why, so a human reviewer can double check the claim.

6. **`format(MSG, key: value)` inlines into one literal `%{key}` message per
   hook.** Upstream's message logic is usually
   `format(MSG, name: something)` where `MSG` is a class constant already
   given to you (already converted from `%<name>s` to `%{name}`). If upstream
   computes DIFFERENT literal text per branch (e.g. `member_name.inspect` for
   a Symbol vs `member_name.to_s` for a String — `:foo` vs `"foo"`), and the
   branches are also different hooks (different `on:` types) per rule 3b/4,
   write the DIFFERENT literal text directly into each hook's `message:`
   rather than trying to compute `.inspect`-style formatting inside `bind:` —
   the expression language has no string-transformation operator.

7. **Per-kind literals, per hook.** When one upstream cop handles a block,
   `numblock`, and `itblock` form of the same call (three Prism shapes for
   one Parser concept), the literal value that differs per shape (block param
   name text, `_1`, `it`) belongs in that hook's own `message:`/`bind:`, not
   shared. The `location:`/`correct:` are usually identical across the three
   and can be identical hook-to-hook (you do not need YAML anchors — repeat
   them; the merge step does not deduplicate for you).

8. **Two-segment references only.** `cfg.X`, `bind.X`, `consts.X` — always
   exactly two segments. Never `cfg.X.Y` or a bare `cfg`. A message template
   placeholder is always `%{name}` (never `%<name>s` — that is upstream's
   `format()` spelling, already converted for you in the extraction record).
   A literal percent sign in a message is `%%`.

9. **Bind ordering matters.** `bind:` entries may only reference bind names
   declared BEFORE them in the same hook's `bind:` map (a forward or
   self-reference is a load error). Order your `bind:` keys accordingly.

10. **Matchers compile in name order and can only reference matchers earlier
    in that order as `#helper`.** You are not authoring matchers, but this
    means: when you write `{ any_of: [...] }` / `{ all_of: [...] }` in
    `match:`, any matcher name is fine (that's a hook construct, not the
    matcher-compilation DAG) — this rule is here so you understand why the
    given matcher list is already validated and don't try to "fix" its order.

11. **Locations and corrections are literal transcriptions of upstream's
    `node.loc.<part>` / `range_between` calls**, using the Anchor grammar
    `<target>[.<accessor>]*.<part>[.<start|stop>]` given to you. Do not
    approximate a range you cannot express exactly — if upstream's range
    cannot be expressed in the given vocabulary (e.g. it needs the *line* of
    a `loc` part, which the expression language has no accessor for), do NOT
    guess. Instead, add a hook-level entry to `notes` explaining exactly what
    is missing, and emit your best faithful approximation only if the
    resulting divergence would be provably narrow (say so, precisely, in
    `notes`) — otherwise omit `correct:` for that hook entirely rather than
    ship a wrong autocorrection. Never omit `message:`/`offense:` outright:
    every hook must still report the offense even if it cannot safely
    autocorrect it.

## Output format

A single JSON object:

```json
{
  "hooks": [
    {
      "on": ["send", "csend"],
      "match": "matcher_name_or_any_of_object",
      "when": { "...": "..." },
      "bind": { "name": { "...": "..." } },
      "offense": {
        "location": "node",
        "message": "...",
        "correct": [ { "op": "replace", "range": {"start": "...", "stop": "..."}, "text": "..." } ]
      }
    }
  ],
  "severity": "warning",
  "enabled_default": "pending",
  "min_target_ruby": 3.2,
  "autocorrect": "safe",
  "notes": [
    "one string per rule-5/rule-11 disclosure, or any other reviewer-relevant caveat"
  ]
}
```

Omit `when`/`bind` on a hook that needs neither. Omit `severity`,
`enabled_default`, `min_target_ruby`, `autocorrect`, and `notes` entirely if
you have nothing to say — do not guess a value you cannot justify from the
given source. `enabled_default` should almost always be omitted (it comes
from `config/default.yml`'s `Enabled:`, already in the extraction record —
only include it if you are correcting an obvious extraction miss).
"""


def build_fewshot_examples() -> list[tuple[str, str]]:
    """Return [(cop_name, yaml_text), ...] for every shipped cop under
    `src/resources/ir/`, sorted for determinism. These are the finished,
    hand-reviewed translations this project already shipped — the closest
    thing to ground truth the model can be shown."""
    examples = []
    for path in sorted(FEWSHOT_DIR.rglob("*.cop.yml")):
        text = path.read_text(encoding="utf-8")
        m = re.search(r'^cop:\s*"([^"]+)"', text, re.MULTILINE)
        name = m.group(1) if m else path.stem
        examples.append((name, text))
    return examples


def render_fewshot_block(examples: list[tuple[str, str]]) -> str:
    parts = [
        "## Worked examples (already shipped, hand-reviewed — study these before writing hooks)\n"
    ]
    for name, text in examples:
        parts.append(f"### {name}\n\n```yaml\n{text}\n```\n")
    return "\n".join(parts)


# --- schema for the model's JSON output -------------------------------------


def load_ir_schema() -> dict:
    return json.loads(IR_SCHEMA_PATH.read_text())


def build_synth_output_schema(ir_schema: dict) -> dict:
    """The strict output schema for Stage 3's model call.

    Reuses `$defs.hook`/`$defs.expr`/`$defs.matchSpec`/`$defs.offense` from
    the canonical `ir_schema.json` verbatim (so this can never drift from
    what the Rust loader actually accepts) and declares nothing at the top
    level except `hooks` and the four documented-derivable scalar fields —
    `matchers`/`config`/`constants`/`predicates`/`cop`/`meta` are not
    properties of this schema at all, so `additionalProperties: false` makes
    a compliant response structurally incapable of touching them.
    """
    return {
        "$schema": ir_schema.get("$schema", "https://json-schema.org/draft/2020-12/schema"),
        "$defs": ir_schema["$defs"],
        "type": "object",
        "additionalProperties": False,
        "required": ["hooks"],
        "properties": {
            "hooks": {
                "type": "array",
                "minItems": 1,
                "items": {"$ref": "#/$defs/hook"},
            },
            "severity": ir_schema["properties"]["severity"],
            "enabled_default": ir_schema["properties"]["enabled_default"],
            "min_target_ruby": ir_schema["properties"]["min_target_ruby"],
            "autocorrect": ir_schema["properties"]["autocorrect"],
            "notes": {"type": "array", "items": {"type": "string"}},
        },
    }


# --- mechanical (non-LLM) matcher normalization ------------------------------

_PARAM_TOKEN_RE = re.compile(r"%([A-Za-z_][A-Za-z0-9_]*)")


@dataclass
class NormalizedMatcher:
    key: str
    pattern: str
    captures: list[str]
    params: list[str]
    n_captures: int
    synthetic_captures: bool


def normalize_matchers(record: dict) -> list[NormalizedMatcher]:
    """Mechanically finish the parts of `matchers:` that `ir_extract.py`
    deliberately leaves as TODO (capture names, `%param` bindings) — this is
    NOT LLM work: capture names and params are matcher metadata, and rule 1
    forbids the model from touching `matchers:` at all. Naming happens here,
    deterministically, so hooks can reference `$capture_name`.

    Capture names are synthetic (`capture_1`, `capture_2`, ...) when the
    extraction record didn't supply real ones (`captures: []` in every
    shipped skeleton) — flagged via `synthetic_captures` so the report can
    tell a human to rename them for readability. If a human has since
    hand-edited the record's `matchers[].captures` (e.g. by re-running
    extraction against an edited skeleton), those names are used verbatim.
    """
    config_keys = set(record.get("config", {}).get("options", {}).keys())
    constant_keys = set(record.get("constants", {}).keys())
    known_param_names = config_keys | constant_keys

    normalized = []
    for m in record["matchers"]:
        key = ir_extract._matcher_key(m["name"])
        captures = list(m.get("captures") or [])
        n_captures = m.get("n_captures", len(captures))
        synthetic = False
        if len(captures) < n_captures:
            synthetic = True
            captures = captures + [
                f"capture_{i + 1}" for i in range(len(captures), n_captures)
            ]
        params = list(m.get("params") or [])
        if not params:
            seen = []
            for tok in _PARAM_TOKEN_RE.finditer(m["pattern"]):
                name = tok.group(1)
                if name in known_param_names and name not in seen:
                    seen.append(name)
            params = seen
        normalized.append(
            NormalizedMatcher(
                key=key,
                pattern=m["pattern"],
                captures=captures,
                params=params,
                n_captures=n_captures,
                synthetic_captures=synthetic,
            )
        )
    return normalized


# --- prompt assembly ----------------------------------------------------------


def build_user_prompt(
    record: dict,
    matchers: list[NormalizedMatcher],
    ruby_source: str,
    spec_text: str | None,
) -> str:
    # `ruby_source`, when given, overrides the extraction record's own
    # matcher-bodies-stripped source (e.g. a caller re-reading the file to
    # pick up local edits since extraction ran). Empty string means "use the
    # record as extracted", which is the common case.
    body = ruby_source or record["body_after_matchers"]
    matcher_info = [
        {
            "name": m.key,
            "pattern": m.pattern,
            "captures": m.captures,
            "params": m.params,
            "note": "synthetic capture names — rename for readability, references are still valid"
            if m.synthetic_captures
            else None,
        }
        for m in matchers
    ]
    trimmed_record = {
        "cop": record["cop"],
        "restrict_on_send": record["restrict_on_send"],
        "messages": record["messages"],
        "constants": record["constants"],
        "config_options": record["config"]["options"],
        "raw_on_hooks_found_in_source": sorted(record["hooks"]),
        "mixins": record["mixins"],
        "extends_autocorrector": record["extends_autocorrector"],
        "autocorrect_mode_from_config": record["autocorrect_mode"],
        "safe": record["config"]["safe"],
        "safe_autocorrect": record["config"]["safe_autocorrect"],
        "helper_predicates_referenced_by_matchers": record["helper_predicates"],
    }
    parts = [
        f"# Cop: {record['cop']}\n",
        "## Extraction record (matchers/config/constants are FINAL — do not repeat or alter them)\n",
        "```json\n" + json.dumps(trimmed_record, indent=2, sort_keys=True) + "\n```\n",
        "## Matchers available to `match:` (with their finalized capture/param names)\n",
        "```json\n" + json.dumps(matcher_info, indent=2) + "\n```\n",
        "## Upstream Ruby source, matcher method bodies stripped\n",
        "```ruby\n" + body + "\n```\n",
    ]
    if spec_text:
        parts.append(
            "## Upstream RuboCop spec (for message wording / edge cases only — "
            "do not treat its expected message text as more authoritative than "
            "the MSG constant already in the extraction record; PR #23's "
            "friction log documents a spec asserting the wrong message)\n"
        )
        parts.append("```ruby\n" + spec_text + "\n```\n")
    parts.append(
        "Produce the JSON object described in the system prompt's Output format "
        "section now."
    )
    return "\n".join(parts)


def build_system_blocks(fewshot_text: str, schema: dict) -> list[dict]:
    schema_text = json.dumps(schema, indent=2)
    static_text = (
        SYSTEM_PROMPT_RULES
        + "\n## JSON Schema your response must satisfy\n\n```json\n"
        + schema_text
        + "\n```\n\n"
        + fewshot_text
    )
    return [
        {
            "type": "text",
            "text": static_text,
            "cache_control": {"type": "ephemeral"},
        }
    ]


# --- validation ---------------------------------------------------------------


def _iter_match_names(match_spec) -> list[str]:
    if isinstance(match_spec, str):
        return [match_spec]
    if isinstance(match_spec, dict):
        for key in ("any_of", "all_of"):
            if key in match_spec:
                names = []
                for item in match_spec[key]:
                    names.extend(_iter_match_names(item))
                return names
    return []


def _walk_expr_refs(expr, preds: set[str], cfg_refs: set[str], consts_refs: set[str]):
    """Collect every `pred:` name and `cfg./consts.` reference anywhere in an
    Expr tree (dict/list/scalar), for the whitelist check. Deliberately
    permissive about shape (this is a best-effort static scan, not a second
    compiler) — it must never crash on a structurally-valid-but-unusual expr."""
    if isinstance(expr, dict):
        if "pred" in expr and isinstance(expr["pred"], list) and expr["pred"]:
            name = expr["pred"][1] if len(expr["pred"]) > 1 else None
            if isinstance(name, str):
                preds.add(name)
        for v in expr.values():
            _walk_expr_refs(v, preds, cfg_refs, consts_refs)
    elif isinstance(expr, list):
        for item in expr:
            _walk_expr_refs(item, preds, cfg_refs, consts_refs)
    elif isinstance(expr, str):
        # Simple two-segment scan without over-engineering a path parser: a
        # bare "cfg.X"/"consts.X" token (possibly the head of a longer
        # accessor chain like "cfg.X.Y" — the latter is itself an error the
        # loader schema doesn't catch statically, so this only checks the key
        # name after the first two segments).
        head = expr.split(".")
        if len(head) >= 2 and head[0] == "cfg":
            cfg_refs.add(head[1])
        elif len(head) >= 2 and head[0] == "consts":
            consts_refs.add(head[1])


def validate_synth_doc(
    doc: dict,
    schema: dict,
    matchers: list[NormalizedMatcher],
    record: dict,
) -> list[str]:
    """Returns a list of human-readable error strings; empty means valid.
    Combines JSON Schema structural validation with the whitelist checks
    design §5/§7 risk #7 calls for (predicate names, matcher names, cfg/consts
    references) that a flat schema cannot express."""
    import jsonschema

    errors = []
    validator = jsonschema.Draft202012Validator(schema)
    for e in validator.iter_errors(doc):
        errors.append(f"schema: {'.'.join(str(p) for p in e.path) or '<root>'}: {e.message}")
    if errors:
        # Whitelist checks assume a structurally valid document; bail early.
        return errors

    matcher_names = {m.key for m in matchers}
    capture_names_by_matcher = {m.key: set(m.captures) for m in matchers}
    config_keys = set(record.get("config", {}).get("options", {}).keys())
    constant_keys = set(record.get("constants", {}).keys())

    for i, hook in enumerate(doc["hooks"]):
        used_matchers = set(_iter_match_names(hook["match"]))
        unknown = used_matchers - matcher_names
        if unknown:
            errors.append(f"hooks[{i}].match references unknown matcher(s): {sorted(unknown)}")

        preds: set[str] = set()
        cfg_refs: set[str] = set()
        consts_refs: set[str] = set()
        for section in ("when", "bind", "offense"):
            if section in hook:
                _walk_expr_refs(hook[section], preds, cfg_refs, consts_refs)

        for name in preds:
            if name in KNOWN_PREDICATES or _TYPE_SUFFIX_RE.match(name):
                continue
            errors.append(f"hooks[{i}]: unknown predicate {name!r}")

        unknown_cfg = cfg_refs - config_keys
        if unknown_cfg:
            errors.append(f"hooks[{i}]: references undeclared config key(s): {sorted(unknown_cfg)}")
        unknown_consts = consts_refs - constant_keys
        if unknown_consts:
            errors.append(f"hooks[{i}]: references undeclared constant table(s): {sorted(unknown_consts)}")

        # A bare $capture reference should belong to one of the matchers this
        # hook actually matches against (best-effort — `any_of` hooks are
        # checked against the union of all referenced matchers' captures).
        available_captures: set[str] = set()
        for mname in used_matchers:
            available_captures |= capture_names_by_matcher.get(mname, set())
        hook_text = json.dumps(hook)
        referenced_captures = set(re.findall(r"\$([A-Za-z_][A-Za-z0-9_]*)", hook_text))
        unknown_captures = referenced_captures - available_captures
        if unknown_captures:
            errors.append(
                f"hooks[{i}] references capture(s) not on its matcher(s): {sorted(unknown_captures)}"
            )

        message = hook.get("offense", {}).get("message", "")
        placeholders = set(re.findall(r"%\{([A-Za-z_][A-Za-z0-9_]*)\}", message))
        bind_names = set(hook.get("bind", {}).keys())
        allowed_placeholders = bind_names | available_captures | config_keys
        bad_percent = re.findall(r"%(?![{%])", message)
        if bad_percent:
            errors.append(f"hooks[{i}].offense.message has a bare '%' (write '%%'): {message!r}")
        unresolved = placeholders - allowed_placeholders
        if unresolved:
            errors.append(
                f"hooks[{i}].offense.message has unresolved placeholder(s): {sorted(unresolved)}"
            )

    return errors


def assert_verbatim_sections(record: dict, matchers: list[NormalizedMatcher]) -> None:
    """Defense in depth (design rule 3, see module docstring): even though
    the model's output schema structurally cannot contain `matchers`/
    `config`/`constants`, assert here too that what this script is about to
    merge into the final document is exactly what `ir_extract.py` produced —
    byte-for-byte on every matcher's pattern string, and value-equal on the
    config/constants dicts. A failure here means a bug in THIS script's merge
    logic, not the model's output."""
    # Pattern text is carried through `normalize_matchers` unchanged from
    # `record["matchers"][i]["pattern"]` — assert that invariant directly.
    original_patterns = [m["pattern"] for m in record["matchers"]]
    carried_patterns = [m.pattern for m in matchers]
    if original_patterns != carried_patterns:
        raise SynthError(
            "internal error: matcher pattern text was altered between extraction "
            "and merge — this must never happen (matchers are verbatim by design)"
        )


# --- rendering ----------------------------------------------------------------


def _yaml_str(value) -> str:
    return ir_extract._yaml_str(value)


def _block_scalar(text: str, indent: str) -> str:
    lines = text.rstrip("\n").split("\n")
    body = "\n".join(f"{indent}{line}" if line else "" for line in lines)
    return f"|\n{body}"


def _render_scalar(value, indent: str) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if value is None:
        return "null"
    if isinstance(value, (int, float)):
        return str(value)
    if isinstance(value, str):
        if "\n" in value:
            return _block_scalar(value, indent)
        return _yaml_str(value)
    raise TypeError(f"not a scalar: {value!r}")


def _render_flow(value) -> str:
    """Render a value as YAML flow style (`{...}`/`[...]`) — used for
    `when:`/`bind:`/`offense:` expression trees, which are small and read
    fine as flow collections (matching the shipped examples' style)."""
    if isinstance(value, dict):
        items = ", ".join(f"{k}: {_render_flow(v)}" for k, v in value.items())
        return "{ " + items + " }" if items else "{}"
    if isinstance(value, list):
        items = ", ".join(_render_flow(v) for v in value)
        return "[" + items + "]" if items else "[]"
    if isinstance(value, str):
        return _yaml_str(value)
    if isinstance(value, bool):
        return "true" if value else "false"
    if value is None:
        return "null"
    return str(value)


def _render_block_mapping(value: dict, indent: str) -> list[str]:
    """Render a dict as a YAML block mapping, using block-scalar strings for
    multi-line values (message templates never need it, but this keeps the
    renderer honest for anything long) and flow style for nested exprs."""
    lines = []
    for k, v in value.items():
        if isinstance(v, str) and "\n" not in v:
            lines.append(f"{indent}{k}: {_yaml_str(v)}")
        elif isinstance(v, str):
            lines.append(f"{indent}{k}: {_block_scalar(v, indent + '  ')}")
        elif isinstance(v, (dict, list)):
            lines.append(f"{indent}{k}: {_render_flow(v)}")
        else:
            lines.append(f"{indent}{k}: {_render_scalar(v, indent)}")
    return lines


def render_hook_yaml(hook: dict, indent: str = "    ") -> list[str]:
    lines = [f"{indent[:-2]}- on: {_render_flow(hook['on'])}"]
    lines.append(f"{indent}match: {_render_flow(hook['match'])}")
    if hook.get("when") is not None:
        lines.append(f"{indent}when: {_render_flow(hook['when'])}")
    if hook.get("bind"):
        lines.append(f"{indent}bind:")
        lines.extend(_render_block_mapping(hook["bind"], indent + "  "))
    offense = hook["offense"]
    lines.append(f"{indent}offense:")
    off_indent = indent + "  "
    lines.append(f"{off_indent}location: {_render_flow(offense['location'])}")
    msg = offense["message"]
    if "\n" in msg:
        lines.append(f"{off_indent}message: {_block_scalar(msg, off_indent + '  ')}")
    else:
        lines.append(f"{off_indent}message: {_yaml_str(msg)}")
    if offense.get("severity"):
        lines.append(f"{off_indent}severity: {offense['severity']}")
    if offense.get("correct"):
        lines.append(f"{off_indent}correct:")
        for op in offense["correct"]:
            lines.append(f"{off_indent}  - {_render_flow(op)}")
    return lines


def render_matcher_yaml(m: NormalizedMatcher) -> list[str]:
    lines = [f"  {m.key}:"]
    lines.append("    pattern: |")
    for line in m.pattern.rstrip("\n").split("\n"):
        lines.append(f"      {line}" if line else "")
    if m.captures:
        lines.append(f"    captures: [{', '.join(m.captures)}]")
    if m.params:
        lines.append(f"    params: [{', '.join(m.params)}]")
    return lines


def render_cop_yaml(record: dict, matchers: list[NormalizedMatcher], synth: dict) -> str:
    """Merge Stage 1's verbatim sections with Stage 3's `hooks`/scalar
    overrides into a complete `.cop.yml` document."""
    lines = ["schema: 1", f"cop: {_yaml_str(record['cop'])}"]
    version_added = record["config"].get("version_added")
    if version_added:
        lines.append(f"version_added: {_yaml_str(str(version_added))}")

    enabled = synth.get("enabled_default", record["config"]["enabled"])
    enabled_str = str(enabled).lower() if isinstance(enabled, bool) else enabled
    lines.append(f"enabled_default: {enabled_str}")
    lines.append("tier: preview")

    autocorrect = synth.get("autocorrect", record["autocorrect_mode"])
    lines.append(f"autocorrect: {autocorrect}")

    if synth.get("severity"):
        lines.append(f"severity: {synth['severity']}")
    if synth.get("min_target_ruby") is not None:
        lines.append(f"min_target_ruby: {synth['min_target_ruby']}")
    if record["restrict_on_send"]:
        rest = ", ".join(_yaml_str(m) for m in record["restrict_on_send"])
        lines.append(f"restrict_on_send: [{rest}]")

    options = record["config"].get("options") or {}
    if options:
        lines.append("config:")
        for key, decl in sorted(options.items()):
            lines.append(f"  {key}: {_render_flow(decl)}")

    if record["constants"]:
        lines.append("constants:")
        for name, table in record["constants"].items():
            lines.append(f"  {name}: {_render_flow(table)}")

    if matchers:
        lines.append("matchers:")
        for m in matchers:
            lines.extend(render_matcher_yaml(m))

    lines.append("hooks:")
    for hook in synth["hooks"]:
        lines.extend(render_hook_yaml(hook))

    return "\n".join(lines) + "\n"


# --- LLM call -----------------------------------------------------------------


def resolve_api_client():
    """Returns an `anthropic.Anthropic` client, or raises SynthError with a
    clear message (caller exits 2) if no credential is configured."""
    if not (os.environ.get("ANTHROPIC_API_KEY") or os.environ.get("ANTHROPIC_AUTH_TOKEN")):
        raise SynthError(
            "No Anthropic API credential found. Set ANTHROPIC_API_KEY (or "
            "ANTHROPIC_AUTH_TOKEN) before running ir_synth.py — this script "
            "never calls a model without one, and never falls back to a "
            "different provider."
        )
    try:
        import anthropic
    except ImportError as exc:  # pragma: no cover - dependency declared in pyproject
        raise SynthError(
            "the `anthropic` package is required (uv sync should provide it)"
        ) from exc
    return anthropic.Anthropic()


def call_model(
    client,
    model: str,
    system_blocks: list[dict],
    user_prompt: str,
    max_tokens: int,
    prior_turns: list[dict] | None = None,
) -> tuple[str, object]:
    messages = list(prior_turns or [])
    messages.append({"role": "user", "content": user_prompt})
    response = client.messages.create(
        model=model,
        max_tokens=max_tokens,
        system=system_blocks,
        thinking={"type": "adaptive"},
        messages=messages,
    )
    text = "".join(b.text for b in response.content if b.type == "text")
    return text, response


def extract_json_object(text: str) -> dict:
    """The system prompt demands a bare JSON object; tolerate a model that
    wraps it in a markdown fence anyway (a known Claude habit under stress),
    but never guess at malformed JSON — that's a hard failure, not a retry
    target with a repaired document."""
    stripped = text.strip()
    fence = re.match(r"^```(?:json)?\s*\n(.*)\n```\s*$", stripped, re.DOTALL)
    if fence:
        stripped = fence.group(1)
    return json.loads(stripped)


# --- orchestration --------------------------------------------------------


@dataclass
class SynthResult:
    cop: str
    ok: bool
    yaml_text: str | None
    raw_text: str
    attempts: int
    errors: list[str]
    notes: list[str]


def synthesize(
    record: dict,
    *,
    model: str,
    max_retries: int,
    max_tokens: int,
    spec_text: str | None,
    client=None,
) -> SynthResult:
    schema = load_ir_schema()
    output_schema = build_synth_output_schema(schema)
    matchers = normalize_matchers(record)
    assert_verbatim_sections(record, matchers)

    fewshot_text = render_fewshot_block(build_fewshot_examples())
    system_blocks = build_system_blocks(fewshot_text, output_schema)
    user_prompt = build_user_prompt(record, matchers, ruby_source="", spec_text=spec_text)

    client = client or resolve_api_client()
    conversation: list[dict] = []
    last_raw = ""
    last_errors: list[str] = []

    for attempt in range(1, max_retries + 2):
        prompt = user_prompt if attempt == 1 else (
            "Your previous response failed validation:\n"
            + "\n".join(f"- {e}" for e in last_errors)
            + "\n\nResend the COMPLETE corrected JSON object."
        )
        raw_text, response = call_model(
            client, model, system_blocks, prompt, max_tokens, prior_turns=conversation
        )
        last_raw = raw_text
        conversation.append({"role": "user", "content": prompt})
        conversation.append({"role": "assistant", "content": raw_text})

        try:
            doc = extract_json_object(raw_text)
        except json.JSONDecodeError as exc:
            last_errors = [f"response was not valid JSON: {exc}"]
            continue

        last_errors = validate_synth_doc(doc, output_schema, matchers, record)
        if not last_errors:
            yaml_text = render_cop_yaml(record, matchers, doc)
            return SynthResult(
                cop=record["cop"],
                ok=True,
                yaml_text=yaml_text,
                raw_text=last_raw,
                attempts=attempt,
                errors=[],
                notes=doc.get("notes", []),
            )

    return SynthResult(
        cop=record["cop"],
        ok=False,
        yaml_text=None,
        raw_text=last_raw,
        attempts=max_retries + 1,
        errors=last_errors,
        notes=[],
    )


def load_record(cop: str, roots: list[Path], extract_dir: Path | None) -> dict:
    if extract_dir:
        dept, _, name = cop.partition("/")
        record_path = extract_dir / dept / rs.camel_to_snake(name) / "extract.json"
        if record_path.is_file():
            return json.loads(record_path.read_text())
    return ir_extract.extract_one(cop, roots)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("cops", nargs="+", help="Cop name(s), e.g. Style/TimeNow")
    parser.add_argument(
        "--rubocop-root", type=Path, default=PROJECT_ROOT / "vendor" / "rubocop",
    )
    parser.add_argument("--plugin-root", type=Path, action="append", default=[])
    parser.add_argument(
        "--extract-dir", type=Path, default=None,
        help="Reuse an existing extract.json from this dir instead of re-running Stage 1",
    )
    parser.add_argument("--spec", type=Path, default=None, help="Path to the cop's RuboCop spec file")
    parser.add_argument("--out-dir", type=Path, default=PROJECT_ROOT / "build" / "ir")
    parser.add_argument("--model", default=DEFAULT_MODEL)
    parser.add_argument("--max-retries", type=int, default=DEFAULT_MAX_RETRIES)
    parser.add_argument("--max-tokens", type=int, default=DEFAULT_MAX_TOKENS)
    parser.add_argument(
        "--dry-run", action="store_true",
        help="Print the prompt that would be sent and exit, without calling the API",
    )
    args = parser.parse_args(argv)

    roots = [args.rubocop_root, *args.plugin_root]
    spec_text = args.spec.read_text(encoding="utf-8") if args.spec else None

    if args.dry_run:
        for cop in args.cops:
            try:
                record = load_record(cop, roots, args.extract_dir)
            except ir_extract.ExtractError as exc:
                print(f"ERROR: {exc}", file=sys.stderr)
                return 1
            matchers = normalize_matchers(record)
            schema = build_synth_output_schema(load_ir_schema())
            fewshot_text = render_fewshot_block(build_fewshot_examples())
            system_blocks = build_system_blocks(fewshot_text, schema)
            user_prompt = build_user_prompt(record, matchers, "", spec_text)
            print(f"===== SYSTEM ({cop}) =====")
            print(system_blocks[0]["text"])
            print(f"===== USER ({cop}) =====")
            print(user_prompt)
        return 0

    try:
        client = resolve_api_client()
    except SynthError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 2

    had_failure = False
    for cop in args.cops:
        try:
            record = load_record(cop, roots, args.extract_dir)
        except ir_extract.ExtractError as exc:
            print(f"ERROR: {exc}", file=sys.stderr)
            had_failure = True
            continue

        result = synthesize(
            record,
            model=args.model,
            max_retries=args.max_retries,
            max_tokens=args.max_tokens,
            spec_text=spec_text,
            client=client,
        )

        dept, _, name = cop.partition("/")
        out_dir = args.out_dir / dept / rs.camel_to_snake(name)
        out_dir.mkdir(parents=True, exist_ok=True)
        (out_dir / "synth.raw.txt").write_text(result.raw_text)
        report = {
            "cop": result.cop,
            "ok": result.ok,
            "attempts": result.attempts,
            "errors": result.errors,
            "notes": result.notes,
            "model": args.model,
        }
        (out_dir / "synth.report.json").write_text(json.dumps(report, indent=2) + "\n")

        if result.ok:
            (out_dir / "synth.cop.yml").write_text(result.yaml_text)
            print(f"{cop}: OK after {result.attempts} attempt(s) -> {out_dir}/synth.cop.yml")
            for note in result.notes:
                print(f"  note: {note}")
        else:
            had_failure = True
            print(f"{cop}: NEEDS_HUMAN after {result.attempts} attempt(s):", file=sys.stderr)
            for e in result.errors:
                print(f"  {e}", file=sys.stderr)

    return 1 if had_failure else 0


if __name__ == "__main__":
    raise SystemExit(main())
