# Cop IR (schema v1) — reference

> **Experimental.** IR cops shipped inside the binary run; user-supplied ones do
> not yet. What exists today is the document schema (`src/cop/ir/schema.rs`), a
> fail-closed loader (`src/cop/ir/load.rs`), the expression compiler
> (`src/cop/ir/expr.rs`) and evaluator (`src/cop/ir/eval.rs`), the `Cop`
> implementation (`src/cop/ir/cop.rs`), the embedded table
> (`src/cop/ir/embedded.rs`), the JSON Schema (`scripts/shared/ir_schema.json`)
> and `nitrocop --validate-ir`. Discovery of `.nitrocop/cops/**` and gem-shipped
> packs (design §4.1 items 2-4) lands in a later PR.

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
| `min_target_ruby` | float | — | Upstream `minimum_target_ruby_version`; below it the cop reports nothing. |
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
(`receiver`, `body`, `first_argument`, `left_sibling`, …) and `part` is a
`node.loc.<part>` name
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
| `left_sibling`, `right_sibling` | any | node or nil — the adjacent Parser-gem child of this node's parent |

`left_sibling` / `right_sibling` mirror `RuboCop::AST::Node`'s: they index into
the *Parser-gem* child list, where a `send`'s method name is a Symbol rather
than a node, so `Struct.new(kw: nil)`'s hash has nothing before it while
`Struct.new(:foo, kw: nil)`'s has `:foo`. They find the node's parent by
scanning the enclosing-node chain for the innermost entry that has it as a
direct child, so they work on `node` and on `parent` alike; a `$capture` from
deeper inside the match is not on the chain and answers `nil`.

### Ancestors

`parent`, `parent_type`, `over: ancestors`, `root?`, `value_used?`,
`left_sibling` and `right_sibling` read
`EvalCtx::ancestors`, the walker's chain of enclosing nodes. Maintaining that
chain costs a `Vec` push/pop per branch node, so the walker only does it when
some active cop asks — and `IrCopRunner` asks exactly when the document needs
it, decided once at load by scanning every compiled matcher for `^` and every
compiled expression for `parent`, `parent_type`, `over: ancestors`, `root?`,
`value_used?` or one of the ancestor-reading `pred:` names (`argument?`,
`chained?`, `def_modifier?`, `guard_clause?`, `macro?`, `parent?`). Nothing in
the document has to declare it.

The chain is the raw Prism stack; `node_pattern::ancestors` normalizes it to
Parser-gem ancestry, with the two documented divergences (a call carrying a
literal block is one level where Parser has two; there is no `rescue` level).

## Runtime

`IrCopRunner` (`src/cop/ir/cop.rs`) is the `Cop` implementation. Per node:

1. **tag dispatch** — `interested_node_types()` is the union of the hooks' `on:`
   lists mapped to Prism type tags, so the walker never calls a cop for a node
   type it did not ask for;
2. **type re-discrimination** — one Prism type covers several Parser types, so
   each hook re-checks: `send` vs `csend` off the `&.` operator, and
   `block`/`numblock`/`itblock`/`any_block` off the block's parameters node;
3. **`restrict_on_send`** — a byte compare on the callee name, applied to
   `send`/`csend` hooks only, exactly as upstream's `RESTRICT_ON_SEND` applies
   to `on_send`/`on_csend` and not to `on_block`;
4. **match** — the compiled NodePattern, with `#helper` resolved against the
   document's own `matchers:` and `%param` against its `config:`/`constants:`;
5. **`bind:` then `when:`**;
6. **offense** — the `location:` anchor resolves to a byte range, the message
   template renders, the `Diagnostic` is pushed;
7. **`correct:`** — each edit resolves to a byte range and becomes a
   `Correction`, honoring `autocorrect: safe|unsafe` through
   `Cop::safe_autocorrect` and therefore the existing `-a` allowlist
   (`src/resources/autocorrect_safe_allowlist.json`) and `SafeAutoCorrect`.

Steps 1-4 allocate nothing. The config vector, the bind vector and the rendered
message are built only after a matcher has matched.

### What `on: [block]` dispatches on

The Parser gem's `block` node is nitrocop's `CallNode` carrying a `BlockNode`
(or a `LambdaNode`), which is what `on_block` visits upstream. A `block` hook
therefore dispatches on `CallNode`/`LambdaNode` and **not** on Prism's
`BlockNode` — dispatching on both would report every offense twice.

### Anchors at runtime

`node`, `parent` and `$capture` targets resolve to a node; accessors walk to a
child; the `part` selects a `loc` range; the edge picks an endpoint. Two
vocabulary entries deliberately resolve to nothing: `parent` *mid-path* (it
would need the ancestor chain of a node the walker never visited) and `name`
(bytes, not a node).

An anchor that does not resolve against the node it matched — `node.selector` on
something that is not a call — is a type error the loader's flat vocabulary
check cannot catch. It panics under `debug_assertions`, so `cargo test` catches
it, and drops the offense in a release build rather than reporting at a wrong
location.

