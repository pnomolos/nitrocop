//! Cop IR — declarative cop definitions loaded from `*.cop.yml`.
//!
//! **Experimental and not yet wired into the linter.** This module currently
//! provides only the v1 document schema ([`schema`]) and its fail-closed loader
//! ([`load`]); the expression compiler, the `Cop` implementation, registry
//! integration and user-cop discovery land in later PRs. The only user-visible
//! surface today is `nitrocop --validate-ir <path>...`.
//!
//! See `docs/COP_IR.md` for the schema reference.

pub mod load;
pub mod schema;

pub use load::{
    IrCop, IrError, IrErrorKind, LoadMode, load_path, load_path_with, load_str, load_str_with,
};
pub use schema::{IrDocument, SCHEMA_VERSION};
