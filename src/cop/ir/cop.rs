//! The [`Cop`] implementation for a loaded IR document (design §3).
//!
//! [`IrCop`] is the *document*: the serde surface plus compiled matchers and
//! expressions. [`IrCopRunner`] is the *runtime*: everything that can be
//! decided once is decided once — the leaked `&'static str` name, the Prism tag
//! set the walker dispatches on, whether any matcher or expression reads the
//! ancestor chain, the `location:`/`correct:` anchor paths parsed into steps,
//! and each matcher's capture slots remapped onto the hook's merged numbering.
//!
//! Everything here is immutable after construction and holds no interior
//! mutability, so `Send + Sync` derives structurally and the `Mutex` hazard
//! AGENTS.md warns about cannot arise. Per-node state lives on the stack.
//!
//! # Per-node flow (§3.2)
//!
//! 1. the walker's tag table has already decided the node *might* interest us;
//! 2. each hook re-discriminates the node's Parser-gem type — the tag table
//!    cannot, because one Prism type covers several Parser types (`send`
//!    vs `csend`, `block` vs `numblock` vs `itblock`);
//! 3. `restrict_on_send` prefilters the callee name for `send`/`csend` hooks;
//! 4. the matcher runs, binding capture slots;
//! 5. `bind:` entries evaluate in declaration order, then `when:`;
//! 6. the `location:` anchor resolves to a byte range, the message template
//!    renders, and the `Diagnostic` is pushed;
//! 7. `correct:` edits resolve to byte ranges and are pushed as `Correction`s.
//!
//! Steps 1-4 allocate nothing. The config vector, the bind vector and the
//! rendered message are built only after a matcher has actually matched, which
//! is the "allocate only on offense" rule of §3.5.
//!
//! # Fail-closed at runtime
//!
//! An anchor that cannot resolve against the node it matched (`node.selector`
//! on something that is not a call, say) is a bug in the YAML that the loader's
//! vocabulary check cannot catch, because it is type-dependent. It panics under
//! `debug_assertions` — so `cargo test` catches it — and drops the offense in a
//! release build rather than reporting at a wrong location.

use std::sync::OnceLock;

use ruby_prism::Location as PrismLoc;

use crate::cop::shared::node_type as tag;
use crate::cop::{Cop, CopConfig};
use crate::correction::Correction;
use crate::diagnostic::{Diagnostic, Severity};
use crate::node_pattern::captures::{CaptureValue, Captures, dup_node};
use crate::node_pattern::interpreter::{CompiledPattern, block_type_of, parser_type_for_node};
use crate::node_pattern::parser::PatternNode;
use crate::node_pattern::predicates::Arg;
use crate::node_pattern::resolve::{Params, Resolver};
use crate::parse::source::SourceFile;

use super::eval::{EvalCtx, Value, eval};
use super::expr::{Attr, Collection, CompiledDoc, Expr, Intrinsic, Target};
use super::load::{IrCop, IrError, IrErrorKind};
use super::schema::{
    AutocorrectMode, ConfigType, CorrectionOp, EnabledDefault, LocationSpec, MatchSpec,
    Severity as IrSeverity,
};

/// Try each `as_*_node().<accessor>()` in turn and take the first hit.
///
/// `opt` marks an accessor that already returns an `Option`; `req` one that
/// returns the value directly. Both spellings occur throughout `ruby_prism`.
macro_rules! pick {
    ($node:expr; $($kind:ident $cast:ident . $acc:ident),* $(,)?) => {{
        let n = $node;
        None $( .or_else(|| pick!(@get $kind n, $cast, $acc)) )*
    }};
    (@get opt $n:expr, $cast:ident, $acc:ident) => { $n.$cast().and_then(|t| t.$acc()) };
    (@get req $n:expr, $cast:ident, $acc:ident) => { $n.$cast().map(|t| t.$acc()) };
}

/// A loaded IR document, ready to run as a cop.
pub struct IrCopRunner {
    doc: IrCop,
    /// `Box::leak`'d cop name (design §7 risk 3): bounded by the cop count, and
    /// both `Cop::name` and `Correction::cop_name` want `&'static str`.
    name: &'static str,
    node_tags: &'static [u8],
    wants_ancestors: bool,
    default_enabled: bool,
    severity: Severity,
    /// Doc-level `restrict_on_send`, applied to `send`/`csend` hooks only.
    restrict: Vec<Box<[u8]>>,
    hooks: Vec<HookPlan>,
    /// `%param` bindings resolvable without a `CopConfig` (constant tables).
    const_params: Params,
    /// `%param`s backed by a `config:` key, as `(key, config slot)`; these have
    /// to be rebuilt per file.
    config_params: Vec<(String, usize)>,
}

/// Everything one hook decides at load time.
struct HookPlan {
    /// Declared `on:` types, kept as written so the node can be
    /// re-discriminated at hook entry.
    on: Vec<String>,
    plan: MatchPlan,
    /// Merged capture names in slot order. The expression compiler resolved
    /// `$name` against exactly this list, so the slot numbering agrees.
    capture_names: Vec<String>,
    bind_names: Vec<String>,
    location: AnchorRange,
    corrections: Vec<CorrectionPlan>,
    severity: Severity,
}

/// `match:` resolved to matcher indices, with each matcher's own capture slots
/// remapped onto the hook's merged numbering.
enum MatchPlan {
    One { id: usize, remap: Vec<usize> },
    AnyOf(Vec<MatchPlan>),
    AllOf(Vec<MatchPlan>),
}

/// A `correct:` edit with its anchors pre-parsed.
enum CorrectionPlan {
    Replace { range: AnchorRange, text: String },
    Insert { at: Anchor, text: String },
    Remove { range: AnchorRange },
}

/// Where an anchor path starts.
#[derive(Clone, Copy)]
enum AnchorTarget {
    Node,
    Parent,
    Capture(usize),
}

/// A parsed `<target>[.<accessor>]*[.<part>][.<edge>]` path.
struct Anchor {
    target: AnchorTarget,
    accessors: Vec<String>,
    part: Option<String>,
    /// `true` for `.start`, `false` for `.stop`; `None` on a range shorthand.
    edge: Option<bool>,
}

/// A source range: a shorthand anchor denoting a whole range, or two points.
enum AnchorRange {
    Whole(Anchor),
    Pair(Anchor, Anchor),
}

/// Resolves `#helper` inside a pattern against the document's own matchers.
struct DocResolver<'a>(&'a CompiledDoc);

impl Resolver for DocResolver<'_> {
    fn matcher(&self, name: &str) -> Option<&CompiledPattern> {
        let index = self.0.matcher_names.iter().position(|n| n == name)?;
        self.0.matchers.get(index)
    }
}

