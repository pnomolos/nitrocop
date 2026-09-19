# nitrocop Cop IR — design

## Executive summary (25 lines)

1. **Format: YAML**, one `<Name>.cop.yml` per cop. The repo already depends on `serde_yml` and `CopConfig.options` is `HashMap<String, serde_yml::Value>` (`src/cop/mod.rs:49-56`); `.rubocop.yml` is YAML; block scalars hold NodePattern strings byte-identically to RuboCop's heredocs. TOML/bespoke-DSL/Rust-DSL all lose on one of: multiline strings, tooling, or G4 (no recompile).
2. IR = `meta` + `config` declarations + named `matchers` (verbatim NodePattern) + `predicates` + `hooks`; each hook is `on:` (Parser-gem node types) + `match:` + optional `when:` guard + `offense:` (location/message/correct).
3. **Patterns are copied verbatim from upstream, never synthesized.** Only guards, messages, locations and corrections are authored/LLM-synthesized. This is the single biggest fidelity lever.
4. The "beyond NodePattern" layer is a **total expression language** (`Expr` enum, §2): builtin predicates, attribute access, comparisons, `all`/`any`/`not`, bounded quantifiers over `args`/`ancestors`/`descendants(depth)`. No loops, no recursion, no user functions — matcher references form a compile-time-validated DAG.
5. Predicate registry is seeded from rubocop-ast (`vendor/rubocop-ast/lib/rubocop/ast/node.rb:413-580`, `node/mixin/method_dispatch_node.rb:20-247`) and backed by nitrocop's existing `src/cop/shared/{util,literal_predicates,method_identifier_predicates}.rs`. `#helper?` in a pattern resolves to the cop's own matchers first, then the registry.
6. Bucket C stays in Rust: layout/alignment arithmetic, token streams, `on_new_investigation` file state machines, VariableForce consumers, and RuboCop's new cross-file `ProjectIndexHelp` cops.
7. Runtime: one `IrCop: Cop` type. YAML parsed once at registry build, compiled into a data-driven `Matcher`/`Expr` enum tree (no closures), fully immutable → `Send + Sync` for free.
8. Dispatch: `interested_node_types()` returns the union of hooks' Prism tags, so `BatchedCopWalker` (`src/cop/walker.rs:42-90`) already gives IR cops O(1) dispatch with zero changes.
9. Walker needs one addition: an ancestor stack (`visit_branch_node_leave` exists in ruby-prism 1.9.0 `src/lib.rs:1233`) to support `^`, `.parent`, and `value_used?`.
10. `src/node_pattern` needs ~2,100 LOC to finish: real captures (~200), `#predicate` + registry (~1,000), `%param` (~120), `^` (~250), `` ` `` (~150), `<>` (~300), plus mapping expansion incl. **virtual `numblock`/`itblock`/`any_block` types** Prism does not have (~150).
11. Estimated per-node cost: ~50-150ns for a typical 2-level pattern vs ~20-40ns hand-written; irrelevant cops cost 0 thanks to the dispatch table. Corpus/bench is the gate.
12. **Recommend interpretation, not codegen.** One engine means user cops and translated built-ins cannot diverge semantically; codegen saves only matcher dispatch (small vs parse cost), bloats diffs, and hurts upstreamability. The compiled `Matcher` enum is the natural codegen IR if bench ever demands it.
13. Embedded built-ins ship as `include_str!`'d YAML in `src/resources/ir/`, parsed lazily behind a `OnceLock`. Revisit only if `--debug` shows startup cost.
14. User cops: `.nitrocop/cops/**/*.cop.yml`, plus gem-shipped packs resolved the same way plugin configs already are (`src/config/gem_path.rs`). Configured in `.rubocop.yml` under their own department (conventionally `Custom/`).
15. Invalid definitions **fail closed** (exit 2, `file:line` from serde_yml) — a silently-skipped custom cop is an invisible FN.
16. `Cop::name() -> &'static str` and `Correction.cop_name: &'static str` (`src/correction.rs:10`) force a `Box::leak` of loaded cop names. Accept it; ~30 bytes/cop beats churning 920 signatures.
17. Translation pipeline (G3) = extract (Python, deterministic) → classify A/B/C → synthesize only the non-pattern parts under a strict JSON schema → verify mechanically. LLM lives in `scripts/workflows/ir_synth.py`, never in the binary.
18. Verification chain: schema validate → RuboCop spec `expect_offense` blocks converted to nitrocop fixtures (the `^^^` annotation format is already nitrocop's, modulo the `Cop/Name:` prefix `scripts/generate_fixture.py:100-112` adds) → `cargo test` → `generate_fixture.py` differential against real RuboCop → per-cop corpus gate in CI.
19. Of the 23 net-new upstream cops, **5 are blocked** on RuboCop's new `ProjectIndexHelp` cross-file index (ArgumentMismatch, DeprecatedReference, NameTypo, SuperArgumentMismatch, UnusedPrivateMethod) — new infrastructure, not IR work. Call this out early.
20. Pilot set (bucket A/B, IR-expressible): TimeNow, FileOpen, DataDefineOverride, RedundantMinMaxBy, PredicateWithKind, TallyMethod, RedundantStructKeywordInit, SelectByKind, MatchWithSimpleRegex — 9 cops.
21. Bucket C from the 23 (hand-written Rust): MisplacedMagicComment, DirectiveScope, PartitionInsteadOfDoubleSelect, plus the 5 index cops.
22. PR sequence: 5 node_pattern PRs (captures, predicates, params, `^`/`` ` ``, `<>`+mapping) → IR schema+loader → interpreter MVP → 1 cop → 8 cops → user discovery → extractor → spec→fixture → synthesizer. Each 200-900 LOC, independently reviewable, corpus-neutral until the first cop lands.
23. Every IR-translated cop enters at `preview` tier, so it cannot regress the oracle before `check_cop.py` clears it.
24. Biggest risks: `<>` backtracking blowup (cap children at 8, reject otherwise), message-format fidelity (`%<x>s` → `%{x}` is mechanical; validate against RuboCop's own output), and IR expressiveness creep toward Turing-completeness (hard schema, reject `when:` depth > 6).
25. Lua stays deferred. If the `when:` layer proves insufficient for a specific cop, that cop stays Rust; Lua is only worth it when users demand logic the expression layer refuses.

---

## 0. Scope and existing-code facts this rests on

- `Cop` trait: `src/cop/mod.rs:273-380`. Three entry points (`check_lines`, `check_source`, `check_node`), selective dispatch via `interested_node_types() -> &'static [u8]` (`src/cop/mod.rs:359-362`).
- `BatchedCopWalker` buckets cops into `[Vec<...>; NODE_TYPE_COUNT]` at construction (`src/cop/walker.rs:46-73`); per-node cost is one array index plus the interested cops' calls.
- `CopRegistry::default_registry()` is a flat, hand-written list of `register_all` calls (`src/cop/registry.rs:20-38`); there is no dynamic registration path.
- `CopConfig` is the cops' only config surface (`src/cop/mod.rs:49-150`), built by `Config::cop_config` (`src/config/mod.rs:2320`) and precomputed per cop index (`src/config/mod.rs:2569-2576`).
- `Correction` is a flat byte-range edit with `cop_name: &'static str` (`src/correction.rs:1-13`); `CorrectionSet::from_vec` sorts by `(start, cop_index)` and drops overlaps (`src/correction.rs:26-46`).
- `src/node_pattern/*` is a research spike not wired to any cop: `HelperCall`/`ParamRef`/`ParentRef`/`DescendRef` all evaluate to `true` (`src/node_pattern/interpreter.rs:536-539, 564-567, 582-585`), `Capture` is transparent (`:534`), and `<>` has no token or AST node at all (`src/node_pattern/parser.rs:7-51`).
- Tiering: `TierMap::load()` reads `include_str!("../resources/tiers.json")` with `default_tier: preview` (`src/cop/tiers.rs:29-51`, `src/resources/tiers.json:3`).
- Fixture harness: `cop_fixture_tests!` (`src/cop/mod.rs:398-420`), `cop_variant_fixture_tests!` (`:487`), `cop_autocorrect_fixture_tests!` (`:556-566`, expects `offense.rb` + `corrected.rb`), directives `# nitrocop-config:` (`src/testutil.rs:485`), `# nitrocop-expect:`, `# nitrocop-filename:` (`src/testutil.rs:75-122`).

---

## 1. IR format and schema

### 1.1 Format choice — YAML

| Option | Verdict |
|---|---|
| **YAML** | **Chosen.** `serde_yml` already a dependency and already the type of `CopConfig.options`. Block scalars (`\|`) carry multi-line NodePattern text verbatim — a byte-for-byte copy of RuboCop's `<<~PATTERN` heredocs, which is the whole fidelity argument. Users already edit `.rubocop.yml`. Anchors/aliases available for shared offense specs. JSON Schema validation off the shelf. |
| TOML | Rejected. Multi-line strings inside arrays-of-tables are miserable for the `hooks:` list, and deep `when:` trees become `[[hooks.when.all]]` noise. No upside over YAML here. |
| Bespoke text DSL | Rejected. A second hand-written parser to maintain, no editor support, no schema tooling — and it buys nothing, because the only genuinely bespoke grammar (NodePattern) is already an opaque string either way. |
| Rust-embedded DSL (macro/builder) | Rejected outright for G4: it requires recompilation. Viable only as a codegen *target* (§4.5), not as the authoring format. |

YAML's known footguns are contained by schema validation: `no`/`on`/`off` as booleans (all our enum values are quoted in the schema), and tag injection (we `serde_yml::from_str` into typed structs, never `Value` for structural keys).

### 1.2 Schema (v1)

```
schema: 1                       # int, required, refuse unknown majors
cop: "Style/TimeNow"            # "Dept/Name"; department derived from the prefix
version_added: "1.90"           # string, informational
docs: |                         # optional; surfaced by --show-cops
  ...
severity: convention            # convention|warning|error  (default: convention)
enabled_default: pending        # true|false|pending  → EnabledState (src/cop/mod.rs:29-45)
tier: preview                   # preview|stable (default preview; ignored for user cops)
autocorrect: safe               # none|safe|unsafe → supports_autocorrect/safe_autocorrect
include: ["**/*.gemspec"]       # → default_include()
exclude: []                     # → default_exclude()
restrict_on_send: [new]         # optional method-name prefilter on send/csend hooks

config:                         # declared config keys; type-checked at load
  EnforcedStyle:
    type: enum
    values: ["is_a?", "kind_of?"]
    default: "is_a?"
  AllowedMethods: { type: string_array, default: [] }
  Max:            { type: int,    default: 3 }
  AllowGlobals:   { type: bool,   default: false }
  Methods:        { type: string_map, default: {} }

constants:                      # frozen lookup tables (upstream's FOO = {...}.freeze)
  replacements: { max_by: "max", min_by: "min", minmax_by: "minmax" }

matchers:                       # named NodePattern strings, copied verbatim
  time_new:
    pattern: |
      (call (const {nil? cbase} :Time) :new)
    captures: []                # names for positional $ captures, in order
    params: []                  # names for %1.. / %name params, bound from config or consts

predicates:                     # named Expr guards; usable as `#name` inside patterns
  kind_method:
    expr: { in: [self.method_name, [":is_a?", ":kind_of?", ":instance_of?"]] }

hooks:
  - on: [send, csend]           # Parser-gem type names; compiled to Prism tags
    match: time_new             # matcher name, or {any_of: [...]} / {all_of: [...]}
    when: <Expr>                # optional guard, evaluated with captures bound
    bind:                       # ordered name → Expr, available to message/correct
      method: $send.method_name
    offense:
      location: <LocationSpec>
      message: "..."            # %{name} interpolation over captures + binds
      severity: warning         # optional per-hook override
      correct:                  # ordered list of edits; omitted ⇒ no autocorrect
        - { op: replace, range: <RangeSpec>, text: "now" }
        - { op: insert_before, at: <Anchor>, text: "(" }
        - { op: insert_after,  at: <Anchor>, text: ")" }
        - { op: remove, range: <RangeSpec> }
```

**Node type names in `on:`** are Parser-gem names (`send`, `csend`, `block`, `numblock`, `itblock`, `def`, `if`, …), matching upstream's `on_*` hooks 1:1 so translation is textual. The loader maps them to Prism `u8` tags via an extended `src/node_pattern/mapping.rs`. Three of them (`numblock`, `itblock`, `any_block`) are **virtual**: Prism has no such nodes — a `BlockNode` whose `parameters()` is `NumberedParametersNode` is `numblock`, `ItParametersNode` is `itblock` (AGENTS.md "Prism block body shapes"). `interested_node_types()` therefore yields `BLOCK_NODE` for all three and the hook re-discriminates at entry.

**LocationSpec** — one of:
- shorthand string: `node`, `node.selector`, `node.receiver`, `$capture`, `$capture.selector`
- `{ start: <Anchor>, stop: <Anchor> }`

**Anchor** = `<target>.<part>.<edge>`, where `target` ∈ {`node`, `parent`, `$name`}, optionally chained through structural accessors (`.receiver`, `.body`, `.first_argument`), `part` ∈ {`expression` (default), `selector`, `dot`, `keyword`, `end_keyword`, `operator`, `begin`, `end`}, `edge` ∈ {`start`, `stop`}. This is a direct transcription of RuboCop's `node.loc.<part>.{begin,end}_pos`, which is what every `RangeHelp`-using correction actually does.

**Message templates** use `%{name}`. Upstream `format(MSG, method: x)` with `MSG = '... %<method>s ...'` converts mechanically: `%<name>s` → `%{name}`, `%%` → `%`. Multiple upstream `MSG_*` constants become multiple hooks (or a `bind` + one template).

**Correction ordering**: edits are emitted in listed order, each becoming one `Correction` with the cop's registry index. Overlaps *within* one cop are a load-time error where statically detectable (identical anchors) and otherwise fall through to `CorrectionSet`'s drop rule (`src/correction.rs:38-43`) — same semantics a hand-written cop gets.

### 1.3 Example 1 — Style/TimeNow (trivial; upstream `lib/rubocop/cop/style/time_now.rb` @ v1.91.0)

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

Upstream is `corrector.replace(node.loc.selector.join(node.source_range.end), 'now')` — the `RangeSpec` is a literal transcription. Note `(call ...)` (not `(send ...)`) plus the `alias on_csend on_send` is why `on:` lists both.

### 1.4 Config binding (excerpt, modeled on the shipped `Style/ClassCheck`, `src/cop/style/class_check.rs:31-56`)

```yaml
config:
  EnforcedStyle: { type: enum, values: ["is_a?", "kind_of?"], default: "is_a?" }

hooks:
  - on: [send, csend]
    match: class_check
    bind:
      prefer:  { if: [{ eq: [cfg.EnforcedStyle, "is_a?"] }, "is_a?",   "kind_of?"] }
      current: { if: [{ eq: [cfg.EnforcedStyle, "is_a?"] }, "kind_of?", "is_a?"] }
    when: { eq: [node.method_name, bind.current] }
    offense:
      location: node.selector
      message: "Prefer `Object#%{prefer}` over `Object#%{current}`."
```

`cfg.<Key>` reads through `CopConfig::get_*` with the declared type and default; an undeclared key referenced in an expression is a load-time error, which also gives us a free `--show-cops` config listing.

### 1.5 Example 2 — Style/RedundantMinMaxBy (captures, const table, capture-driven message and correction)

```yaml
schema: 1
cop: "Style/RedundantMinMaxBy"
version_added: "1.85"
enabled_default: pending
tier: preview
autocorrect: safe

constants:
  replacements: { max_by: "max", min_by: "min", minmax_by: "minmax" }

matchers:
  block_form:
    pattern: |
      (block $(call _ {:max_by :min_by :minmax_by}) (args (arg $_x)) (lvar _x))
    captures: [send, var]
  numblock_form:
    pattern: |
      (numblock $(call _ {:max_by :min_by :minmax_by}) 1 (lvar :_1))
    captures: [send]
  itblock_form:
    pattern: |
      (itblock $(call _ {:max_by :min_by :minmax_by}) _ (lvar :it))
    captures: [send]

hooks:
  - on: [block]
    match: block_form
    bind: &rmb_bind
      method: $send.method_name
      replacement: { lookup: [consts.replacements, $send.method_name] }
    offense: &rmb_offense
      location: &rmb_range
        start: $send.selector.start
        stop:  node.end_keyword.stop
      correct:
        - { op: replace, range: *rmb_range, text: "%{replacement}" }
      message: "Use `%{replacement}` instead of `%{method} { |%{var}| %{var} }`."

  - on: [numblock]
    match: numblock_form
    bind: *rmb_bind
    offense:
      <<: *rmb_offense
      message: "Use `%{replacement}` instead of `%{method} { _1 }`."

  - on: [itblock]
    match: itblock_form
    bind: *rmb_bind
    offense:
      <<: *rmb_offense
      message: "Use `%{replacement}` instead of `%{method} { it }`."
```

`$var` binds a `Capture::Name` (the block arg symbol) and interpolates as bare text; `$send` binds a `Capture::Node`. `node.end_keyword` is the `}`/`end` location — for a Prism `BlockNode` this is `closing_loc()`, resolved by the anchor table, not by the IR author.

### 1.6 Example 3 — Style/FileOpen (needs conditions NodePattern cannot express)

Upstream guards are `node.block_argument?`, `node.value_used?`, `node.parent.lvasgn_type?`, and `node.parent.receiver == node`.

```yaml
schema: 1
cop: "Style/FileOpen"
version_added: "1.85"
enabled_default: pending
tier: preview
autocorrect: none
restrict_on_send: [open]

