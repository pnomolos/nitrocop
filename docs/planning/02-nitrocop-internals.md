# nitrocop internals census (Part 1)

Survey only — no design opinions. All references are `file:line` against the
working tree at the time of writing.

## 1. `src/node_pattern/*.rs` (2,964 lines total)

**What it is:** a standalone, experimental NodePattern *interpreter* plus a
one-shot Rust *codegen* prototype. It is **not** wired into any shipping cop.
It exists to support a verifier/codegen research binary
(`src/bin/node_pattern_codegen.rs`) that is explicitly marked experimental.

- `src/node_pattern/mod.rs:1-19` — module root, re-exports lexer/parser/mapping/interpreter/extract/pattern_db. No cop-facing API.
- `src/node_pattern/lexer.rs` (387 lines) — tokenizes NodePattern DSL text. Token set (`src/node_pattern/lexer.rs:6-30`): `LParen/RParen`, `LBrace/RBrace` (`{}` union), `LBracket/RBracket` (`[]` intersection), `Capture` (`$`), `Wildcard` (`_`), `Rest` (`...`), `Negation` (`!`), `Pipe`, `NilPredicate`/`TruePredicate`/`FalsePredicate`, `Caret` (`^`, parent ref), `Backtick` (`` ` ``, descend). **No token for `<>` (unordered match)** — grep confirms no `Angle`/`Unordered` anywhere in `src/node_pattern/*.rs`.
- `src/node_pattern/parser.rs` (515 lines) — recursive-descent parser producing `PatternNode` (`src/node_pattern/parser.rs:7-51`): `NodeMatch`, `Alternatives` (`{}`), `Conjunction` (`[]`), `Capture` (`$`), `Wildcard`, `Rest` (`...`), `Negation` (`!`), `HelperCall` (`#method`), `SymbolLiteral`/`IntLiteral`/`FloatLiteral`/`StringLiteral`, `NilPredicate`, `True/False/NilLiteral`, `ParamRef` (`%name`/`%1`), `TypePredicate` (`str?`, `send?`, …), `ParentRef` (`^`), `DescendRef` (`` ` ``), `Ident`. So the *grammar* covers most of RuboCop's NodePattern surface except unordered `<>` sequences, which parser.rs has no representation for at all.
- `src/node_pattern/interpreter.rs` (962 lines) — `interpret_pattern(pattern_str, node)` (`src/node_pattern/interpreter.rs:38-45`) lexes+parses+matches a pattern string against a live `ruby_prism::Node`. Its own doc comment (`src/node_pattern/interpreter.rs:6-16`) states what's supported ("Phase 1": NodeMatch, Wildcard, Rest, NilPredicate, literals, Alternatives, Conjunction, Negation, transparent Capture, TypePredicate, Ident) vs. **deferred**: `HelperCall` (`#method`) and `ParamRef` (`%1`) always evaluate `true` (optimistic no-op), and `ParentRef` (`^`) / `DescendRef` (`` ` ``) also always evaluate `true`. So even where the parser accepts a construct, the interpreter treats 4 of the ~14 constructs as always-matching stubs rather than actually implementing them.
- `src/node_pattern/mapping.rs` (415 lines) — a hand-maintained table (`build_mapping_table()`, `src/node_pattern/mapping.rs:26-372`) mapping ~41 Parser-gem node-type names (`send`, `csend`, `block`, `def`, `defs`, `const`, `begin`, `pair`, `hash`, `lvar`, `ivar`, `cvar`, `gvar`, `sym`, `str`, `int`, `float`, `true/false/nil`, `self`, `array`, `if`, `case`, `when`, `while`, `until`, `for`, `return`, `yield`, `and`, `or`, `regexp`, `class`, `module`, `lvasgn`, `ivasgn`, `casgn`, `splat`, `super`, `zsuper`, `lambda`, `dstr`, `dsym`, `args`, `any_block`, `cbase`, `op-asgn`) to `(prism_type, cast_method, child_accessors)` triples. This is the Parser-gem → Prism Rosetta stone; `docs/node_pattern_analysis.md` calls out only 52 such node types are needed across the whole corpus, so this table covers the large majority but not all.
- `src/node_pattern/pattern_db.rs` (249 lines) — a **curated, hand-copied** list of ~50 `(cop_name, pattern_string)` pairs pulled from real vendor cops (`src/node_pattern/pattern_db.rs:24-249`), used only as fixture data for the interpreter's own test/verify suite. Not generated at build time, not read by any cop.
- `src/node_pattern/extract.rs` (417 lines) — regex/text-based extraction of `def_node_matcher`/`def_node_search` calls straight out of Ruby source (`ExtractedPattern`, `PatternKind::Matcher|Search`, `src/node_pattern/extract.rs:7-18`), plus `walk_vendor_patterns` to recursively scan a vendor tree. This is the same kind of extraction I re-implemented independently for Part 2 of this census (see `census.py`), which validates the approach.

**Is it used by cops at runtime?** No. `grep -rn "node_pattern" src/cop/` (excluding the module itself) returns nothing — no cop in `src/cop/**` imports or calls into `src/node_pattern`. All consumption is from `src/bin/node_pattern_codegen.rs` and the module's own `#[cfg(test)]` blocks.

**Bin target:** `src/bin/node_pattern_codegen.rs` (its own header, lines 1-27, is explicit): *"Status: NOT used in CI or the standard cop-writing workflow. Kept in-tree as a reference implementation."* It documents two subcommands (`generate`, `verify`) and states plainly what does **not** work: alternatives codegen (`{a|b}`), captures (`$name`), literal value matching, `nil?`/`cbase` handling, and that verify mode is a stub. It recommends `docs/node_pattern_analysis.md`'s mapping table over the codegen output for actual cop-writing.

**Bottom line:** `src/node_pattern` is best described as a *research spike* that proves out (a) a lexer/parser for most of the NodePattern grammar and (b) a Parser→Prism node-type mapping, but its interpreter only fully implements structural matching (types, literals, wildcards, rest, unions, intersections, negation, capture-as-transparent) and stubs out predicates/params/parent/descend. Unordered `<>` sequences aren't represented anywhere in the grammar. None of it executes at lint time today.

## 2. Cop trait, walker, config, diagnostics, corrections, dispatch, registry

### The `Cop` trait — `src/cop/mod.rs:273-380`

```rust
pub trait Cop: Send + Sync {
    fn name(&self) -> &'static str;                                   // mod.rs:275
    fn default_severity(&self) -> Severity { Severity::Convention }    // mod.rs:277-279
    fn default_include(&self) -> &'static [&'static str] { &[] }       // mod.rs:283-285
    fn default_exclude(&self) -> &'static [&'static str] { &[] }       // mod.rs:289-291
    fn default_enabled(&self) -> bool { true }                         // mod.rs:299-301
    fn diagnostic(&self, source, line, column, message) -> Diagnostic  // mod.rs:304-319 (default impl)
    fn supports_autocorrect(&self) -> bool { false }                   // mod.rs:322-324
    fn safe_autocorrect(&self) -> bool { true }                        // mod.rs:327-329
    fn check_lines(&self, source, config, diagnostics, corrections);   // mod.rs:333-339 (no-op default)
    fn check_source(&self, source, parse_result, code_map, config,
                     diagnostics, corrections);                       // mod.rs:342-355 (no-op default)
    fn interested_node_types(&self) -> &'static [u8] { &[] }           // mod.rs:359-362
    fn check_node(&self, source, node, parse_result, config,
                  diagnostics, corrections);                          // mod.rs:365-373 (no-op default)
    fn as_variable_force_consumer(&self) -> Option<&dyn VariableForceConsumer> { None } // mod.rs:378-380
}
```

Three independent entry points a cop can implement (all optional, no-op by default): `check_lines` (raw line scan, pre-AST), `check_source` (once per file, full `ParseResult` + `CodeMap`, for source/token/comment-driven cops), `check_node` (called per AST node during a single shared traversal). `interested_node_types()` lets a cop opt into selective dispatch by returning a slice of `u8` node-type tags instead of `&[]` (= called for every node).

### Visitor/walker — `src/cop/walker.rs` (135 lines)

Two walker types:
- `CopWalker` (`src/cop/walker.rs:8-38`) — implements `ruby_prism::Visit`, calls a single cop's `check_node` on every branch/leaf node. Simple, single-cop.
- `BatchedCopWalker` (`src/cop/walker.rs:42-90+`) — the production path. Builds a `dispatch_table: [Vec<(&dyn Cop, &CopConfig)>; NODE_TYPE_COUNT]` indexed by node-type tag (`src/cop/walker.rs:46-73`); cops with empty `interested_node_types()` go into a separate `universal_cops` vec called on every node, everything else is bucketed once at walker-construction time so each AST node only invokes the cops actually registered for that node type — O(1) dispatch per node instead of O(cops) per node.

### Config — `src/config/` (`gem_path.rs`, `lockfile.rs`, `mod.rs`)

`CopConfig` (`src/cop/mod.rs:49-56`) is the per-cop, post-resolution config struct: `enabled: EnabledState` (4-way tri-state: `True/False/Pending/Unset`, mirroring RuboCop's `Enabled: pending` semantics — `src/cop/mod.rs:29-45`), `severity`, `exclude`/`include` glob overrides, and `options: HashMap<String, serde_yml::Value>` — the raw per-cop YAML keys (`EnforcedStyle`, `Max`, `AllowedMethods`, etc.). Accessors `get_str/get_usize/get_bool/get_string_array/get_string_hash/get_flat_string_values` (`src/cop/mod.rs:68-150ish`) wrap `options` with typed defaults. `.rubocop.yml` parsing/merging/inheritance lives in `src/config/mod.rs` (not read in full for this census; `CopConfig` is the boundary cops actually see).

### Diagnostics — `src/diagnostic.rs` (258 lines)

`Severity` enum (`Convention/Warning/Error/Fatal`, `src/diagnostic.rs:5-10`) with letter codes and `from_str`. `Location { line, column }` (1-indexed line, 0-indexed byte column, `src/diagnostic.rs:41-46`). `Diagnostic { path, location, severity, cop_name, message, corrected }` (`src/diagnostic.rs:48-56`) is the flat, serializable offense record — no tree structure, no correction payload embedded (that's separate, see below).

### Corrections — `src/correction.rs` (231 lines)

`Correction { start, end, replacement, cop_name, cop_index }` (`src/correction.rs:1-13`) — a plain byte-range replace/delete. `CorrectionSet::from_vec` (`src/correction.rs:26-46`) sorts by `(start, cop_index)` and drops any correction whose range overlaps a previously-accepted one (first-registered-cop wins on overlap, matching RuboCop's merge semantics), then applies all accepted corrections in one linear O(n) scan (`src/correction.rs:48-60+`) rather than doing repeated string splicing. Cops build `Correction` values inline inside `check_node`/`check_source` (see `NegatedIf` example below) — there's no separate "corrector" object/DSL; it's direct byte-offset math against `ruby_prism` location APIs.

### Node-type dispatch table — `src/cop/shared/node_type.rs` (480 lines)

`NODE_TYPE_COUNT = 151` (`src/cop/shared/node_type.rs:8`), one `pub const XXX_NODE: u8 = N` per `ruby_prism::Node` variant (`src/cop/shared/node_type.rs:10-59+`), generated by hand/script from the 151-variant Prism enum. This is purely the tag space `interested_node_types()` and `BatchedCopWalker`'s dispatch table are built from — no logic, just an enum-to-u8 mirror for O(1) array indexing.

### Registry — `src/cop/registry.rs` (144 lines)

`CopRegistry { cops: Vec<Box<dyn Cop>>, index: HashMap<&'static str, usize> }` (`src/cop/registry.rs:5-8`). `default_registry()` (`src/cop/registry.rs:20-38`) is a flat, hand-written list of `<department>::register_all(&mut registry)` calls (bundler, factory_bot, gemspec, layout, migration, lint, metrics, naming, performance, rails, rake, rspec, rspec_rails, security, style) — i.e. every cop is a statically-registered `Box<dyn Cop>` compiled into the binary. `get(name)`/`names()`/`len()` are simple index lookups. There is no dynamic/plugin registration path here (that's the aspirational Lua doc, see below) — everything in this registry is a `.rs` file compiled into `nitrocop`.

## 3. `docs/LUA_CUSTOM_COPS.md` and `docs/node_pattern_analysis.md`

### `docs/LUA_CUSTOM_COPS.md` (538 lines) — summary

1. Proposes embedding a Lua runtime (~200KB, "LuaJIT-class" performance) directly in the nitrocop binary.
2. Pitch: organizations write custom cops in Lua instead of forking nitrocop or writing Rust.
3. Auto-discovery: every `.lua` file under `.nitrocop/cops/` in the project root is loaded, no config needed.
4. Gem distribution story: ship a `.nitrocop/cops/` dir inside a gem; nitrocop finds it via `bundle info --path`.
5. Cop definition = a Lua table with `name`, `severity`, `node_types`, `needs_parent`, and one or more of `check_lines`/`check_source`/`check_node` callbacks.
6. Callbacks return `nil`, a single `{line, column, message}` table, or an array of them.
7. Documents a `Node` API wrapping Prism nodes: `type()/line()/column()/source_text()/parent()/children()`, plus per-node-kind accessors (`name()`, `receiver()`, `arguments()`, `const_name()`, etc.) and `descendants()`/`find(type)` iterators.
8. Documents `Source` (path/text/lines/is_code/root) and `Config` (`get_str/get_bool/get_number/get_list`) wrapper APIs.
9. Gives three worked examples at increasing complexity (ban a method call, check a send chain, enforce a code structure via body traversal).
10. Reads as a complete, implementation-ready spec — not implemented anywhere in `src/`.

**Confirmed not implemented:** `grep -in "lua\|mlua\|rhai" Cargo.toml` → no hits. `grep -rn "\.nitrocop\b\|nitrocop/cops\|CustomCop\|custom_cop" src/` → only unrelated hits (`.nitrocop-cache`, `.nitrocop.cache` in `cache.rs`/`lockfile.rs` tests, and a `custom_cop.rb`/`.custom_cops.yml` comment/test fixture in `src/config/mod.rs:1445,5583-5587` about *RuboCop's own* `require:`/`extend_config:` YAML keys — unrelated to a Lua runtime). There is no `.lua` loader, no `.nitrocop/cops/` directory handling, no Lua VM anywhere in the codebase.

### `docs/node_pattern_analysis.md` (231 lines) — summary

1. States its purpose: evaluate feasibility of automatic NodePattern codegen for nitrocop.
2. Reports a bug-pattern analysis of 22 fixes across 11 nitrocop commits: 36% AST-shape misunderstandings, 36% logic errors, 27% config-handling errors, 9% wrong node-type-check casts.
3. Claims AST-shape + node-type-check bugs (45% combined) are "exactly the class of errors a code generator eliminates by construction."
4. Gives corpus-wide NodePattern stats: **1,010** total `def_node_matcher`/`def_node_search` patterns across rubocop + rubocop-rails + rubocop-rspec + rubocop-performance.
5. Of those, **830 (82%)** allegedly use only node types/literals/wildcards/alternatives — "fully auto-generatable."
6. **180 (18%)** use `#helper_method` calls needing manual implementation; only 47 unique helper methods total.
7. Only 52 unique Parser-gem node types appear across all those patterns.
8. Frames the 82/18 split as favorable for a mechanical translator.
9. Provides a full Parser-gem → Prism mapping table (the same content later encoded as data in `src/node_pattern/mapping.rs`).
10. Reads as the design rationale that presumably motivated building `src/node_pattern/`, but stops at analysis — no codegen tool in this doc is claimed to be complete, and (per `src/bin/node_pattern_codegen.rs`'s own header) the actual codegen binary explicitly can't yet handle alternatives, captures, or literal matching, i.e. it undershoots this doc's own optimistic framing.

Nothing beyond `src/node_pattern/` implements anything from either doc; both are purely aspirational/analytical relative to the current codebase.

## 4. Three example cops, by complexity

### Tiny: `Style/HashExcept` — `src/cop/style/hash_except.rs` (45 lines)
- Whole cop is a thin `Cop` impl: `name()`, `interested_node_types() -> &[CALL_NODE]`, and a 10-line `check_node` (`src/cop/style/hash_except.rs:28-38`) that just delegates to a **shared** module: `hash_subset::check_hash_subset(self, HashSubsetMode::Except, source, node, diagnostics)`.
- All actual matching/message logic lives in `src/cop/style/hash_subset.rs` (a per-department shared module per AGENTS.md), shared with `Style/HashSlice` (`HashSubsetMode::Slice`) — real cop logic is one function reused across two cop structs.
- No config options read directly in this file (mode is a compile-time enum, not YAML-driven).
- No autocorrect (`supports_autocorrect` not overridden → default `false`).
- Struct itself carries a `///` doc comment recording a 2026-03 corpus-fix history (added `eql?`/`in?` shapes, implicit-receiver support, scoped exclusion for safe-nav predicate calls) — this is the "document quirks/fixes on the cop struct" convention from AGENTS.md in practice.

### EnforcedStyle: `Style/ClassCheck` — `src/cop/style/class_check.rs` (92 lines)
- Single `interested_node_types() -> &[CALL_NODE]`; reads `config.get_str("EnforcedStyle", "is_a?")` (`src/cop/style/class_check.rs:31`) directly — no shared "ConfigurableEnforcedStyle" abstraction in Rust, it's just a string compare.
- Logic: match method name against `is_a?`/`kind_of?`, pick `(prefer, current)` pair based on style, flag only the non-preferred call (`src/cop/style/class_check.rs:38-56`), report at `message_loc()` not the whole call.
- No autocorrect.
- Doc comment on the struct explains a corpus false-negative history (receiverless `kind_of?(Foo)` calls) rather than a design rationale — again a fix-history note, not upfront design.
- Tests include both the standard fixture macro and a hand-written config-driven unit test (`src/cop/style/class_check.rs:77-90`) constructing a `CopConfig` with `EnforcedStyle: kind_of?` inline — shows the pattern for testing variant behavior without fixture-directive plumbing.

### Autocorrect: `Style/NegatedIf` — `src/cop/style/negated_if.rs` (374 lines, ~171 non-test)
- `interested_node_types() -> &[CALL_NODE, IF_NODE]`; `supports_autocorrect() -> true`.
- `check_node` (`src/cop/style/negated_if.rs:45-170`) is dense procedural logic: unwraps parens via `util::unwrap_parentheses`, detects modifier vs. prefix form via `end_keyword_loc().is_none()`, walks raw source bytes backward to detect `in :pattern if !cond` guard clauses (pattern-match guards can't become `unless`, so must be excluded — done by scanning bytes before the node start rather than any AST check), filters by `EnforcedStyle` (`both`/`prefix`/`postfix`), then on match builds **two** `Correction`s inline — replace the `if` keyword span with `unless`, and replace the predicate span with its de-negated inner expression (`util::get_negation_inner`), including manual space-insertion logic when the keyword and predicate were adjacent.
- Struct doc comment (`src/cop/style/negated_if.rs:7-29`) is the fullest example of the AGENTS.md "document RuboCop quirks + prior FP/FN root causes" convention: five bullet points of specific historical bugs (double-negation, parenthesized conditions, modifier-form location, pattern-guard FPs, safe-nav-chain FPs), each traceable to a specific corpus finding.
- 15 unit tests plus the two fixture-macro tests, covering every branch above individually (parenthesized negation, double negation, `not` keyword, pattern guards, safe-nav chains, each `EnforcedStyle` value in both directions, multiline modifier-form location).

This progression (10 lines of real logic delegating to a shared module → ~25 lines of direct string/config comparison → ~130 lines of byte-level source inspection plus two-part inline correction construction) is representative of the A/B/C bucket boundaries used in Part 2 of this census.