impl IrCopRunner {
    /// Turn a loaded document into a runnable cop.
    ///
    /// # Errors
    ///
    /// [`IrErrorKind::NodeType`] when a hook's `on:` list maps to no Prism node
    /// type at all (`cbase`, which only ever appears as a *child*). Such a hook
    /// would contribute nothing to the tag set, and a cop with an empty tag set
    /// opts into the walker's universal dispatch — the opposite of what the
    /// document asked for.
    ///
    /// [`IrErrorKind::Location`] when an anchor path is shaped in a way the
    /// loader's vocabulary check accepts but the runtime cannot walk.
    pub fn new(doc: IrCop) -> Result<Self, IrError> {
        let name: &'static str = Box::leak(doc.document.cop.clone().into_boxed_str());
        let err = |kind, message| IrError {
            origin: doc.origin.clone(),
            line: None,
            column: None,
            kind,
            message,
        };

        let mut tag_set: Vec<u8> = Vec::new();
        for hook in &doc.document.hooks {
            let before = tag_set.len();
            for ty in &hook.on {
                tag_set.extend_from_slice(parser_type_tags(ty));
            }
            if tag_set.len() == before {
                return Err(err(
                    IrErrorKind::NodeType,
                    format!(
                        "hook `on: {:?}` maps to no Prism node type; it could never fire",
                        hook.on
                    ),
                ));
            }
        }
        tag_set.sort_unstable();
        tag_set.dedup();
        let node_tags: &'static [u8] = Box::leak(tag_set.into_boxed_slice());

        let severity = severity_of(doc.document.severity);
        let mut hooks = Vec::with_capacity(doc.document.hooks.len());
        for hook in &doc.document.hooks {
            let (plan, capture_names) = build_match_plan(&hook.match_spec, &doc.document)
                .ok_or_else(|| {
                    err(
                        IrErrorKind::UnknownMatcher,
                        "hook `match:` names a matcher the compiled document does not have"
                            .to_string(),
                    )
                })?;
            let anchor = |path: &str| {
                parse_anchor(path, &capture_names, true).ok_or_else(|| {
                    err(
                        IrErrorKind::Location,
                        format!("anchor `{path}` is not a point the runtime can resolve"),
                    )
                })
            };
            let range = |spec: &LocationSpec| {
                parse_range(spec, &capture_names).ok_or_else(|| {
                    err(
                        IrErrorKind::Location,
                        "`location:`/`range:` is not a range the runtime can resolve".to_string(),
                    )
                })
            };
            let corrections = hook
                .offense
                .correct
                .iter()
                .map(|op| {
                    Ok(match op {
                        CorrectionOp::Replace { range: r, text } => CorrectionPlan::Replace {
                            range: range(r)?,
                            text: text.clone(),
                        },
                        CorrectionOp::Remove { range: r } => {
                            CorrectionPlan::Remove { range: range(r)? }
                        }
                        CorrectionOp::InsertBefore { at, text }
                        | CorrectionOp::InsertAfter { at, text } => CorrectionPlan::Insert {
                            at: anchor(at)?,
                            text: text.clone(),
                        },
                    })
                })
                .collect::<Result<Vec<_>, IrError>>()?;

            hooks.push(HookPlan {
                on: hook.on.clone(),
                location: range(&hook.offense.location)?,
                corrections,
                severity: hook.offense.severity.map_or(severity, severity_of),
                bind_names: hook.bind.0.iter().map(|(k, _)| k.clone()).collect(),
                capture_names,
                plan,
            });
        }

        let (const_params, config_params) = split_params(&doc);
        Ok(Self {
            name,
            node_tags,
            wants_ancestors: doc_wants_ancestors(&doc.compiled),
            default_enabled: doc.document.enabled_default != EnabledDefault::False,
            severity,
            restrict: doc
                .document
                .restrict_on_send
                .iter()
                .map(|m| m.as_bytes().to_vec().into_boxed_slice())
                .collect(),
            hooks,
            const_params,
            config_params,
            doc,
        })
    }

    /// The document this runner was built from.
    #[must_use]
    pub fn document(&self) -> &IrCop {
        &self.doc
    }

    fn compiled(&self) -> &CompiledDoc {
        &self.doc.compiled
    }

    /// Resolved `config:` values in [`CompiledDoc::config_names`] order.
    ///
    /// Only called once a matcher has matched. An `enum` key falls back to the
    /// declared default when the configured value is not a member: the load
    /// already checked the *default*, but a `.rubocop.yml` can still name a
    /// style this cop does not have.
    fn config_values(&self, config: &CopConfig) -> Vec<serde_yml::Value> {
        self.compiled()
            .config_names
            .iter()
            .enumerate()
            .map(|(i, key)| {
                let fallback = || self.compiled().config_defaults[i].clone();
                let Some(decl) = self.doc.document.config.get(key) else {
                    return fallback();
                };
                let Some(value) = config.options.get(key) else {
                    return fallback();
                };
                if decl.ty == ConfigType::Enum
                    && !value
                        .as_str()
                        .is_some_and(|s| decl.values.iter().any(|v| v == s))
                {
                    return fallback();
                }
                value.clone()
            })
            .collect()
    }

    /// `%param` bindings for this file, or `None` when the document has no
    /// config-backed params and the precomputed set can be used as-is.
    fn params(&self, config: &CopConfig) -> Option<Params> {
        if self.config_params.is_empty() {
            return None;
        }
        let mut params = self.const_params.clone();
        for (name, slot) in &self.config_params {
            let value = config
                .options
                .get(name)
                .cloned()
                .unwrap_or_else(|| self.compiled().config_defaults[*slot].clone());
            params = params.with_named(name.clone(), yaml_to_arg(&value));
        }
        Some(params)
    }

    /// Which of the hook's declared `on:` types this node answers to.
    fn hook_type<'h>(hook: &'h HookPlan, node: &ruby_prism::Node<'_>) -> Option<&'h str> {
        let direct = parser_type_for_node(node);
        let block = block_type_of(node);
        hook.on.iter().find_map(|declared| {
            let declared = declared.as_str();
            // Virtual types: Prism keeps one `BlockNode` and varies the
            // parameters node, and the Parser `block` node is nitrocop's
            // `CallNode`-carrying-a-block (or a `LambdaNode`). `block_type_of`
            // is the single point of truth for both facts.
            let hit = match declared {
                "block" | "numblock" | "itblock" => block == Some(declared),
                "any_block" => block.is_some(),
                _ => direct == Some(declared),
            };
            hit.then_some(declared)
        })
    }

    /// `RESTRICT_ON_SEND`: a byte compare that kills most `send` traffic.
    fn restricted(&self, node: &ruby_prism::Node<'_>) -> bool {
        if self.restrict.is_empty() {
            return false;
        }
        let Some(call) = node.as_call_node() else {
            return false;
        };
        let name = call.name();
        !self.restrict.iter().any(|m| m.as_ref() == name.as_slice())
    }

