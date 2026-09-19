use std::sync::LazyLock;

use regex::Regex;

use crate::cop::{Cop, CopConfig};
use crate::diagnostic::Diagnostic;
use crate::parse::source::SourceFile;

/// Detects `# rubocop:disable`, `# rubocop:enable`, and `# rubocop:todo`
/// directives in source code comments.
///
/// ## Investigation notes (2026-03-15)
///
/// Root cause of FPs: The original line-based `check_lines` implementation
/// scanned raw source lines, which picked up directive-like text embedded
/// inside string literals (heredocs, quoted strings). RuboCop only checks
/// actual parser comments via `processed_source.comments`, so it correctly
/// ignores directives inside strings.
///
/// Root cause of FNs: The original implementation used exact string matching
/// (`"# rubocop:disable "`) requiring exactly one space after `#` and a
/// trailing space after the mode keyword. RuboCop's `DirectiveComment` uses
/// a regex that allows flexible whitespace: `#\s*rubocop\s*:\s*(disable|enable|todo)`.
///
/// Fix: Switched from `check_lines` to `check_source`, iterating over
/// `parse_result.comments()` (Prism's AST-derived comment list) and using
/// a regex matching RuboCop's flexible spacing. Also fixed per-cop offense
/// emission with `AllowedCops`: RuboCop emits one offense per comment joining
/// all disallowed cop names, not one offense per disallowed cop.
///
/// ## Investigation notes (2026-03-17)
///
/// 5 remaining FPs: The directive regex was not anchored, so it matched
/// directive-like text embedded inside YARD documentation comments (e.g.,
/// `# Checks that \`# rubocop:enable ...\` and \`# rubocop:disable ...\``).
/// Prism's comment bytes always start with `#`, so anchoring the regex with
/// `^` ensures only actual directives at the start of the comment are matched.
/// All 5 FPs were from rubocop's own source (YARD docs) and a shoryuken spec.
///
/// ## Investigation notes (2026-03-30)
///
/// 85 FNs + 1 FP remaining: The `^` anchor prevented matching inline
/// directives embedded in longer comments (e.g., `# some text # rubocop:disable Foo`
/// or `#: type_annotation # rubocop:disable Style/RedundantSelf`). Prism
/// treats the whole line comment as one node, so the directive is not at
/// position 0 of the comment bytes.
///
/// Fix: Removed the `^` anchor and instead use a strict cop-name pattern
/// (`[A-Za-z]\w+(/[A-Za-z]\w+)*` or `all`) so that YARD prose like
/// `# rubocop:enable ...` doesn't match (since `...` isn't a valid cop name).
/// Also skip `disable all` and `todo all` directives because they suppress
/// all cops including this one — RuboCop never reports an offense for them.
///
/// ## `DisallowedCops`, `AllowWithReason`, `AllowedDirectives` (rubocop 1.91, vendor bump 2026-09)
///
/// `DisallowedCops`: when non-empty, flips the cop from an allowlist to a
/// denylist — only directives naming a listed cop (or department) are
/// flagged, using the same per-cop message form as `AllowedCops`
/// (`` RuboCop disable/enable directives for `Cop` are not permitted. ``).
/// `DisallowedCops` takes precedence over `AllowedCops` when both are set
/// (matches `compute_disallowed_cops`). Implementing this properly also
/// meant fixing `AllowedCops`/`DisallowedCops` name matching to accept a bare
/// department name (e.g. `Metrics`) covering any cop in it, not just exact
/// cop-name string equality — matching RuboCop's `listed?` helper — since
/// the two options share that matcher.
///
/// **Known gap:** upstream's `compute_disallowed_cops` returns the full
/// (unfiltered) directive cop list whenever it contains `all`, so under
/// `DisallowedCops`, `# rubocop:disable all` / `# rubocop:todo all` ARE
/// flagged (per the spec's "when disabling all cops" example under
/// `DisallowedCops`). This implementation instead keeps the pre-existing,
/// corpus-validated exemption for `disable all`/`todo all` unconditional
/// (see the 2026-03-30 note above) rather than making it conditional on
/// `DisallowedCops`, because the interaction between that exemption (which
/// was needed to fix a real corpus FP against RuboCop's actual output) and
/// this cop's own self-referential `Enabled: true` immunity could not be
/// verified without corpus access. A project relying on `DisallowedCops` to
/// catch bare `disable all`/`todo all` directives will not get that specific
/// case flagged here.
///
/// `AllowedDirectives`: lists directive modes (`disable`, `todo`,
/// `disable-next`, `todo-next`, ...) to ignore entirely, regardless of which
/// cops they name. Implementing this required teaching the directive regex
/// to also recognize `disable-next`/`todo-next` as distinct modes (they were
/// previously unrecognized by nitrocop at all — a pre-existing FN, now fixed
/// as a side effect: a bare `# rubocop:disable-next Foo` is now flagged by
/// default, matching RuboCop). `push`/`pop`/`next`/`enable-next` directives
/// (RuboCop's `+`/`-`-prefixed signed-argument forms) are still not
/// recognized as directives at all by nitrocop, so listing them in
/// `AllowedDirectives` has no effect — there is nothing for it to exempt.
/// Implementing those would require a different argument grammar
/// (`+Cop -Cop` instead of a plain cop list) and is out of scope here.
///
/// `AllowWithReason`: when `true`, a disable/todo/disable-next/todo-next
/// directive followed by a `-- reason` trailing comment is allowed, and
/// enable-mode directives are ignored outright (matching
/// `directive.enabled?`), since they end a suppression rather than start
/// one. When an offense still fires under `AllowWithReason`, the message is
/// always "RuboCop disable directives without a `--` justification comment
/// are not permitted." regardless of `AllowedCops`/`DisallowedCops`,
/// matching `offense_message`'s `if allow_with_reason?` short-circuit.
pub struct DisableCopsWithinSourceCodeDirective;

