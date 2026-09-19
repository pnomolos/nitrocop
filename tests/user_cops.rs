//! End-to-end tests for user-supplied cop IR documents (design §4.1-4.4).
//!
//! Each test builds a throwaway project in a temp directory and drives the real
//! `nitrocop` binary over it, so config resolution, discovery, registration,
//! tier exemption and the exit code are all exercised together. The one
//! exception is the cache test, which reaches for `ResultCache` directly
//! because the observable effect of the digest is which index file gets used.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A cop that fires on `<Receiver>.transaction`, where `Receiver` is a declared
/// config key. Covers a capture, a config-driven guard and message
/// interpolation in one document.
const NO_BASE_TRANSACTION: &str = r#"
schema: 1
cop: "Custom/NoBaseTransaction"
severity: warning
restrict_on_send: [transaction]

config:
  Receiver: { type: string, default: "Base" }

matchers:
  transaction_call:
    pattern: "(send (const _ $_) :transaction ...)"
    captures: [receiver]

hooks:
  - on: [send]
    match: transaction_call
    when: { eq: ["$receiver", cfg.Receiver] }
    offense:
      location: node.selector
      message: "Use `ApplicationRecord.transaction` instead of `%{receiver}.transaction`."
"#;

struct Project(PathBuf);

impl Project {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("nitrocop_user_cops_{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn write(&self, rel: &str, body: &str) -> &Self {
        let path = self.0.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
        self
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// Run the real binary against this project. `--no-cache` keeps the run off
    /// the lockfile and out of the shared result cache.
    fn run(&self, extra: &[&str]) -> (i32, String, String) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_nitrocop"));
        cmd.current_dir(&self.0).arg("--no-cache");
        cmd.args(extra);
        let out = cmd.output().expect("nitrocop should run");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }

    fn lint(&self) -> (i32, String, String) {
        self.run(&["--format", "text", "."])
    }
}

/// The baseline project: one user cop, two files, one of which trips it.
fn project(name: &str, rubocop_yml: &str) -> Project {
    let p = Project::new(name);
    p.write(".rubocop.yml", rubocop_yml)
        .write(
            ".nitrocop/cops/no_base_transaction.cop.yml",
            NO_BASE_TRANSACTION,
        )
        .write(
            "app/a.rb",
            "# frozen_string_literal: true\n\nBase.transaction { 1 }\n",
        )
        .write(
            "app/b.rb",
            "# frozen_string_literal: true\n\nBar.transaction { 1 }\n",
        );
    p
}

const NO_FSL: &str = "Style/FrozenStringLiteralComment:\n  Enabled: false\n";

#[test]
fn a_discovered_cop_runs_without_preview() {
    // The cop is preview-tier by `tiers.json`'s default; §4.3 exempts user cops
    // from gating, so no `--preview` here.
    let (code, stdout, _) = project("runs", NO_FSL).lint();
    assert_eq!(code, 1, "{stdout}");
    assert!(
        stdout.contains(
            "app/a.rb:3:5: W: Custom/NoBaseTransaction: \
             Use `ApplicationRecord.transaction` instead of `Base.transaction`."
        ),
        "{stdout}"
    );
    assert!(!stdout.contains("app/b.rb"), "{stdout}");
}

#[test]
fn rubocop_yml_binds_a_declared_config_key_and_severity() {
    let p = project(
        "config",
        &format!("{NO_FSL}Custom/NoBaseTransaction:\n  Severity: error\n  Receiver: \"Bar\"\n"),
    );
    let (_, stdout, _) = p.lint();
    // `Receiver: "Bar"` moves the offense from a.rb to b.rb, and `Severity:`
    // overrides the document's own `severity: warning`.
    assert!(
        stdout.contains("app/b.rb:3:4: E: Custom/NoBaseTransaction"),
        "{stdout}"
    );
    assert!(!stdout.contains("app/a.rb:"), "{stdout}");
}

