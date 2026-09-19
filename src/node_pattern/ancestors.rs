//! The enclosing-node chain, normalized to Parser-gem ancestry.
//!
//! `^`, `root?`, `chained?`, `argument?`, `macro?` and `value_used?` all ask
//! "what is this node's parent?". Prism nodes have no parent pointer, so the
//! chain is carried in from outside: `BatchedCopWalker` maintains it for cops
//! that opt in (`Cop::wants_ancestors`), and [`super::captures::MatchEnv`]
//! extends it as the matcher descends into a sequence.
//!
//! ## Why the raw chain is not the answer
//!
//! The chain the walker keeps is a path through the *Prism* tree, and Prism
//! materializes nodes the Parser gem does not. RuboCop's patterns are written
//! against Parser ancestry, so the chain is filtered before it is counted:
//!
//! | Prism node | Parser | Rule here |
//! |---|---|---|
//! | `ProgramNode`, `ArgumentsNode`, `ElseNode`, `EnsureNode`, … | no node | dropped — [`super::interpreter::parser_type_for_node`] already returns `None` |
//! | `BlockNode` | part of `(block send args body)` | dropped; the enclosing `CallNode` *is* the Parser `block` node |
//! | `StatementsNode` | `begin`, but only when it holds ≠ 1 statement | dropped when it holds exactly one statement, or when its own parent is a `ParenthesesNode` / `BeginNode` / `EmbeddedStatementsNode` — those already spell the `begin`/`kwbegin` |
//!
//! ## Two levels Prism cannot supply
//!
//! Both divergences below are properties of the mapping, not of any one
//! pattern, which is why they are recorded here rather than in a cop. Both
//! affect `^` *only*: the corresponding **child** slots are exact, because
//! `interpreter::get_children` can synthesize a level that the chain cannot
//! (see [`super::interpreter::begin_clause_child`]).
//!
//! **1. The `send` inside a `block`.** A `CallNode` carrying a literal block is
//! **one** chain entry answering both `send` and `block`, where Parser has two
//! nested nodes. For a node in the block's *body* this is already right — `^`
//! is the block and `^^` is the block's parent, as upstream. For a node in the
//! *send* half (an argument of `foo(x) { }`) `^` is right and `^^` lands one
//! level high, because upstream `^^` there is the `block` that this same entry
//! is already standing for.
//!
//! **2. The `rescue` above a `resbody`.** Parser nests
//! `kwbegin → rescue → resbody`; Prism has `BeginNode → RescueNode`, with the
//! `rescue` level existing only as a field. `^^resbody`
//! (`Style/RedundantParentheses#rescue?`) therefore sees `kwbegin` where
//! upstream sees `rescue`. A keyword-less `BeginNode` *is* the `rescue` node
//! here ([`super::interpreter::begin_parser_type`]), so the implicit form —
//! `def m; a; rescue; b; end` — already gives the upstream answer; only the
//! explicit `begin … end` form is short a level.
//!
//! ### What a fix would need
//!
//! Neither is a missing accessor: in both cases the Parser level has no Prism
//! node to point at, and [`visible_index`] / [`nth_ancestor`] return an index
//! into the real chain and a `&Node`. Closing them means an
//! `Ancestor::{Node(&Node), Virtual(&'static str)}` return type and teaching
//! every consumer about the type-only case: `^` in
//! [`super::interpreter::matches_ascend`] (which today hands the ancestor to
//! `matches_node`, so `^(rescue $_ ...)` could not read children off a virtual
//! level at all), plus `PredCtx::nth_ancestor` and the five predicates built on
//! it (`root?`, `argument?`, `macro?`, `chained?`, `value_used?`). That is a
//! ~250-line change to semantics PR #11 has just settled, for two vendored
//! patterns, so it is deliberately deferred.

use ruby_prism::Node;

use super::interpreter::parser_type_for_node;

/// Whether `chain[index]` is a node the Parser gem would also have built.
fn is_parser_visible(chain: &[Node<'_>], index: usize) -> bool {
    let node = &chain[index];

    // A `BlockNode` is subsumed by the `CallNode` above it, which answers to
    // `block`/`numblock`/`itblock` through `block_type_of`.
    if node.as_block_node().is_some() {
        return false;
    }

    if let Some(statements) = node.as_statements_node() {
        // Parser only builds a `begin` for a *list*; a single statement is the
        // body itself.
        if statements.body().iter().count() == 1 {
            return false;
        }
        // `(1; 2)`, `begin 1; 2 end` and `"#{1; 2}"` are one Parser node, and
        // the outer Prism node is the one that carries it.
        if let Some(outer) = index.checked_sub(1).map(|i| &chain[i]) {
            if outer.as_parentheses_node().is_some()
                || outer.as_begin_node().is_some()
                || outer.as_embedded_statements_node().is_some()
            {
                return false;
            }
        }
        return true;
    }

    parser_type_for_node(node).is_some()
}

/// Index in `chain` of the `n`-th Parser-visible ancestor, `n == 0` being the
/// parent.
pub(crate) fn visible_index(chain: &[Node<'_>], n: usize) -> Option<usize> {
    let mut remaining = n;
    for index in (0..chain.len()).rev() {
        if !is_parser_visible(chain, index) {
            continue;
        }
        if remaining == 0 {
            return Some(index);
        }
        remaining -= 1;
    }
    None
}

/// The `n`-th Parser-visible ancestor, `n == 0` being the parent.
pub(crate) fn nth_ancestor<'a, 'pr>(chain: &'a [Node<'pr>], n: usize) -> Option<&'a Node<'pr>> {
    visible_index(chain, n).map(|index| &chain[index])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_pattern::interpreter::test_support::chain_at;

    /// The Parser-visible ancestor type names of the first node whose source
    /// text is `needle`, innermost first.
    fn ancestry(source: &str, needle: &str) -> Vec<String> {
        chain_at(source, needle, |chain| {
            let mut out = Vec::new();
            let mut n = 0;
            while let Some(node) = nth_ancestor(chain, n) {
                out.push(parser_type_for_node(node).unwrap_or("?").to_string());
                n += 1;
            }
            out
        })
    }

    #[test]
    fn a_lone_statement_list_is_not_a_begin() {
        assert_eq!(ancestry("def foo; bar; end\n", "bar"), vec!["def"]);
    }

    #[test]
    fn a_multi_statement_list_is_a_begin() {
        assert_eq!(
            ancestry("def foo; bar; baz; end\n", "bar"),
            vec!["begin", "def"],
        );
    }

    #[test]
    fn parentheses_are_one_begin_not_two() {
        assert_eq!(ancestry("x = (bar)\n", "bar"), vec!["begin", "lvasgn"]);
        assert_eq!(ancestry("x = (bar; baz)\n", "bar"), vec!["begin", "lvasgn"]);
    }

    #[test]
    fn a_block_is_one_level_carried_by_its_call() {
        // Parser: `(block (send nil :foo) (args) (send nil :bar))`.
        assert_eq!(ancestry("foo { bar }\n", "bar"), vec!["send"]);
    }

    #[test]
    fn interpolation_is_a_begin_inside_the_dstr() {
        assert_eq!(ancestry("\"#{bar}\"\n", "bar"), vec!["begin", "dstr"]);
    }

    #[test]
    fn the_root_expression_has_no_ancestors() {
        assert!(ancestry("bar\n", "bar").is_empty());
    }
}
