use crate::cop::shared::node_type::{
    CALL_NODE, CLASS_NODE, CONSTANT_PATH_NODE, CONSTANT_PATH_WRITE_NODE, CONSTANT_READ_NODE,
    CONSTANT_WRITE_NODE, SELF_NODE, STATEMENTS_NODE,
};
use crate::cop::{Cop, CopConfig};
use crate::diagnostic::Diagnostic;
use crate::parse::source::SourceFile;

/// Corpus fixes:
/// - Prism represents qualified constant assignments like
///   `Win32::Service = Class.new` and `::Foo = Class.new` as
///   `ConstantPathWriteNode`, so this cop must check both constant assignment
///   node types.
/// - `Class.new(Base, &BLOCK)` stores `&BLOCK` in `call.block()` as a
///   `BlockArgumentNode`, not a real class body `BlockNode`. RuboCop still
///   flags that form, so only actual block bodies should be skipped.
/// - Under `EnforcedStyle: class_new`, Prism wraps class bodies with `rescue`
///   in a `BeginNode`, so non-`StatementsNode` bodies must be treated as
///   non-empty instead of being flagged as empty classes.
/// - The same style should still flag single-statement bodies like `self`,
///   `nil`, `true`, and `false`, because RuboCop treats those parser leaf
///   bodies as empty class definitions.
///
/// ## `AllowedParentClasses` (rubocop 1.91, vendor bump 2026-09)
///
/// `AllowedParentClasses` (default `[]`) permits both styles for specific
/// parent classes, matched against the parent-class expression's exact
/// source text (so a namespaced entry must be listed exactly as written,
/// e.g. `Alchemy::Admin::PreviewUrl`, not just its final segment).
///
/// Implementing this for `EnforcedStyle: class_new` required adding a
/// missing `return unless node.parent_class`-equivalent guard: nitrocop's
/// `check_class_new_style` previously flagged empty classes with NO
/// superclass at all (e.g. `class MyClass; end`), which contradicts both
/// RuboCop's docs ("Class definitions without a superclass... are not
/// detected") and its spec ("does not register an offense for two-line
/// class definition without inheritance"). That pre-existing fixture case
/// was factually wrong and has been moved from `offense.class_new.rb` to
/// `no_offense.class_new.rb`; a superclass is now required before this style
/// fires at all, matching upstream and making "what's the parent class to
/// check against `AllowedParentClasses`" well-defined.
pub struct EmptyClassDefinition;

impl Cop for EmptyClassDefinition {
    fn name(&self) -> &'static str {
        "Style/EmptyClassDefinition"
    }

    fn interested_node_types(&self) -> &'static [u8] {
        &[
            CALL_NODE,
            CLASS_NODE,
            CONSTANT_PATH_NODE,
            CONSTANT_PATH_WRITE_NODE,
            CONSTANT_READ_NODE,
            CONSTANT_WRITE_NODE,
            SELF_NODE,
            STATEMENTS_NODE,
        ]
    }

    fn check_node(
        &self,
        source: &SourceFile,
        node: &ruby_prism::Node<'_>,
        _parse_result: &ruby_prism::ParseResult<'_>,
        config: &CopConfig,
        diagnostics: &mut Vec<Diagnostic>,
        _corrections: Option<&mut Vec<crate::correction::Correction>>,
    ) {
        // Vendor bump (rubocop 1.84.2 -> 1.91.0): default flipped from
        // `class_definition` to `class_keyword`. Real RuboCop treats both names as the
        // same style (`%i[class_keyword class_definition].include?(style)` in
        // lib/rubocop/cop/style/empty_class_definition.rb) — `class_definition` is kept
        // only as a deprecated alias — so both match arms below share one behavior.
        let enforced_style = config.get_str("EnforcedStyle", "class_keyword");
        let allowed_parent_classes = config
            .get_string_array("AllowedParentClasses")
            .unwrap_or_default();

        match enforced_style {
            "class_definition" | "class_keyword" => diagnostics.extend(
                check_class_definition_style(self, source, node, &allowed_parent_classes),
            ),
            "class_new" => diagnostics.extend(check_class_new_style(
                self,
                source,
                node,
                &allowed_parent_classes,
            )),
            _ => {}
        }
    }
}