matchers:
  file_open:
    pattern: |
      (send (const {nil? cbase} :File) :open ...)

hooks:
  - on: [send, csend]
    match: file_open
    when:
      all:
        - { not: { pred: [node, "block_argument?"] } }
        - any:
            - { not: { pred: [node, "value_used?"] } }
            - { pred: [parent, "type?", "lvasgn"] }
            - { eq: [parent.receiver, node] }
    offense:
      location: node
      message: "`File.open` without a block may leak a file descriptor; use the block form."
```

`value_used?` and `block_argument?` are registry predicates implemented once in Rust (§2.2); `parent` comes from the walker's ancestor stack (§3.3); `eq` on two node values compares identity by byte range, which is what `==` on Parser nodes effectively means here.

---

## 2. The "beyond NodePattern" layer

The census puts 70% of cops in bucket B: matchers plus 25-80 LOC of procedural glue. Almost all of that glue is (a) guard clauses, (b) message formatting, (c) a location/range computation. (a) is what this section defines.

### 2.1 Expression language — total by construction

```rust
/// Compiled guard/bind expression. Immutable; no closures; Send + Sync.
pub enum Expr {
    // --- values ---------------------------------------------------------
    Node(Target),                     // node | parent | $cap | ancestor(n)
    Attr { of: Box<Expr>, attr: Attr },// .receiver .body .first_argument .method_name
                                       // .name .source .line .column .arg_count .type
    Lit(Lit),                          // Str | Int | Bool | Sym | Nil
    Cfg(ConfigKeyId),                  // cfg.EnforcedStyle (typed at load)
    Bind(BindId),                      // bind.current
    Lookup { table: ConstId, key: Box<Expr> },
    If { cond: Box<Expr>, then: Box<Expr>, els: Box<Expr> },

