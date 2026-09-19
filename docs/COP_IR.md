# Cop IR (schema v1) — reference

> **Experimental.** Nothing loads IR cops at runtime yet. What exists today is
> the document schema (`src/cop/ir/schema.rs`), a fail-closed loader
> (`src/cop/ir/load.rs`), the expression compiler (`src/cop/ir/expr.rs`) and
> evaluator (`src/cop/ir/eval.rs`), the JSON Schema
> (`scripts/shared/ir_schema.json`) and `nitrocop --validate-ir`. The `Cop`
> implementation, registry integration and user-cop discovery land in later
> PRs.

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
fully compiled at load (`CompiledPattern::compile_with`), so an unresolvable
`#helper`, `%param` or `pred?` is a load error rather than a silent `true`.
Matchers are compiled in name order and each sees the ones before it as
`#helper` targets, which makes the pattern reference graph acyclic by
construction; declaring more capture names than the pattern has `$` slots is an
error. A name may not be declared as both a matcher and a predicate.

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

`load_str` compiles every expression into the typed `Expr` tree of design §2.1
(`src/cop/ir/expr.rs`); `IrCop::compiled` carries the result. The language is
**total by construction**: no recursion (the `matches:` graph between named
predicates is checked to be a DAG), no user-defined functions, and the only
iteration is the bounded quantifier family. Operator nesting is capped at 6
levels.

### Operands

* A **scalar** is either a literal or a path reference:
  * `true`, `false`, `3`, `null` — literals;
  * `:sym` — a symbol literal;
  * `node`, `parent`, `$capture`, a quantifier's `var:` name — a node value,
    optionally followed by `.attr` segments (see the attribute table);
  * `cfg.<Key>`, `bind.<Name>`, `consts.<Table>` — exactly two segments, the
    second naming a declared key. Undeclared names are a load error;
  * anything else is a string literal (`"is_a?"`, `"block_argument?"`). Wrap a
    string that would otherwise read as a reference in `{ lit: … }`.
* A **sequence** is an operand list, never an expression in its own right.
* A **mapping** is a single-key operator application.

`bind:` entries see only the binds declared **before** them, so a forward or
self reference is a load error.

### Operator grammar

| Operator | Operand shape | Result |
|---|---|---|
| `all` | expr list | bool — every operand truthy |
| `any` | expr list | bool — some operand truthy |
| `not` | 1 expr | bool |
| `eq`, `ne` | 2 exprs | bool |
| `lt`, `le`, `gt`, `ge` | 2 exprs | bool |
| `in` | `[expr, [expr, …]]` | bool — `eq` against any member |
| `if` | 3 exprs (cond, then, else) | the taken branch's value |
| `lit` | 1 scalar | that literal, never a reference |
| `lookup` | `[consts.<Table>, expr]` | the table's value, or `nil` |
| `attr` | `[expr, "<attr>"]`, or `[expr, "arg", <int>]` | see attribute table |
| `pred` | `[expr, "<name>", <arg>…]` | bool |
| `matches` | `[expr, "<matcher or predicate>"]` | bool |
| `regex` | `[expr, "<source>"]` or `[expr, "<source>", "<imx flags>"]` | bool |
| `any_of`, `all_of`, `none_of` | quantifier mapping | bool |
| `count` | quantifier mapping | int — matching elements |

Comparison rules: two nodes compare by byte range (identity, which is what `==`
on Parser nodes means); ints, bools and `nil` compare by value; anything else
that has a text form — strings, symbols, and a node's source — compares
bytewise. Operands with no common form are only ever `ne`.

Truthiness follows Ruby: only `nil` and `false` are falsey.

### `pred:` names

`pred:` resolves against the NodePattern builtin registry
(`src/node_pattern/predicates.rs`, ~80 entries), with the declared arity
enforced at load (`Arity`) and an unknown name rejected (`UnknownPredicate`).
Four spellings the registry deliberately omits are compiled directly instead:

| Name | Meaning |
|---|---|
| `type?(t, …)` | the node answers to any of those Parser types, groups included |
| `<t>_type?` | `type?(t)` |
| `root?` | no enclosing node |
| `value_used?` | conservative reading of `node.rb:704-721`: a statement that is not the last of its `StatementsNode` is unused, anything else is assumed used |

### Quantifiers

```yaml
when:
  any_of:
    of: node                 # optional subject, defaults to `node`
    over: args               # args | children | ancestors | descendants(N)
    var: a                   # bound to each element inside `body:`
    body: { eq: [a.type, "int"] }
```

`over:` collections:

| Name | Elements |
|---|---|
| `args` | the subject call's arguments |
| `children` | direct children, source order (`descendants(1)`) |
| `ancestors` | enclosing nodes, innermost first |
| `descendants` / `descendants(N)` | every node at most `N` levels below; `N` defaults to and is capped at 8 |

Prism traverses its synthetic `ArgumentsNode` wrapper transparently, so a
call's `children` are its receiver, its individual arguments and its block —
not an arguments wrapper.

### Attributes

Applied with `.name` in a path, or with `{ attr: [<expr>, "<name>"] }`. An
attribute of a non-node, or one the node does not have, is `nil`.

| Attribute | Applies to | Result |
|---|---|---|
| `method_name` | call, def | symbol — the callee/definition name |
| `name` | call, def, const, local/ivar/cvar/gvar, symbol | symbol |
| `receiver` | call | node or nil |
| `body` | def, block, class, module | node or nil |
| `arg_count` | call | int |
| `arg` (index) | call | node or nil — `{ attr: [x, "arg", 1] }` |
| `first_argument`, `last_argument` | call | node or nil |
| `source` | any | string — verbatim source text |
| `line` | any | int — 1-based start line |
| `column` | any | int — 0-based start column |
| `value` | str, sym, int, true, false | the literal's value |
| `type` | any | string — Parser-gem type name |
| `parent_type` | any | string — sugar for `parent.type` |
| `first_child`, `last_child` | any | node or nil — direct children, source order |

### Ancestors

`parent`, `parent_type`, `over: ancestors`, `root?` and `value_used?` all read
`EvalCtx::ancestors`, which is empty until `BatchedCopWalker` maintains an
ancestor stack (design §3.3, separate PR). Until then they answer as if the
node had no enclosing node.

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
`scripts/shared/ir_schema.json` has not drifted from the Rust schema (top-level
fields, operator vocabulary, quantifier keys, `over:` collections, config
types).

Expression evaluation has its own table-driven tests in `src/cop/ir/eval.rs`:
a Ruby snippet, a matcher, a `when:` guard and the boolean it must produce, one
row per operator and per attribute, plus the §1.6 `Style/FileOpen` guard lifted
byte-identically out of `tests/fixtures/ir/valid/file_open.cop.yml` and
evaluated against a real NodePattern match.
