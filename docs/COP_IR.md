# Cop IR (schema v1) — reference

> **Experimental.** Nothing loads IR cops at runtime yet. This PR ships the
> document schema (`src/cop/ir/schema.rs`), a fail-closed loader
> (`src/cop/ir/load.rs`), the JSON Schema (`scripts/shared/ir_schema.json`) and
> `nitrocop --validate-ir`. The expression compiler, the `Cop` implementation,
> registry integration and user-cop discovery land in later PRs.

A cop IR document is one YAML file per cop, conventionally `<Name>.cop.yml`. It
is `meta` + `config` declarations + named `matchers` (verbatim upstream
NodePattern strings) + `predicates` + `hooks`.

## Validating

```bash
nitrocop --validate-ir path/to/a.cop.yml path/to/b.cop.yml
```

Exit 0 if every document loads, 2 if any fails. Errors render as
`path:line:col: message`. Loading is all-or-nothing: there is no partially
loaded cop, because a silently dropped cop is an invisible false negative.

## Top-level fields

| Field | Type | Default | Notes |
|---|---|---|---|
| `schema` | int | — | **Required.** Must be `1`. |
| `cop` | string | — | **Required.** `Dept/Name`, both halves CamelCase. |
| `hooks` | list | — | **Required**, non-empty. See below. |
| `version_added` | string | — | Informational upstream version. |
| `docs` | string | — | Long-form documentation. |
| `severity` | `convention` \| `warning` \| `error` | `convention` | |
| `enabled_default` | `true` \| `false` \| `pending` | `pending` | Maps to `EnabledState`. |
| `tier` | `preview` \| `stable` | `preview` | Ignored for user cops. |
| `autocorrect` | `none` \| `safe` \| `unsafe` | `none` | Must agree with the presence of `correct:` edits. |
| `include` / `exclude` | list of globs | `[]` | Become `default_include`/`default_exclude`. |
| `restrict_on_send` | list of method names | `[]` | Requires at least one `send`/`csend` hook. |
| `config` | map | `{}` | Declared config keys. |
| `constants` | map of map | `{}` | Frozen lookup tables. |
| `matchers` | map | `{}` | Named NodePattern strings. |
| `predicates` | map | `{}` | Named expression guards, usable as `#name` in patterns. |

Unknown keys are a load error everywhere (`deny_unknown_fields`), so typos fail
closed rather than being silently ignored.

## `config:`

```yaml
config:
  EnforcedStyle: { type: enum, values: ["is_a?", "kind_of?"], default: "is_a?" }
  AllowedMethods: { type: string_array, default: [] }
  Max:            { type: int,    default: 3 }
  AllowGlobals:   { type: bool,   default: false }
  Methods:        { type: string_map, default: {} }
```

`type` is one of `enum`, `string`, `string_array`, `int`, `float`, `bool`,
`string_map`, and maps onto `CopConfig::get_*`. `default` is required and must
match the declared type; `values:` is required for, and only for, `type: enum`,
and the default must be a member.

## `matchers:` and `predicates:`

```yaml
matchers:
  block_form:
    pattern: |
      (block $(call _ {:max_by :min_by}) (args (arg $_x)) (lvar _x))
    captures: [send, var]   # names for the positional `$` captures, in order
    params: []              # `%name` params; each must be a config key or constant table
```

Patterns are **copied verbatim from upstream, never synthesized**; each is
checked with nitrocop's NodePattern parser at load. A name may not be declared
as both a matcher and a predicate.

## `hooks:`

```yaml
hooks:
  - on: [send, csend]        # Parser-gem node type names; numblock/itblock are virtual
    match: time_new          # a matcher name, or { any_of: [...] } / { all_of: [...] }
    when: <expr>             # optional guard
    bind:                    # ordered name -> expr, visible to message/correct
      method: $send.method_name
    offense:
      location: node                    # anchor shorthand, or { start:, stop: }
      message: "Prefer `Time.now`."     # %{name} template; a literal percent is %%
      severity: warning                 # optional per-hook override
      correct:
        - { op: replace, range: { start: node.selector.start, stop: node.expression.stop }, text: "now" }
        - { op: insert_before, at: node.expression.start, text: "(" }
        - { op: insert_after,  at: node.expression.stop,  text: ")" }
        - { op: remove, range: { start: node.dot.start, stop: node.selector.stop } }
```

**Anchors** are `<target>[.<accessor|part>]*[.start|.stop]`, where `target` is
`node`, `parent` or a declared `$capture`; `accessor` is a structural accessor
(`receiver`, `body`, `first_argument`, …) and `part` is a `node.loc.<part>` name
(`expression`, `selector`, `dot`, `keyword`, `end_keyword`, `operator`, `begin`,
`end`). A `location:` shorthand denotes a range and must **not** end in an edge;
an anchor inside `{ start:, stop: }` or `at:` must.

**Messages** interpolate `%{name}` over the hook's captures, its binds and the
declared config keys. Every placeholder must resolve; a bare `%` is an error
(write `%%`).

## Expressions (`when:`, `bind:`, `predicates[].expr`)

Schema v1 keeps expressions as structured-but-untyped YAML; the typed `Expr`
enum arrives with the compiler PR. The loader still enforces the shape:

* a **scalar** is a literal (`"is_a?"`, `3`, `true`) or a path reference —
  `node`, `parent`, `$capture[.attr]*`, `cfg.<Key>`, `bind.<Name>`,
  `consts.<Table>`. References must resolve to something declared; anything that
  does not look like a reference is a string literal;
* a **mapping** is an operator application with exactly one key, drawn from
  `all any not eq ne lt le gt ge in if lit lookup attr pred matches regex any_of
  all_of none_of count`;
* a **sequence** is an operand list;
* operator nesting is capped at 6 levels, so the language cannot drift into a
  programming language by accretion.

## Complete example

```yaml
schema: 1
cop: "Style/TimeNow"
version_added: "1.90"
enabled_default: pending
tier: preview
autocorrect: safe
restrict_on_send: [new]

matchers:
  time_new:
    pattern: |
      (call (const {nil? cbase} :Time) :new)

hooks:
  - on: [send, csend]
    match: time_new
    offense:
      location: node
      message: "Prefer `Time.now` over `Time.new` to retrieve the current time."
      correct:
        - op: replace
          range: { start: node.selector.start, stop: node.expression.stop }
          text: "now"
```

YAML anchors, aliases and merge keys (`<<:`) are supported, which is how a cop
with `block`/`numblock`/`itblock` variants shares one offense spec — see
`tests/fixtures/ir/valid/redundant_min_max_by.cop.yml`.

## Fixtures

* `tests/fixtures/ir/valid/*.cop.yml` — must load.
* `tests/fixtures/ir/invalid/*.cop.yml` — must fail; the sidecar `*.expected`
  gives the expected `IrErrorKind` variant name on line 1 and required message substrings on
  the rest. A `*.user.cop.yml` fixture is loaded with `LoadMode::User`.

`tests/ir_fixtures.rs` walks both directories and also asserts
`scripts/shared/ir_schema.json` has not drifted from the Rust schema.
