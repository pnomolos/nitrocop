//! Serde schema for the v1 cop IR document (`<Name>.cop.yml`).
//!
//! This mirrors §1.2 of `docs/planning/04-cop-ir-design.md` field for field. Every
//! struct is `deny_unknown_fields` so a typo in a hand-written or synthesized cop
//! definition is a load error rather than a silently ignored key — a dropped cop is
//! an invisible false negative (§4.4).
//!
//! **Experimental.** Nothing here is wired into the cop registry yet; the only
//! user-visible surface is `nitrocop --validate-ir`.

use std::collections::BTreeMap;
use std::fmt;

use serde::Deserialize;

/// The only IR document major version this build understands.
pub const SCHEMA_VERSION: u32 = 1;

/// A whole `*.cop.yml` document.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrDocument {
    /// Document major version. Must equal [`SCHEMA_VERSION`].
    pub schema: u32,
    /// Cop name, `Dept/Name`.
    pub cop: String,
    /// Informational upstream version, e.g. `"1.90"`.
    #[serde(default)]
    pub version_added: Option<String>,
    /// Long-form documentation, surfaced by `--show-cops`.
    #[serde(default)]
    pub docs: Option<String>,
    #[serde(default)]
    pub severity: Severity,
    #[serde(default)]
    pub enabled_default: EnabledDefault,
    #[serde(default)]
    pub tier: Tier,
    #[serde(default)]
    pub autocorrect: AutocorrectMode,
    /// Default `Include` globs.
    #[serde(default)]
    pub include: Vec<String>,
    /// Default `Exclude` globs.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Method-name prefilter for `send`/`csend` hooks (upstream `RESTRICT_ON_SEND`).
    #[serde(default)]
    pub restrict_on_send: Vec<String>,
    /// Declared config keys, type-checked at load.
    #[serde(default)]
    pub config: BTreeMap<String, ConfigDecl>,
    /// Frozen lookup tables (upstream's `FOO = {...}.freeze`).
    #[serde(default)]
    pub constants: BTreeMap<String, BTreeMap<String, ConstValue>>,
    /// Named NodePattern strings, copied verbatim from upstream.
    #[serde(default)]
    pub matchers: BTreeMap<String, MatcherDecl>,
    /// Named expression guards, usable as `#name` inside patterns.
    #[serde(default)]
    pub predicates: BTreeMap<String, PredicateDecl>,
    pub hooks: Vec<Hook>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    #[default]
    Convention,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    #[default]
    Preview,
    Stable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AutocorrectMode {
    #[default]
    None,
    Safe,
    Unsafe,
}

/// `enabled_default:` accepts `true`, `false` or `pending` (RuboCop's tri-state).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(try_from = "EnabledDefaultRaw")]
pub enum EnabledDefault {
    True,
    False,
    #[default]
    Pending,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum EnabledDefaultRaw {
    Bool(bool),
    Str(String),
}

impl TryFrom<EnabledDefaultRaw> for EnabledDefault {
    type Error = String;

    fn try_from(raw: EnabledDefaultRaw) -> Result<Self, String> {
        match raw {
            EnabledDefaultRaw::Bool(true) => Ok(Self::True),
            EnabledDefaultRaw::Bool(false) => Ok(Self::False),
            EnabledDefaultRaw::Str(s) => match s.as_str() {
                "true" => Ok(Self::True),
                "false" => Ok(Self::False),
                "pending" => Ok(Self::Pending),
                other => Err(format!(
                    "invalid enabled_default `{other}`, expected true, false or pending"
                )),
            },
        }
    }
}

/// Declared type of a `config:` key. Maps 1:1 onto `CopConfig::get_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigType {
    Enum,
    String,
    StringArray,
    Int,
    Float,
    Bool,
    StringMap,
}

impl fmt::Display for ConfigType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Enum => "enum",
            Self::String => "string",
            Self::StringArray => "string_array",
            Self::Int => "int",
            Self::Float => "float",
            Self::Bool => "bool",
            Self::StringMap => "string_map",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigDecl {
    #[serde(rename = "type")]
    pub ty: ConfigType,
    /// Allowed members; required for (and only for) `type: enum`.
    #[serde(default)]
    pub values: Vec<String>,
    pub default: serde_yml::Value,
}