/// Regex matching rubocop directive comments with flexible whitespace,
/// mirroring RuboCop's `DirectiveComment::DIRECTIVE_COMMENT_REGEXP`.
/// Not anchored to `^` so it matches inline directives embedded in longer
/// comments (e.g. `# some text # rubocop:disable Foo`). Uses a strict
/// cop-name pattern instead to avoid matching YARD prose that mentions
/// directives (e.g. `# rubocop:enable ...` where `...` is not a cop name).
/// Captures: (1) mode (disable/enable/todo/disable-next/todo-next), (2) cop list.
static DIRECTIVE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"#\s*rubocop\s*:\s*(disable-next|todo-next|disable|enable|todo)\s+(all\b|[A-Za-z]\w+(?:/[A-Za-z]\w+)*(?:\s*,\s*[A-Za-z]\w+(?:/[A-Za-z]\w+)*)*)",
    )
    .unwrap()
});

/// Matches a cop/department name against an `AllowedCops`/`DisallowedCops`-style
/// list: an exact cop name, or the cop's department name alone (so listing
/// `Metrics` covers `Metrics/AbcSize`, matching RuboCop's `listed?` helper).
fn listed(names: &[String], cop: &str) -> bool {
    let department = cop.split('/').next().unwrap_or(cop);
    names.iter().any(|n| n == cop || n == department)
}

