//! Discovery of user-supplied IR cops (design §4.1 items 2 and 4).
//!
//! Two channels, both resolved against the **config root** — the directory of
//! the resolved `.rubocop.yml`, falling back to the scan root when there is no
//! config file:
//!
//! 1. `.nitrocop/cops/**/*.cop.yml`, walked recursively. Nothing to configure:
//!    a cop exists because its file exists.
//! 2. `AllCops: { CustomCopPaths: [...] }` in `.rubocop.yml`, for out-of-tree
//!    directories (monorepos, a shared `config/` checkout). Each entry is a
//!    file or a directory; a directory is walked like channel 1.
//!
//! Gem-shipped packs (design §4.1 item 3 — a `.nitrocop/cops/` directory inside
//! a `require:`d gem, located through [`crate::config::gem_path`]) are **not**
//! implemented. [`roots`] is the hook: pushing each resolved gem path onto its
//! `roots` argument is the whole change on this side, and the rest of this
//! module — walking, collision detection, the digest — already handles it.
//!
//! Loading is fail-closed (design §4.4): [`load`] returns every error it found
//! rather than the first, and the caller aborts before linting. Two failures
//! this module adds on top of the document loader's own:
//!
//! * a name that collides with a built-in (or embedded IR) cop, which would
//!   otherwise silently shadow a parity-tested cop (design §7 risk 8);
//! * the same name defined by two user files.
//!
//! User cops load in [`LoadMode::User`], so a built-in department is rejected
//! by the document loader itself; `Custom/` is the convention.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::cop::registry::CopRegistry;

use super::cop::IrCopRunner;
use super::load::{IrError, IrErrorKind, LoadMode, load_str_with};

/// Directory, relative to the config root, holding project-local cops.
pub const USER_COP_DIR: &str = ".nitrocop/cops";

/// Suffix every user cop document must carry. Files that do not end in this are
/// ignored, so a `README.md` or an editor swap file next to the cops is fine.
pub const USER_COP_SUFFIX: &str = ".cop.yml";

/// The loaded user cops, plus the fingerprint of the documents they came from.
#[derive(Default)]
pub struct UserCops {
    /// One runnable cop per document, in discovery order.
    pub runners: Vec<IrCopRunner>,
    /// `Dept/Name` for each of them, for `--list-cops` and classification.
    pub names: Vec<String>,
    /// The document each came from, parallel to [`Self::names`].
    pub paths: Vec<PathBuf>,
    digest: String,
}

impl UserCops {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.runners.is_empty()
    }

    /// Hex SHA-256 over every discovered document's path **and contents**.
    ///
    /// Empty when nothing was discovered, so a project without user cops keeps
    /// the cache key it had before this feature existed. Feeding this into the
    /// result cache's session hash is what makes editing a cop invalidate
    /// previously cached results.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Register every loaded cop into `registry`.
    pub fn register_all(self, registry: &mut CopRegistry) {
        for runner in self.runners {
            registry.register(Box::new(runner));
        }
    }
}

/// Resolve the directories to search, in load order.
///
/// `config_root` is the directory of the resolved `.rubocop.yml`; `scan_root`
/// is the CLI target, used when there is no config file. `extra` are
/// `AllCops.CustomCopPaths` entries, resolved against `config_root`.
#[must_use]
pub fn roots(
    config_root: Option<&Path>,
    scan_root: Option<&Path>,
    extra: &[String],
) -> Vec<PathBuf> {
    let base = config_root.or(scan_root);
    let mut out = Vec::new();
    if let Some(base) = base {
        out.push(base.join(USER_COP_DIR));
        for entry in extra {
            let path = Path::new(entry);
            out.push(if path.is_absolute() {
                path.to_path_buf()
            } else {
                base.join(path)
            });
        }
    }
    // Gem-shipped packs (design §4.1 item 3) would be appended here, between
    // the project directory and the explicit paths.
    out
}

/// Collect every candidate document under `roots`, deterministically ordered.
///
/// A root that does not exist is skipped: `.nitrocop/cops/` is optional by
/// design, and an explicit `CustomCopPaths` entry that is missing is reported
/// by [`load`] rather than here, so the message can carry the config key.
#[must_use]
pub fn discover(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for root in roots {
        if root.is_file() {
            files.push(root.clone());
        } else if root.is_dir() {
            collect_dir(root, &mut files);
        }
    }
    files.sort();
    files.dedup();
    files
}