    // --- predicates -----------------------------------------------------
    Pred { of: Box<Expr>, pred: PredId, args: Vec<Expr> },  // registry, §2.2
    Matches { of: Box<Expr>, matcher: MatcherId },          // named matcher / #helper
    Regex { of: Box<Expr>, re: RegexId },                   // on .source / .name

    // --- logic / comparison ---------------------------------------------
    All(Vec<Expr>), Any(Vec<Expr>), Not(Box<Expr>),
    Cmp { op: CmpOp, lhs: Box<Expr>, rhs: Box<Expr> },      // eq ne lt le gt ge
    In { needle: Box<Expr>, haystack: Vec<Expr> },

    // --- bounded quantifiers (the only iteration) -----------------------
    Quant { kind: QuantKind,           // AnyOf | AllOf | NoneOf | Count
            over: Collection,          // Args | Children | BodyStatements | HashPairs
                                       // | Ancestors | Descendants { max_depth: u8 }
            var: SlotId,
            body: Box<Expr> },
}

pub enum Target { Node, Parent, Ancestor(u8), Capture(SlotId), Var(SlotId) }
```

Totality argument: there is no recursion, no user-defined function, and no unbounded loop. `Quant` iterates a finite collection derived from a finite AST; `Descendants` carries an explicit depth cap. Named matcher/predicate references are resolved to indices at load time and the reference graph is validated to be a **DAG** — mutual recursion is a load error. Evaluation therefore terminates in time linear in (expression size × subtree size). The schema additionally rejects `when:` nesting deeper than 6, which keeps the language from drifting toward a programming language by accretion.

Evaluation returns `Value<'pr> { Bool, Int, Str(Cow<[u8]>), Node(ruby_prism::Node<'pr>), Absent }`; type errors are load-time where statically knowable and `false` at runtime otherwise (never a panic — a panicking cop is worse than a missing one).

### 2.2 Builtin predicate registry

Seeded from what RuboCop patterns and cop bodies actually call. The census counts 47 unique `#helper` names across 949 patterns (20.9% of patterns use one); sampling the vendored gems shows the majority are *cop-local* helpers (`#array_receiver?`, `#focusable_selector?`) or rubocop-rspec `Language` module matchers (`#rspec?`, `#ExampleGroups.all`) — i.e. they resolve to **other patterns**, which the IR expresses as `matchers:`/`predicates:` entries, not as Rust. Only a minority are rubocop-ast builtins. Resolution order for `#foo?`: cop-local `matchers` → cop-local `predicates` → shared IR prelude (a `_prelude.cop.yml` carrying the rubocop-rspec `Language` patterns) → Rust registry → load error.

