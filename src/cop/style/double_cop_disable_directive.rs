use crate::cop::{Cop, CopConfig};
use crate::diagnostic::Diagnostic;
use crate::parse::source::SourceFile;

/// Detects double `rubocop:disable` (or `rubocop:todo`) comments on a single line.
///
/// ## Investigation notes
///
/// Synthetic corpus FP was caused by test data using short cop names like `Style/A`
/// in `# rubocop:disable Style/A # rubocop:disable Style/B`. RuboCop's
/// `DirectiveComment::DIRECTIVE_COMMENT_REGEXP` has a `COP_NAME_PATTERN` that
/// matches `([A-Za-z]\w+/)*(?:[A-Za-z]\w+)`. For `Style/A`, the regex greedily
/// matches just `Style` (valid department name), leaving `/A ...` as post-match.
/// `CommentConfig` then treats this as a department-level disable for all of `Style`,
/// which suppresses the `Style/DoubleCopDisableDirective` offense on that line.
/// Fix: renamed synthetic test cop names to multi-character names (`Style/Aaa`).
/// The nitrocop cop logic is correct and matches RuboCop's behavior.
///
/// ## Vendor bump (rubocop 1.84.2 -> 1.91.0)
///
/// RuboCop 1.91.0 removed this cop entirely, superseded by `Lint/CopDirectiveSyntax`,
/// which now reports and corrects more than one directive per line itself
/// (https://github.com/rubocop/rubocop/pull/15616 — see vendor `config/obsoletion.yml`,
/// which tells RuboCop users to switch to `Lint/CopDirectiveSyntax`). It no longer
/// appears in vendor `config/default.yml`, so `scripts/generate_baseline_cops.py` now
/// drops it from `src/resources/baseline_cops.json`; it reads as `outside_baseline` in
/// `--rules`/`--migrate` output for any config that still references it, matching real
/// RuboCop 1.91, which rejects that cop name as unrecognized.
/// `default_enabled()` is overridden to `false` here (rather than removing the cop
/// outright) so nitrocop doesn't keep firing offenses for a cop that no longer exists
/// upstream when no explicit `.rubocop.yml` config is present. The nitrocop
/// implementation itself is left in place: fully retiring it (registry, fixtures,
/// tier entry) is cop-logic-adjacent cleanup out of scope for this vendor bump.
pub struct DoubleCopDisableDirective;

impl Cop for DoubleCopDisableDirective {
    fn name(&self) -> &'static str {
        "Style/DoubleCopDisableDirective"
    }

    fn default_enabled(&self) -> bool {
        false
    }

    fn check_source(
        &self,
        source: &SourceFile,
        _parse_result: &ruby_prism::ParseResult<'_>,
        code_map: &crate::parse::codemap::CodeMap,
        _config: &CopConfig,
        diagnostics: &mut Vec<Diagnostic>,
        _corrections: Option<&mut Vec<crate::correction::Correction>>,
    ) {
        // Compute line byte offsets for heredoc checking
        let lines: Vec<&[u8]> = source.lines().collect();
        let mut line_offsets = Vec::with_capacity(lines.len());
        let mut offset = 0usize;
        for line in &lines {
            line_offsets.push(offset);
            offset += line.len() + 1;
        }

        for (i, line) in lines.iter().enumerate() {
            // Skip lines inside heredocs
            if i < line_offsets.len() && code_map.is_heredoc(line_offsets[i]) {
                continue;
            }

            let line_str = match std::str::from_utf8(line) {
                Ok(s) => s,
                Err(_) => continue,
            };

            // Find first rubocop:disable or rubocop:todo directive
            let first_pos = line_str
                .find("# rubocop:disable ")
                .or_else(|| line_str.find("# rubocop:todo "));

            let first_pos = match first_pos {
                Some(p) => p,
                None => continue,
            };

            // Check if there's a second directive on the same line.
            // Skip past the entire first directive prefix to avoid self-matching.
            let skip_len = if line_str[first_pos..].starts_with("# rubocop:disable ") {
                "# rubocop:disable ".len()
            } else {
                "# rubocop:todo ".len()
            };
            let after_first = first_pos + skip_len;
            let rest = &line_str[after_first..];
            if rest.contains("# rubocop:disable ") || rest.contains("# rubocop:todo ") {
                let col = first_pos;
                diagnostics.push(self.diagnostic(
                    source,
                    i + 1,
                    col,
                    "More than one disable comment on one line.".to_string(),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    crate::cop_fixture_tests!(
        DoubleCopDisableDirective,
        "cops/style/double_cop_disable_directive"
    );
}