fn check_class_definition_style(
    cop: &EmptyClassDefinition,
    source: &SourceFile,
    node: &ruby_prism::Node<'_>,
    allowed_parent_classes: &[String],
) -> Vec<Diagnostic> {
    let value = node
        .as_constant_write_node()
        .map(|const_write| const_write.value())
        .or_else(|| {
            node.as_constant_path_write_node()
                .map(|const_path_write| const_path_write.value())
        });

    // Check for FooError = Class.new(StandardError) and Mod::Foo = Class.new(Base)
    if let Some(value) = value {
        if let Some(call) = value.as_call_node() {
            let method_name = std::str::from_utf8(call.name().as_slice()).unwrap_or("");
            if method_name == "new" {
                if let Some(receiver) = call.receiver() {
                    if is_class_const(&receiver) {
                        // Skip if it has an actual class body block (`do...end` or `{}`),
                        // but still flag `&block` block-pass arguments.
                        if call
                            .block()
                            .and_then(|block| block.as_block_node())
                            .is_some()
                        {
                            return Vec::new();
                        }
                        // Skip if chained with another method
                        // (can't easily detect from Prism AST alone)

                        // Check parent class arg
                        if let Some(args) = call.arguments() {
                            let arg_list: Vec<_> = args.arguments().iter().collect();
                            if arg_list.len() <= 1 {
                                // Verify the parent is a constant, not a variable
                                if arg_list.len() == 1 {
                                    let arg = &arg_list[0];
                                    if arg.as_constant_read_node().is_none()
                                        && arg.as_constant_path_node().is_none()
                                        && arg.as_self_node().is_none()
                                    {
                                        return Vec::new();
                                    }
                                    // Skip if parent is self
                                    if arg.as_self_node().is_some() {
                                        return Vec::new();
                                    }

                                    // AllowedParentClasses: matched against the
                                    // parent argument's source text (so a
                                    // namespaced name like `Foo::Bar` must be
                                    // listed exactly as written), same as
                                    // RuboCop's `allowed_parent_class?`.
                                    let arg_loc = arg.location();
                                    let arg_text = source.byte_slice(
                                        arg_loc.start_offset(),
                                        arg_loc.end_offset(),
                                        "",
                                    );
                                    if allowed_parent_classes.iter().any(|c| c == arg_text) {
                                        return Vec::new();
                                    }
                                }

                                let loc = node.location();
                                let (line, column) = source.offset_to_line_col(loc.start_offset());
                                return vec![cop.diagnostic(
                                    source,
                                    line,
                                    column,
                                    "Prefer a two-line class definition over `Class.new` for classes with no body.".to_string(),
                                )];
                            }
                        } else {
                            // Class.new with no args
                            let loc = node.location();
                            let (line, column) = source.offset_to_line_col(loc.start_offset());
                            return vec![cop.diagnostic(
                                source,
                                line,
                                column,
                                "Prefer a two-line class definition over `Class.new` for classes with no body.".to_string(),
                            )];
                        }
                    }
                }
            }
        }
    }

    Vec::new()
}

fn check_class_new_style(
    cop: &EmptyClassDefinition,
    source: &SourceFile,
    node: &ruby_prism::Node<'_>,
    allowed_parent_classes: &[String],
) -> Vec<Diagnostic> {
    // Check for empty class definitions
    if let Some(class_node) = node.as_class_node() {
        // Classes without a superclass are not involved in inheritance and
        // are left to `Lint/EmptyClass`, matching RuboCop's
        // `return unless node.parent_class`.
        let Some(superclass) = class_node.superclass() else {
            return Vec::new();
        };

        if !class_new_style_empty_body(class_node.body()) {
            return Vec::new();
        }

        // AllowedParentClasses: matched against the superclass expression's
        // source text, same as RuboCop's `allowed_parent_class?`.
        let sc_loc = superclass.location();
        let sc_text = source.byte_slice(sc_loc.start_offset(), sc_loc.end_offset(), "");
        if allowed_parent_classes.iter().any(|c| c == sc_text) {
            return Vec::new();
        }

        let loc = class_node.location();
        let (line, column) = source.offset_to_line_col(loc.start_offset());
        return vec![cop.diagnostic(
            source,
            line,
            column,
            "Prefer `Class.new` over class definition for classes with no body.".to_string(),
        )];
    }

    Vec::new()
}

fn class_new_style_empty_body(body: Option<ruby_prism::Node<'_>>) -> bool {
    let Some(body) = body else {
        return true;
    };

    let Some(statements) = body.as_statements_node() else {
        return false;
    };

    let mut body = statements.body().iter();
    let Some(statement) = body.next() else {
        return true;
    };

    if body.next().is_some() {
        return false;
    }

    statement.as_self_node().is_some()
        || statement.as_nil_node().is_some()
        || statement.as_true_node().is_some()
        || statement.as_false_node().is_some()
}

