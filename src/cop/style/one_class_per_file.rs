use crate::cop::{Cop, CopConfig};
use crate::diagnostic::{Diagnostic, Severity};
use crate::parse::source::SourceFile;

const MSG: &str = "Do not define multiple classes/modules at the top level in a single file.";

/// Checks that each source file defines at most one top-level class or module.
///
/// Only *top-level* definitions count: upstream's `top_level_definition?` is
/// "the node's parent is the root `begin`, or the node is itself the root", so a
/// `class` nested inside another class/module, a conditional, or a `begin` block
/// is ignored. `class << self` is parser's `sclass` type, which `on_class` never
/// sees, so singleton classes neither offend nor count toward the limit.
///
/// `AllowedClasses` is matched against the *short* name — `node.identifier
/// .short_name` — so with `AllowedClasses: ['SpecificError']`,
/// `class Nested::SpecificError` is allowed while `class SpecificError::Nested`
/// is not.
///
/// Prism-vs-Parser quirks:
/// - parser distinguishes "one top-level statement" (the class is the root)
///   from "several" (the class's parent is a root `begin`); Prism always wraps
///   the program body in a single `StatementsNode`, so both collapse to "the
///   node is a direct child of `ProgramNode.statements()`".
/// - The offense range is `source_range.begin_pos ... loc.name.end_pos`, i.e.
///   the `class`/`module` keyword through the constant name. nitrocop reports
///   the start offset, which is the keyword — Prism's `ClassNode`/`ModuleNode`
///   location start.
/// - parser's `node.identifier.short_name` is Prism's `ConstantReadNode.name()`
///   or `ConstantPathNode.name()` (the trailing segment of `Foo::Bar`).
pub struct OneClassPerFile;

impl OneClassPerFile {
    /// The short (last-segment) constant name of a class/module definition.
    fn short_name<'pr>(constant_path: &ruby_prism::Node<'pr>) -> Option<Vec<u8>> {
        if let Some(read) = constant_path.as_constant_read_node() {
            return Some(read.name().as_slice().to_vec());
        }
        if let Some(path) = constant_path.as_constant_path_node() {
            return path.name().map(|n| n.as_slice().to_vec());
        }
        None
    }
}

impl Cop for OneClassPerFile {
    fn name(&self) -> &'static str {
        "Style/OneClassPerFile"
    }

    fn default_severity(&self) -> Severity {
        Severity::Convention
    }

    fn default_exclude(&self) -> &'static [&'static str] {
        &["spec/**/*", "test/**/*"]
    }

    fn check_source(
        &self,
        source: &SourceFile,
        parse_result: &ruby_prism::ParseResult<'_>,
        _code_map: &crate::parse::codemap::CodeMap,
        config: &CopConfig,
        diagnostics: &mut Vec<Diagnostic>,
        _corrections: Option<&mut Vec<crate::correction::Correction>>,
    ) {
        let allowed = config
            .get_string_array("AllowedClasses")
            .unwrap_or_default();

        let node = parse_result.node();
        let Some(program) = node.as_program_node() else {
            return;
        };

        let mut seen = 0usize;
        for statement in program.statements().body().iter() {
            let (constant_path, location) = if let Some(class_node) = statement.as_class_node() {
                (class_node.constant_path(), class_node.location())
            } else if let Some(module_node) = statement.as_module_node() {
                (module_node.constant_path(), module_node.location())
            } else {
                continue;
            };

            let Some(short_name) = Self::short_name(&constant_path) else {
                continue;
            };
            if allowed
                .iter()
                .any(|a| a.as_bytes() == short_name.as_slice())
            {
                continue;
            }

            seen += 1;
            if seen > 1 {
                let (line, column) = source.offset_to_line_col(location.start_offset());
                diagnostics.push(self.diagnostic(source, line, column, MSG.to_string()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    crate::cop_fixture_tests!(OneClassPerFile, "cops/style/one_class_per_file");
    crate::cop_variant_fixture_tests!(
        OneClassPerFile,
        "cops/style/one_class_per_file",
        allowed_classes,
    );
}
