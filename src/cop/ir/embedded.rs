//! IR cops shipped inside the binary (design §4.1).
//!
//! [`FILES`] is hand-maintained: one `include_str!` per
//! `src/resources/ir/<dept>/<snake>.cop.yml`. A generated table would need a
//! `build.rs` or a directory-walking macro crate, and the list is expected to
//! stay in the low tens through the pilot phases; `ir_embedded_files_are_listed`
//! fails the build if a file on disk is missing from it.
//!
//! Loading is lazy behind a [`OnceLock`] and **panics** on failure. A shipped
//! document that does not load is a bug in the binary, not in the user's
//! project, and every one of them is exercised by `cargo test`
//! (`ir_embedded_cops_load`), so a malformed one cannot reach a release.
//! User-supplied cops, when discovery lands (§4.1 item 2), get the
//! `--ignore-invalid-cops` treatment instead.

use std::sync::OnceLock;

use crate::cop::Cop;
use crate::cop::registry::CopRegistry;

use super::cop::IrCopRunner;
use super::load::{LoadMode, load_str_with};

/// `(path relative to the repo root, contents)` for every embedded IR cop.
pub static FILES: &[(&str, &str)] = &[
    (
        "src/resources/ir/lint/data_define_override.cop.yml",
        include_str!("../../resources/ir/lint/data_define_override.cop.yml"),
    ),
    (
        "src/resources/ir/style/file_open.cop.yml",
        include_str!("../../resources/ir/style/file_open.cop.yml"),
    ),
    (
        "src/resources/ir/style/predicate_with_kind.cop.yml",
        include_str!("../../resources/ir/style/predicate_with_kind.cop.yml"),
    ),
    (
        "src/resources/ir/style/redundant_min_max_by.cop.yml",
        include_str!("../../resources/ir/style/redundant_min_max_by.cop.yml"),
    ),
    (
        "src/resources/ir/style/time_now.cop.yml",
        include_str!("../../resources/ir/style/time_now.cop.yml"),
    ),
];

crate::ir_cop_fixture_tests!(
    lint_data_define_override,
    "Lint/DataDefineOverride",
    "cops/lint/data_define_override"
);
crate::ir_cop_fixture_tests!(style_file_open, "Style/FileOpen", "cops/style/file_open");
crate::ir_cop_fixture_tests!(
    style_predicate_with_kind,
    "Style/PredicateWithKind",
    "cops/style/predicate_with_kind"
);
crate::ir_cop_fixture_tests!(
    style_redundant_min_max_by,
    "Style/RedundantMinMaxBy",
    "cops/style/redundant_min_max_by"
);
crate::ir_cop_fixture_tests!(style_time_now, "Style/TimeNow", "cops/style/time_now");

/// Load and compile every embedded document.
///
/// # Panics
///
/// If any embedded document fails to load or to compile into a runner. Both are
/// build-time bugs; see the module docs.
#[must_use]
pub fn load_all() -> Vec<IrCopRunner> {
    FILES
        .iter()
        .map(|(path, source)| {
            let doc = load_str_with(source, path, LoadMode::Builtin)
                .unwrap_or_else(|e| panic!("embedded IR cop failed to load: {e}"));
            IrCopRunner::new(doc)
                .unwrap_or_else(|e| panic!("embedded IR cop failed to compile: {e}"))
        })
        .collect()
}

/// The embedded cops, compiled once, for fixture tests and `embedded::get`.
///
/// The registry gets its own instances ([`register_all`]) rather than sharing
/// these: a `Box<dyn Cop>` owns its cop, and a second parse of a handful of
/// two-kilobyte documents is cheaper than the machinery to avoid it.
#[must_use]
pub fn all() -> &'static [IrCopRunner] {
    static COPS: OnceLock<Vec<IrCopRunner>> = OnceLock::new();
    COPS.get_or_init(load_all).as_slice()
}

/// One embedded cop by name, for fixture tests.
#[must_use]
pub fn get(name: &str) -> Option<&'static IrCopRunner> {
    all().iter().find(|cop| Cop::name(*cop) == name)
}

/// Register every embedded IR cop. Called from
/// [`CopRegistry::default_registry`].
pub fn register_all(registry: &mut CopRegistry) {
    for cop in load_all() {
        registry.register(Box::new(cop));
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn ir_embedded_cops_load() {
        // Forces every `include_str!`'d document through the loader and the
        // runner builder; either failing is a panic, which is the point.
        for cop in all() {
            let name = Cop::name(cop);
            assert!(name.contains('/'), "{name} is not `Dept/Name`");
            assert!(
                !cop.interested_node_types().is_empty(),
                "{name} would opt into universal dispatch"
            );
        }
    }

    #[test]
    fn ir_embedded_files_are_listed() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/resources/ir");
        if !root.exists() {
            assert!(
                FILES.is_empty(),
                "FILES lists cops but src/resources/ir is gone"
            );
            return;
        }
        let mut found = Vec::new();
        for dept in std::fs::read_dir(&root).expect("read src/resources/ir") {
            let dept = dept.expect("dir entry").path();
            if !dept.is_dir() {
                continue;
            }
            for file in std::fs::read_dir(&dept).expect("read department dir") {
                let path = file.expect("dir entry").path();
                if path.extension().is_some_and(|e| e == "yml") {
                    found.push(path);
                }
            }
        }
        for path in &found {
            let listed = path.to_string_lossy();
            assert!(
                FILES.iter().any(|(p, _)| listed.ends_with(p)),
                "{} is not listed in embedded::FILES",
                path.display()
            );
        }
        assert_eq!(found.len(), FILES.len(), "embedded::FILES is out of date");
    }
}
