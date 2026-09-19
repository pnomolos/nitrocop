//! Cop IR — declarative cop definitions loaded from `*.cop.yml`.
//!
//! This module provides the v1 document schema ([`schema`]), its fail-closed
//! loader ([`load`]), the typed expression compiler ([`expr`]), its evaluator
//! ([`eval`]), the [`Cop`](crate::cop::Cop) implementation ([`cop`]) and the
//! table of documents shipped inside the binary ([`embedded`]). User-supplied
//! cops are found by [`discover`] (design §4.1 items 2 and 4).
//!
//! See `docs/COP_IR.md` for the schema and expression reference.

pub mod cop;
pub mod discover;
pub mod embedded;
pub mod eval;
pub mod expr;
pub mod load;
pub mod schema;

pub use cop::IrCopRunner;
pub use discover::UserCops;
pub use eval::{EvalCtx, Value, eval};
pub use expr::{CompileCtx, CompiledDoc, CompiledHook, Expr, compile};
pub use load::{
    IrCop, IrError, IrErrorKind, LoadMode, load_path, load_path_with, load_str, load_str_with,
};
pub use schema::{IrDocument, SCHEMA_VERSION};