The Rust registry (`src/cop/ir/predicates.rs`), grouped by backing module:

| Group | Predicates | Backing |
|---|---|---|
| Type | `type?(t,…)`, `<t>_type?`, `any_block_type?`, `any_def_type?`, `boolean_type?`, `numeric_type?`, `range_type?`, `any_str_type?`, `any_sym_type?`, `call_type?`, `argument_type?` | `node_type.rs` + virtual-type discriminator |
| Structure | `parent?`, `root?`, `chained?`, `argument?`, `multiline?`, `single_line?`, `empty_source?`, `parenthesized_call?`, `value_used?`, `guard_clause?` | new; `value_used?` mirrors `vendor/rubocop-ast/.../node.rb:704-721` |
| Literal | `literal?`, `basic_literal?`, `truthy_literal?`, `falsey_literal?`, `mutable_literal?`, `immutable_literal?`, `composite_literal?` | `src/cop/shared/literal_predicates.rs:33-120` (already 1:1) |
| Variable/assign | `variable?`, `reference?`, `assignment?`, `equals_asgn?`, `shorthand_asgn?` | `node.rb:461-477` |
| Conditional | `conditional?`, `basic_conditional?`, `post_condition_loop?`, `loop_keyword?`, `keyword?`, `special_keyword?`, `operator_keyword?` | `node.rb:481-513` |
| Dispatch | `macro?`, `access_modifier?`, `bare_access_modifier?`, `command?(n)`, `setter_method?`, `dot?`, `double_colon?`, `safe_navigation?`, `self_receiver?`, `const_receiver?`, `implicit_call?`, `block_literal?`, `block_argument?`, `arithmetic_operation?`, `lambda?`, `lambda_literal?`, `unary_operation?`, `binary_operation?`, `def_modifier?` | `method_dispatch_node.rb:57-247`; access-modifier half already in `src/cop/shared/access_modifier_predicates.rs` |
| Method name | `operator_method?`, `comparison_method?`, `assignment_method?`, `predicate_method?`, `bang_method?`, `camel_case_method?`, `enumerable_method?`, `enumerator_method?`, `nonmutating_*_method?` | `src/cop/shared/method_identifier_predicates.rs:352-455` (already 1:1, 17 fns) |
| Source/text | `source_matches?(re)`, `starts_line?`, `line_count`, `first_line`, `last_line`, `column`, `comment_on_line?`, `preceding_comment?` | `src/cop/shared/util.rs:453-478, 543-568` |
| Config/env | `target_ruby_at_least(v)`, `target_rails_at_least(v)`, `gem_present?(g)` | `CopConfig` injections (`src/config/mod.rs:2320-2360`) |

Roughly 80 predicates; ~55 are thin wrappers over code that already exists. Each new one is a small, testable Rust fn — and each is reusable by hand-written cops, so the registry is not dead weight if the IR stalls.

### 2.3 Explicitly out of scope for the IR

These stay hand-written Rust (or, much later, Lua):

1. **Layout/alignment arithmetic** — indentation width, column alignment, `RangeHelp` range stitching across tokens, `Alignment`/`SurroundingSpace`/`EndKeywordAlignment` mixins. 8 of the 30 largest core cops are Layout for exactly this reason.
2. **Token streams and comment directives** — anything reading `processed_source.tokens` or the comment table: `Lint/RedundantCopDisableDirective`, `Style/DirectiveScope`, `Lint/MisplacedMagicComment`.
3. **File-level state machines** — `on_new_investigation` (76 cops) accumulating state across nodes and reporting at EOF. The IR has no mutable per-file state by design (it is what makes `Send + Sync` free). A future `on_file_end` hook with a declarative accumulator is possible; not v1.
4. **VariableForce consumers** — the dataflow engine has its own trait (`src/cop/mod.rs:378-380`).
5. **Cross-file indexes** — RuboCop 1.9x's `ProjectIndexHelp` cops need a whole-project symbol index nitrocop does not have.
6. **Heredoc/percent-literal/encoding edge handling** and anything needing `CodeMap`.

Rule of thumb for the classifier: if the cop needs `check_source`/`check_lines` rather than `check_node`, it is not an IR cop.

---

## 3. Runtime execution model

### 3.1 Types

```rust
pub struct IrCop {
    meta: CopMeta,                    // name: &'static str (leaked), severity, tier, ac mode
    node_tags: &'static [u8],         // leaked union of hooks' Prism tags
    hooks: Vec<CompiledHook>,
    matchers: Vec<Matcher>,           // index = MatcherId
    exprs: Arena<Expr>,
    consts: Vec<ConstTable>,
    regexes: Vec<regex::Regex>,
    cfg_keys: Vec<ConfigKeySpec>,     // name + type + default, index = ConfigKeyId
}

struct CompiledHook {
    types: TypeMask,                  // incl. virtual numblock/itblock discrimination
    restrict_send: Option<&'static [&'static str]>,
    matcher: MatcherId,
    guard: Option<ExprId>,
    binds: Vec<(Box<str>, ExprId)>,
    offense: OffenseSpec,             // location, message template, severity, edits
}

impl Cop for IrCop {
    fn name(&self) -> &'static str { self.meta.name }
    fn interested_node_types(&self) -> &'static [u8] { self.node_tags }
    fn supports_autocorrect(&self) -> bool { self.meta.ac != Ac::None }
    fn safe_autocorrect(&self) -> bool { self.meta.ac == Ac::Safe }
    fn check_node(&self, src, node, pr, cfg, diags, corrs) { /* §3.2 */ }
}
```