impl Cop for DisableCopsWithinSourceCodeDirective {
    fn name(&self) -> &'static str {
        "Style/DisableCopsWithinSourceCodeDirective"
    }

    fn default_enabled(&self) -> bool {
        false
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
        let allowed_cops = config.get_string_array("AllowedCops").unwrap_or_default();
        let disallowed_cops_config = config
            .get_string_array("DisallowedCops")
            .unwrap_or_default();
        let allow_with_reason = config.get_bool("AllowWithReason", false);
        let allowed_directives = config
            .get_string_array("AllowedDirectives")
            .unwrap_or_default();

        for comment in parse_result.comments() {
            let loc = comment.location();
            let comment_bytes = &source.as_bytes()[loc.start_offset()..loc.end_offset()];
            let Ok(comment_str) = std::str::from_utf8(comment_bytes) else {
                continue;
            };

            let Some(caps) = DIRECTIVE_RE.captures(comment_str) else {
                continue;
            };

            let full_match = caps.get(0).unwrap();
            let mode = &caps[1];
            let cop_list_raw = &caps[2];

            // `# rubocop:disable all` and `# rubocop:todo all` suppress all
            // cops including this one, so RuboCop never reports an offense for
            // them.  Skip to avoid FPs. Kept unconditional (not gated on
            // DisallowedCops) — see the `///` doc comment on this cop for why.
            if (mode == "disable" || mode == "todo") && cop_list_raw.trim() == "all" {
                continue;
            }

            // AllowedDirectives: exempt this directive form entirely.
            if allowed_directives.iter().any(|d| d == mode) {
                continue;
            }

            // AllowWithReason: a `-- reason` trailing comment justifies a
            // disable/todo directive; enable-mode directives are always
            // ignored since they end a suppression rather than start one.
            let tail = comment_str[full_match.end()..].trim_start();
            let has_reason = tail
                .strip_prefix("--")
                .is_some_and(|r| !r.trim().is_empty());
            if allow_with_reason && (mode == "enable" || has_reason) {
                continue;
            }

            let cop_names: Vec<&str> = cop_list_raw
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();

            let (line, col) = source.offset_to_line_col(loc.start_offset());

            // `DisallowedCops` flips the cop into a denylist and takes
            // precedence over `AllowedCops` when both are set, matching
            // RuboCop's `compute_disallowed_cops`.
            let (disallowed, use_cops_message): (Vec<&str>, bool) =
                if !disallowed_cops_config.is_empty() {
                    if cop_names.contains(&"all") {
                        // Matches `directive_cops.include?('all')` short-circuit:
                        // an `all` directive is disallowed outright, unfiltered
                        // (reached here only for modes other than disable/todo,
                        // e.g. `enable all`, since disable/todo `all` is exempted
                        // above).
                        (cop_names.clone(), true)
                    } else {
                        let filtered: Vec<&str> = cop_names
                            .iter()
                            .copied()
                            .filter(|c| listed(&disallowed_cops_config, c))
                            .collect();
                        (filtered, true)
                    }
                } else {
                    let filtered: Vec<&str> = cop_names
                        .iter()
                        .copied()
                        .filter(|c| *c == "all" || !listed(&allowed_cops, c))
                        .collect();
                    (filtered, !allowed_cops.is_empty())
                };

            if disallowed.is_empty() {
                continue;
            }

            let message = if allow_with_reason {
                "RuboCop disable directives without a `--` justification comment are not permitted."
                    .to_string()
            } else if use_cops_message {
                let cops_formatted: Vec<String> =
                    disallowed.iter().map(|c| format!("`{}`", c)).collect();
                format!(
                    "RuboCop disable/enable directives for {} are not permitted.",
                    cops_formatted.join(", ")
                )
            } else {
                "RuboCop disable/enable directives are not permitted.".to_string()
            };

            diagnostics.push(self.diagnostic(source, line, col, message));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::run_cop_full_with_config;

    crate::cop_fixture_tests!(
        DisableCopsWithinSourceCodeDirective,
        "cops/style/disable_cops_within_source_code_directive"
    );

    fn config_with(pairs: &[(&str, serde_yml::Value)]) -> CopConfig {
        CopConfig {
            options: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
            ..CopConfig::default()
        }
    }

    fn string_list(items: &[&str]) -> serde_yml::Value {
        serde_yml::Value::Sequence(
            items
                .iter()
                .map(|s| serde_yml::Value::String((*s).to_string()))
                .collect(),
        )
    }

    // ---- DisallowedCops ----

    #[test]
    fn disallowed_cops_flags_a_listed_cop() {
        let config = config_with(&[(
            "DisallowedCops",
            string_list(&["Lint/Void", "Security/Eval"]),
        )]);
        let diags = run_cop_full_with_config(
            &DisableCopsWithinSourceCodeDirective,
            b"foo # rubocop:disable Lint/Void\n",
            config,
        );
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].message,
            "RuboCop disable/enable directives for `Lint/Void` are not permitted."
        );
    }

    #[test]
    fn disallowed_cops_ignores_a_non_listed_cop() {
        let config = config_with(&[(
            "DisallowedCops",
            string_list(&["Lint/Void", "Security/Eval"]),
        )]);
        let diags = run_cop_full_with_config(
            &DisableCopsWithinSourceCodeDirective,
            b"def foo # rubocop:disable Metrics/AbcSize\nend\n",
            config,
        );
        assert!(diags.is_empty());
    }

    #[test]
    fn disallowed_cops_flags_department_membership() {
        let config = config_with(&[("DisallowedCops", string_list(&["Lint"]))]);
        let diags = run_cop_full_with_config(
            &DisableCopsWithinSourceCodeDirective,
            b"foo # rubocop:disable Lint/Void\n",
            config,
        );
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].message,
            "RuboCop disable/enable directives for `Lint/Void` are not permitted."
        );
    }

    #[test]
    fn disallowed_cops_takes_precedence_over_allowed_cops() {
        let config = config_with(&[
            ("AllowedCops", string_list(&["Lint/Void"])),
            ("DisallowedCops", string_list(&["Security/Eval"])),
        ]);
        let diags = run_cop_full_with_config(
            &DisableCopsWithinSourceCodeDirective,
            b"foo # rubocop:disable Lint/Void, Security/Eval\n",
            config,
        );
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].message,
            "RuboCop disable/enable directives for `Security/Eval` are not permitted."
        );
    }

    #[test]
    fn disallowed_cops_all_directive_still_flagged_for_enable() {
        // `enable all` isn't covered by the disable/todo-all exemption, so it
        // must still be flagged (as `all`) even under DisallowedCops.
        let config = config_with(&[("DisallowedCops", string_list(&["Lint/Void"]))]);
        let diags = run_cop_full_with_config(
            &DisableCopsWithinSourceCodeDirective,
            b"def foo # rubocop:enable all\nend\n",
            config,
        );
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].message,
            "RuboCop disable/enable directives for `all` are not permitted."
        );
    }

    #[test]
    fn allowed_cops_matches_department_membership() {
        // AllowedCops/DisallowedCops share the same department-aware matcher.
        let config = config_with(&[("AllowedCops", string_list(&["Metrics"]))]);
        let diags = run_cop_full_with_config(
            &DisableCopsWithinSourceCodeDirective,
            b"def foo # rubocop:disable Metrics/AbcSize\nend\n",
            config,
        );
        assert!(diags.is_empty());
    }

    // ---- AllowedDirectives ----

    #[test]
    fn allowed_directives_exempts_todo_but_not_disable() {
        let config = config_with(&[("AllowedDirectives", string_list(&["todo"]))]);

        let diags_todo = run_cop_full_with_config(
            &DisableCopsWithinSourceCodeDirective,
            b"x = 0 # rubocop:todo Layout/SpaceAroundOperators\n",
            config.clone(),
        );
        assert!(diags_todo.is_empty());

        let diags_disable = run_cop_full_with_config(
            &DisableCopsWithinSourceCodeDirective,
            b"y = 0 # rubocop:disable Layout/SpaceAroundOperators\n",
            config,
        );
        assert_eq!(diags_disable.len(), 1);
        assert_eq!(
            diags_disable[0].message,
            "RuboCop disable/enable directives are not permitted."
        );
    }

    #[test]
    fn disable_next_directive_flagged_by_default() {
        // Previously unrecognized by nitrocop at all; now flagged like disable/todo.
        let diags = crate::testutil::run_cop_full(
            &DisableCopsWithinSourceCodeDirective,
            b"# rubocop:disable-next Metrics/AbcSize\ndef foo\nend\n",
        );
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].message,
            "RuboCop disable/enable directives are not permitted."
        );
    }

    #[test]
    fn allowed_directives_exempts_disable_next_and_todo_next() {
        let config = config_with(&[("AllowedDirectives", string_list(&["todo", "todo-next"]))]);
        let diags = run_cop_full_with_config(
            &DisableCopsWithinSourceCodeDirective,
            b"# rubocop:todo-next Layout/SpaceAroundOperators\nx = 0\n",
            config,
        );
        assert!(diags.is_empty());
    }

    // ---- AllowWithReason ----

    #[test]
    fn allow_with_reason_permits_disable_next_with_justification() {
        let config = config_with(&[("AllowWithReason", serde_yml::Value::Bool(true))]);
        let diags = run_cop_full_with_config(
            &DisableCopsWithinSourceCodeDirective,
            b"# rubocop:disable-next Metrics/AbcSize -- legacy method\ndef foo\nend\n",
            config,
        );
        assert!(diags.is_empty());
    }

    #[test]
    fn allow_with_reason_flags_disable_without_justification() {
        let config = config_with(&[("AllowWithReason", serde_yml::Value::Bool(true))]);
        let diags = run_cop_full_with_config(
            &DisableCopsWithinSourceCodeDirective,
            b"x = 0 # rubocop:disable Layout/SpaceAroundOperators\n",
            config,
        );
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].message,
            "RuboCop disable directives without a `--` justification comment are not permitted."
        );
    }

    #[test]
    fn allow_with_reason_permits_disable_with_justification() {
        let config = config_with(&[("AllowWithReason", serde_yml::Value::Bool(true))]);
        let diags = run_cop_full_with_config(
            &DisableCopsWithinSourceCodeDirective,
            b"x = 0 # rubocop:disable Layout/SpaceAroundOperators -- aligning with the table below\n",
            config,
        );
        assert!(diags.is_empty());
    }

    #[test]
    fn allow_with_reason_permits_enable_closing_a_justified_disable() {
        let config = config_with(&[("AllowWithReason", serde_yml::Value::Bool(true))]);
        let diags = run_cop_full_with_config(
            &DisableCopsWithinSourceCodeDirective,
            b"# rubocop:disable Metrics/AbcSize -- legacy method, tracked in JIRA-123\ndef foo\nend\n# rubocop:enable Metrics/AbcSize\n",
            config,
        );
        assert!(diags.is_empty());
    }

    #[test]
    fn allow_with_reason_message_takes_precedence_over_disallowed_cops_message() {
        // offense_message checks allow_with_reason? first, regardless of
        // AllowedCops/DisallowedCops.
        let config = config_with(&[
            ("AllowWithReason", serde_yml::Value::Bool(true)),
            ("DisallowedCops", string_list(&["Lint"])),
        ]);
        let diags = run_cop_full_with_config(
            &DisableCopsWithinSourceCodeDirective,
            b"foo # rubocop:disable Lint/Void\n",
            config,
        );
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].message,
            "RuboCop disable directives without a `--` justification comment are not permitted."
        );
    }

    #[test]
    fn allow_with_reason_combined_with_allowed_cops_department() {
        let config = config_with(&[
            ("AllowWithReason", serde_yml::Value::Bool(true)),
            ("AllowedCops", string_list(&["Metrics"])),
        ]);
        let diags_allowed = run_cop_full_with_config(
            &DisableCopsWithinSourceCodeDirective,
            b"# rubocop:disable Metrics/AbcSize\ndef foo\nend\n# rubocop:enable Metrics/AbcSize\n",
            config.clone(),
        );
        assert!(diags_allowed.is_empty());

        let diags_outside = run_cop_full_with_config(
            &DisableCopsWithinSourceCodeDirective,
            b"x = 0 # rubocop:disable Layout/SpaceAroundOperators\n",
            config,
        );
        assert_eq!(diags_outside.len(), 1);
    }
}
