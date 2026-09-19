//! NodePattern DSL support — lexer, parser, mapping table, and interpreter.
//!
//! This module extracts the shared infrastructure from the `node_pattern_codegen`
//! binary into reusable library code. The codegen binary imports from here.

pub mod ancestors;
pub mod captures;
pub mod extract;
pub mod interpreter;
pub mod lexer;
pub mod mapping;
pub mod parser;
pub mod pattern_db;
pub mod predicates;
pub mod resolve;

pub use captures::{CaptureValue, Captures, MatchEnv};
pub use extract::{
    ExtractedPattern, PatternKind, cop_name_from_path, extract_patterns, walk_vendor_patterns,
};
pub use interpreter::{
    CompiledPattern, Unresolved, collect_unresolved, interpret_pattern, match_with_captures,
};
pub use lexer::{Lexer, Token};
pub use mapping::{NodeMapping, build_mapping_table};
pub use parser::{Parser, PatternError, PatternNode, pattern_summary};
pub use predicates::{Arg, Arity, Builtin, PredCtx, PredTarget};
pub use resolve::{NoResolver, Params, Resolver};