### Config binding

`cfg.<Key>` and `%{Key}` read the per-file `CopConfig`, falling back to the
declared `default:`. An `enum` key whose configured value is not a member also
falls back to the default rather than erroring, because a `.rubocop.yml` can
name a style this cop does not have.

### Registering a shipped cop

1. write `src/resources/ir/<dept>/<snake>.cop.yml`;
2. add it to `FILES` in `src/cop/ir/embedded.rs` (`ir_embedded_files_are_listed`
   fails if you forget);
3. add fixtures under `tests/fixtures/cops/<dept>/<snake>/` and one
   `crate::ir_cop_fixture_tests!(<mod>, "Dept/Name", "cops/<dept>/<snake>")`
   line. A fourth argument is a YAML mapping of config the fixtures run under,
   values keeping their YAML type — `"TargetRubyVersion: 3.2"` for a cop with a
   `min_target_ruby:`, which would otherwise never fire under the harness's
   default `CopConfig`. The autocorrect assertion loops until the source stops
   changing, as RuboCop's own `expect_correction` and `nitrocop -A` do.

### Translating an upstream cop

Two shapes recur and are worth knowing before you start.

**`on_send` plus `node.block_node`.** Parser splits `a.select { … }` into
`(block (send a :select) …)`, so upstream's `on_send` sees the bare send and
walks *up*. Prism has one `CallNode` for both levels, so the hook becomes
`on: [block]` (plus `numblock` / `itblock`) and `node` is both: its
`method_name` is the selector and its `selector` part is that selector's `loc`.
`restrict_on_send` then has no `send` hook to gate and has to be written as an
`in:` guard on `node.method_name`. The converse also bites: a `send` hook *does*
fire on a call carrying a literal block, so a cop whose guard reads
`node.parent` needs `not block_literal?` to reproduce upstream's answer
(`Style/FileOpen`).

**`on_send` plus a loop over children.** A hook's `offense:` reports once and
there is no "for each" construct, so a cop that calls `add_offense` per argument
or per pair inverts its dispatch: the hook fires on the *child* and the matcher
ascends with `^` / `^^`. `^` resolves against Parser-visible ancestry, so
Prism's `ArgumentsNode` is traversed and only a genuine direct child matches.
What `^` cannot say is *which* child, so a positional condition
(`node.last_argument.hash_type?`) still needs a guard —
`any_of: { over: ancestors, var: a, body: { eq: [a.last_argument, parent] } }`
is the identity test for it.

An `if`/`elsif`/`else` over correction *ranges* has no `correct:` spelling.
Write one hook per branch, each with the `when:` upstream tests for it
(`Style/RedundantStructKeywordInit` has four).

A shipped document that fails to load is a **panic at startup**: it is a bug in
the binary, not in the user's project, and `ir_embedded_cops_load` exercises
every one of them. New IR cops inherit `src/resources/tiers.json`'s
`default_tier: preview`, so they are invisible without `--preview` until the
corpus gate clears them.

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

## Pipeline

The translation pipeline (design §5) turns one upstream RuboCop cop into one
`.cop.yml` document plus fixtures, in five stages. Stages 1, 2 and 4 are
Python and deterministic (no LLM); stage 3 is the only place a model is
called, and stage 5 is the gate nothing skips.

```
ir_extract.py  ─▶  ir_classify.py  ─▶  ir_synth.py  ─▶  spec_to_fixture.py  ─▶  ir_verify.py
  (Stage 1)          (Stage 2)          (Stage 3)          (Stage 4)              (Stage 5)
 extract.json      A/B/C bucket      synth.cop.yml      tests/fixtures/…       pass/fail + report
```

### Commands

```bash
# Stage 1: pull matchers/constants/config/messages/hooks out of the upstream
# Ruby source, verbatim where possible. Produces extract.json + a skeleton
# .cop.yml with hooks[].offense deliberately left out.
python3 scripts/workflows/ir_extract.py Style/TimeNow \
    --rubocop-root vendor/rubocop --out-dir build/ir

# Stage 2: bucket A (translate directly), B (translate with care), or C (stays
# hand-written Rust) — prints the rationale.
python3 scripts/workflows/ir_classify.py Style/TimeNow

# Stage 3: call the model to fill hooks[].when/bind/offense (never
# matchers:/config:/constants:, which are copied verbatim by the script, not
# the model — see ir_synth.py's module docstring for the enforcement). Needs
# ANTHROPIC_API_KEY (or ANTHROPIC_AUTH_TOKEN); exits 2 with a clear message
# if neither is set.
export ANTHROPIC_API_KEY=...
python3 scripts/workflows/ir_synth.py Style/TimeNow \
    --extract-dir build/ir \
    --spec vendor/rubocop/spec/rubocop/cop/style/time_now_spec.rb
# --dry-run prints the exact prompt without calling the API.
python3 scripts/workflows/ir_synth.py Style/TimeNow --dry-run

# Stage 4: convert the upstream RuboCop spec's expect_offense/
# expect_no_offenses/expect_correction blocks into nitrocop fixtures.
python3 scripts/spec_to_fixture.py \
    vendor/rubocop/spec/rubocop/cop/style/time_now_spec.rb

# Stage 5: the gate. validate-ir, matcher byte-equality, a differential run
# against real RuboCop at the pinned upstream version (harvesting inputs from
# the spec, but trusting only real `rubocop --format json` for the expected
# offenses — see "Honest limitations" below), cargo test, -A convergence,
# no_offense silence, and a fixture coverage floor.
mise exec -- gem install rubocop -v 1.91.0 --install-dir /tmp/rubocop-1.91.0 --no-document
python3 scripts/ir_verify.py Style/TimeNow src/resources/ir/style/time_now.cop.yml \
    --rubocop-root vendor/rubocop --rubocop-gem-dir /tmp/rubocop-1.91.0
```

