//! Capture storage for NodePattern `$` captures.
//!
//! RuboCop's compiler turns every `$` into a numbered slot (`captures[0]`,
//! `captures[1]`, …) allocated in source order as the pattern AST is walked
//! pre-order (`vendor/rubocop-ast/lib/rubocop/ast/node_pattern/compiler.rb:71-100`).
//! `{}` union branches all start from the same slot base and must declare the
//! same number of captures, otherwise the pattern is rejected
//! (`compiler.rb:82-95`, `Invalid: each branch must have same number of captures`).
//!
//! The slot numbering lives in the parser (`PatternNode::Capture { slot, .. }`);
//! this module owns the runtime side: [`MatchEnv`] is the mutable scratch space
//! used while matching, and [`Captures`] is the immutable result handed back on
//! a successful match.
//!
//! ## Backtracking discipline
//!
//! Unlike RuboCop — which emits straight-line `captures[i] = x` assignments and
//! tolerates stale slots left behind by a failed union branch, because every
//! branch of a union writes the same slots — the interpreter backtracks over
//! variable-length terms (`...`), so a capture written during an attempt that is
//! later abandoned must be undone. [`MatchEnv`] therefore journals every write:
//! callers take a [`Mark`] before a speculative attempt and [`MatchEnv::rollback`]
//! to it when the attempt fails.

use std::ops::Index;

use super::predicates::Arg;
use super::resolve::{NO_RESOLVER, Params, Resolver};

// A duplicated `Node` handle must not imply duplicated ownership; this fails the
// build if upstream ever gives the handle drop glue.
const _: () = assert!(!std::mem::needs_drop::<ruby_prism::Node<'static>>());

/// Duplicate a Prism node handle.
///
/// `ruby_prism::Node<'pr>` is a non-owning `(parser pointer, node pointer,
/// PhantomData)` handle — every typed node exposes
/// `as_node(&self) -> Node<'pr>` which performs exactly this field copy — but
/// the generic enum implements neither `Copy` nor `Clone` and its fields are
/// private, so duplicating a `&Node<'pr>` in safe code would need a 160-arm
/// match over `as_*_node()`.
pub(crate) fn dup_node<'pr>(node: &ruby_prism::Node<'pr>) -> ruby_prism::Node<'pr> {
    // SAFETY: `Node` is a plain handle into the parser's arena with no
    // destructor (asserted above), so a bitwise copy aliases the same
    // arena-allocated node for the same lifetime `'pr` and owns nothing.
    unsafe { std::ptr::read(node) }
}

/// A value bound by a `$` capture.
///
/// NodePattern children are heterogeneous, so captures are too: `$(send ...)`
/// binds a node, `$:foo` / `$_` in a name position binds the raw name bytes,
/// `$nil?` binds the absent child, and `$...` binds the whole run of children
/// the rest term consumed (RuboCop yields an `Array` there —
/// `compiler/sequence_subcompiler.rb:155-160`).
#[derive(Debug)]
pub enum CaptureValue<'pr> {
    /// A captured AST node.
    Node(ruby_prism::Node<'pr>),
    /// A captured name or value byte slice (method name, symbol value, …).
    Name(&'pr [u8]),
    /// A captured absent child (`$nil?` and friends).
    Absent,
    /// A captured variable-length run of children (`$...`).
    List(Vec<CaptureValue<'pr>>),
}

impl<'pr> CaptureValue<'pr> {
    /// The captured node, if this capture bound a node.
    #[must_use]
    pub fn as_node(&self) -> Option<&ruby_prism::Node<'pr>> {
        match self {
            CaptureValue::Node(node) => Some(node),
            _ => None,
        }
    }

    /// The captured name bytes, if this capture bound a name/value.
    #[must_use]
    pub fn as_name(&self) -> Option<&'pr [u8]> {
        match self {
            CaptureValue::Name(bytes) => Some(bytes),
            _ => None,
        }
    }

    /// The captured run of children, if this capture bound a `$...` rest term.
    #[must_use]
    pub fn as_list(&self) -> Option<&[CaptureValue<'pr>]> {
        match self {
            CaptureValue::List(items) => Some(items),
            _ => None,
        }
    }

    /// Whether this capture bound an absent child.
    #[must_use]
    pub fn is_absent(&self) -> bool {
        matches!(self, CaptureValue::Absent)
    }
}

/// The values bound by a successful match, indexed by capture slot.
///
/// Slot order is `$`-occurrence order, matching the order RuboCop yields its
/// block parameters.
#[derive(Debug, Default)]
pub struct Captures<'pr> {
    slots: Vec<Option<CaptureValue<'pr>>>,
}

impl<'pr> Captures<'pr> {
    /// Number of capture slots in the pattern (filled or not).
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether the pattern had no captures at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// The value bound to `index`, or `None` if the slot is out of range or was
    /// never written (possible when the capture sits under a stubbed term).
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&CaptureValue<'pr>> {
        self.slots.get(index)?.as_ref()
    }

    /// Shorthand for `get(index).and_then(CaptureValue::as_node)`.
    #[must_use]
    pub fn node(&self, index: usize) -> Option<&ruby_prism::Node<'pr>> {
        self.get(index)?.as_node()
    }

    /// Shorthand for `get(index).and_then(CaptureValue::as_name)`.
    #[must_use]
    pub fn name(&self, index: usize) -> Option<&'pr [u8]> {
        self.get(index)?.as_name()
    }

    /// Iterate over every slot in order.
    pub fn iter(&self) -> impl Iterator<Item = Option<&CaptureValue<'pr>>> {
        self.slots.iter().map(Option::as_ref)
    }
}