/// A value inside a `constants:` lookup table.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum ConstValue {
    Bool(bool),
    Int(i64),
    Str(String),
    List(Vec<String>),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatcherDecl {
    /// Verbatim NodePattern source (usually a `|` block scalar).
    pub pattern: String,
    /// Names for the positional `$` captures, in occurrence order.
    #[serde(default)]
    pub captures: Vec<String>,
    /// Names for `%name` params, bound from `config:` or `constants:`.
    #[serde(default)]
    pub params: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredicateDecl {
    pub expr: ExprSyntax,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hook {
    /// Parser-gem node type names (`send`, `block`, `numblock`, …).
    pub on: Vec<String>,
    #[serde(rename = "match")]
    pub match_spec: MatchSpec,
    #[serde(default)]
    pub when: Option<ExprSyntax>,
    #[serde(default)]
    pub bind: BindList,
    pub offense: OffenseSpec,
}

/// `match:` is a matcher name or a combinator over matcher names.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum MatchSpec {
    Named(String),
    Combined(MatchCombinator),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum MatchCombinator {
    AnyOf(Vec<MatchSpec>),
    AllOf(Vec<MatchSpec>),
}

impl MatchCombinator {
    /// The combinator's operands, regardless of which combinator it is.
    pub fn operands(&self) -> &[MatchSpec] {
        match self {
            Self::AnyOf(items) | Self::AllOf(items) => items,
        }
    }
}

/// Ordered `name -> expr` bindings. Order matters: a bind may reference an
/// earlier one, so this is a `Vec`, not a map.
#[derive(Debug, Clone, Default)]
pub struct BindList(pub Vec<(String, ExprSyntax)>);

impl<'de> Deserialize<'de> for BindList {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = BindList;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a mapping of bind name to expression")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<BindList, A::Error> {
                let mut out = Vec::new();
                while let Some(entry) = map.next_entry::<String, ExprSyntax>()? {
                    out.push(entry);
                }
                Ok(BindList(out))
            }
        }

        de.deserialize_map(Visitor)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OffenseSpec {
    pub location: LocationSpec,
    /// `%{name}` template over the hook's captures, binds and config keys.
    pub message: String,
    #[serde(default)]
    pub severity: Option<Severity>,
    /// Ordered edits; an empty list means the hook does not autocorrect.
    #[serde(default)]
    pub correct: Vec<CorrectionOp>,
}

/// A source range: either an anchor shorthand (`node`, `$send.selector`) or an
/// explicit `{ start:, stop: }` anchor pair.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum LocationSpec {
    Shorthand(String),
    Range(RangeSpec),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RangeSpec {
    pub start: String,
    pub stop: String,
}

/// One autocorrect edit. `op:` is the discriminant.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum CorrectionOp {
    Replace { range: LocationSpec, text: String },
    InsertBefore { at: String, text: String },
    InsertAfter { at: String, text: String },
    Remove { range: LocationSpec },
}

/// A `when:`/`bind:`/`predicates[].expr` expression.
///
/// Schema v1 keeps expressions as a **structured-but-untyped** YAML value: the
/// typed `Expr` enum of design §2.1 lands with the compiler/evaluator PR. The
/// loader still enforces the shape statically (`load::validate_expr`):
///
/// * a scalar is a literal (`"str"`, `3`, `true`) or a path reference —
///   `node`, `parent`, `$capture[.attr]*`, `cfg.<Key>`, `bind.<Name>`,
///   `consts.<Table>`;
/// * a mapping is an operator application with exactly one key drawn from
///   [`OPERATORS`] whose value is the operand (a single expression, or a
///   sequence of them);
/// * nesting is capped at [`MAX_EXPR_DEPTH`] (design §2.1: "reject `when:`
///   depth > 6") so the language cannot drift into a programming language.
#[derive(Debug, Clone, Deserialize)]
#[serde(transparent)]
pub struct ExprSyntax(pub serde_yml::Value);

/// Maximum `when:`/`bind:` nesting depth (design §2.1).
pub const MAX_EXPR_DEPTH: usize = 6;

/// Operator keys accepted as the single key of an expression mapping.
///
/// This is the surface syntax of design §2.1's `Expr` enum; the compiler PR
/// turns these into typed nodes. Unknown keys fail closed at load.
pub const OPERATORS: &[&str] = &[
    // logic
    "all", "any", "not", // comparison
    "eq", "ne", "lt", "le", "gt", "ge", "in", // values
    "if", "lit", "lookup", "attr", // predicates / matchers
    "pred", "matches", "regex", // bounded quantifiers
    "any_of", "all_of", "none_of", "count",
];