    #[allow(clippy::too_many_arguments)]
    fn report<'a, 'pr>(
        &'pr self,
        index: usize,
        source: &'pr SourceFile,
        node: &ruby_prism::Node<'pr>,
        ancestors: &'a [ruby_prism::Node<'pr>],
        caps: &'a Captures<'pr>,
        config_values: &'pr [serde_yml::Value],
        diagnostics: &mut Vec<Diagnostic>,
        corrections: Option<&mut Vec<Correction>>,
    ) {
        let plan = &self.hooks[index];
        let spec = &self.doc.document.hooks[index].offense;
        let compiled = &self.compiled().hooks[index];

        // Binds evaluate in declaration order, each able to see the previous
        // ones. `EvalCtx::with_binds` borrows the vector, so the context is
        // rebuilt per bind and the borrow ends before the next push.
        let mut binds: Vec<Value<'pr>> = Vec::with_capacity(compiled.binds.len());
        for (_, expr) in &compiled.binds {
            let value = {
                let ctx = self
                    .eval_ctx(source, node, ancestors, caps, config_values)
                    .with_binds(&binds);
                eval(expr, &ctx)
            };
            binds.push(value);
        }

        if let Some(when) = &compiled.when {
            let ctx = self
                .eval_ctx(source, node, ancestors, caps, config_values)
                .with_binds(&binds);
            if !eval(when, &ctx).truthy() {
                return;
            }
        }

        let Some((start, _)) = plan.location.resolve(node, ancestors, caps) else {
            debug_assert!(false, "{}: `location:` anchor did not resolve", self.name);
            return;
        };

        let render = |template: &str| {
            render_message(
                template,
                plan,
                caps,
                &binds,
                source,
                &self.compiled().config_names,
                config_values,
            )
        };

        let (line, column) = source.offset_to_line_col(start);
        let mut diag = self.diagnostic(source, line, column, render(&spec.message));
        diag.severity = plan.severity;

        if let Some(corr) = corrections
            && !plan.corrections.is_empty()
        {
            let mut edits = Vec::with_capacity(plan.corrections.len());
            for op in &plan.corrections {
                let edit = match op {
                    CorrectionPlan::Replace { range, text } => range
                        .resolve(node, ancestors, caps)
                        .map(|(s, e)| (s, e, render(text))),
                    CorrectionPlan::Remove { range } => range
                        .resolve(node, ancestors, caps)
                        .map(|(s, e)| (s, e, String::new())),
                    CorrectionPlan::Insert { at, text } => at
                        .resolve_point(node, ancestors, caps)
                        .map(|p| (p, p, render(text))),
                };
                let Some((s, e, replacement)) = edit else {
                    debug_assert!(false, "{}: `correct:` anchor did not resolve", self.name);
                    diagnostics.push(diag);
                    return;
                };
                edits.push(Correction {
                    start: s,
                    end: e,
                    replacement,
                    cop_name: self.name,
                    cop_index: 0,
                });
            }
            corr.extend(edits);
            diag.corrected = true;
        }

        diagnostics.push(diag);
    }

    fn eval_ctx<'a, 'pr>(
        &'pr self,
        source: &'pr SourceFile,
        node: &ruby_prism::Node<'pr>,
        ancestors: &'a [ruby_prism::Node<'pr>],
        caps: &'a Captures<'pr>,
        config_values: &'pr [serde_yml::Value],
    ) -> EvalCtx<'a, 'pr> {
        EvalCtx::new(node, source, self.compiled())
            .with_ancestors(ancestors)
            .with_captures(caps)
            .with_config(config_values)
    }
}

impl Cop for IrCopRunner {
    fn name(&self) -> &'static str {
        self.name
    }

    fn default_severity(&self) -> Severity {
        self.severity
    }

    /// `enabled_default: pending` reads as `true` here, exactly as a
    /// hand-written cop for a pending upstream cop does: the tri-state lives in
    /// config resolution (`EnabledState::Pending` + `AllCops: NewCops`), and
    /// this hook only exists so a vendor-disabled cop stays off when the
    /// vendored config fails to load.
    fn default_enabled(&self) -> bool {
        self.default_enabled
    }

    fn interested_node_types(&self) -> &'static [u8] {
        self.node_tags
    }

    fn supports_autocorrect(&self) -> bool {
        self.doc.document.autocorrect != AutocorrectMode::None
    }

    fn safe_autocorrect(&self) -> bool {
        self.doc.document.autocorrect != AutocorrectMode::Unsafe
    }

    fn wants_ancestors(&self) -> bool {
        self.wants_ancestors
    }

    fn check_node(
        &self,
        source: &SourceFile,
        node: &ruby_prism::Node<'_>,
        parse_result: &ruby_prism::ParseResult<'_>,
        config: &CopConfig,
        diagnostics: &mut Vec<Diagnostic>,
        corrections: Option<&mut Vec<Correction>>,
    ) {
        self.check_node_with_ancestors(
            source,
            node,
            &[],
            parse_result,
            config,
            diagnostics,
            corrections,
        );
    }

    fn check_node_with_ancestors(
        &self,
        source: &SourceFile,
        node: &ruby_prism::Node<'_>,
        ancestors: &[ruby_prism::Node<'_>],
        _parse_result: &ruby_prism::ParseResult<'_>,
        config: &CopConfig,
        diagnostics: &mut Vec<Diagnostic>,
        mut corrections: Option<&mut Vec<Correction>>,
    ) {
        let resolver = DocResolver(self.compiled());
        let owned_params = self.params(config);
        let params = owned_params.as_ref().unwrap_or(&self.const_params);

        for (index, hook) in self.hooks.iter().enumerate() {
            let Some(matched_type) = Self::hook_type(hook, node) else {
                continue;
            };
            if matches!(matched_type, "send" | "csend") && self.restricted(node) {
                continue;
            }
            let mut slots: Vec<Option<CaptureValue<'_>>> = Vec::new();
            slots.resize_with(hook.capture_names.len(), || None);
            if !hook.plan.run(
                self.compiled(),
                node,
                ancestors,
                params,
                &resolver,
                &mut slots,
            ) {
                continue;
            }
            let caps = Captures::from_slots(slots);

            // Allocated only now that a matcher has matched (§3.5).
            let values;
            let config_values: &[serde_yml::Value] = if self.compiled().config_names.is_empty() {
                &self.compiled().config_defaults
            } else {
                values = self.config_values(config);
                &values
            };
            self.report(
                index,
                source,
                node,
                ancestors,
                &caps,
                config_values,
                diagnostics,
                corrections.as_deref_mut(),
            );
        }
    }
}

