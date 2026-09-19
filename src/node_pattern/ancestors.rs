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
//! Two divergences are accepted and not worked around:
//!
//! - A `CallNode` carrying a literal block is **one** chain entry answering
//!   both `send` and `block`, where Parser has two nested nodes. `^^` from
//!   inside such a block therefore lands one level higher than upstream.
//! - Prism's `BeginNode` + `RescueNode` pair has no `rescue` level, so
//!   `^^resbody` (`Style/RedundantParentheses`) sees `kwbegin` where upstream
//!   sees `rescue`.
//!
//! Both are recorded here rather than in a cop because they are properties of
//! the mapping, not of any one pattern.

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
