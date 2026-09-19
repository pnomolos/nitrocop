# Custom cops

> **Experimental.** The IR schema is v1 and the expression layer is still
> growing. A document that loads today will keep loading, but new operators and
> predicates land continuously and the `schema: 1` contract is the only thing
> pinned. Nothing here affects nitrocop's RuboCop parity — a custom cop is a cop
> RuboCop has never heard of.

A custom cop is one YAML file. No Rust, no fork, no gem. It uses the same
engine, the same NodePattern dialect and the same config plumbing as the cops
shipped inside the binary.

```
my-project/
  .nitrocop/
    cops/
      no_base_transaction.cop.yml
  .rubocop.yml
  app/
```

```bash
$ nitrocop app
app/models/order.rb:42:24: W: Custom/NoBaseTransaction: Use `ApplicationRecord.transaction` instead of `Base.transaction`.
```

## Where they live

nitrocop loads, in this order:

1. every `*.cop.yml` under **`.nitrocop/cops/`**, recursively, relative to the
   **config root** — the directory holding the `.rubocop.yml` it resolved, or
   the directory you pointed it at if there is none;
2. every `*.cop.yml` under each entry of **`AllCops.CustomCopPaths`**, for
   out-of-tree directories (a monorepo's shared `config/` checkout, say):

   ```yaml
   AllCops:
     CustomCopPaths:
       - ../shared/rubocop/cops     # a directory, walked recursively
       - config/cops/no_sleep.cop.yml   # or a single file
   ```

   Relative entries resolve against the config root; absolute ones are taken
   as-is.

Files that do not end in `.cop.yml` are ignored, so a `README.md` next to the
cops is fine. Discovery order is sorted and stable, which matters because the
documents' contents feed the result-cache key.

Cops shipped inside a gem (a `.nitrocop/cops/` directory in an installed gem,
picked up through `require:`) are **not** supported yet.

### Sharing a `.rubocop.yml` with RuboCop

`AllCops.CustomCopPaths` is deliberately a parameter of `AllCops` rather than a
new top-level key: RuboCop 1.84 prints

```
Warning: AllCops does not support CustomCopPaths parameter.
```

and carries on (exit 1 for offenses), whereas an unrecognized **top-level** key
is a hard `ValidationError` (exit 2). One `.rubocop.yml`, both tools.

The same is not true of a *cop section*. RuboCop rejects

```yaml
Custom/NoBaseTransaction:
  Enabled: true
```

with `Error: unrecognized cop or department Custom/NoBaseTransaction found in
.rubocop.yml` and exits 2. If you run both linters over the same config file,
leave your custom cops unconfigured (they default to `Enabled: true`, which is
usually what you want) or keep their sections in a nitrocop-only file passed
with `--config`.

## Writing one

The document schema, the expression language, the anchor grammar and the
predicate registry are all in **[COP_IR.md](COP_IR.md)**. This is the
end-to-end shape:

```yaml
# .nitrocop/cops/no_base_transaction.cop.yml
schema: 1
cop: "Custom/NoBaseTransaction"
severity: warning
docs: |
  `Base.transaction` always opens on the primary connection. Use
  `ApplicationRecord.transaction`, which is routed by the current role.

# A byte compare that skips every call whose selector is not `transaction`.
restrict_on_send: [transaction]

config:
  Receiver: { type: string, default: "Base" }

matchers:
  transaction_call:
    pattern: "(send (const _ $_) :transaction ...)"
    captures: [receiver]

hooks:
  - on: [send]
    match: transaction_call
    when: { eq: ["$receiver", cfg.Receiver] }
    offense:
      location: node.selector
      message: "Use `ApplicationRecord.transaction` instead of `%{receiver}.transaction`."
```

Reading it:

| Piece | Meaning |
|---|---|
| `cop:` | `Dept/Name`. The department must **not** be a built-in one (`Style`, `Lint`, `Rails`, …) and the full name must not collide with a built-in cop; both are load errors. `Custom/` is the convention. |
| `restrict_on_send:` | Upstream's `RESTRICT_ON_SEND`. Optional, but it is the difference between one byte compare and a pattern walk on every method call in the codebase. |
| `config:` | Declared keys, with types and defaults. Readable as `cfg.<Key>` in expressions and `%{<Key>}` in messages. Undeclared keys cannot be read — a typo is a load error, not a silent `nil`. |
| `matchers:` | Named NodePattern strings. `$` captures are named positionally by `captures:`. |
| `hooks[].on:` | Parser-gem node type names (`send`, `csend`, `block`, `def`, `const`, …), not Prism's. |
| `hooks[].when:` | Optional guard expression, evaluated only after the pattern matched. |
| `offense.location:` | An anchor: `node`, `node.selector`, `$capture.expression`, or an explicit `{ start:, stop: }` pair. |
| `offense.message:` | `%{name}` interpolates a capture, a `bind:` or a config key. A literal percent is `%%`. |

Autocorrect works the same way it does for shipped IR cops — add
`autocorrect: safe` (or `unsafe`) and a `correct:` edit list. See
[COP_IR.md](COP_IR.md#hooks).

## Configuring one

A custom cop is an ordinary cop as far as `.rubocop.yml` is concerned:

```yaml
Custom/NoBaseTransaction:
  Enabled: true            # the default; it is enabled because the file exists
  Severity: error          # overrides the document's `severity:`
  Receiver: "ActiveRecord::Base"   # a key the document declared under `config:`
  Exclude:
    - "spec/support/**/*"
  Include:
    - "app/**/*.rb"
```

Department-level config works too (`Custom: { Enabled: false }`), as do
`--only Custom`, `--only Custom/NoBaseTransaction` and `--except`.

Two deliberate differences from a shipped cop:

- **No preview gating.** Tiers say how confident nitrocop is that a cop matches
  RuboCop; that is meaningless for a cop RuboCop does not have. A custom cop
  runs without `--preview` and reports as `stable`.
- **Classified as `custom`.** `--rules`, `--migrate` and `--doctor` list it as a
  custom cop rather than "outside baseline", which is how an unknown cop name
  in your config still looks.

## Validating

```bash
nitrocop --validate-ir                      # the cops discovered for this project
nitrocop --validate-ir .nitrocop/cops       # a directory
nitrocop --validate-ir path/to/one.cop.yml  # one document
```

Exit 0 if everything loads, 2 if anything does not. Errors render as
`path:line: message`:

```
$ nitrocop --validate-ir
.nitrocop/cops/no_sleep.cop.yml:14: hook `on:` references unknown node type `sned`
1 invalid cop IR definition(s)
```

**Loading is fail-closed.** An invalid document aborts the whole run with exit
2 *before a single file is linted*, because a silently dropped cop is an
invisible false negative — the one failure mode a linter must never have. While
you are mid-edit, `--ignore-invalid-cops` downgrades that to a warning and runs
with the cops that did load.

Editing a cop invalidates the result cache: the documents' contents are part of
the cache's session key, so you never see a stale offense list after changing a
rule.

## Checking what loaded

```bash
$ nitrocop --list-cops | grep custom
Custom/NoBaseTransaction (custom)

$ nitrocop --doctor
...
Custom cops: 1 loaded
  Custom/NoBaseTransaction
```

## What the IR cannot do

The expression layer is total by construction: no recursion, no user-defined
functions, bounded quantifiers only, nesting capped at 6. If your rule needs
real control flow, mutable state across a file, or a cross-file index, the IR
is the wrong tool and there is no escape hatch today — see
[COP_IR.md](COP_IR.md) for the exact boundary and
[LUA_CUSTOM_COPS.md](LUA_CUSTOM_COPS.md) for the scripting design that remains
deferred.
