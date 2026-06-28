//! **codonsplice-core** — the CodonSplice engine.
//!
//! This crate is the second half of the two-crate architecture:
//!
//! ```text
//! spliceql (language)        →  codonsplice-core (engine)
//! Lexer → Parser → AST       →  Compiler → Bytecode → VM
//! ```
//!
//! It takes a [`spliceql`] AST and lowers it to stack-machine [`Program`]
//! bytecode ([`compiler`]), then executes that bytecode in a [`Vm`].  Phase 3
//! implements the compiler and a VM that fully evaluates expressions; the
//! pipeline opcodes that touch genomic data stub out until the Phase 4
//! `cnvlens-core` bridge.
//!
//! # Example
//!
//! ```
//! let asm = codonsplice_core::compile_and_disassemble(
//!     r#"FROM bam "s.bam" CALL variants"#,
//! ).unwrap();
//! assert!(asm.contains("OPEN_SOURCE"));
//! assert!(asm.contains("CALL_VARIANTS"));
//! ```

pub mod bytecode;
pub mod compiler;
pub mod materialize;
pub mod runtime;
pub mod shard;
pub mod vm;

pub use compiler::{
    did_you_mean, disassemble, extract_region, is_builtin, levenshtein, param_names_for,
    suggest_function, suggest_param, CompileError, Compiler, DebugInfo, OpCode, Program, Value,
};
pub use bytecode::BytecodeError;
pub use materialize::{materialize, materialize_streaming};
pub use runtime::{
    AlnRow, Cursor, Dataset, DatasetInner, ProjItem, QueryOptions, Record, Region, RuntimeValue,
    VarMap,
};
pub use vm::{Io, Vm, VmError, VmOutput};

use spliceql::ast::Query;

/// Parse and compile `source` into a [`Program`].
pub fn compile(source: &str) -> Result<Program, CompileError> {
    spliceql::parse(source)
        .map_err(Into::into)
        .and_then(|q| Compiler::new(source).compile(&q))
}

/// Compile `source` and return its disassembly — the workhorse for the TUI's
/// bytecode view.
pub fn compile_and_disassemble(source: &str) -> Result<String, CompileError> {
    compile(source).map(|p| disassemble(&p))
}

/// Compile and run `source`.  In Phase 3 the first pipeline opcode of a real
/// query stubs out as [`VmError::NotYetImplemented`]; this is wired to
/// `cnvlens-core` in Phase 4.
pub fn execute(source: &str) -> Result<VmOutput, VmError> {
    let program = compile(source).map_err(|e| VmError::NotYetImplemented(e.to_string()))?;
    Vm::new(program).run()
}

/// Compile a standalone expression (not a full query) into an expression-only
/// [`Program`] ending in `HALT`.  Used by the REPL/tests to exercise the VM's
/// expression interpreter directly.
///
/// The expression is parsed by wrapping it as a `WHERE` predicate; only the
/// predicate sub-program is returned, relocated to offset 0.
pub fn compile_expr(expr_source: &str) -> Result<Program, CompileError> {
    let wrapped = format!("FROM bam \"_\" WHERE {expr_source}");
    let query: Query = spliceql::parse(&wrapped)?;
    let expr = query
        .filter
        .as_ref()
        .expect("wrapped query always has a WHERE filter");
    Ok(Compiler::new(expr_source).compile_expr_program(expr))
}

/// Evaluate a constant expression and return the resulting [`RuntimeValue`].
/// Convenience wrapper over [`compile_expr`] + [`Vm::eval_expr`].
pub fn eval_expr(expr_source: &str) -> Result<RuntimeValue, EvalError> {
    let program = compile_expr(expr_source).map_err(EvalError::Compile)?;
    Vm::new(program).eval_expr().map_err(EvalError::Vm)
}

/// Error from [`eval_expr`]: either compilation or execution failed.
#[derive(Debug)]
pub enum EvalError {
    Compile(CompileError),
    Vm(VmError),
}

impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EvalError::Compile(e) => write!(f, "{e}"),
            EvalError::Vm(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for EvalError {}