A human step sits between stages 3 and 5 for any cop that is not already
shipped: copy the synthesized `build/ir/<Dept>/<snake>/synth.cop.yml` to
`src/resources/ir/<dept>/<snake>.cop.yml`, add it to `FILES` +
an `ir_cop_fixture_tests!` line in `src/cop/ir/embedded.rs`, add fixtures
under `tests/fixtures/cops/<dept>/<snake>/` (stage 4's output, reviewed), and
rebuild. `ir_verify.py`'s `embedded_freshness` check exists specifically to
catch a stale binary in this handoff — see below.

### Honest limitations (from the pilot batch's friction log — PRs #19, #23)

- **A stale binary looks green.** `nitrocop`'s runtime only ever executes
  cops `include_str!`'d into `src/cop/ir/embedded.rs` at build time — there is
  no dynamic-loading path yet for an arbitrary `.cop.yml`. `ir_verify.py`'s
  differential/-A-convergence/no_offense checks run the compiled binary
  end to end, so they can only mean anything for a document that IS the
  currently embedded one (`embedded_freshness` hard-fails otherwise, with
  the exact steps to fix it). Pointing `ir_verify.py` at
  `build/ir/.../synth.cop.yml` before promoting it will not silently pass —
  it fails loudly and tells you why.
- **Captures and `%param`s are not extracted, and the model must not name
  them either.** `ir_extract.py`'s skeleton leaves `captures: []`; naming
  them is matcher metadata, which the model is forbidden from touching. Stage
  3 fills them in mechanically instead (`ir_synth.normalize_matchers`):
  synthetic, positional names (`capture_1`, `capture_2`, ...) when none were
  given, real ones if a human already edited the extraction record. Rename
  them for readability before shipping — the pilot batch's own examples
  (`redundant_min_max_by.cop.yml`) use `send`/`var`, not `capture_1`/`capture_2`.
- **The predicate whitelist is vendored, not queried.** Design §2.2 calls for
  `nitrocop --list-ir-predicates`; it does not exist yet. `ir_synth.py` and
  `ir_verify.py` both hardcode the ~80 names from
  `src/node_pattern/predicates.rs`, checked against a live grep of that file
  in `tests/python/workflows/test_ir_synth.py`. A new predicate added to the
  Rust registry needs a matching addition here until the flag exists.
- **The differential check needs the exact pinned upstream RuboCop version,
  not the corpus bundle's.** `bench/corpus/vendor/bundle` is pinned to
  RuboCop 1.84.2; cops translated from newer upstream releases (all the
  pilot batch, from 1.91.0) need their own scratch `gem install`, per AGENTS.md.
  `Style/TimeNow` and its pilot siblings do not exist at 1.84.2 at all.
- **One upstream version per document, for now.** CI's `ir-verify` job
  hardcodes `1.91.0` because every shipped IR cop happens to come from it.
  The first cop pinned to a different upstream release needs this
  generalized to read per-document (e.g. from `version_added:`).
- **`it`-block diffs at an older `TargetRubyVersion` are informational, not
  proof of correctness.** `ir_verify.py` only hard-fails a differential
  mismatch at the newest requested `TargetRubyVersion` (3.4 by default); a
  mismatch that disappears there is logged but does not fail the gate — it
  usually means RuboCop's parser gem needs 3.4 to parse `it`/numbered-param
  blocks at all, not that nitrocop is wrong (friction log item 12/13 in PR
  #19/#23).
- **A RuboCop spec's own asserted message can be wrong.** PR #23's friction
  log documents exactly this (`redundant_min_max_by_spec.rb`). This is why
  Stage 5's differential trusts only real `rubocop --format json` output for
  expected offenses, never the spec's inline `^^^` annotation text, even
  though Stage 4 harvests the spec's *input* source from the same file.
- **The corpus oracle gate (design §5 Stage 5 item 4, `check_cop.py`) is a
  separate, CI-only, future step** — `ir_verify.py` is the mechanical gate
  only. A cop is not `stable` without a clean corpus run too, same as any
  hand-written cop (AGENTS.md).
