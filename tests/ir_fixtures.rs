//! Fixture-driven tests for the cop IR loader.
//!
//! * `tests/fixtures/ir/valid/*.cop.yml` — must load cleanly. These are the
//!   three worked examples from the IR design doc (§1.3, §1.5, §1.6).
//! * `tests/fixtures/ir/invalid/*.cop.yml` — must fail. Each has a sidecar
//!   `*.expected` whose first line is the expected `IrErrorKind` variant name
//!   and whose remaining lines are substrings the rendered error must contain. A fixture named `*.user.cop.yml` is loaded
//!   with `LoadMode::User` instead of `LoadMode::Builtin`.
//!
//! A third test asserts `scripts/shared/ir_schema.json` has not drifted from
//! `src/cop/ir/schema.rs`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use nitrocop::cop::ir::schema::{COLLECTIONS, ConfigType, OPERATORS, QUANTIFIER_KEYS, QUANTIFIERS};
use nitrocop::cop::ir::{LoadMode, load_path_with};

fn fixture_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/ir")
        .join(name)
}

fn cop_files(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.to_string_lossy().ends_with(".cop.yml"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no fixtures in {}", dir.display());
    paths
}

#[test]
fn valid_fixtures_load() {
    for path in cop_files(&fixture_dir("valid")) {
        match load_path_with(&path, LoadMode::Builtin) {
            Ok(cop) => {
                assert!(
                    cop.name().contains('/'),
                    "{}: cop name should be Dept/Name",
                    path.display()
                );
                assert!(
                    !cop.document.hooks.is_empty(),
                    "{}: expected hooks",
                    path.display()
                );
            }
            Err(err) => panic!("{}: expected to load, got {err}", path.display()),
        }
    }
}

#[test]
fn invalid_fixtures_fail_with_expected_kind() {
    for path in cop_files(&fixture_dir("invalid")) {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let mode = if name.ends_with(".user.cop.yml") {
            LoadMode::User
        } else {
            LoadMode::Builtin
        };
        let sidecar = path.with_file_name(name.replace(".cop.yml", ".expected"));
        let expected = std::fs::read_to_string(&sidecar)
            .unwrap_or_else(|e| panic!("reading {}: {e}", sidecar.display()));
        let mut lines = expected.lines().filter(|l| !l.trim().is_empty());
        let want_kind = lines.next().expect("expected file needs an error kind");

        let err = match load_path_with(&path, mode) {
            Ok(cop) => panic!(
                "{}: expected failure, loaded {}",
                path.display(),
                cop.name()
            ),
            Err(err) => err,
        };
        assert_eq!(
            format!("{:?}", err.kind),
            want_kind,
            "{}: wrong error kind (message: {err})",
            path.display()
        );
        let rendered = err.to_string();
        assert!(
            rendered.starts_with(&path.display().to_string()),
            "{}: error should be prefixed with the origin, got {rendered}",
            path.display()
        );
        for needle in lines {
            assert!(
                rendered.contains(needle),
                "{}: error should mention {needle:?}, got {rendered}",
                path.display()
            );
        }
    }
}

#[test]
fn json_schema_matches_rust_schema() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/shared/ir_schema.json");
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

    assert_eq!(json["additionalProperties"], serde_json::json!(false));
    assert_eq!(
        json["required"],
        serde_json::json!(["schema", "cop", "hooks"])
    );
    assert_eq!(json["properties"]["schema"]["const"], serde_json::json!(1));

    // Top-level keys must be exactly IrDocument's fields.
    let documented: BTreeSet<&str> = json["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let expected: BTreeSet<&str> = [
        "schema",
        "cop",
        "version_added",
        "docs",
        "severity",
        "enabled_default",
        "tier",
        "autocorrect",
        "include",
        "exclude",
        "restrict_on_send",
        "config",
        "constants",
        "matchers",
        "predicates",
        "hooks",
    ]
    .into_iter()
    .collect();
    assert_eq!(
        documented, expected,
        "ir_schema.json drifted from IrDocument"
    );

    // Operator vocabulary must match schema::OPERATORS exactly. Branch 2 of
    // the `expr` union carries the plain operators, branch 3 the quantifiers
    // (whose operand is a quantifier mapping, not an expression).
    let enum_at = |branch: usize| -> BTreeSet<&str> {
        json["$defs"]["expr"]["oneOf"][branch]["propertyNames"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect()
    };
    let quantifiers = enum_at(3);
    assert_eq!(
        quantifiers,
        QUANTIFIERS.iter().copied().collect::<BTreeSet<_>>()
    );
    let ops: BTreeSet<&str> = enum_at(2).union(&quantifiers).copied().collect();
    assert_eq!(ops, OPERATORS.iter().copied().collect::<BTreeSet<_>>());

    // Quantifier operand keys and `over:` collections must match too.
    let keys: BTreeSet<&str> = json["$defs"]["quantifier"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        QUANTIFIER_KEYS.iter().copied().collect::<BTreeSet<_>>()
    );
    let over = json["$defs"]["quantifier"]["properties"]["over"]["pattern"]
        .as_str()
        .unwrap();
    for collection in COLLECTIONS {
        assert!(
            over.contains(collection),
            "ir_schema.json `over:` pattern is missing {collection}"
        );
    }

    // Config types must match ConfigType's variants.
    let types: Vec<String> = json["$defs"]["configType"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    let rust_types = [
        ConfigType::Enum,
        ConfigType::String,
        ConfigType::StringArray,
        ConfigType::Int,
        ConfigType::Float,
        ConfigType::Bool,
        ConfigType::StringMap,
    ]
    .map(|t| t.to_string());
    assert_eq!(types, rust_types.to_vec());
}
