use ruby_prism::Visit;

use crate::cop::shared::node_type::{NODE_TYPE_COUNT, node_type_tag};
use crate::cop::{Cop, CopConfig};
use crate::diagnostic::Diagnostic;
use crate::parse::source::SourceFile;

pub struct CopWalker<'a, 'pr> {
    pub cop: &'a dyn Cop,
    pub source: &'a SourceFile,
    pub parse_result: &'a ruby_prism::ParseResult<'pr>,
    pub cop_config: &'a CopConfig,
    pub diagnostics: Vec<Diagnostic>,
    pub corrections: Option<Vec<crate::correction::Correction>>,
}

impl<'pr> Visit<'pr> for CopWalker<'_, 'pr> {
    fn visit_branch_node_enter(&mut self, node: ruby_prism::Node<'pr>) {
        self.cop.check_node(
            self.source,
            &node,
            self.parse_result,
            self.cop_config,
            &mut self.diagnostics,
            self.corrections.as_mut(),
        );
    }

    fn visit_leaf_node_enter(&mut self, node: ruby_prism::Node<'pr>) {
        self.cop.check_node(
            self.source,
            &node,
            self.parse_result,
            self.cop_config,
            &mut self.diagnostics,
            self.corrections.as_mut(),
        );
    }
}

/// One cop in a dispatch bucket, with the two flags the walker needs to know
/// without a virtual call per node.
struct Entry<'a> {
    cop: &'a dyn Cop,
    config: &'a CopConfig,
    /// `cop.wants_ancestors()`, read once at construction.
    wants_ancestors: bool,
}