impl MatchPlan {
    /// Run the plan, writing into the hook's merged capture slots.
    ///
    /// Merge rule: first binding wins, matching the loader's first-occurrence
    /// rule for the merged name list. An `any_of` stops at its first matching
    /// branch, so only that branch's captures are bound; the rest stay `None`,
    /// which the expression layer reads as `nil`.
    fn run<'pr>(
        &self,
        doc: &CompiledDoc,
        node: &ruby_prism::Node<'pr>,
        ancestors: &[ruby_prism::Node<'pr>],
        params: &Params,
        resolver: &dyn Resolver,
        slots: &mut [Option<CaptureValue<'pr>>],
    ) -> bool {
        match self {
            Self::One { id, remap } => {
                let Some(caps) = doc.matchers[*id]
                    .match_captures_with_ancestors(node, ancestors, params, resolver)
                else {
                    return false;
                };
                for (slot, &merged) in remap.iter().enumerate() {
                    if slots[merged].is_none()
                        && let Some(value) = caps.get(slot)
                    {
                        slots[merged] = Some(value.clone());
                    }
                }
                true
            }
            Self::AnyOf(branches) => branches
                .iter()
                .any(|b| b.run(doc, node, ancestors, params, resolver, slots)),
            Self::AllOf(branches) => branches
                .iter()
                .all(|b| b.run(doc, node, ancestors, params, resolver, slots)),
        }
    }
}

/// Build a hook's [`MatchPlan`] and its merged capture-name list.
///
/// The name list has to reproduce the loader's own merge — declaration order,
/// first occurrence winning — because the expression compiler resolved `$name`
/// against exactly that list.
fn build_match_plan(
    spec: &MatchSpec,
    doc: &super::schema::IrDocument,
) -> Option<(MatchPlan, Vec<String>)> {
    let mut names: Vec<String> = Vec::new();
    let mut collect = |spec: &MatchSpec| {
        let mut stack = vec![spec];
        while let Some(current) = stack.pop() {
            match current {
                MatchSpec::Named(name) => {
                    for capture in &doc.matchers.get(name)?.captures {
                        if !names.contains(capture) {
                            names.push(capture.clone());
                        }
                    }
                }
                MatchSpec::Combined(combinator) => {
                    // Reverse, because the stack pops last-first and the merge
                    // order is declaration order.
                    for operand in combinator.operands().iter().rev() {
                        stack.push(operand);
                    }
                }
            }
        }
        Some(())
    };
    collect(spec)?;
    let names_snapshot = names.clone();
    let plan = build_plan_node(spec, doc, &names_snapshot)?;
    Some((plan, names_snapshot))
}

fn build_plan_node(
    spec: &MatchSpec,
    doc: &super::schema::IrDocument,
    names: &[String],
) -> Option<MatchPlan> {
    match spec {
        MatchSpec::Named(name) => {
            let id = doc.matchers.keys().position(|k| k == name)?;
            let remap = doc.matchers[name]
                .captures
                .iter()
                .map(|capture| names.iter().position(|n| n == capture))
                .collect::<Option<Vec<_>>>()?;
            Some(MatchPlan::One { id, remap })
        }
        MatchSpec::Combined(combinator) => {
            let branches = combinator
                .operands()
                .iter()
                .map(|operand| build_plan_node(operand, doc, names))
                .collect::<Option<Vec<_>>>()?;
            Some(match combinator {
                super::schema::MatchCombinator::AnyOf(_) => MatchPlan::AnyOf(branches),
                super::schema::MatchCombinator::AllOf(_) => MatchPlan::AllOf(branches),
            })
        }
    }
}

// ---- anchors ----------------------------------------------------------------

fn parse_range(spec: &LocationSpec, captures: &[String]) -> Option<AnchorRange> {
    match spec {
        LocationSpec::Shorthand(path) => {
            Some(AnchorRange::Whole(parse_anchor(path, captures, false)?))
        }
        LocationSpec::Range(range) => Some(AnchorRange::Pair(
            parse_anchor(&range.start, captures, true)?,
            parse_anchor(&range.stop, captures, true)?,
        )),
    }
}

/// Split `<target>[.<accessor>]*[.<part>][.<edge>]`.
///
/// The loader already checked that every segment is in the `ACCESSORS` /
/// `PARTS` / `EDGES` vocabulary and that the target is declared; this is the
/// structural split those flat lists cannot express.
fn parse_anchor(path: &str, captures: &[String], want_edge: bool) -> Option<Anchor> {
    let mut segments = path.split('.');
    let target = match segments.next()? {
        "node" => AnchorTarget::Node,
        "parent" => AnchorTarget::Parent,
        other => AnchorTarget::Capture(
            captures
                .iter()
                .position(|c| c == other.strip_prefix('$').unwrap_or(other))?,
        ),
    };
    let mut anchor = Anchor {
        target,
        accessors: Vec::new(),
        part: None,
        edge: None,
    };
    for segment in segments {
        match segment {
            "start" => anchor.edge = Some(true),
            "stop" => anchor.edge = Some(false),
            _ if anchor.edge.is_some() => return None,
            part if PARTS.contains(&part) => {
                if anchor.part.is_some() {
                    return None;
                }
                anchor.part = Some(part.to_string());
            }
            accessor if anchor.part.is_none() => anchor.accessors.push(accessor.to_string()),
            _ => return None,
        }
    }
    (anchor.edge.is_some() == want_edge).then_some(anchor)
}

/// `node.loc.<part>` names. Mirrors `load::PARTS`; the split above needs to
/// tell a part from an accessor, which the loader never has to.
const PARTS: &[&str] = &[
    "expression",
    "selector",
    "dot",
    "keyword",
    "end_keyword",
    "operator",
    "begin",
    "end",
];

impl AnchorRange {
    fn resolve(
        &self,
        node: &ruby_prism::Node<'_>,
        ancestors: &[ruby_prism::Node<'_>],
        caps: &Captures<'_>,
    ) -> Option<(usize, usize)> {
        match self {
            Self::Whole(anchor) => anchor.resolve_range(node, ancestors, caps),
            Self::Pair(start, stop) => Some((
                start.resolve_point(node, ancestors, caps)?,
                stop.resolve_point(node, ancestors, caps)?,
            )),
        }
    }
}

impl Anchor {
    /// Walk the accessors from the target node, then take the `part:` range —
    /// the node's own extent when the path names no part.
    fn resolve_range(
        &self,
        node: &ruby_prism::Node<'_>,
        ancestors: &[ruby_prism::Node<'_>],
        caps: &Captures<'_>,
    ) -> Option<(usize, usize)> {
        let mut current = match self.target {
            AnchorTarget::Node => dup_node(node),
            AnchorTarget::Parent => dup_node(ancestors.last()?),
            AnchorTarget::Capture(slot) => dup_node(caps.node(slot)?),
        };
        for accessor in &self.accessors {
            current = accessor_node(&current, accessor)?;
        }
        let loc = match &self.part {
            Some(part) => part_loc(&current, part)?,
            None => current.location(),
        };
        Some((loc.start_offset(), loc.end_offset()))
    }

    fn resolve_point(
        &self,
        node: &ruby_prism::Node<'_>,
        ancestors: &[ruby_prism::Node<'_>],
        caps: &Captures<'_>,
    ) -> Option<usize> {
        let (start, stop) = self.resolve_range(node, ancestors, caps)?;
        Some(if self.edge.unwrap_or(true) {
            start
        } else {
            stop
        })
    }
}

/// The structural accessors of `load::ACCESSORS`.
///
/// `parent` and `name` are in that vocabulary but resolve to no node here:
/// `parent` mid-path would need the ancestor chain of a node the walker never
/// visited, and `name` is bytes rather than a node. Both fail closed.
fn accessor_node<'pr>(
    node: &ruby_prism::Node<'pr>,
    accessor: &str,
) -> Option<ruby_prism::Node<'pr>> {
    match accessor {
        "receiver" => node.as_call_node()?.receiver(),
        "block" => node.as_call_node()?.block(),
        "arguments" => node.as_call_node()?.arguments().map(|a| a.as_node()),
        "first_argument" => call_arg(node, 0),
        "last_argument" => {
            let count = node.as_call_node()?.arguments()?.arguments().iter().count();
            call_arg(node, count.checked_sub(1)?)
        }
        "body" => pick!(node;
            opt as_def_node.body,
            opt as_block_node.body,
            opt as_class_node.body,
            opt as_module_node.body,
            opt as_lambda_node.body,
            opt as_singleton_class_node.body,
        ),
        "value" => pick!(node;
            req as_assoc_node.value,
            req as_local_variable_write_node.value,
            req as_instance_variable_write_node.value,
            req as_class_variable_write_node.value,
            req as_global_variable_write_node.value,
            req as_constant_write_node.value,
            req as_optional_parameter_node.value,
        ),
        "condition" => pick!(node;
            req as_if_node.predicate,
            req as_unless_node.predicate,
            req as_while_node.predicate,
            req as_until_node.predicate,
            opt as_case_node.predicate,
        ),
        _ => None,
    }
}

fn call_arg<'pr>(node: &ruby_prism::Node<'pr>, index: usize) -> Option<ruby_prism::Node<'pr>> {
    node.as_call_node()?
        .arguments()?
        .arguments()
        .iter()
        .nth(index)
}