#[test]
fn exclude_and_include_apply_like_any_other_cop() {
    let p = project(
        "exclude",
        &format!("{NO_FSL}Custom/NoBaseTransaction:\n  Exclude:\n    - \"app/a.rb\"\n"),
    );
    let (code, stdout, _) = p.lint();
    assert_eq!(code, 0, "{stdout}");

    let p = project(
        "include",
        &format!("{NO_FSL}Custom/NoBaseTransaction:\n  Include:\n    - \"lib/**/*.rb\"\n"),
    );
    let (code, stdout, _) = p.lint();
    assert_eq!(
        code, 0,
        "Include should scope the cop away from app/: {stdout}"
    );

    p.write(
        "lib/c.rb",
        "# frozen_string_literal: true\n\nBase.transaction { 1 }\n",
    );
    let (code, stdout, _) = p.lint();
    assert_eq!(code, 1, "{stdout}");
    assert!(stdout.contains("lib/c.rb:3:5"), "{stdout}");
}

#[test]
fn enabled_false_turns_a_user_cop_off() {
    let p = project(
        "disabled",
        &format!("{NO_FSL}Custom/NoBaseTransaction:\n  Enabled: false\n"),
    );
    let (code, stdout, _) = p.lint();
    assert_eq!(code, 0, "{stdout}");
}

#[test]
fn custom_cop_paths_picks_up_an_out_of_tree_directory() {
    let p = Project::new("custom_paths");
    p.write(
        ".rubocop.yml",
        &format!("{NO_FSL}AllCops:\n  CustomCopPaths:\n    - shared/cops\n"),
    )
    .write(
        "shared/cops/no_base_transaction.cop.yml",
        NO_BASE_TRANSACTION,
    )
    .write(
        "app/a.rb",
        "# frozen_string_literal: true\n\nBase.transaction { 1 }\n",
    );
    let (code, stdout, _) = p.lint();
    assert_eq!(code, 1, "{stdout}");
    assert!(stdout.contains("Custom/NoBaseTransaction"), "{stdout}");
}

#[test]
fn a_built_in_department_is_a_load_error() {
    let p = Project::new("dept_collision");
    p.write(".rubocop.yml", NO_FSL)
        .write(
            ".nitrocop/cops/x.cop.yml",
            &NO_BASE_TRANSACTION.replace("Custom/NoBaseTransaction", "Style/NoBaseTransaction"),
        )
        .write("app/a.rb", "# frozen_string_literal: true\n");
    let (code, _, stderr) = p.lint();
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("department `Style` is reserved"),
        "{stderr}"
    );
    assert!(stderr.contains(".nitrocop/cops/x.cop.yml:3:"), "{stderr}");
}

#[test]
fn two_documents_may_not_define_the_same_cop() {
    let p = Project::new("name_collision");
    p.write(".rubocop.yml", NO_FSL)
        .write(".nitrocop/cops/a.cop.yml", NO_BASE_TRANSACTION)
        .write(".nitrocop/cops/b.cop.yml", NO_BASE_TRANSACTION)
        .write("app/a.rb", "# frozen_string_literal: true\n");
    let (code, _, stderr) = p.lint();
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("`Custom/NoBaseTransaction` is already defined by"),
        "{stderr}"
    );
}

#[test]
fn an_invalid_document_aborts_the_run_with_a_location() {
    let p = Project::new("invalid");
    p.write(".rubocop.yml", NO_FSL)
        .write(
            ".nitrocop/cops/broken.cop.yml",
            "schema: 1\ncop: \"Custom/Broken\"\nhooks: []\n",
        )
        .write(
            "app/a.rb",
            "# frozen_string_literal: true\n\nBase.transaction { 1 }\n",
        );

    let (code, stdout, stderr) = p.lint();
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains(".nitrocop/cops/broken.cop.yml:3: at least one hook is required"),
        "{stderr}"
    );
    assert!(
        !stdout.contains("files inspected"),
        "the abort must happen before linting: {stdout}"
    );

    // ...unless the user asked for a warning instead.
    let (code, stdout, stderr) = p.run(&["--ignore-invalid-cops", "--format", "text", "."]);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(stdout.contains("1 file inspected"), "{stdout}");
}

