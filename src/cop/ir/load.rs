//! Loader and validator for v1 cop IR documents.
//!
//! Loading is **fail-closed** (design §4.4): any structural or semantic problem
//! aborts the whole document. There is no partially-loaded cop, because a
//! silently dropped cop is an invisible false negative.
//!
//! Validation performed here, beyond what serde already rejects:
//!
//! * `schema:` is a version this build understands;
//! * `cop:` has the shape `Dept/Name`, and (for user cops) does not squat on a
//!   built-in department;
//! * every `on:` entry is a known Parser-gem node type;
//! * every `match:` resolves to a declared matcher;
//! * every matcher `pattern:` parses as a NodePattern;
//! * matcher `params:` resolve to a declared config key or constant table;
//! * `%{...}` message placeholders resolve to a capture, bind or config key;
//! * location/anchor strings are well-formed and reference declared captures;
//! * `config:` defaults match their declared type, and enum defaults are members;
//! * `when:`/`bind:` expressions compile to the typed [`super::expr::Expr`]
//!   tree, which is where reference resolution, arity and the depth cap now live.

use std::collections::{BTreeSet, HashSet};
use std::fmt;
use std::path::Path;
use std::sync::OnceLock;

use crate::node_pattern;

use super::expr::{CompileCtx, CompiledDoc, CompiledHook, LocPart};
use super::schema::{
    AutocorrectMode, ConfigType, CorrectionOp, IrDocument, LocationSpec, MatchSpec, SCHEMA_VERSION,
};

/// Departments owned by hand-written Rust cops. User cops may not use these
/// (design §4.2); IR translations of upstream cops obviously may.
#[rustfmt::skip]
pub const BUILTIN_DEPARTMENTS: &[&str] = &[
    "Bundler", "Capybara", "FactoryBot", "Gemspec", "Layout", "Lint", "Metrics", "Migration",
    "Naming", "Performance", "Rails", "Rake", "RSpec", "RSpecRails", "Security", "Style",
];

/// Parser-gem node types with no Prism equivalent; the hook re-discriminates a
/// `BlockNode` at entry (design §1.2).
const VIRTUAL_NODE_TYPES: &[&str] = &["numblock", "itblock"];

/// Structural accessors usable inside an anchor path.
#[rustfmt::skip]
const ACCESSORS: &[&str] = &[
    "receiver", "parent", "body", "value", "name", "block", "condition", "arguments",
    "first_argument", "last_argument", "left_sibling", "right_sibling",
];

const EDGES: &[&str] = &["start", "stop"];

/// A loaded, fully validated IR cop definition.
///
/// This is the *document* level type. The runtime `Cop` implementation that
/// compiles this into matchers and expressions arrives with a later PR.
#[derive(Debug)]
pub struct IrCop {
    /// Path (or pseudo-path) the document was read from, used in diagnostics.
    pub origin: String,
    /// The document exactly as written; the serde surface.
    pub document: IrDocument,
    /// The document's compiled matchers, predicates, constants and per-hook
    /// `when:`/`bind:` expressions.
    pub compiled: CompiledDoc,
}

impl IrCop {
    /// Full cop name, `Dept/Name`.
    pub fn name(&self) -> &str {
        &self.document.cop
    }

    /// Department half of the cop name.
    pub fn department(&self) -> &str {
        self.document.cop.split('/').next().unwrap_or_default()
    }

    /// Name half of the cop name.
    pub fn base_name(&self) -> &str {
        self.document.cop.split('/').nth(1).unwrap_or_default()
    }
}

/// Which discovery channel a document came from; controls the department rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadMode {
    /// Shipped IR translations of upstream cops: built-in departments allowed.
    Builtin,
    /// User- or gem-supplied cops: built-in departments rejected.
    User,
}

/// Classification of a load failure. Tests (and the `*.expected` sidecars of
/// `tests/fixtures/ir/invalid/`) assert on this variant name rather than on
/// message text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrErrorKind {
    Io,
    Yaml,
    UnsupportedSchema,
    CopName,
    Department,
    Hooks,
    NodeType,
    UnknownMatcher,
    Pattern,
    Capture,
    Param,
    ConfigDecl,
    ConfigDefault,
    Message,
    Location,
    Expr,
    /// An operator or predicate applied to the wrong number of operands.
    Arity,
    /// A `pred:` name no builtin explains.
    UnknownPredicate,
    /// A `matches:` reference cycle between named predicates.
    Cycle,
    Duplicate,
    Autocorrect,
}