fn is_class_const(node: &ruby_prism::Node<'_>) -> bool {
    if let Some(read) = node.as_constant_read_node() {
        return std::str::from_utf8(read.name().as_slice()).unwrap_or("") == "Class";
    }
    if let Some(path) = node.as_constant_path_node() {
        let name = std::str::from_utf8(path.name_loc().as_slice()).unwrap_or("");
        return name == "Class" && path.parent().is_none();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{
        assert_cop_no_offenses_full_with_config, assert_cop_offenses_full_with_config,
    };

    fn class_new_config() -> CopConfig {
        let mut options = std::collections::HashMap::new();
        options.insert(
            "EnforcedStyle".to_string(),
            serde_yml::Value::String("class_new".to_string()),
        );
        CopConfig {
            options,
            ..CopConfig::default()
        }
    }

    crate::cop_fixture_tests!(EmptyClassDefinition, "cops/style/empty_class_definition");

    #[test]
    fn offense_class_new_fixture() {
        assert_cop_offenses_full_with_config(
            &EmptyClassDefinition,
            include_bytes!(
                "../../../tests/fixtures/cops/style/empty_class_definition/offense.class_new.rb"
            ),
            class_new_config(),
        );
    }

    #[test]
    fn no_offense_class_new_fixture() {
        assert_cop_no_offenses_full_with_config(
            &EmptyClassDefinition,
            include_bytes!(
                "../../../tests/fixtures/cops/style/empty_class_definition/no_offense.class_new.rb"
            ),
            class_new_config(),
        );
    }

    fn config_with(style: &str, allowed_parent_classes: &[&str]) -> CopConfig {
        let mut options = std::collections::HashMap::new();
        options.insert(
            "EnforcedStyle".to_string(),
            serde_yml::Value::String(style.to_string()),
        );
        options.insert(
            "AllowedParentClasses".to_string(),
            serde_yml::Value::Sequence(
                allowed_parent_classes
                    .iter()
                    .map(|s| serde_yml::Value::String((*s).to_string()))
                    .collect(),
            ),
        );
        CopConfig {
            options,
            ..CopConfig::default()
        }
    }

    #[test]
    fn allowed_parent_classes_permits_class_new_with_allowed_parent() {
        let config = config_with("class_keyword", &["StandardError"]);
        let diags = crate::testutil::run_cop_full_with_config(
            &EmptyClassDefinition,
            b"FooError = Class.new(StandardError)\n",
            config,
        );
        assert!(diags.is_empty());
    }

    #[test]
    fn allowed_parent_classes_still_flags_non_allowed_parent() {
        let config = config_with("class_keyword", &["StandardError"]);
        let diags = crate::testutil::run_cop_full_with_config(
            &EmptyClassDefinition,
            b"FooError = Class.new(ActiveRecord::Base)\n",
            config,
        );
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn allowed_parent_classes_matches_namespaced_name_exactly() {
        let config = config_with("class_keyword", &["Alchemy::Admin::PreviewUrl"]);
        let diags = crate::testutil::run_cop_full_with_config(
            &EmptyClassDefinition,
            b"MyClass = Class.new(Alchemy::Admin::PreviewUrl)\n",
            config.clone(),
        );
        assert!(diags.is_empty());

        let diags_non_allowed = crate::testutil::run_cop_full_with_config(
            &EmptyClassDefinition,
            b"MyClass = Class.new(StandardError)\n",
            config,
        );
        assert_eq!(diags_non_allowed.len(), 1);
    }

    #[test]
    fn allowed_parent_classes_permits_class_new_style_with_allowed_parent() {
        let config = config_with("class_new", &["ApplicationRecord"]);
        let diags = crate::testutil::run_cop_full_with_config(
            &EmptyClassDefinition,
            b"class MyModel < ApplicationRecord\nend\n",
            config,
        );
        assert!(diags.is_empty());
    }

    #[test]
    fn allowed_parent_classes_still_flags_class_new_style_non_allowed_parent() {
        let config = config_with("class_new", &["ApplicationRecord"]);
        let diags = crate::testutil::run_cop_full_with_config(
            &EmptyClassDefinition,
            b"class FooError < StandardError\nend\n",
            config,
        );
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn class_new_style_without_superclass_never_flagged() {
        // Fixed alongside AllowedParentClasses: a class with no superclass
        // isn't involved in inheritance and is left to Lint/EmptyClass.
        let diags = crate::testutil::run_cop_full_with_config(
            &EmptyClassDefinition,
            b"class MyClass\nend\n",
            class_new_config(),
        );
        assert!(diags.is_empty());
    }
}