impl<'pr> Index<usize> for Captures<'pr> {
    type Output = CaptureValue<'pr>;

    /// # Panics
    ///
    /// Panics if the slot is out of range or unfilled; use
    /// [`Captures::get`] when a slot may be unwritten.
    fn index(&self, index: usize) -> &Self::Output {
        self.get(index)
            .unwrap_or_else(|| panic!("capture slot {index} is not bound"))
    }
}

/// A point in the capture journal that a failed attempt can rewind to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark(usize);

/// Mutable capture state threaded through a match attempt, plus the two
/// read-only inputs a term may need to resolve itself: the `%param` bindings
/// and the owner's resolver (`resolve.rs`).
pub struct MatchEnv<'pr, 'r> {
    slots: Vec<Option<CaptureValue<'pr>>>,
    /// Journal of `(slot, previous value)` pairs, newest last.
    trail: Vec<(usize, Option<CaptureValue<'pr>>)>,
    params: &'r Params,
    resolver: &'r dyn Resolver,
}

impl std::fmt::Debug for MatchEnv<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MatchEnv")
            .field("slots", &self.slots)
            .field("trail", &self.trail)
            .field("params", &self.params)
            .finish_non_exhaustive()
    }
}

/// The parameter set a `MatchEnv` with no bindings points at.
static NO_PARAMS: std::sync::LazyLock<Params> = std::sync::LazyLock::new(Params::new);

impl<'pr, 'r> MatchEnv<'pr, 'r> {
    /// Create an environment with `capture_count` empty slots, no `%param`
    /// bindings and the empty resolver.
    #[must_use]
    pub fn new(capture_count: usize) -> Self {
        Self::with_inputs(capture_count, &NO_PARAMS, &NO_RESOLVER)
    }

    /// Create an environment carrying `params` and `resolver`.
    #[must_use]
    pub fn with_inputs(
        capture_count: usize,
        params: &'r Params,
        resolver: &'r dyn Resolver,
    ) -> Self {
        let mut slots = Vec::new();
        slots.resize_with(capture_count, || None);
        Self {
            slots,
            trail: Vec::new(),
            params,
            resolver,
        }
    }

    /// The `%param` bindings this match was invoked with.
    #[must_use]
    pub fn params(&self) -> &'r Params {
        self.params
    }

    /// The owner's resolver.
    #[must_use]
    pub fn resolver(&self) -> &'r dyn Resolver {
        self.resolver
    }

    /// The value of a `%param`, or [`Arg::Unresolved`] when it is unbound.
    #[must_use]
    pub fn positional_param(&self, number: usize) -> Arg {
        self.params.get(number).cloned().unwrap_or(Arg::Unresolved)
    }

    /// The value of a `%name`, or [`Arg::Unresolved`] when it is unbound.
    #[must_use]
    pub fn named_param(&self, name: &str) -> Arg {
        self.params
            .get_named(name)
            .cloned()
            .unwrap_or(Arg::Unresolved)
    }

    /// Record the current journal position, to be passed to [`Self::rollback`].
    #[must_use]
    pub fn mark(&self) -> Mark {
        Mark(self.trail.len())
    }

    /// Undo every capture written since `mark`.
    pub fn rollback(&mut self, mark: Mark) {
        while self.trail.len() > mark.0 {
            let (slot, previous) = self.trail.pop().expect("trail is non-empty above the mark");
            self.slots[slot] = previous;
        }
    }

    /// Bind `slot`, journaling the previous value so the write can be undone.
    ///
    /// Out-of-range slots are ignored; that can only happen if a pattern is
    /// matched against a capture count it was not compiled with.
    pub fn set(&mut self, slot: usize, value: CaptureValue<'pr>) {
        let Some(cell) = self.slots.get_mut(slot) else {
            return;
        };
        let previous = cell.replace(value);
        self.trail.push((slot, previous));
    }

    /// Consume the environment, yielding the bound values.
    #[must_use]
    pub fn into_captures(self) -> Captures<'pr> {
        Captures { slots: self.slots }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rollback_restores_previous_value() {
        let mut env: MatchEnv<'_, '_> = MatchEnv::new(2);
        env.set(0, CaptureValue::Name(b"first"));
        let mark = env.mark();
        env.set(0, CaptureValue::Name(b"second"));
        env.set(1, CaptureValue::Absent);
        env.rollback(mark);

        let captures = env.into_captures();
        assert_eq!(captures.name(0), Some(&b"first"[..]));
        assert!(captures.get(1).is_none());
        assert_eq!(captures.len(), 2);
    }

    #[test]
    fn test_rollback_to_start_clears_everything() {
        let mut env: MatchEnv<'_, '_> = MatchEnv::new(1);
        let mark = env.mark();
        env.set(0, CaptureValue::Absent);
        env.rollback(mark);
        assert!(env.into_captures().get(0).is_none());
    }

    #[test]
    fn test_out_of_range_set_is_ignored() {
        let mut env: MatchEnv<'_, '_> = MatchEnv::new(1);
        env.set(5, CaptureValue::Absent);
        assert_eq!(env.into_captures().len(), 1);
    }

    #[test]
    #[should_panic(expected = "capture slot 0 is not bound")]
    fn test_index_panics_on_unbound_slot() {
        let captures: Captures<'_> = MatchEnv::new(1).into_captures();
        let _ = &captures[0];
    }
}
