//! Cop IR — declarative cop definitions loaded from `*.cop.yml`.
//!
//! This module provides the v1 document schema ([`schema`]), its fail-closed
//! loader ([`load`]), the typed expression compiler ([`expr`]), its evaluator
//! ([`eval`]), the [`Cop`](crate::cop::Cop) implementation ([`cop`]) and the
//! table of documents shipped inside the binary ([`embedded`]). User-cop
//! discovery (design §4.1 items 2-4) lands in a later PR.
//!
//! See `docs/COP_IR.md` for the schema and expression reference.

pub mod cop;
pub mod embedded;
pub mod eval;
pub mod expr;
pub mod load;
pub mod schema;

pub use cop::IrCopRunner;
pub use eval::{EvalCtx, Value, eval};
pub use expr::{CompileCtx, CompiledDoc, CompiledHook, Expr, compile};
pub use load::{
    IrCop, IrError, IrErrorKind, LoadMode, load_path, load_path_with, load_str, load_str_with,
};
pub use schema::{IrDocument, SCHEMA_VERSION};