/// A load failure, rendered as `origin:line:col: message`.
#[derive(Debug, Clone)]
pub struct IrError {
    pub origin: String,
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub kind: IrErrorKind,
    pub message: String,
}

impl fmt::Display for IrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.line, self.column) {
            (Some(line), Some(col)) => {
                write!(f, "{}:{}:{}: {}", self.origin, line, col, self.message)
            }
            (Some(line), None) => write!(f, "{}:{}: {}", self.origin, line, self.message),
            _ => write!(f, "{}: {}", self.origin, self.message),
        }
    }
}

impl std::error::Error for IrError {}

/// Load and validate a document from a file.
pub fn load_path(path: &Path) -> Result<IrCop, IrError> {
    load_path_with(path, LoadMode::Builtin)
}

/// Load and validate a document from a file under an explicit [`LoadMode`].
pub fn load_path_with(path: &Path, mode: LoadMode) -> Result<IrCop, IrError> {
    let origin = path.display().to_string();
    let source = std::fs::read_to_string(path).map_err(|e| IrError {
        origin: origin.clone(),
        line: None,
        column: None,
        kind: IrErrorKind::Io,
        message: e.to_string(),
    })?;
    load_str_with(&source, &origin, mode)
}

/// Load and validate a document from a string. `origin` is used for diagnostics.
pub fn load_str(source: &str, origin: &str) -> Result<IrCop, IrError> {
    load_str_with(source, origin, LoadMode::Builtin)
}

/// Load and validate a document under an explicit [`LoadMode`].
pub fn load_str_with(source: &str, origin: &str, mode: LoadMode) -> Result<IrCop, IrError> {
    let document = parse_document(source, origin)?;
    let compiled = Validator {
        document: &document,
        origin,
        source,
        mode,
    }
    .run()?;
    Ok(IrCop {
        origin: origin.to_string(),
        document,
        compiled,
    })
}

/// Deserialize the document.
///
/// The typed path is tried first because serde_yml attaches a `Location` to its
/// errors. YAML merge keys (`<<: *anchor`, used by the design's own examples)
/// are not expanded during typed deserialization, so on failure we retry through
/// an untyped `Value` with `apply_merge()`, at the cost of losing the location.
fn parse_document(source: &str, origin: &str) -> Result<IrDocument, IrError> {
    let direct = serde_yml::from_str::<IrDocument>(source);
    let err = match direct {
        Ok(doc) => return Ok(doc),
        Err(e) => e,
    };
    if source.contains("<<") {
        let mut value: serde_yml::Value =
            serde_yml::from_str(source).map_err(|e| yaml_err(origin, &e))?;
        if value.apply_merge().is_ok()
            && let Ok(doc) = serde_yml::from_value::<IrDocument>(value)
        {
            return Ok(doc);
        }
    }
    Err(yaml_err(origin, &err))
}

fn yaml_err(origin: &str, e: &serde_yml::Error) -> IrError {
    let loc = e.location();
    IrError {
        origin: origin.to_string(),
        line: loc.as_ref().map(|l| l.line()),
        column: loc.as_ref().map(|l| l.column()),
        kind: IrErrorKind::Yaml,
        message: e.to_string(),
    }
}

/// Known Parser-gem node type names accepted in `on:`.
fn node_type_names() -> &'static HashSet<&'static str> {
    static NAMES: OnceLock<HashSet<&'static str>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let mut set: HashSet<&'static str> =
            node_pattern::build_mapping_table().into_keys().collect();
        set.extend(VIRTUAL_NODE_TYPES.iter().copied());
        set
    })
}

/// `bail!(self, Kind, needle, "fmt {}", arg)` — build and return an [`IrError`].
/// `needle` is a source substring used to guess the offending line.
macro_rules! bail {
    ($v:expr, $kind:ident, $needle:expr, $($arg:tt)*) => {
        return Err($v.err(IrErrorKind::$kind, $needle, format!($($arg)*)))
    };
}

struct Validator<'a> {
    document: &'a IrDocument,
    origin: &'a str,
    source: &'a str,
    mode: LoadMode,
}