/// Recursive `*.cop.yml` walk. Entries are sorted at every level so the
/// resulting order — and therefore the digest — does not depend on the
/// filesystem's readdir order.
fn collect_dir(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect_dir(&path, out);
        } else if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(USER_COP_SUFFIX))
        {
            out.push(path);
        }
    }
}

/// Load every document in `files`, rejecting collisions against `registry`.
///
/// Returns the cops that loaded **and** every error, so `--ignore-invalid-cops`
/// can keep the good ones while the default path aborts on the first sight of a
/// non-empty error list.
#[must_use]
pub fn load(files: &[PathBuf], registry: &CopRegistry) -> (UserCops, Vec<IrError>) {
    let mut cops = UserCops::default();
    let mut errors = Vec::new();
    let mut hasher = Sha256::new();
    hasher.update(b"nitrocop-user-cops-v1:");
    let mut seen: HashMap<String, PathBuf> = HashMap::new();

    for path in files {
        let origin = path.display().to_string();
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                errors.push(io_error(&origin, &e.to_string()));
                continue;
            }
        };
        // Hash every document that was *found*, valid or not: an edit that
        // turns an invalid cop valid must invalidate the cache too.
        hasher.update(origin.as_bytes());
        hasher.update(b"\0");
        hasher.update(source.as_bytes());
        hasher.update(b"\0");

        let doc = match load_str_with(&source, &origin, LoadMode::User) {
            Ok(doc) => doc,
            Err(e) => {
                errors.push(e);
                continue;
            }
        };
        let name = doc.name().to_string();
        if registry.get(&name).is_some() {
            errors.push(collision(&origin, &source, &format!(
                "cop `{name}` is already defined by a built-in cop; user cops must use a novel name"
            )));
            continue;
        }
        if let Some(first) = seen.get(&name) {
            errors.push(collision(
                &origin,
                &source,
                &format!("cop `{name}` is already defined by {}", first.display()),
            ));
            continue;
        }
        let runner = match IrCopRunner::new(doc) {
            Ok(runner) => runner,
            Err(e) => {
                errors.push(e);
                continue;
            }
        };
        seen.insert(name.clone(), path.clone());
        cops.names.push(name);
        cops.paths.push(path.clone());
        cops.runners.push(runner);
    }

    if !files.is_empty() {
        cops.digest = format!("{:x}", hasher.finalize());
    }
    (cops, errors)
}

/// Discover and load in one step. See [`roots`] for how the roots are resolved.
#[must_use]
pub fn discover_and_load(
    config_root: Option<&Path>,
    scan_root: Option<&Path>,
    extra: &[String],
    registry: &CopRegistry,
) -> (UserCops, Vec<IrError>) {
    let roots = roots(config_root, scan_root, extra);
    let mut errors = Vec::new();
    // A `CustomCopPaths` entry that resolves to nothing is a typo, not an
    // optional directory; the implicit `.nitrocop/cops` root (index 0) is the
    // one that may legitimately be absent.
    for root in roots.iter().skip(1).filter(|r| !r.exists()) {
        errors.push(io_error(
            &root.display().to_string(),
            "AllCops.CustomCopPaths entry does not exist",
        ));
    }
    let (cops, load_errors) = load(&discover(&roots), registry);
    errors.extend(load_errors);
    (cops, errors)
}

fn io_error(origin: &str, message: &str) -> IrError {
    IrError {
        origin: origin.to_string(),
        line: None,
        column: None,
        kind: IrErrorKind::Io,
        message: message.to_string(),
    }
}

