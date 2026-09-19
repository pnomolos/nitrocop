//! Resolution of the parts of a pattern that the pattern text alone cannot
//! answer: `#helper` calls the owner defines, and `%param` bindings.
//!
//! Upstream, a compiled NodePattern is a method body on the object that
//! declared it, so `#foo` is `self.foo(node, …)` and `%1` is the method's
//! first argument (`compiler.rb:27-35`). Neither is knowable from the pattern
//! string. This module is the hook the future IR cop plugs into: it supplies
//! the cop's own named matchers and its constants, and carries the parameter
//! values a matcher was invoked with.
//!
//! The builtin half — every predicate `RuboCop::AST::Node` itself defines —
//! lives in [`super::predicates`] and needs no hook.

use super::interpreter::CompiledPattern;
use super::predicates::Arg;

/// Values bound to a pattern's `%param` references.
///
/// `%1` is the first positional parameter (`compiler.rb:27-30`, `param#{n}`);
/// `%` with no digits means `%1` (`lexer.rex`). `%0` is the node the matcher
/// was called on, which this type does not model — see [`Params::positional`].
#[derive(Debug, Default, Clone)]
pub struct Params {
    /// Index 0 holds `%1`, index 1 `%2`, and so on.
    positional: Vec<Arg>,
    /// `%name` bindings.
    named: Vec<(String, Arg)>,
}

impl Params {
    /// An empty binding set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Build from positional values, `%1` first.
    #[must_use]
    pub fn positional(values: Vec<Arg>) -> Self {
        Self {
            positional: values,
            named: Vec::new(),
        }
    }

    /// Bind `%name`.
    #[must_use]
    pub fn with_named(mut self, name: impl Into<String>, value: Arg) -> Self {
        self.named.push((name.into(), value));
        self
    }

    /// Bind another positional parameter, after the ones already set.
    #[must_use]
    pub fn with_positional(mut self, value: Arg) -> Self {
        self.positional.push(value);
        self
    }

    /// The value of `%n`.
    ///
    /// `%0` is the node the matcher was called on rather than a value, so it
    /// never resolves here; predicates needing it (`equal?`) are not
    /// registered.
    #[must_use]
    pub fn get(&self, number: usize) -> Option<&Arg> {
        self.positional.get(number.checked_sub(1)?)
    }

    /// The value of `%name`.
    #[must_use]
    pub fn get_named(&self, name: &str) -> Option<&Arg> {
        self.named
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
    }

    /// Whether anything is bound.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.positional.is_empty() && self.named.is_empty()
    }
}

/// Supplies what is owner-specific about a pattern.
///
/// Both methods default to "not mine", which is the empty resolver used when a
/// pattern is compiled on its own: every `#helper` then has to be a builtin or
/// the compile fails.
pub trait Resolver {
    /// The named matcher `#name` refers to.
    ///
    /// Covers cop-local `def_node_matcher`s and the const-qualified form
    /// (`#Examples.all`, rubocop-rspec's `Language` modules), which arrives
    /// with the `Const.` prefix intact.
    fn matcher(&self, name: &str) -> Option<&CompiledPattern> {
        let _ = name;
        None
    }

    /// The value of a `%Const` (or bare `Const`) reference.
    fn constant(&self, name: &str) -> Option<&Arg> {
        let _ = name;
        None
    }
}

/// A resolver that knows nothing, so only builtins resolve.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoResolver;

impl Resolver for NoResolver {}

/// The empty resolver, as a `&dyn` for the default `MatchEnv`.
pub(crate) static NO_RESOLVER: NoResolver = NoResolver;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positional_params_are_one_based() {
        let params = Params::positional(vec![Arg::Symbol("a".into()), Arg::Symbol("b".into())]);
        assert_eq!(params.get(1), Some(&Arg::Symbol("a".into())));
        assert_eq!(params.get(2), Some(&Arg::Symbol("b".into())));
        assert_eq!(params.get(3), None);
        // `%0` is the node itself upstream, never a value.
        assert_eq!(params.get(0), None);
    }

    #[test]
    fn named_params_round_trip() {
        let params = Params::new().with_named("method_name", Arg::Symbol("foo".into()));
        assert_eq!(
            params.get_named("method_name"),
            Some(&Arg::Symbol("foo".into()))
        );
        assert_eq!(params.get_named("other"), None);
        assert!(!params.is_empty());
        assert!(Params::new().is_empty());
    }

    #[test]
    fn the_empty_resolver_resolves_nothing() {
        assert!(NoResolver.matcher("array_receiver?").is_none());
        assert!(NoResolver.constant("RESTRICT_ON_SEND").is_none());
    }
}