Everything is immutable after compilation and contains no interior mutability → `Send + Sync` derives structurally. No `Mutex`, so the TOCTOU hazard AGENTS.md warns about cannot arise. Per-node match state lives on the stack:

```rust
pub struct MatchEnv<'pr, 'a> {
    node: ruby_prism::Node<'pr>,
    ancestors: &'a [ruby_prism::Node<'pr>],   // innermost last
    caps: SmallVec<[Capture<'pr>; 8]>,
    binds: SmallVec<[Value<'pr>; 4]>,
    src: &'a SourceFile,
    cfg: &'a CopConfig,
}

pub enum Capture<'pr> {
    Node(ruby_prism::Node<'pr>),
    Name(&'pr [u8]),        // method/variable/symbol names
    Absent,                 // matched `nil?`
}
```

Capture slots are numbered at compile time in `$`-occurrence order, exactly as RuboCop yields block params, and named by the matcher's `captures:` list — so `$` in the pattern stays byte-identical to upstream while the IR reads legibly.

### 3.2 Per-node flow

1. `BatchedCopWalker::dispatch` indexes the tag table (`src/cop/walker.rs:96-119`) — IR cops not interested in this node type cost zero.
2. `IrCop::check_node`: for each hook whose `types` mask contains the node's (possibly virtual) type, and whose `restrict_send` prefilter passes on the callee name — a byte compare that kills ~90% of `send` traffic for typical cops, mirroring RuboCop's `RESTRICT_ON_SEND`.
3. `Matcher::eval(node, &mut env)` — recursive descent over the compiled pattern tree; on match, capture slots are filled.
4. `guard` evaluated; `binds` evaluated in order.
5. Location resolved, message rendered (`%{}` interpolation into a `String`), `Diagnostic` pushed via the trait's default `diagnostic()` (`src/cop/mod.rs:304-319`).
6. If `corrections` is `Some` and `autocorrect != none`, each edit is resolved to a byte range and pushed as a `Correction`; `diag.corrected = true`.

### 3.3 Ancestors / `^` / `parent`

Prism nodes carry no parent pointer. ruby-prism 1.9.0's `Visit` has a paired `visit_branch_node_leave(&mut self)` (`~/.cargo/.../ruby-prism-1.9.0/src/lib.rs:1233`), so `BatchedCopWalker` gains:

```rust
ancestors: Vec<ruby_prism::Node<'pr>>,   // push in branch_enter (after dispatch), pop in branch_leave
```

Leaf nodes never push (they have no `leave` hook and no children). Cost: one `Vec` push/pop per branch node, capacity-reserved once — sub-nanosecond, and it makes `parent`, `^`, `value_used?`, and `chained?` all trivially available. The stack is passed to `check_node` via a new `&[Node]` parameter — **a `Cop` trait signature change touching all 920 cops**. To avoid that churn, add a separate optional method instead:

```rust
fn check_node_with_ancestors(&self, …, ancestors: &[ruby_prism::Node<'_>], …) { }
```
defaulting to delegate to `check_node`; the walker calls it only for cops that declare `wants_ancestors() -> bool { false }`. Only `IrCop` overrides it. Zero diff to existing cops, and the ancestor stack is only maintained when at least one such cop is active.

### 3.4 Config binding