/// The `node.loc.<part>` vocabulary of `load::PARTS`.
fn part_loc<'pr>(node: &ruby_prism::Node<'pr>, part: &str) -> Option<PrismLoc<'pr>> {
    match part {
        "expression" => Some(node.location()),
        "selector" => pick!(node; opt as_call_node.message_loc, req as_def_node.name_loc),
        "dot" => node.as_call_node()?.call_operator_loc(),
        "keyword" => pick!(node;
            opt as_if_node.if_keyword_loc,
            req as_unless_node.keyword_loc,
            req as_while_node.keyword_loc,
            req as_until_node.keyword_loc,
            req as_case_node.case_keyword_loc,
            req as_case_match_node.case_keyword_loc,
            req as_class_node.class_keyword_loc,
            req as_module_node.module_keyword_loc,
            req as_def_node.def_keyword_loc,
            req as_return_node.keyword_loc,
            req as_yield_node.keyword_loc,
            req as_break_node.keyword_loc,
            req as_next_node.keyword_loc,
            req as_super_node.keyword_loc,
            req as_defined_node.keyword_loc,
            opt as_begin_node.begin_keyword_loc,
        ),
        "end_keyword" => pick!(node;
            opt as_def_node.end_keyword_loc,
            opt as_if_node.end_keyword_loc,
            opt as_unless_node.end_keyword_loc,
            opt as_while_node.closing_loc,
            opt as_until_node.closing_loc,
            req as_case_node.end_keyword_loc,
            req as_case_match_node.end_keyword_loc,
            req as_class_node.end_keyword_loc,
            req as_module_node.end_keyword_loc,
            opt as_begin_node.end_keyword_loc,
        ),
        "operator" => pick!(node;
            req as_and_node.operator_loc,
            req as_or_node.operator_loc,
            opt as_assoc_node.operator_loc,
            req as_local_variable_write_node.operator_loc,
            req as_instance_variable_write_node.operator_loc,
            req as_class_variable_write_node.operator_loc,
            req as_global_variable_write_node.operator_loc,
            req as_constant_write_node.operator_loc,
            req as_range_node.operator_loc,
        ),
        "begin" => pick!(node;
            opt as_call_node.opening_loc,
            opt as_array_node.opening_loc,
            req as_hash_node.opening_loc,
            req as_block_node.opening_loc,
            req as_parentheses_node.opening_loc,
            opt as_string_node.opening_loc,
            req as_lambda_node.opening_loc,
            opt as_interpolated_string_node.opening_loc,
        ),
        "end" => pick!(node;
            opt as_call_node.closing_loc,
            opt as_array_node.closing_loc,
            req as_hash_node.closing_loc,
            req as_block_node.closing_loc,
            req as_parentheses_node.closing_loc,
            opt as_string_node.closing_loc,
            req as_lambda_node.closing_loc,
            opt as_interpolated_string_node.closing_loc,
        ),
        _ => None,
    }
}

// ---- messages ---------------------------------------------------------------

/// Render a `%{name}` template. `%%` is a literal `%`.
///
/// RuboCop's own `%<name>s` spelling was rewritten to `%{name}` at translation
/// time, so only this one form reaches the runtime. Names resolve against the
/// hook's captures, then its binds, then the document's config keys — the same
/// order, and the same three namespaces, the loader validated against.
fn render_message(
    template: &str,
    plan: &HookPlan,
    caps: &Captures<'_>,
    binds: &[Value<'_>],
    source: &SourceFile,
    config_names: &[String],
    config: &[serde_yml::Value],
) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(index) = rest.find('%') {
        out.push_str(&rest[..index]);
        rest = &rest[index..];
        if let Some(stripped) = rest.strip_prefix("%%") {
            out.push('%');
            rest = stripped;
            continue;
        }
        let Some(body) = rest.strip_prefix("%{").and_then(|r| r.split_once('}')) else {
            out.push('%');
            rest = &rest[1..];
            continue;
        };
        let (name, tail) = body;
        rest = tail;
        if let Some(slot) = plan.capture_names.iter().position(|c| c == name) {
            out.push_str(&capture_text(caps.get(slot), source));
        } else if let Some(slot) = plan.bind_names.iter().position(|b| b == name) {
            out.push_str(&value_text(binds.get(slot)));
        } else if let Some(slot) = config_names.iter().position(|c| c == name) {
            out.push_str(&yaml_text(config.get(slot)));
        }
    }
    out.push_str(rest);
    out
}

fn capture_text(value: Option<&CaptureValue<'_>>, source: &SourceFile) -> String {
    match value {
        Some(CaptureValue::Node(node)) => {
            let loc = node.location();
            source
                .try_byte_slice(loc.start_offset(), loc.end_offset())
                .unwrap_or_default()
                .to_string()
        }
        Some(CaptureValue::Name(bytes)) => String::from_utf8_lossy(bytes).into_owned(),
        Some(CaptureValue::List(items)) => items
            .iter()
            .map(|item| capture_text(Some(item), source))
            .collect::<Vec<_>>()
            .join(", "),
        Some(CaptureValue::Absent) | None => String::new(),
    }
}

fn value_text(value: Option<&Value<'_>>) -> String {
    match value {
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Int(i)) => i.to_string(),
        Some(Value::Str(bytes) | Value::Sym(bytes)) => String::from_utf8_lossy(bytes).into_owned(),
        Some(Value::Node(node)) => String::from_utf8_lossy(node.location().as_slice()).into_owned(),
        Some(Value::List(items)) => items
            .iter()
            .map(|item| value_text(Some(item)))
            .collect::<Vec<_>>()
            .join(", "),
        Some(Value::Nil) | None => String::new(),
    }
}

fn yaml_text(value: Option<&serde_yml::Value>) -> String {
    match value {
        Some(serde_yml::Value::String(text)) => text.clone(),
        Some(serde_yml::Value::Bool(b)) => b.to_string(),
        Some(serde_yml::Value::Number(n)) => n.to_string(),
        Some(serde_yml::Value::Sequence(items)) => items
            .iter()
            .map(|item| yaml_text(Some(item)))
            .collect::<Vec<_>>()
            .join(", "),
        _ => String::new(),
    }
}

// ---- load-time analysis -----------------------------------------------------

/// Whether any matcher or expression in the document reads the ancestor chain
/// (design §3.3). Computed once, so the walker maintains its stack only for
/// documents that actually need it.
fn doc_wants_ancestors(doc: &CompiledDoc) -> bool {
    doc.matchers.iter().any(|m| pattern_ascends(m.ast()))
        || doc.predicates.iter().any(expr_ascends)
        || doc.hooks.iter().any(|hook| {
            hook.when.as_ref().is_some_and(expr_ascends)
                || hook.binds.iter().any(|(_, expr)| expr_ascends(expr))
        })
}

fn pattern_ascends(node: &PatternNode) -> bool {
    match node {
        PatternNode::ParentRef(_) => true,
        PatternNode::NodeMatch { children, .. }
        | PatternNode::Alternatives(children)
        | PatternNode::Subsequence(children)
        | PatternNode::Conjunction(children)
        | PatternNode::AnyOrder(children) => children.iter().any(pattern_ascends),
        PatternNode::HelperCall { args, .. } | PatternNode::Predicate { args, .. } => {
            args.iter().any(pattern_ascends)
        }
        PatternNode::Capture { inner, .. }
        | PatternNode::Negation(inner)
        | PatternNode::DescendRef(inner)
        | PatternNode::Repetition { inner, .. } => pattern_ascends(inner),
        _ => false,
    }
}

/// Predicates whose upstream definition reads `node.parent` (PR #11's table).
const ANCESTOR_PREDICATES: &[&str] = &[
    "argument?",
    "chained?",
    "def_modifier?",
    "guard_clause?",
    "macro?",
    "parent?",
    "root?",
    "value_used?",
];

fn expr_ascends(expr: &Expr) -> bool {
    match expr {
        Expr::Path(Target::Parent) => true,
        Expr::Lit(_) | Expr::Path(_) => false,
        Expr::Attr { of, attr } => *attr == Attr::ParentType || expr_ascends(of),
        Expr::Pred { of, pred, .. } => ANCESTOR_PREDICATES.contains(&pred.name) || expr_ascends(of),
        Expr::Intrinsic { of, which } => {
            matches!(which, Intrinsic::Root | Intrinsic::ValueUsed) || expr_ascends(of)
        }
        Expr::Matches { of, .. } | Expr::Regex { of, .. } | Expr::Not(of) => expr_ascends(of),
        Expr::All(items) | Expr::Any(items) => items.iter().any(expr_ascends),
        Expr::Cmp { lhs, rhs, .. } => expr_ascends(lhs) || expr_ascends(rhs),
        Expr::In { needle, haystack } => expr_ascends(needle) || haystack.iter().any(expr_ascends),
        Expr::If { cond, then, els } => {
            expr_ascends(cond) || expr_ascends(then) || expr_ascends(els)
        }
        Expr::Lookup { key, .. } => expr_ascends(key),
        Expr::Quant { of, over, body, .. } => {
            *over == Collection::Ancestors || expr_ascends(of) || expr_ascends(body)
        }
    }
}