#[test]
fn validate_ir_with_no_path_checks_the_discovered_set() {
    let p = project("validate", NO_FSL);
    let (code, stdout, _) = p.run(&["--validate-ir"]);
    assert_eq!(code, 0, "{stdout}");
    assert!(
        stdout.contains("no_base_transaction.cop.yml: ok (Custom/NoBaseTransaction)"),
        "{stdout}"
    );

    p.write(
        ".nitrocop/cops/broken.cop.yml",
        "schema: 1\ncop: \"Custom/Broken\"\nhooks: []\n",
    );
    let (code, _, stderr) = p.run(&["--validate-ir"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("1 invalid cop IR definition(s)"),
        "{stderr}"
    );
}

#[test]
fn list_cops_marks_user_cops_and_leaves_the_rest_alone() {
    let p = project("list_cops", NO_FSL);
    let (code, stdout, _) = p.run(&["--list-cops", "."]);
    assert_eq!(code, 0);

    let lines: Vec<&str> = stdout.lines().collect();
    let marked: Vec<&&str> = lines.iter().filter(|l| l.ends_with(" (custom)")).collect();
    assert_eq!(marked, [&"Custom/NoBaseTransaction (custom)"]);
    // Every other line is a bare cop name, and the list stays sorted.
    assert!(
        lines.contains(&"Style/TimeNow"),
        "embedded IR cop is still listed"
    );
    let mut sorted = lines.clone();
    sorted.sort_unstable();
    assert_eq!(lines, sorted);

    // A project without user cops prints exactly what it printed before.
    let bare = Project::new("list_cops_bare");
    bare.write(".rubocop.yml", NO_FSL);
    let (_, baseline, _) = bare.run(&["--list-cops", "."]);
    assert_eq!(lines.len(), baseline.lines().count() + 1);
    assert!(!baseline.contains("(custom)"));
}

#[test]
fn rules_and_migrate_classify_a_user_cop_as_custom() {
    let p = project("classify", NO_FSL);
    let (_, stdout, _) = p.run(&["--rules", "--format", "json", "."]);
    let rules: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let entry = rules
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "Custom/NoBaseTransaction")
        .expect("user cop should appear in --rules");
    assert_eq!(entry["custom"], serde_json::json!(true));
    assert_eq!(entry["tier"], serde_json::json!("stable"));
    assert_eq!(entry["in_baseline"], serde_json::json!(false));

    let (_, stdout, _) = p.run(&["--migrate", "--format", "json", "."]);
    let report: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(report["counts"]["custom"], serde_json::json!(1));
    assert_eq!(report["counts"]["outside_baseline"], serde_json::json!(0));
}

#[test]
fn editing_a_cop_invalidates_cached_results() {
    use nitrocop::cache::ResultCache;
    use nitrocop::cop::ir::discover;
    use nitrocop::cop::registry::CopRegistry;

    let p = Project::new("cache");
    p.write(
        ".nitrocop/cops/no_base_transaction.cop.yml",
        NO_BASE_TRANSACTION,
    );
    let registry = CopRegistry::new();
    let digest_before = discover::discover_and_load(Some(p.path()), None, &[], &registry)
        .0
        .digest()
        .to_string();
    assert!(!digest_before.is_empty());

    p.write(
        ".nitrocop/cops/no_base_transaction.cop.yml",
        &NO_BASE_TRANSACTION.replace("Use `ApplicationRecord", "Prefer `ApplicationRecord"),
    );
    let digest_after = discover::discover_and_load(Some(p.path()), None, &[], &registry)
        .0
        .digest()
        .to_string();
    assert_ne!(digest_before, digest_after);

    // Distinct digests select distinct cache index files, which is what makes
    // the previously cached diagnostics unreachable.
    let args = <nitrocop::cli::Args as clap::Parser>::parse_from(["nitrocop", "."]);
    let cache_dir = p.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    let rb = p.path().join("app/a.rb");
    fs::create_dir_all(rb.parent().unwrap()).unwrap();
    fs::write(&rb, "Base.transaction { 1 }\n").unwrap();

    let before = ResultCache::with_root(&cache_dir, "0.0.0", &[], &args, &digest_before);
    before.put(&rb, b"Base.transaction { 1 }\n", &[]);
    before.flush();
    let after = ResultCache::with_root(&cache_dir, "0.0.0", &[], &args, &digest_after);
    assert!(
        matches!(after.get_by_stat(&rb), nitrocop::cache::CacheLookup::Miss),
        "a different cop digest must not reuse the old session's results"
    );
}