/// A collision error, pointed at the document's own `cop:` line.
fn collision(origin: &str, source: &str, message: &str) -> IrError {
    IrError {
        origin: origin.to_string(),
        line: source
            .lines()
            .position(|l| l.trim_start().starts_with("cop:"))
            .map(|i| i + 1),
        column: None,
        kind: IrErrorKind::Duplicate,
        message: message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"
schema: 1
cop: "Custom/NoBaseTransaction"
matchers:
  base_transaction:
    pattern: "(send (const nil? :Base) :transaction)"
hooks:
  - on: [send]
    match: base_transaction
    offense:
      location: node
      message: "Do not call `Base.transaction`."
"#;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nitrocop_discover_{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn walks_nested_directories_and_ignores_other_files() {
        let dir = tmp("walk");
        write(&dir, ".nitrocop/cops/a.cop.yml", DOC);
        write(&dir, ".nitrocop/cops/nested/b.cop.yml", DOC);
        write(&dir, ".nitrocop/cops/README.md", "not a cop");
        write(&dir, ".nitrocop/cops/c.yml", "not a cop either");
        let found = discover(&roots(Some(&dir), None, &[]));
        assert_eq!(found.len(), 2);
        assert!(found[0].ends_with("a.cop.yml"));
        assert!(found[1].ends_with("nested/b.cop.yml"));
    }

    #[test]
    fn missing_directory_is_not_an_error() {
        let dir = tmp("missing");
        let registry = CopRegistry::new();
        let (cops, errors) = discover_and_load(Some(&dir), None, &[], &registry);
        assert!(cops.is_empty());
        assert!(errors.is_empty());
        assert_eq!(cops.digest(), "");
    }

    #[test]
    fn explicit_paths_are_resolved_against_the_config_root() {
        let dir = tmp("explicit");
        write(&dir, "shared/cops/a.cop.yml", DOC);
        let registry = CopRegistry::new();
        let (cops, errors) =
            discover_and_load(Some(&dir), None, &["shared/cops".to_string()], &registry);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(cops.names, ["Custom/NoBaseTransaction"]);
    }

    #[test]
    fn a_missing_explicit_path_is_an_error() {
        let dir = tmp("explicit_missing");
        let registry = CopRegistry::new();
        let (_, errors) = discover_and_load(Some(&dir), None, &["nope".to_string()], &registry);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, IrErrorKind::Io);
    }

    #[test]
    fn duplicate_names_across_files_collide() {
        let dir = tmp("dupe");
        write(&dir, ".nitrocop/cops/a.cop.yml", DOC);
        write(&dir, ".nitrocop/cops/b.cop.yml", DOC);
        let registry = CopRegistry::new();
        let (cops, errors) = discover_and_load(Some(&dir), None, &[], &registry);
        assert_eq!(cops.names.len(), 1);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, IrErrorKind::Duplicate);
        assert_eq!(errors[0].line, Some(3), "points at the `cop:` line");
    }

    #[test]
    fn a_built_in_name_collides() {
        let dir = tmp("builtin_collision");
        write(&dir, ".nitrocop/cops/a.cop.yml", DOC);
        let mut registry = CopRegistry::new();
        crate::cop::style::register_all(&mut registry);
        // `Custom/` is fine; squatting an existing *name* is not.
        let (cops, errors) = discover_and_load(Some(&dir), None, &[], &registry);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(cops.names.len(), 1);

        let dir = tmp("builtin_collision2");
        write(
            &dir,
            ".nitrocop/cops/a.cop.yml",
            &DOC.replace("Custom/NoBaseTransaction", "Custom/TimeNow"),
        );
        let mut registry = CopRegistry::new();
        crate::cop::ir::embedded::register_all(&mut registry);
        // Rename the embedded cop's department to match so the names collide.
        let renamed = DOC.replace("Custom/NoBaseTransaction", "Style/TimeNow");
        write(&dir, ".nitrocop/cops/b.cop.yml", &renamed);
        let (_, errors) = discover_and_load(Some(&dir), None, &[], &registry);
        // `Style/` is a built-in department, so the loader rejects it first.
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, IrErrorKind::Department);
    }

    #[test]
    fn an_invalid_document_is_reported_but_does_not_stop_the_others() {
        let dir = tmp("invalid");
        write(&dir, ".nitrocop/cops/a.cop.yml", DOC);
        write(
            &dir,
            ".nitrocop/cops/z.cop.yml",
            "schema: 99\ncop: \"Custom/X\"\nhooks: []\n",
        );
        let registry = CopRegistry::new();
        let (cops, errors) = discover_and_load(Some(&dir), None, &[], &registry);
        assert_eq!(cops.names, ["Custom/NoBaseTransaction"]);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, IrErrorKind::UnsupportedSchema);
    }

    #[test]
    fn the_digest_tracks_document_contents() {
        let dir = tmp("digest");
        write(&dir, ".nitrocop/cops/a.cop.yml", DOC);
        let registry = CopRegistry::new();
        let (before, _) = discover_and_load(Some(&dir), None, &[], &registry);
        write(
            &dir,
            ".nitrocop/cops/a.cop.yml",
            &DOC.replace("Do not", "Never"),
        );
        let (after, _) = discover_and_load(Some(&dir), None, &[], &registry);
        assert!(!before.digest().is_empty());
        assert_ne!(before.digest(), after.digest());
    }
}