/// Split declared matcher `params:` into the ones a constant table answers
/// (fixed at load) and the ones a `config:` key answers (per file).
///
/// **Provisional.** The pilot batch (design §6.1 bucket A/B) is what will pin
/// the `Arg` shape a `%param` should take for each config type; nothing in the
/// first two IR cops binds one.
fn split_params(doc: &IrCop) -> (Params, Vec<(String, usize)>) {
    let mut const_params = Params::new();
    let mut config_params = Vec::new();
    let mut seen: Vec<&String> = Vec::new();
    for matcher in doc.document.matchers.values() {
        for param in &matcher.params {
            if seen.contains(&param) {
                continue;
            }
            seen.push(param);
            if let Some(table) = doc.document.constants.get(param) {
                let members = table.keys().map(|k| Arg::Symbol(k.clone())).collect();
                const_params = const_params.with_named(param.clone(), Arg::Set(members));
            } else if let Some(slot) = doc.compiled.config_names.iter().position(|n| n == param) {
                config_params.push((param.clone(), slot));
            }
        }
    }
    (const_params, config_params)
}

fn yaml_to_arg(value: &serde_yml::Value) -> Arg {
    match value {
        serde_yml::Value::String(text) => Arg::Str(text.clone()),
        serde_yml::Value::Number(n) => n.as_i64().map_or(Arg::Unresolved, Arg::Int),
        serde_yml::Value::Sequence(items) => Arg::Set(items.iter().map(yaml_to_arg).collect()),
        _ => Arg::Unresolved,
    }
}

fn severity_of(severity: IrSeverity) -> Severity {
    match severity {
        IrSeverity::Convention => Severity::Convention,
        IrSeverity::Warning => Severity::Warning,
        IrSeverity::Error => Severity::Error,
    }
}

// ---- Parser type -> Prism tags ----------------------------------------------

/// Prism node-type tags a Parser-gem type name can appear as.
///
/// The inverse of `node_pattern::interpreter::parser_type_for_node`, which is
/// many-to-one in both directions: one Parser name can cover several Prism
/// types (`const`, `regexp`, `op_asgn`) and one Prism type several Parser names
/// (`CallNode` is `send`, `csend` *and*, when it carries a block, `block`).
/// `ir_node_tags_cover_every_parser_type` pins the two against each other.
///
/// `block`/`numblock`/`itblock`/`any_block` deliberately map to `CallNode` and
/// `LambdaNode` rather than `BlockNode`: the Parser `block` node is nitrocop's
/// call-carrying-a-block, which is what `on_block` visits upstream. Dispatching
/// on `BlockNode` too would report every such offense twice.
fn parser_type_tags(name: &str) -> &'static [u8] {
    use tag::*;
    match name {
        "send" | "csend" => &[CALL_NODE],
        "block" | "numblock" | "itblock" | "any_block" => &[CALL_NODE, LAMBDA_NODE],
        "def" | "defs" => &[DEF_NODE],
        "const" => &[CONSTANT_READ_NODE, CONSTANT_PATH_NODE],
        "begin" => &[
            STATEMENTS_NODE,
            PARENTHESES_NODE,
            EMBEDDED_STATEMENTS_NODE,
            BEGIN_NODE,
        ],
        "kwbegin" => &[BEGIN_NODE],
        "pair" => &[ASSOC_NODE],
        "hash" => &[HASH_NODE, KEYWORD_HASH_NODE],
        "lvar" => &[LOCAL_VARIABLE_READ_NODE],
        "ivar" => &[INSTANCE_VARIABLE_READ_NODE],
        "cvar" => &[CLASS_VARIABLE_READ_NODE],
        "gvar" => &[GLOBAL_VARIABLE_READ_NODE],
        "sym" => &[SYMBOL_NODE],
        "str" => &[STRING_NODE],
        "int" => &[INTEGER_NODE],
        "float" => &[FLOAT_NODE],
        "true" => &[TRUE_NODE],
        "false" => &[FALSE_NODE],
        "nil" => &[NIL_NODE],
        "self" => &[SELF_NODE],
        "array" => &[ARRAY_NODE],
        "if" => &[IF_NODE],
        "case" => &[CASE_NODE],
        "when" => &[WHEN_NODE],
        "while" => &[WHILE_NODE],
        "until" => &[UNTIL_NODE],
        "for" => &[FOR_NODE],
        "return" => &[RETURN_NODE],
        "yield" => &[YIELD_NODE],
        "and" => &[AND_NODE],
        "or" => &[OR_NODE],
        "regexp" => &[
            REGULAR_EXPRESSION_NODE,
            INTERPOLATED_REGULAR_EXPRESSION_NODE,
        ],
        "class" => &[CLASS_NODE],
        "module" => &[MODULE_NODE],
        "lvasgn" => &[LOCAL_VARIABLE_WRITE_NODE],
        "ivasgn" => &[INSTANCE_VARIABLE_WRITE_NODE],
        "cvasgn" => &[CLASS_VARIABLE_WRITE_NODE],
        "gvasgn" => &[GLOBAL_VARIABLE_WRITE_NODE],
        "casgn" => &[CONSTANT_WRITE_NODE],
        "splat" => &[SPLAT_NODE],
        "super" => &[SUPER_NODE],
        "zsuper" => &[FORWARDING_SUPER_NODE],
        "lambda" => &[LAMBDA_NODE],
        "dstr" => &[INTERPOLATED_STRING_NODE],
        "dsym" => &[INTERPOLATED_SYMBOL_NODE],
        "args" => &[PARAMETERS_NODE, BLOCK_PARAMETERS_NODE],
        "arg" => &[REQUIRED_PARAMETER_NODE],
        "optarg" => &[OPTIONAL_PARAMETER_NODE],
        "restarg" => &[REST_PARAMETER_NODE],
        "kwarg" => &[REQUIRED_KEYWORD_PARAMETER_NODE],
        "kwoptarg" => &[OPTIONAL_KEYWORD_PARAMETER_NODE],
        "kwrestarg" => &[KEYWORD_REST_PARAMETER_NODE],
        "blockarg" => &[BLOCK_PARAMETER_NODE],
        "forward_arg" => &[FORWARDING_PARAMETER_NODE],
        "shadowarg" => &[BLOCK_LOCAL_VARIABLE_NODE],
        "mlhs" => &[MULTI_TARGET_NODE],
        "masgn" => &[MULTI_WRITE_NODE],
        "block_pass" => &[BLOCK_ARGUMENT_NODE],
        "kwsplat" => &[ASSOC_SPLAT_NODE],
        "case_match" => &[CASE_MATCH_NODE],
        "in_pattern" => &[IN_NODE],
        "sclass" => &[SINGLETON_CLASS_NODE],
        "next" => &[NEXT_NODE],
        "break" => &[BREAK_NODE],
        "defined?" => &[DEFINED_NODE],
        "resbody" => &[RESCUE_NODE],
        "rational" => &[RATIONAL_NODE],
        "complex" => &[IMAGINARY_NODE],
        "xstr" => &[X_STRING_NODE, INTERPOLATED_X_STRING_NODE],
        "irange" | "erange" => &[RANGE_NODE],
        "op_asgn" => &[
            LOCAL_VARIABLE_OPERATOR_WRITE_NODE,
            INSTANCE_VARIABLE_OPERATOR_WRITE_NODE,
            CLASS_VARIABLE_OPERATOR_WRITE_NODE,
            GLOBAL_VARIABLE_OPERATOR_WRITE_NODE,
            CONSTANT_OPERATOR_WRITE_NODE,
            CONSTANT_PATH_OPERATOR_WRITE_NODE,
            CALL_OPERATOR_WRITE_NODE,
            INDEX_OPERATOR_WRITE_NODE,
        ],
        "or_asgn" => &[
            LOCAL_VARIABLE_OR_WRITE_NODE,
            INSTANCE_VARIABLE_OR_WRITE_NODE,
            CLASS_VARIABLE_OR_WRITE_NODE,
            GLOBAL_VARIABLE_OR_WRITE_NODE,
            CONSTANT_OR_WRITE_NODE,
            CONSTANT_PATH_OR_WRITE_NODE,
            CALL_OR_WRITE_NODE,
            INDEX_OR_WRITE_NODE,
        ],
        "and_asgn" => &[
            LOCAL_VARIABLE_AND_WRITE_NODE,
            INSTANCE_VARIABLE_AND_WRITE_NODE,
            CLASS_VARIABLE_AND_WRITE_NODE,
            GLOBAL_VARIABLE_AND_WRITE_NODE,
            CONSTANT_AND_WRITE_NODE,
            CONSTANT_PATH_AND_WRITE_NODE,
            CALL_AND_WRITE_NODE,
            INDEX_AND_WRITE_NODE,
        ],
        _ => &[],
    }
}