/// Walks the AST once and dispatches each node only to cops that declared
/// interest in that node type. Cops that haven't declared interest (empty
/// `interested_node_types()`) are called for every node (universal dispatch).
pub struct BatchedCopWalker<'a, 'pr> {
    /// Cops that haven't declared node type interest — called for every node.
    universal_cops: Vec<Entry<'a>>,
    /// Dispatch table: indexed by node type tag, each entry = cops for that type.
    dispatch_table: [Vec<Entry<'a>>; NODE_TYPE_COUNT],
    pub source: &'a SourceFile,
    pub parse_result: &'a ruby_prism::ParseResult<'pr>,
    pub diagnostics: Vec<Diagnostic>,
    corrections: Option<Vec<crate::correction::Correction>>,
    /// Enclosing branch nodes, outermost first. Maintained only while
    /// `track_ancestors` is set, i.e. only when some active cop asked for it.
    ancestors: Vec<ruby_prism::Node<'pr>>,
    /// Whether any active cop returned `wants_ancestors()`.
    track_ancestors: bool,
}

impl<'a, 'pr> BatchedCopWalker<'a, 'pr> {
    pub fn new(
        cops: Vec<(&'a dyn Cop, &'a CopConfig)>,
        source: &'a SourceFile,
        parse_result: &'a ruby_prism::ParseResult<'pr>,
    ) -> Self {
        let mut universal = Vec::new();
        let mut table: [Vec<Entry<'a>>; NODE_TYPE_COUNT] = std::array::from_fn(|_| Vec::new());
        let mut track_ancestors = false;

        for (cop, config) in cops {
            let wants_ancestors = cop.wants_ancestors();
            track_ancestors |= wants_ancestors;
            let entry = || Entry {
                cop,
                config,
                wants_ancestors,
            };
            let types = cop.interested_node_types();
            if types.is_empty() {
                universal.push(entry());
            } else {
                for &t in types {
                    table[t as usize].push(entry());
                }
            }
        }

        Self {
            universal_cops: universal,
            dispatch_table: table,
            source,
            parse_result,
            diagnostics: Vec::new(),
            corrections: None,
            ancestors: Vec::new(),
            track_ancestors,
        }
    }

    /// Enable corrections collection for this walker.
    pub fn with_corrections(mut self) -> Self {
        self.corrections = Some(Vec::new());
        self
    }

    /// Consume the walker and return (diagnostics, corrections).
    pub fn into_results(self) -> (Vec<Diagnostic>, Option<Vec<crate::correction::Correction>>) {
        (self.diagnostics, self.corrections)
    }

    #[inline]
    fn dispatch(&mut self, node: &ruby_prism::Node<'pr>) {
        let tag = node_type_tag(node) as usize;

        for entry in &self.universal_cops {
            Self::call(
                entry,
                self.source,
                node,
                &self.ancestors,
                self.parse_result,
                &mut self.diagnostics,
                self.corrections.as_mut(),
            );
        }

        if let Some(cops) = self.dispatch_table.get(tag) {
            for entry in cops {
                Self::call(
                    entry,
                    self.source,
                    node,
                    &self.ancestors,
                    self.parse_result,
                    &mut self.diagnostics,
                    self.corrections.as_mut(),
                );
            }
        }
    }

    /// One cop call. `wants_ancestors` is a per-entry bool read at
    /// construction, so a cop that did not ask for the stack keeps the exact
    /// call it had before this existed.
    #[allow(clippy::too_many_arguments)]
    #[inline]
    fn call(
        entry: &Entry<'a>,
        source: &'a SourceFile,
        node: &ruby_prism::Node<'pr>,
        ancestors: &[ruby_prism::Node<'pr>],
        parse_result: &'a ruby_prism::ParseResult<'pr>,
        diagnostics: &mut Vec<Diagnostic>,
        corrections: Option<&mut Vec<crate::correction::Correction>>,
    ) {
        if entry.wants_ancestors {
            entry.cop.check_node_with_ancestors(
                source,
                node,
                ancestors,
                parse_result,
                entry.config,
                diagnostics,
                corrections,
            );
        } else {
            entry.cop.check_node(
                source,
                node,
                parse_result,
                entry.config,
                diagnostics,
                corrections,
            );
        }
    }
}

impl<'pr> Visit<'pr> for BatchedCopWalker<'_, 'pr> {
    fn visit_branch_node_enter(&mut self, node: ruby_prism::Node<'pr>) {
        self.dispatch(&node);
        if self.track_ancestors {
            self.ancestors.push(node);
        }
    }

    fn visit_branch_node_leave(&mut self) {
        if self.track_ancestors {
            self.ancestors.pop();
        }
    }

    fn visit_leaf_node_enter(&mut self, node: ruby_prism::Node<'pr>) {
        self.dispatch(&node);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Mutex;

    use super::*;

    /// Records `(node type tag, ancestor type tags)` for every node it sees.
    #[derive(Default)]
    struct RecordingCop {
        wants: bool,
        seen: Mutex<Vec<(u8, Vec<u8>)>>,
    }

    impl Cop for RecordingCop {
        fn name(&self) -> &'static str {
            "Test/Recording"
        }

        fn wants_ancestors(&self) -> bool {
            self.wants
        }

        fn check_node(
            &self,
            _source: &SourceFile,
            node: &ruby_prism::Node<'_>,
            _parse_result: &ruby_prism::ParseResult<'_>,
            _config: &CopConfig,
            _diagnostics: &mut Vec<Diagnostic>,
            _corrections: Option<&mut Vec<crate::correction::Correction>>,
        ) {
            self.seen
                .lock()
                .unwrap()
                .push((node_type_tag(node), Vec::new()));
        }

        fn check_node_with_ancestors(
            &self,
            _source: &SourceFile,
            node: &ruby_prism::Node<'_>,
            ancestors: &[ruby_prism::Node<'_>],
            _parse_result: &ruby_prism::ParseResult<'_>,
            _config: &CopConfig,
            _diagnostics: &mut Vec<Diagnostic>,
            _corrections: Option<&mut Vec<crate::correction::Correction>>,
        ) {
            self.seen.lock().unwrap().push((
                node_type_tag(node),
                ancestors.iter().map(node_type_tag).collect(),
            ));
        }
    }

    fn run(cop: &RecordingCop, ruby: &str) -> Vec<(u8, Vec<u8>)> {
        use ruby_prism::Visit;

        let source = SourceFile::from_string(PathBuf::from("test.rb"), ruby.to_string());
        let parse_result = ruby_prism::parse(&source.content);
        let config = CopConfig::default();
        let mut walker =
            BatchedCopWalker::new(vec![(cop as &dyn Cop, &config)], &source, &parse_result);
        walker.visit(&parse_result.node());
        std::mem::take(&mut *cop.seen.lock().unwrap())
    }

    #[test]
    fn ancestor_stack_is_not_maintained_unless_a_cop_asks() {
        let cop = RecordingCop {
            wants: false,
            seen: Mutex::default(),
        };
        let seen = run(&cop, "def foo; bar; end\n");
        assert!(!seen.is_empty());
        assert!(
            seen.iter().all(|(_, ancestors)| ancestors.is_empty()),
            "check_node_with_ancestors must not be called for a cop that did not ask",
        );
    }

    #[test]
    fn ancestor_stack_is_outermost_first_and_balanced() {
        use crate::cop::shared::node_type;

        let cop = RecordingCop {
            wants: true,
            seen: Mutex::default(),
        };
        let seen = run(&cop, "def foo; bar; end\n");

        // The root node has no ancestors.
        assert_eq!(seen[0].1, Vec::<u8>::new());

        // The `bar` call sits under program → def → body statements.
        //
        // The program's own `StatementsNode` is absent: ruby-prism's
        // `visit_program_node` calls the *typed* `visit_statements_node`
        // rather than `visit`, so that one node never reaches the
        // enter/leave hooks (and is not dispatched to cops either — this is
        // pre-existing walker behavior, not something the stack introduces).
        let (_, call_ancestors) = seen
            .iter()
            .find(|(tag, _)| *tag == node_type::CALL_NODE)
            .expect("the call node is visited");
        assert_eq!(
            call_ancestors,
            &vec![
                node_type::PROGRAM_NODE,
                node_type::DEF_NODE,
                node_type::STATEMENTS_NODE,
            ],
        );

        // Every node's stack is a prefix-consistent path: depth never jumps.
        let mut previous = 0usize;
        for (_, ancestors) in &seen {
            assert!(
                ancestors.len() <= previous + 1,
                "ancestor depth jumped from {previous} to {}",
                ancestors.len(),
            );
            previous = ancestors.len();
        }
    }
}