impl Validator<'_> {
    fn err(&self, kind: IrErrorKind, needle: Option<&str>, message: String) -> IrError {
        IrError {
            origin: self.origin.to_string(),
            line: needle.and_then(|n| locate(self.source, n)),
            column: None,
            kind,
            message,
        }
    }

    fn doc(&self) -> &IrDocument {
        self.document
    }

    /// Give a compiler error the best-effort line the rest of the loader
    /// computes for its own errors. Compiler messages open with the name of
    /// the expression they came from, which is the needle to scan for.
    fn located(&self, mut err: IrError) -> IrError {
        let needle = err
            .message
            .split_once('`')
            .and_then(|(_, rest)| rest.split('`').next());
        err.line = needle.and_then(|n| locate(self.source, n));
        err
    }

    fn run(&self) -> Result<CompiledDoc, IrError> {
        self.check_meta()?;
        self.check_config()?;
        // Compiles `matchers:` (NodePattern) and `predicates:` (expressions,
        // with cycle detection) before anything can reference them.
        let mut ctx = CompileCtx::new(self.origin, self.doc()).map_err(|e| self.located(e))?;
        self.check_matchers(&ctx)?;
        let hooks = self.check_hooks(&mut ctx)?;
        Ok(ctx.finish(hooks))
    }

    fn check_meta(&self) -> Result<(), IrError> {
        let doc = self.doc();
        if doc.schema != SCHEMA_VERSION {
            bail!(
                self,
                UnsupportedSchema,
                Some("schema:"),
                "unsupported IR schema version {}, this build understands {SCHEMA_VERSION}",
                doc.schema
            );
        }
        let parts: Vec<&str> = doc.cop.split('/').collect();
        if parts.len() != 2 || !parts.iter().all(|p| is_camel_case(p)) {
            bail!(
                self,
                CopName,
                Some("cop:"),
                "invalid cop name `{}`, expected `Dept/Name`",
                doc.cop
            );
        }
        if self.mode == LoadMode::User && BUILTIN_DEPARTMENTS.contains(&parts[0]) {
            bail!(
                self,
                Department,
                Some("cop:"),
                "department `{}` is reserved for built-in cops; use a novel department (conventionally `Custom/`)",
                parts[0]
            );
        }
        if doc.hooks.is_empty() {
            bail!(self, Hooks, Some("hooks:"), "at least one hook is required");
        }
        let names: BTreeSet<&String> = doc.matchers.keys().collect();
        if let Some(dup) = doc.predicates.keys().find(|k| names.contains(k)) {
            bail!(
                self,
                Duplicate,
                Some(dup),
                "`{dup}` is declared as both a matcher and a predicate"
            );
        }
        if !doc.restrict_on_send.is_empty()
            && !doc
                .hooks
                .iter()
                .any(|h| h.on.iter().any(|t| t == "send" || t == "csend"))
        {
            bail!(
                self,
                NodeType,
                Some("restrict_on_send:"),
                "restrict_on_send requires at least one `send` or `csend` hook"
            );
        }
        Ok(())
    }

    fn check_config(&self) -> Result<(), IrError> {
        for (key, decl) in &self.doc().config {
            let is_enum = decl.ty == ConfigType::Enum;
            if is_enum == decl.values.is_empty() {
                let msg = if is_enum {
                    format!("config key `{key}`: `type: enum` requires a non-empty `values:` list")
                } else {
                    format!("config key `{key}`: `values:` is only valid for `type: enum`")
                };
                return Err(self.err(IrErrorKind::ConfigDecl, Some(key), msg));
            }
            if !default_matches(decl.ty, &decl.default) {
                bail!(
                    self,
                    ConfigDefault,
                    Some(key),
                    "config key `{key}`: default does not match declared type `{}`",
                    decl.ty
                );
            }
            if is_enum {
                let d = decl.default.as_str().unwrap_or_default();
                if !decl.values.iter().any(|v| v == d) {
                    bail!(
                        self,
                        ConfigDefault,
                        Some(key),
                        "config key `{key}`: default `{d}` is not one of {:?}",
                        decl.values
                    );
                }
            }
        }
        Ok(())
    }

    fn check_matchers(&self, ctx: &CompileCtx<'_>) -> Result<(), IrError> {
        let doc = self.doc();
        for (name, matcher) in &doc.matchers {
            let mut seen = HashSet::new();
            for capture in &matcher.captures {
                if !is_ident(capture) || !seen.insert(capture) {
                    bail!(
                        self,
                        Capture,
                        Some(name),
                        "matcher `{name}`: invalid or duplicate capture name `{capture}`"
                    );
                }
            }
            let slots = ctx.capture_count(name).unwrap_or_default();
            if matcher.captures.len() > slots {
                bail!(
                    self,
                    Capture,
                    Some(name),
                    "matcher `{name}`: {} capture names declared but the pattern has {slots} `$` captures",
                    matcher.captures.len()
                );
            }
            for param in &matcher.params {
                if !doc.config.contains_key(param) && !doc.constants.contains_key(param) {
                    bail!(
                        self,
                        Param,
                        Some(name),
                        "matcher `{name}`: param `{param}` is neither a declared config key nor a constant table"
                    );
                }
            }
        }
        Ok(())
    }

    fn check_hooks(&self, ctx: &mut CompileCtx<'_>) -> Result<Vec<CompiledHook>, IrError> {
        let doc = self.doc();
        let types = node_type_names();
        let wants_correction = doc.autocorrect != AutocorrectMode::None;
        let mut has_correction = false;
        let mut compiled = Vec::with_capacity(doc.hooks.len());

        for hook in &doc.hooks {
            if hook.on.is_empty() {
                bail!(
                    self,
                    NodeType,
                    Some("on:"),
                    "hook `on:` must list at least one node type"
                );
            }
            for ty in &hook.on {
                if !types.contains(ty.as_str()) {
                    bail!(
                        self,
                        NodeType,
                        Some(ty),
                        "hook `on:` references unknown node type `{ty}`"
                    );
                }
            }

            // Resolve `match:` and collect the captures it makes available.
            let mut matcher_names = Vec::new();
            collect_matchers(&hook.match_spec, &mut matcher_names);
            // Capture *slots* are per matcher, in `$`-occurrence order; a
            // combinator's operands contribute their names in declaration
            // order, first occurrence winning.
            let mut ordered: Vec<String> = Vec::new();
            for name in &matcher_names {
                let Some(matcher) = doc.matchers.get(name) else {
                    bail!(
                        self,
                        UnknownMatcher,
                        Some(name),
                        "hook `match:` references undeclared matcher `{name}`"
                    );
                };
                for capture in &matcher.captures {
                    if !ordered.contains(capture) {
                        ordered.push(capture.clone());
                    }
                }
            }
            let captures: HashSet<&str> = ordered.iter().map(String::as_str).collect();

            let binds: HashSet<&str> = hook.bind.0.iter().map(|(k, _)| k.as_str()).collect();
            ctx.enter_hook(ordered.clone());
            let mut hook_exprs = CompiledHook::default();
            for (name, expr) in &hook.bind.0 {
                let compiled = ctx.compile_in(expr, name).map_err(|e| self.located(e))?;
                hook_exprs.binds.push((name.clone(), compiled));
                // Declared only now, so a bind may reference earlier binds only.
                ctx.declare_bind(name);
            }
            if let Some(when) = &hook.when {
                hook_exprs.when = Some(ctx.compile_in(when, "when").map_err(|e| self.located(e))?);
            }
            compiled.push(hook_exprs);

            self.check_location(&hook.offense.location, &captures)?;
            self.check_message(&hook.offense.message, &captures, &binds)?;

            for op in &hook.offense.correct {
                has_correction = true;
                if !wants_correction {
                    bail!(
                        self,
                        Autocorrect,
                        Some("correct:"),
                        "hook declares `correct:` edits but `autocorrect: none`"
                    );
                }
                match op {
                    CorrectionOp::Replace { range, text } => {
                        self.check_location(range, &captures)?;
                        self.check_message(text, &captures, &binds)?;
                    }
                    CorrectionOp::Remove { range } => {
                        self.check_location(range, &captures)?;
                    }
                    CorrectionOp::InsertBefore { at, text }
                    | CorrectionOp::InsertAfter { at, text } => {
                        self.check_anchor(at, &captures)?;
                        self.check_message(text, &captures, &binds)?;
                    }
                }
            }
        }

        if wants_correction && !has_correction {
            bail!(
                self,
                Autocorrect,
                Some("autocorrect:"),
                "`autocorrect: {}` declared but no hook defines `correct:` edits",
                match doc.autocorrect {
                    AutocorrectMode::Safe => "safe",
                    _ => "unsafe",
                }
            );
        }
        Ok(compiled)
    }

    /// `location:`/`range:`: an anchor shorthand denoting a range, or an explicit
    /// `{ start:, stop: }` pair of point anchors.
    fn check_location(&self, spec: &LocationSpec, captures: &HashSet<&str>) -> Result<(), IrError> {
        match spec {
            LocationSpec::Shorthand(path) => {
                let segments = self.anchor_segments(path, captures)?;
                if segments.last().is_some_and(|s| EDGES.contains(s)) {
                    bail!(
                        self,
                        Location,
                        Some(path),
                        "`{path}` denotes a point, not a range; use `{{ start:, stop: }}`"
                    );
                }
                Ok(())
            }
            LocationSpec::Range(range) => {
                self.check_anchor(&range.start, captures)?;
                self.check_anchor(&range.stop, captures)
            }
        }
    }

    /// A point anchor: `<target>[.<accessor|part>]*.<edge>`.
    fn check_anchor(&self, path: &str, captures: &HashSet<&str>) -> Result<(), IrError> {
        let segments = self.anchor_segments(path, captures)?;
        if !segments.last().is_some_and(|s| EDGES.contains(s)) {
            bail!(
                self,
                Location,
                Some(path),
                "anchor `{path}` must end in `.start` or `.stop`"
            );
        }
        Ok(())
    }

    fn anchor_segments<'p>(
        &self,
        path: &'p str,
        captures: &HashSet<&str>,
    ) -> Result<Vec<&'p str>, IrError> {
        let segments: Vec<&str> = path.split('.').collect();
        let target = segments[0];
        let known_target = target == "node"
            || target == "parent"
            || target
                .strip_prefix('$')
                .is_some_and(|c| captures.contains(c));
        if !known_target {
            bail!(
                self,
                Location,
                Some(path),
                "anchor `{path}`: unknown target `{target}` (expected `node`, `parent` or a declared `$capture`)"
            );
        }
        // `loc` is optional before a part, so an anchor may be written the way
        // upstream writes it (`node.loc.dot.start`) or the way the design's
        // examples do (`node.dot.start`); both name the same `PARTS` entry
        // that `{ attr: [node.loc.dot, line] }` names in an expression.
        let mut expect_part = false;
        for segment in &segments[1..] {
            if std::mem::take(&mut expect_part) {
                if LocPart::from_name(segment).is_none() {
                    bail!(
                        self,
                        Location,
                        Some(path),
                        "anchor `{path}`: unknown `loc` part `{segment}`, expected one of {:?}",
                        LocPart::NAMES
                    );
                }
                continue;
            }
            if *segment == "loc" {
                expect_part = true;
                continue;
            }
            if !ACCESSORS.contains(segment)
                && LocPart::from_name(segment).is_none()
                && !EDGES.contains(segment)
            {
                bail!(
                    self,
                    Location,
                    Some(path),
                    "anchor `{path}`: unknown component `{segment}`"
                );
            }
        }
        if expect_part {
            bail!(
                self,
                Location,
                Some(path),
                "anchor `{path}`: `loc` must be followed by a part name"
            );
        }
        Ok(segments)
    }

    /// `%{name}` placeholders must resolve; a bare `%` must be escaped as `%%`.
    fn check_message(
        &self,
        template: &str,
        captures: &HashSet<&str>,
        binds: &HashSet<&str>,
    ) -> Result<(), IrError> {
        let bytes = template.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] != b'%' {
                i += 1;
                continue;
            }
            match bytes.get(i + 1) {
                Some(b'%') => i += 2,
                Some(b'{') => {
                    let Some(end) = template[i + 2..].find('}') else {
                        bail!(
                            self,
                            Message,
                            Some(template),
                            "message template `{template}` has an unclosed `%{{`"
                        );
                    };
                    let name = &template[i + 2..i + 2 + end];
                    if !captures.contains(name)
                        && !binds.contains(name)
                        && !self.doc().config.contains_key(name)
                    {
                        bail!(
                            self,
                            Message,
                            Some(template),
                            "message template references `%{{{name}}}`, which is not a capture, bind or config key"
                        );
                    }
                    i += end + 3;
                }
                _ => {
                    bail!(
                        self,
                        Message,
                        Some(template),
                        "message template `{template}` contains a bare `%`; write `%%`"
                    );
                }
            }
        }
        Ok(())
    }
}