/// Parser type names the runtime can dispatch on, for the drift test and for
/// error messages.
#[must_use]
pub fn dispatchable_parser_types() -> &'static [&'static str] {
    static NAMES: OnceLock<Vec<&'static str>> = OnceLock::new();
    NAMES
        .get_or_init(|| {
            crate::node_pattern::build_mapping_table()
                .into_keys()
                .filter(|name| !parser_type_tags(name).is_empty())
                .collect()
        })
        .as_slice()
}

#[cfg(test)]
mod tests {
    use ruby_prism::Visit;

    use super::*;
    use crate::cop::registry::CopRegistry;
    use crate::cop::walker::BatchedCopWalker;
    use crate::testutil::{run_cop_autocorrect, run_cop_full, run_cop_full_with_config};

    /// Build a runner from a YAML document, panicking with the loader's own
    /// `origin:line: message` on failure.
    fn runner(yaml: &str) -> IrCopRunner {
        let doc = super::super::load::load_str(yaml, "test.cop.yml")
            .unwrap_or_else(|e| panic!("load failed: {e}"));
        IrCopRunner::new(doc).unwrap_or_else(|e| panic!("runner failed: {e}"))
    }

    /// One throwaway cop exercising the whole offense path: every anchor target
    /// (`node`, `$capture`, `parent`), an accessor step, a `loc` part, an
    /// explicit `{start:, stop:}` pair, and all four correction ops.
    ///
    /// `Thing.wrap(x)` is the shape; each hook below anchors somewhere else in
    /// it so one Ruby snippet covers the vocabulary.
    const ANCHORS: &str = r##"
schema: 1
cop: "Test/Anchors"
autocorrect: safe
restrict_on_send: [wrap]
config:
  Label: { type: string, default: "L" }
matchers:
  wrap:
    pattern: |
      (send (const nil? :Thing) :wrap $_)
    captures: [arg]
hooks:
  - on: [send]
    match: wrap
    bind:
      shout: { if: [true, "yes", "no"] }
    offense:
      location: node
      message: "%{Label}/%{shout}: wrap %{arg} %%"
      correct:
        - op: replace
          range: { start: node.selector.start, stop: node.expression.stop }
          text: "keep(%{arg})"
        - op: insert_before
          at: node.expression.start
          text: "# "
        - op: insert_after
          at: node.expression.stop
          text: " #"
"##;

    #[test]
    fn offense_location_message_and_replace() {
        let cop = runner(ANCHORS);
        let (diags, corrections) = run_cop_autocorrect(&cop, b"Thing.wrap(1)\n");
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].message, "L/yes: wrap 1 %");
        assert_eq!(diags[0].location.line, 1);
        assert_eq!(diags[0].location.column, 0);
        assert!(diags[0].corrected);

        let mut edits: Vec<(usize, usize, &str)> = corrections
            .iter()
            .map(|c| (c.start, c.end, c.replacement.as_str()))
            .collect();
        edits.sort_unstable();
        // `Thing.wrap(1)` — selector starts at 6, expression ends at 13.
        assert_eq!(
            edits,
            vec![(0, 0, "# "), (6, 13, "keep(1)"), (13, 13, " #")]
        );
    }

    #[test]
    fn config_overrides_the_declared_default() {
        let cop = runner(ANCHORS);
        let mut config = CopConfig::default();
        config.options.insert(
            "Label".to_string(),
            serde_yml::Value::String("X".to_string()),
        );
        let diags = run_cop_full_with_config(&cop, b"Thing.wrap(1)\n", config);
        assert_eq!(diags[0].message, "X/yes: wrap 1 %");
    }

    #[test]
    fn restrict_on_send_rejects_other_selectors() {
        let cop = runner(ANCHORS);
        assert!(run_cop_full(&cop, b"Thing.other(1)\n").is_empty());
    }

    #[test]
    fn anchor_targets_and_parts() {
        // `location:` on a capture, on an accessor step, and on a `loc` part.
        let cases: &[(&str, usize, usize)] = &[
            ("$arg", 11, 12),
            ("node.receiver", 0, 5),
            ("node.selector", 6, 10),
            ("node.begin", 10, 11),
            ("node.end", 12, 13),
        ];
        for (anchor, start_col, end_col) in cases {
            let yaml = ANCHORS
                .replace("location: node\n", &format!("location: {anchor}\n"))
                .replace("autocorrect: safe", "autocorrect: none")
                .replace(
                    "      correct:\n        - op: replace\n          range: { start: node.selector.start, stop: node.expression.stop }\n          text: \"keep(%{arg})\"\n        - op: insert_before\n          at: node.expression.start\n          text: \"# \"\n        - op: insert_after\n          at: node.expression.stop\n          text: \" #\"\n",
                    "",
                );
            let cop = runner(&yaml);
            let diags = run_cop_full(&cop, b"Thing.wrap(1)\n");
            assert_eq!(diags.len(), 1, "{anchor}: {diags:?}");
            assert_eq!(diags[0].location.column, *start_col, "{anchor}");
            let _ = end_col;
        }
    }

    #[test]
    fn remove_op_deletes_the_range() {
        let yaml = ANCHORS.replace(
            "        - op: replace\n          range: { start: node.selector.start, stop: node.expression.stop }\n          text: \"keep(%{arg})\"\n        - op: insert_before\n          at: node.expression.start\n          text: \"# \"\n        - op: insert_after\n          at: node.expression.stop\n          text: \" #\"\n",
            "        - op: remove\n          range: node.receiver\n",
        );
        let cop = runner(&yaml);
        let (_, corrections) = run_cop_autocorrect(&cop, b"Thing.wrap(1)\n");
        assert_eq!(corrections.len(), 1);
        assert_eq!(
            (
                corrections[0].start,
                corrections[0].end,
                corrections[0].replacement.as_str()
            ),
            (0, 5, "")
        );
    }

    /// `parent` as an anchor target needs the walker's ancestor stack, which is
    /// only maintained for a cop that asked — so this also pins
    /// `wants_ancestors` inference through a pattern's `^`.
    const PARENT_ANCHOR: &str = r#"
schema: 1
cop: "Test/ParentAnchor"
matchers:
  inner:
    pattern: |
      (send nil? :inner)