At load, each `config:` key becomes a `ConfigKeySpec { name, ty, default }`. `Expr::Cfg(id)` reads `CopConfig::get_str/get_bool/get_usize/get_string_array/get_string_hash` (`src/cop/mod.rs:68-150`) with the declared default. `enum` keys additionally validate the resolved value against `values:` and fall back to the default on mismatch (RuboCop errors; nitrocop's 1:1 behavior here should be checked against a real `.rubocop.yml` with a bogus `EnforcedStyle` before finalizing). Injected pseudo-keys (`TargetRubyVersion`, `__RailtiesInLockfile`, …) are readable through `target_ruby_at_least`-style predicates rather than raw `cfg.` access.

### 3.5 Expected overhead

A hand-written cop like `Style/ClassCheck` (`src/cop/style/class_check.rs:33-56`) is: one `as_call_node`, two byte compares, one string compare — call it 20-40ns per `CallNode`. The IR equivalent for `(call (const {nil? cbase} :Time) :new)` is ~6 `Matcher` enum dispatches, one child-vector materialization, and 2 byte compares — estimate **50-150ns**, i.e. 2-4× a hand-written cop, on nodes the cop is interested in. With `restrict_on_send` prefiltering and tag dispatch, the amortized cost across a file is dominated by the nodes that reach step 3.

Anchor for "does this matter": parse is ~1-3µs/KB and the corpus run touches 590k files. A few dozen IR cops at ~100ns/interesting-node is single-digit-percent of lint time at worst. Gate it with `NITROCOP_COP_PROFILE=1` (`src/linter.rs:300-340`) and `bench_nitrocop` before and after the first batch; if the matcher walk shows up, the fix is (in order) child-vector avoidance via an accessor-index table, then a bytecode VM, then codegen — not a redesign.

Two allocation hazards to kill in the MVP: `get_children` currently returns `Vec<MatchChild>` per node (`src/node_pattern/interpreter.rs:130-140`) — must become a `SmallVec<[_; 8]>` or an index-based accessor; and message rendering must only allocate on an actual offense.

### 3.6 `src/node_pattern` completion work

| Item | What it needs | Est. LOC |
|---|---|---|
| Real captures | Replace transparent `Capture` (`interpreter.rs:534`) with slot writes into `MatchEnv`; slot numbering in the parser; backtracking must unwind capture writes on alternation failure | ~200 |
| `#predicate` resolution | `HelperCall` → `MatcherId`/`PredId` resolution at compile time (currently `true`, `interpreter.rs:536`) | ~150 |
| Predicate registry | ~80 builtins, mostly wrapping existing shared modules (§2.2) | ~850 |
| `%param` | `ParamRef` binding from `constants`/`config` (currently `true`, `:537`) | ~120 |
| `^` parent | Ancestor-stack plumbing + `ParentRef` eval (currently `true`, `:538`) | ~250 |
| `` ` `` descend | Bounded descendant search + `DescendRef` eval (currently `true`, `:539`) | ~150 |
| `<>` unordered | New lexer token, `PatternNode::Unordered`, bitmask-assignment matcher with backtracking, ≤8 children cap | ~300 |
| Mapping expansion | 52 Parser types incl. virtual `numblock`/`itblock`/`any_block`, `kwbegin`, `block_pass`, `arg`, `case_match`, `in_pattern`, `str`/`dstr` unification | ~150 |
| Child-vector de-allocation | `SmallVec` + accessor-index refactor of `get_children` | ~120 |
| **Total** | | **~2,290** |

---

## 4. Loading and packaging

### 4.1 Discovery

1. **Embedded built-ins**: `src/resources/ir/<dept>/<snake>.cop.yml`, `include_str!`'d through a generated `src/cop/ir/embedded.rs` (a `&[(&str, &str)]` table, regenerated by `scripts/workflows/ir_embed.py` and checked in — same pattern as `tiers.json`).
2. **Project cops**: `.nitrocop/cops/**/*.cop.yml` relative to the config root. Matches the Lua doc's discovery story (`docs/LUA_CUSTOM_COPS.md:42-57`) so the two stay interchangeable if Lua ever lands.
3. **Gem-shipped packs**: for each `require:`d gem, look for `.nitrocop/cops/` under its install path, resolved by the existing `bundle info --path` machinery (`src/config/gem_path.rs`). Same as the Lua doc's §"Gem distribution" (`:59-62`).
4. **Explicit config key**: `AllCops: CustomCopPaths: [...]` for out-of-tree dirs. Lowest priority; mainly for monorepos.

Load order = embedded, then gem packs, then project. A later definition **may not** redefine an earlier cop name; collisions are a hard error naming both files.

### 4.2 `.rubocop.yml` integration

User cops appear as ordinary cops:

```yaml
Custom/NoBaseTransaction:
  Enabled: true
  Severity: error
  Exclude: ["spec/support/**/*"]
  ServicePatterns: [Service, Client]   # a declared `config:` key
```

`Enabled`/`Severity`/`Include`/`Exclude` flow through `CopConfig` unchanged. Departments: a user cop's department must not collide with a built-in department (`Style`, `Lint`, …) — enforced at load — so `Custom/` is the convention but any novel department works. This also means `--only Custom`, `--except`, and department-level config all work for free.

### 4.3 Tiering

- IR-translated built-ins get real `tiers.json` entries and default to `preview` (`src/resources/tiers.json:3`), so they are invisible to a default `cargo run -- .` until the corpus gate clears them — exactly the existing safety property.
- User cops are **exempt** from tier gating: `TierMap::tier_for` returns `Stable` for any cop whose department is not a built-in one. Tiers exist to express nitrocop-vs-RuboCop parity confidence, which is meaningless for a cop RuboCop has never heard of.

### 4.4 Error reporting

Fail closed. Any of: unknown `schema`, unknown key, undeclared config reference, unresolvable `#helper`, cyclic matcher graph, unknown node type in `on:`, unparseable NodePattern, unknown predicate → print `path/to/x.cop.yml:LINE:COL: <message>` (serde_yml surfaces `Location`) and exit 2 before linting anything. Rationale: a silently-dropped cop is an invisible false negative, which is the one failure mode a linter must never have. `--ignore-invalid-cops` downgrades to a warning for people mid-edit. Embedded built-ins are additionally validated by a `cargo test` that loads all of them, so a malformed shipped IR cannot escape CI.

### 4.5 Interpret vs. codegen — recommend **interpret only**

| | Interpret | Codegen to Rust at build time |
|---|---|---|
| Semantics | One engine; user cops and built-ins provably identical | Two engines; drift is inevitable and silent |
| G4 | Native | Irrelevant (user cops still need the interpreter) |
| Perf | ~2-4× hand-written per interesting node, ≈0 when not interested | ~1× |
| Diff/reviewability | ~40-line YAML per cop | Hundreds of lines of generated Rust per cop; unreviewable PRs |
| Upstreamability | Small, atomic | Huge mechanical diffs; generator and output must be reviewed together |
| Build | None | build.rs + generator + generated-file drift checks |

Codegen's only real win is the matcher-dispatch cost, which is small relative to parsing, and it is exactly the part a bytecode VM could recover later without touching the IR. The compiled `Matcher`/`Expr` enums *are* a codegen IR if `bench_nitrocop` ever proves it necessary — keep the door open, do not walk through it now. Startup cost of parsing ~100 embedded YAML docs (~2KB each) is ~5ms; if that shows up, serialize to `postcard` in `build.rs` and `include_bytes!` — a contained, later change.

---

## 5. Translation pipeline (G3)

All stages are Python under `scripts/` (snake_case, ruff-clean, pytest-covered per AGENTS.md). Artifacts land in `build/ir/<Dept>/<Name>/` (gitignored) except the final `.cop.yml` and fixtures.

### Stage 1 — extract (deterministic, no LLM) — `scripts/workflows/ir_extract.py`

Input: `vendor/<gem>/lib/rubocop/cop/<dept>/<snake>.rb` + `config/default.yml`.
Output: `extract.json`:

```json
{"cop":"Style/RedundantMinMaxBy","loc":51,"matchers":[{"name":"redundant_minmax_by_block",
 "pattern":"(block $(call _ {:max_by :min_by :minmax_by}) (args (arg $_x)) (lvar _x))",
 "n_captures":2}],
 "messages":{"MSG_BLOCK":"Use `%<replacement>s` instead of `%<original>s { |%<var>s| %<var>s }`."},
 "constants":{"REPLACEMENTS":{"max_by":"max","min_by":"min","minmax_by":"minmax"}},
 "restrict_on_send":[], "hooks":["on_block","on_numblock","on_itblock"],
 "mixins":["RangeHelp","AutoCorrector"], "config":{}, "version_added":"1.85",
 "enabled":"pending","safe":true,"safe_autocorrect":true,
 "body_after_matchers":"…ruby source with matcher bodies stripped…"}
```

The regex extraction is already proven twice in-tree: `src/node_pattern/extract.rs:7-18` and the census's `census.py`. Port the union of both.

### Stage 2 — classify — `scripts/workflows/ir_classify.py`

Mechanical, matching the census rules plus hard disqualifiers:
- **C (Rust)** if any of: `code_loc > 80`; mixins ∩ {`RangeHelp`(when used for multi-token stitching), `Alignment`, `SurroundingSpace`, `EndKeywordAlignment`, `Heredoc`, `PercentLiteral`, `ProjectIndexHelp`, `IndexedMethodArity`}; defines `on_new_investigation`; references `processed_source`/`tokens`/`comments`.
- **A** if `n_matchers >= 1` and `code_loc <= 25` and no disqualifier.
- **B** otherwise.
Output: `classification.json` with the reason, so the report is auditable. A/B go to stage 3; C gets a tracking issue, not an IR file.

### Stage 3 — synthesize — `scripts/workflows/ir_synth.py`

**Offline only. No LLM in the binary.** Invokes the model with:
- the cop's Ruby source,
- `extract.json`,
- the IR JSON Schema (`scripts/shared/ir_schema.json`),
- the predicate registry listing (dumped by `nitrocop --list-ir-predicates`),
constrained to emit only: `hooks[].when`, `hooks[].bind`, `hooks[].offense.{location,message,correct}`, and `predicates`. **`matchers`, `config`, `constants`, and metadata are copied verbatim from `extract.json` by the script**, never by the model. Output is validated against the schema and against a whitelist check (every predicate/attr it used exists); violations are re-prompted up to N times, then the cop is marked `needs_human`.

### Stage 4 — fixtures from RuboCop specs — `scripts/spec_to_fixture.py`

RuboCop specs are already in nitrocop's fixture format. From `spec/rubocop/cop/style/time_now_spec.rb` @ v1.91.0:

```ruby
expect_offense(<<~RUBY)
  Time.new
  ^^^^^^^^ Prefer `Time.now` over `Time.new` to retrieve the current time.
RUBY
expect_correction(<<~RUBY)
  Time.now
RUBY
```

Conversion rules:
- `expect_offense` heredoc → `offense.rb`, with each annotation line rewritten from `^^^ <message>` to `^^^ <Cop/Name>: <message>` — the exact format `scripts/generate_fixture.py:100-112` emits and `src/testutil.rs:133-190` parses.
- `expect_no_offenses` heredoc → appended to `no_offense.rb`.
- `expect_correction` heredoc → `corrected.rb`, paired with the preceding `expect_offense` body, driving `cop_autocorrect_fixture_tests!` (`src/cop/mod.rs:556-566`).
- `:config` blocks with `let(:cop_config) { {...} }` → `# nitrocop-config:` directive (`src/testutil.rs:485`) and `offense.<variant>.rb` naming for `cop_variant_fixture_tests!`.
- Specs using `%{...}` heredoc interpolation, `expect_offense` with `[...]` placeholders, or multiple `expect_offense` calls per example → flagged `manual`, not silently dropped.

Empirically these specs cover 20-60 cases per cop, which is a far denser oracle than hand-written fixtures.

### Stage 5 — verify — `scripts/ir_verify.py` (the gate)

1. `nitrocop --validate-ir <file>` — schema + resolution + DAG check.
2. `cargo test --lib -- cop::ir::generated::<snake>` — the generated fixture tests.
3. Differential: for every fixture body, run real RuboCop via `generate_fixture.py` and diff annotations byte-for-byte. Catches message-format and column-off-by-one errors the spec transcription might mask.
4. CI-only: `python3 scripts/check_cop.py <Dept/Name>` per-cop corpus gate (and `--style` variants). Cop stays `preview` until 0 FP / 0 FN.

Steps 1-3 run locally and in PR CI; step 4 runs in the corpus workflow. A cop is not merged as `stable` without step 4 clean.

### Artifacts summary

| Path | Committed | Produced by |
|---|---|---|
| `src/resources/ir/<dept>/<snake>.cop.yml` | yes | stages 1-3 |
| `tests/fixtures/cops/<dept>/<snake>/{offense,no_offense,corrected}.rb` | yes | stage 4 |
| `src/cop/ir/embedded.rs` | yes (generated) | `ir_embed.py` |
| `build/ir/<Dept>/<Name>/{extract,classification,synth,report}.json` | no | stages 1-3, 5 |
| `scripts/shared/ir_schema.json` | yes | hand-written, mirrors §1.2 |

---

## 6. Pilot and phasing (stacked atomic PRs)

Prerequisite handled elsewhere: submodule bump to rubocop 1.91.0 / rubocop-rspec 3.10.2.

### 6.1 Bucketing the 23 net-new cops

Measured from `git -C vendor/rubocop show v1.91.0:lib/rubocop/cop/<dept>/<snake>.rb` (LOC excludes matcher bodies and comments; `M` = matcher count):

| Cop | LOC | M | Bucket | Note |
|---|---:|---:|---|---|
| Style/TimeNow | 18 | 1 | **A** | pilot #1 |
| Style/FileOpen | 25 | 1 | **A** | needs `value_used?`, `parent` |
| Lint/DataDefineOverride | 31 | 1 | **A** | |
| Style/PredicateWithKind | 37 | 2 | **A** | `%KIND_METHODS` param; block/numblock/itblock |
| Style/MapJoin | 46 | 4 | **B** | correction needs line-aware dot/receiver range |
| RSpec/MatchWithSimpleRegex | 50 | 1 | **A** | rubocop-rspec v3.10.2 |
| Style/RedundantMinMaxBy | 51 | 3 | **A** | pilot #2 |
| Style/TallyMethod | 57 | 4 | **B** | |
| Style/RedundantStructKeywordInit | 64 | 4 | **B** | `TargetRubyVersion` gate |
| RSpec/DiscardedMatcher | 66 | 0 | **B** | needs `InsideExample` mixin semantics |
| Style/SelectByKind | 71 | 5 | **B** | |
| Style/SelectByRange | 85 | 5 | **B** | |
| Lint/UnreachablePatternBranch | 39 | 0 | **B** | `on_case_match`, pattern-node mapping gap |
| Style/OneClassPerFile | 40 | 0 | **B** | `on_new_investigation` → **C in practice** |
| Style/ReduceToHash | 105 | 2 | **C** | |
| Style/PartitionInsteadOfDoubleSelect | 171 | 1 | **C** | |
| Style/DirectiveScope | 180 | 0 | **C** | comment directives |
| Lint/MisplacedMagicComment | 97 | 0 | **C** | tokens + `on_new_investigation` |
| Lint/ArgumentMismatch | 41 | 0 | **C-blocked** | `ProjectIndexHelp` |
| Lint/DeprecatedReference | 104 | 0 | **C-blocked** | `ProjectIndexHelp` |
| Lint/NameTypo | 146 | 0 | **C-blocked** | `ProjectIndexHelp` |
| Lint/SuperArgumentMismatch | 82 | 0 | **C-blocked** | `ProjectIndexHelp` |
| Lint/UnusedPrivateMethod | 113 | 0 | **C-blocked** | `ProjectIndexHelp` |

**The 5 `ProjectIndexHelp` cops require a whole-project symbol index nitrocop has no equivalent of.** That is a separate infrastructure program, not IR work, and should be surfaced to the owner as its own decision.

**Pilot set (9):** TimeNow, FileOpen, DataDefineOverride, PredicateWithKind, RedundantMinMaxBy, MatchWithSimpleRegex, TallyMethod, RedundantStructKeywordInit, SelectByKind.

### 6.2 PR sequence

| # | PR | Scope | Files | Tests | Size | Deps |
|---|---|---|---|---|---:|---|
| 1 | np: real captures | Capture slots, backtracking unwind, `MatchEnv` skeleton | `src/node_pattern/{parser,interpreter}.rs` | unit tests over `pattern_db.rs` | ~350 | — |
| 2 | np: `<>` unordered + mapping expansion | Lexer token, `Unordered` node, bitmask matcher; virtual `numblock`/`itblock`/`any_block`, `kwbegin`, `block_pass`, `case_match` | `src/node_pattern/{lexer,parser,interpreter,mapping}.rs` | unit + 64 real `<>` patterns | ~550 | 1 |
| 3 | predicate registry (Rust) | ~80 builtins wrapping `shared/*`; `--list-ir-predicates` | `src/cop/ir/predicates.rs`, `src/cli.rs` | per-predicate unit tests | ~900 | — |
| 4 | np: `#helper` + `%param` | Resolution against registry/matchers; param binding | `src/node_pattern/interpreter.rs` | unit | ~300 | 1,3 |
| 5 | walker ancestors + `^`/`` ` `` | `wants_ancestors()`, `check_node_with_ancestors`, stack push/pop | `src/cop/{mod,walker}.rs`, `src/node_pattern/interpreter.rs` | walker unit tests; zero diff to existing cops | ~300 | 1 |
| 6 | IR schema + loader | Types, serde structs, JSON Schema, `--validate-ir`, error reporting | `src/cop/ir/{mod,schema,load}.rs`, `scripts/shared/ir_schema.json` | load/error-message tests | ~700 | — |
| 7 | `Expr` compiler + evaluator | §2.1, DAG validation, depth cap | `src/cop/ir/{expr,eval}.rs` | table-driven unit tests | ~800 | 3,6 |
| 8 | `IrCop` + registry integration | `Cop` impl, offense/location/message/correct, `Box::leak` names, embedded loader | `src/cop/ir/cop.rs`, `src/cop/registry.rs`, `src/cop/ir/embedded.rs` | one throwaway IR cop under `#[cfg(test)]` | ~600 | 5,6,7 |
| 9 | first translated cop: Style/TimeNow | 1 YAML + fixtures + tiers entry | `src/resources/ir/style/time_now.cop.yml`, fixtures, `tiers.json` | fixture + autocorrect fixture | ~150 | 8 |
| 10 | pilot batch (8 more cops) | one commit per cop, one PR | as above ×8 | fixtures ×8 | ~900 | 9 |
| 11 | user-cop discovery | `.nitrocop/cops/`, gem packs, department validation, tier exemption, `--list-cops` integration | `src/cop/ir/discover.rs`, `src/config/mod.rs`, `docs/CUSTOM_COPS.md` | integration tests w/ tempdir | ~500 | 8 |
| 12 | `ir_extract.py` + `ir_classify.py` | Stages 1-2 | `scripts/workflows/`, `tests/python/` | pytest against all 23 cops | ~450 | — |
| 13 | `spec_to_fixture.py` | Stage 4 | `scripts/` | pytest on 5 known specs | ~400 | 12 |
| 14 | `ir_synth.py` + `ir_verify.py` | Stages 3, 5; CI wiring | `scripts/workflows/`, `.github/workflows/` | pytest w/ recorded model outputs | ~450 | 12,13 |

PRs 1-8 are corpus-neutral by construction (no registered cop changes behavior). PR 9 is the first that can move the oracle, and it enters at `preview`, so the default-config corpus number is unchanged until `check_cop.py` clears it. PRs 12-14 can land in parallel with 1-8 — they produce artifacts, not binary behavior.

---

## 7. Risks and open questions

1. **`<>` backtracking blowup.** Unordered matching over n children is worst-case n!. *Resolution:* cap at 8 children with greedy assignment + backtracking; reject patterns exceeding it at load. The 64 real `<>` patterns are all ≤4 elements — verify in PR 2 and hard-fail otherwise.
2. **Message/format fidelity.** `%<x>s` vs `%{x}`, `%d`, `%.2f`, and pluralization. *Resolution:* mechanical converter with a whitelist of format specs; anything else is a `needs_human` flag. Stage-5 differential against real RuboCop is the backstop.
3. **`&'static str` names.** `Cop::name()` and `Correction.cop_name` (`src/correction.rs:10`). *Resolution:* `Box::leak` at load; bounded by cop count, freed at exit. Revisit only if someone loads thousands of cops.
4. **Expression-layer creep.** Every hard cop tempts a new `Expr` variant until the IR is a bad programming language. *Resolution:* the schema's depth cap and the "no recursion, no user functions" rule are non-negotiable; a cop that needs more is bucket C. Re-evaluate after 30 translated cops, not per-cop.
5. **Prism/Parser shape divergence.** `numblock`/`itblock`, block bodies wrapped as `BeginNode` when `rescue`/`ensure` present (AGENTS.md), `csend` vs `send`, `const` covering both `ConstantReadNode` and `ConstantPathNode` (`src/node_pattern/interpreter.rs:80-81`). *Resolution:* the mapping table is the single point of truth; PR 2 adds a `cargo test` that runs all ~950 vendored patterns against a fixture corpus and asserts no `None` type mappings.
6. **Per-cop corpus gate throughput.** 9 pilot cops × `check_cop.py --rerun` is CI-expensive. *Resolution:* batch them into one corpus run per PR-10 commit series; keep everything `preview` until the batch clears.
7. **LLM hallucinating predicates or locations.** *Resolution:* whitelist validation on every identifier the model emits, plus the fixture/differential gates. A cop that cannot pass mechanically is never merged, regardless of how plausible the YAML looks.
8. **User-cop name collisions and shadowing.** *Resolution:* hard error on redefining a known cop name or using a built-in department. Prevents a custom cop from silently replacing a parity-tested one.
9. **Autocorrect ordering semantics.** RuboCop applies corrections through a `Corrector` with clobbering rules subtly different from `CorrectionSet`'s drop-on-overlap (`src/correction.rs:38-43`). *Resolution:* out of scope for the IR — it inherits whatever nitrocop already does, which the corpus has validated across 900+ cops. Flag any IR cop whose `correct:` produces overlapping edits as a load error.
10. **`on_new_investigation` demand.** 76 upstream cops use it; 2 of the 23 pilot candidates do. *Resolution:* keep it out of v1. If demand persists after 30 cops, add a declarative `collect:`/`report_at_end:` pair with a fixed accumulator vocabulary (count, list-of-nodes, first/last) rather than mutable state.
11. **Does `preview` tier interact correctly with user cops?** Currently `default_tier: preview` would silently disable every user cop unless `--preview` is passed. *Resolution:* the department-based exemption in §4.3 — but confirm against `src/cop/tiers.rs:63-70` and the skip-summary reporting before shipping PR 11.
12. **Lua.** *Resolution:* stay deferred. Revisit only when a real user hits the expression layer's ceiling; the `.nitrocop/cops/` discovery path and per-cop config binding built here are exactly what a Lua backend would reuse, so nothing is wasted.