fn collect_matchers(spec: &MatchSpec, out: &mut Vec<String>) {
    match spec {
        MatchSpec::Named(name) => out.push(name.clone()),
        MatchSpec::Combined(combinator) => {
            for inner in combinator.operands() {
                collect_matchers(inner, out);
            }
        }
    }
}

fn default_matches(ty: ConfigType, value: &serde_yml::Value) -> bool {
    match ty {
        ConfigType::Enum | ConfigType::String => value.is_string(),
        ConfigType::StringArray => value
            .as_sequence()
            .is_some_and(|s| s.iter().all(serde_yml::Value::is_string)),
        ConfigType::Int => value.is_i64() || value.is_u64(),
        ConfigType::Float => value.is_f64() || value.is_i64() || value.is_u64(),
        ConfigType::Bool => value.is_bool(),
        ConfigType::StringMap => value.is_mapping(),
    }
}

fn is_camel_case(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && s.chars().all(|c| c.is_ascii_alphanumeric())
}

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Best-effort 1-based line number of the first line mentioning `needle`.
fn locate(source: &str, needle: &str) -> Option<usize> {
    let probe = needle.lines().next()?.trim();
    if probe.is_empty() {
        return None;
    }
    source
        .lines()
        .position(|line| line.contains(probe))
        .map(|i| i + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
schema: 1
cop: "Custom/NoBaseTransaction"
matchers:
  base_transaction:
    pattern: "(send (const nil? :Base) :transaction)"
hooks:
  - on: [send]
    match: base_transaction
    offense:
      location: node
      message: "Do not call `Base.transaction`."
"#;

    #[test]
    fn loads_a_minimal_user_cop() {
        let cop = load_str_with(MINIMAL, "memory.cop.yml", LoadMode::User).unwrap();
        assert_eq!(cop.name(), "Custom/NoBaseTransaction");
        assert_eq!(cop.department(), "Custom");
        assert_eq!(cop.base_name(), "NoBaseTransaction");
    }

    #[test]
    fn builtin_department_is_reserved_for_user_cops_only() {
        let source = MINIMAL.replace("Custom/NoBaseTransaction", "Style/NoBaseTransaction");
        // Shipped IR translations of upstream cops may use built-in departments.
        assert!(load_str(&source, "memory.cop.yml").is_ok());
        let err = load_str_with(&source, "memory.cop.yml", LoadMode::User).unwrap_err();
        assert_eq!(err.kind, IrErrorKind::Department);
    }

    /// `matchers:` compile in two passes, so a `#helper` may name a matcher
    /// declared later (`a?` sorts before `b?` here, and `?` sorts before `_`,
    /// which is what defeated the single pass), a mutually recursive partner,
    /// or itself.
    #[test]
    fn matchers_resolve_forward_and_mutual_references() {
        const MUTUAL: &str = r#"
schema: 1
cop: "Custom/Mutual"
matchers:
  a?:
    pattern: "{(send nil? :stop) (send #b? _)}"
  b?:
    pattern: "{(send nil? :stop) (send #a? _)}"
  self?:
    pattern: "{(send nil? :stop) (send #self? _)}"
hooks:
  - on: [send]
    match: "a?"
    when: { matches: [node, "self?"] }
    offense:
      location: node
      message: "x"
"#;
        let cop = load_str_with(MUTUAL, "memory.cop.yml", LoadMode::User).unwrap();
        assert_eq!(cop.compiled.matcher_names, ["a?", "b?", "self?"]);
    }

    /// The DAG rule is kept for `predicates:`, whose bodies have no descending
    /// step to make a recursion well-founded.
    #[test]
    fn predicate_cycles_are_still_rejected() {
        const CYCLE: &str = r#"
schema: 1
cop: "Custom/Cycle"
matchers:
  probe:
    pattern: "(send nil? :foo)"
predicates:
  one:
    expr: { matches: [node, "two"] }
  two:
    expr: { matches: [node, "one"] }
hooks:
  - on: [send]
    match: probe
    when: { matches: [node, "one"] }
    offense:
      location: node
      message: "x"
"#;
        let err = load_str_with(CYCLE, "memory.cop.yml", LoadMode::User).unwrap_err();
        assert_eq!(err.kind, IrErrorKind::Cycle);
    }

    #[test]
    fn error_renders_as_origin_line_column_message() {
        let err = IrError {
            origin: "a/b.cop.yml".to_string(),
            line: Some(7),
            column: Some(3),
            kind: IrErrorKind::Yaml,
            message: "boom".to_string(),
        };
        assert_eq!(err.to_string(), "a/b.cop.yml:7:3: boom");
        let err = IrError {
            column: None,
            ..err
        };
        assert_eq!(err.to_string(), "a/b.cop.yml:7: boom");
        let err = IrError { line: None, ..err };
        assert_eq!(err.to_string(), "a/b.cop.yml: boom");
    }
}