hooks:
  - on: [send]
    match: inner
    when: { pred: [parent, "send_type?"] }
    offense:
      location: parent
      message: "nested"
"#;

    #[test]
    fn parent_anchor_and_wants_ancestors() {
        let cop = runner(PARENT_ANCHOR);
        assert!(
            cop.wants_ancestors(),
            "`parent` in `when:`/`location:` must request the ancestor stack"
        );
        let diags = run_cop_full(&cop, b"outer(inner)\n");
        assert_eq!(diags.len(), 1, "{diags:?}");
        // Anchored on the parent call, which starts at column 0.
        assert_eq!(diags[0].location.column, 0);
    }

    #[test]
    fn wants_ancestors_is_false_without_an_ancestor_reference() {
        assert!(!runner(ANCHORS).wants_ancestors());
    }

    #[test]
    fn interested_node_types_is_the_union_of_hook_types() {
        let cop = runner(ANCHORS);
        assert_eq!(cop.interested_node_types(), &[tag::CALL_NODE]);
    }

    /// `on: [block]` is the Parser `block` node, which is nitrocop's
    /// `CallNode`-carrying-a-block — never the Prism `BlockNode`, or every
    /// offense would be reported twice.
    #[test]
    fn block_hooks_dispatch_on_the_call_not_the_block_node() {
        let cop = runner(
            r#"
schema: 1
cop: "Test/Block"
matchers:
  any:
    pattern: "(block (send nil? :each) ...)"
hooks:
  - on: [block]
    match: any
    offense:
      location: node
      message: "block"
"#,
        );
        assert_eq!(
            cop.interested_node_types(),
            &[tag::CALL_NODE, tag::LAMBDA_NODE]
        );
        let diags = run_cop_full(&cop, b"each { |x| x }\n");
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn numblock_and_itblock_are_re_discriminated_at_hook_entry() {
        let yaml = |on: &str| {
            format!(
                r#"
schema: 1
cop: "Test/NumBlock"
matchers:
  any:
    pattern: "(numblock (send nil? :each) ...)"
hooks:
  - on: [{on}]
    match: any
    offense:
      location: node
      message: "numblock"
"#
            )
        };
        let cop = runner(&yaml("numblock"));
        assert_eq!(run_cop_full(&cop, b"each { _1 }\n").len(), 1);
        assert!(run_cop_full(&cop, b"each { |x| x }\n").is_empty());
        assert!(run_cop_full(&cop, b"each { it }\n").is_empty());
    }

    #[test]
    fn a_hook_on_a_type_with_no_prism_node_is_rejected() {
        let doc = super::super::load::load_str(
            r#"
schema: 1
cop: "Test/NoTags"
matchers:
  any:
    pattern: "(cbase)"
hooks:
  - on: [cbase]
    match: any
    offense:
      location: node
      message: "x"
"#,
            "test.cop.yml",
        )
        .expect("document loads");
        let err = match IrCopRunner::new(doc) {
            Err(err) => err,
            Ok(_) => panic!("runner accepted a hook that can never fire"),
        };
        assert_eq!(err.kind, IrErrorKind::NodeType);
    }

    /// The end-to-end check that matters: an IR cop registered into a real
    /// `CopRegistry` and driven by the real `BatchedCopWalker`.
    #[test]
    fn runs_through_the_registry_and_the_batched_walker() {
        let mut registry = CopRegistry::new();
        registry.register(Box::new(runner(ANCHORS)));
        assert_eq!(registry.len(), 1);
        let cop = registry.get("Test/Anchors").expect("registered");

        let source = SourceFile::from_bytes("test.rb", b"Thing.wrap(1)\nThing.other(2)\n".to_vec());
        let parse_result = crate::parse::parse_source(source.as_bytes());
        let config = CopConfig::default();
        let mut walker =
            BatchedCopWalker::new(vec![(cop, &config)], &source, &parse_result).with_corrections();
        walker.visit(&parse_result.node());
        let (diags, corrections) = walker.into_results();

        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].cop_name, "Test/Anchors");
        assert_eq!(diags[0].message, "L/yes: wrap 1 %");
        assert_eq!(corrections.expect("corrections collected").len(), 3);
    }

    /// The tag table and `parser_type_for_node` are two hand-written halves of
    /// one mapping; this walks a snippet covering most node shapes and asserts
    /// every Parser type a node reports as is dispatchable to that node's tag.
    #[test]
    fn ir_node_tags_cover_every_parser_type() {
        const SNIPPET: &str = r#"
module M
  class C < B
    CONST = 1
    def self.build(a, b = 2, *rest, k:, o: 3, **kw, &blk)
      @x = a&.to_s
      @@y ||= [1, 2.0, "s", :sym, nil, true, false, self, 1..2, 1...3]
      $g = { k => v, **kw }
      x = ::Thing.new
      x += 1
      x &&= 2
      p, q = 1, 2
      return unless defined?(yield)
      case a
      when 1 then next
      else break
      end
      case a
      in [1, *] then 1
      end
      while a do a end
      until a do a end
      for i in 1..2 do i end
      begin; a; rescue => e; b; ensure; c; end
      [1].each { |z| z }
      [1].each { _1 }
      [1].each { it }
      -> (w) { w }
      %w[a].map(&:to_s)
      super(a)
      super
      "d#{a}" + `x#{a}` + /r#{a}/ + :"s#{a}"
      class << self; end
    end
  end
end
"#;
        let parse_result = crate::parse::parse_source(SNIPPET.as_bytes());

        struct Collector {
            seen: std::collections::BTreeSet<(&'static str, u8)>,
        }
        impl<'pr> Visit<'pr> for Collector {
            fn visit_branch_node_enter(&mut self, node: ruby_prism::Node<'pr>) {
                self.record(&node);
            }
            fn visit_leaf_node_enter(&mut self, node: ruby_prism::Node<'pr>) {
                self.record(&node);
            }
        }
        impl Collector {
            fn record(&mut self, node: &ruby_prism::Node<'_>) {
                let tag = crate::cop::shared::node_type::node_type_tag(node);
                for name in [parser_type_for_node(node), block_type_of(node)]
                    .into_iter()
                    .flatten()
                {
                    self.seen.insert((name, tag));
                }
            }
        }

        let mut collector = Collector {
            seen: std::collections::BTreeSet::new(),
        };
        collector.visit(&parse_result.node());
        assert!(
            collector.seen.len() > 50,
            "snippet is too thin to be a test"
        );

        let mut missing = Vec::new();
        for (name, tag) in &collector.seen {
            // The one deliberate omission, asserted on its own below: a Prism
            // `BlockNode` also reports as `block`/`numblock`/`itblock`, but the
            // Parser node a hook wants is the enclosing call.
            if *tag == tag::BLOCK_NODE
                && matches!(*name, "block" | "numblock" | "itblock" | "any_block")
            {
                continue;
            }
            if !parser_type_tags(name).contains(tag) {
                missing.push(format!(
                    "{name} reports tag {tag}, which parser_type_tags omits"
                ));
            }
        }
        assert!(missing.is_empty(), "{}", missing.join("\n"));

        for name in ["block", "numblock", "itblock", "any_block"] {
            assert!(
                !parser_type_tags(name).contains(&tag::BLOCK_NODE),
                "{name} must not dispatch on Prism's BlockNode: the offense would be double-reported"
            );
        }
    }

    #[test]
    fn every_dispatchable_type_is_a_known_mapping_entry() {
        let table = crate::node_pattern::build_mapping_table();
        for name in dispatchable_parser_types() {
            assert!(
                table.contains_key(name),
                "{name} is not in the mapping table"
            );
        }
    }
}
