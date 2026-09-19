use crate::cop::Cop;
use crate::diagnostic::Severity;

/// Checks for `# rubocop:disable` comments that can be removed.
///
/// **This cop should be the last one fixed for corpus conformance.** Its
/// accuracy depends on every other cop having zero detection gaps — any FN
/// on another cop cascades into an FP here (the disable directive appears
/// unused because nitrocop missed the offense it suppresses). FP/FN on this
/// cop will decrease naturally as individual cop conformance improves.
///
/// The detection logic lives in `lint_source_inner` in `src/linter.rs`, not
/// here. This struct exists so the cop name is registered and can be
/// referenced in configuration (enabled/disabled/excluded).
///
/// ## Fixed (2026-09-18): Layout/LineLength self-suppression — 740 corpus FPs
///
/// `Layout/LineLength` parsed `rubocop:disable` directives itself (a private
/// `parse_line_length_directive` in `src/cop/layout/line_length.rs`) and
/// `continue`d past disabled lines. Because no diagnostic was ever produced,
/// `DisabledRanges::check_and_mark_used` never ran for those lines, the
/// directive stayed unused, and this cop reported it as redundant. That was
/// ~99% of the corpus FPs for this cop (CultivateLabs/raif alone: 121).
///
/// RuboCop's runner does the opposite: `Runner#file_offenses` passes the full
/// offense list — *including* offenses whose status is `:disabled` — to
/// `RedundantCopDisableDirective#offenses_to_check`, and only then does
/// `offenses.sort.reject(&:disabled?)`. So a suppressed offense still marks
/// its directive as needed.
///
/// The fix deletes the cop-local directive parser so `Layout/LineLength`
/// always reports, and `lint_source_inner` suppresses the diagnostic through
/// the shared `DisabledRanges` bookkeeping (which also handles department
/// disables, `all`, and legacy names like `Metrics/LineLength`). Unlike the
/// two reverted attempts below this removes work instead of adding it: forem
/// (3257 files) runs in 2.5s. No other cop suppresses its own diagnostics on
/// directive lines — `grep -rn 'rubocop:disable' src/cop/` to confirm before
/// assuming a new FP cluster has the same cause.
///
/// ## Reverted (twice): Layout/LineLength self-suppression compensation
///
/// `compensate_line_length_self_suppression` re-checks unused Layout/LineLength
/// disable directives against actual line lengths. The logic is correct (105 FN
/// improvement, 0 FP) but causes a catastrophic perf regression on forem
/// (3257 files): 30s → 25min+ timeout, even with an early-exit guard that
/// skips files without unused LineLength directives.
///
/// **Attempt 1** (ddf672d27): unconditional `lines().collect()` on every file.
///   Reverted in #1610 (7670a3f6b).
/// **Attempt 2** (#1612, 2fcd99d50): added early-exit if no unused LineLength
///   directives exist. Still timed out on forem — confirmed locally that the
///   compensation code itself is the bottleneck, not the `allow_flagging` change.
///   Reverted in f957317c9. The early-exit helps most files but forem has ~40
///   files with LineLength disables, and the per-line `.chars().count()` +
///   `find("# rubocop:")` on large disable ranges is too expensive at scale.
///
/// **What a correct fix needs:**
/// - Pre-compute line lengths during the initial parse/codemap phase (O(1) lookup
///   per line instead of re-scanning), or cache `source.lines()` across phases
/// - Avoid `.chars().count()` — use byte length with a fast UTF-8 char-width check
/// - Test on forem locally: must complete in <60s (currently 30s without the fix)
/// - The compensation is ONLY needed when `all_cops_ran` and `has_directives()`
pub struct RedundantCopDisableDirective;

impl Cop for RedundantCopDisableDirective {
    fn name(&self) -> &'static str {
        "Lint/RedundantCopDisableDirective"
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    // This cop is intentionally a no-op in check_lines/check_node/check_source.
    // The actual detection happens in lint_source_inner after all cops have run,
    // where we can determine which disable directives actually suppressed an offense.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cop_name() {
        assert_eq!(
            RedundantCopDisableDirective.name(),
            "Lint/RedundantCopDisableDirective"
        );
    }

    #[test]
    fn default_severity_is_warning() {
        assert_eq!(
            RedundantCopDisableDirective.default_severity(),
            Severity::Warning
        );
    }

    // Full-pipeline tests for this cop live in tests/integration.rs because
    // they need the complete linter pipeline (all cops running + post-processing).
}
