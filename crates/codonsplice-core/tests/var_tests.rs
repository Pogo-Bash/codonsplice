//! Phase 5 — `$variable` lexing, compilation, and runtime resolution.

use std::path::PathBuf;
use std::sync::Arc;

use codonsplice_core::{compile, compile_and_disassemble, RuntimeValue, VarMap, Vm, VmError, VmOutput};
use spliceql::ast::Expr;
use spliceql::TokenKind;

fn bam() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../cnvlens/public/sample-data/NA12878_EGFR.bam")
        .to_string_lossy()
        .into_owned()
}

#[test]
fn dollar_lexes_as_var_token() {
    let toks = spliceql::tokenize("$min_af").unwrap();
    assert_eq!(toks[0].kind, TokenKind::Var("min_af".to_string()));
}

#[test]
fn parser_produces_var_expr() {
    let q = spliceql::parse(r#"FROM bam "x" WHERE depth > $min"#).unwrap();
    match q.filter.unwrap() {
        Expr::Binary { right, .. } => assert!(matches!(*right, Expr::Var(n, _) if n == "min")),
        other => panic!("expected binary, got {other:?}"),
    }
}

#[test]
fn compiler_emits_load_var() {
    let asm = compile_and_disassemble(r#"FROM bam $bam CALL variants WITH min_af = $af"#).unwrap();
    assert!(asm.contains("LOAD_VAR"), "disassembly:\n{asm}");
}

#[test]
fn varmap_resolves_at_runtime() {
    let mut vars = VarMap::new();
    vars.insert("bam", RuntimeValue::Str(Arc::from(bam().as_str())));
    let program = compile(r#"FROM bam $bam WHERE chr = "7" CALL variants LIMIT 2"#).unwrap();
    match Vm::new(program).with_vars(vars).run().unwrap() {
        VmOutput::Records(recs) => assert!(!recs.is_empty() && recs.len() <= 2),
        other => panic!("expected Records, got {other:?}"),
    }
}

#[test]
fn unbound_variable_errors() {
    let program = compile(r#"FROM bam $bam CALL variants"#).unwrap();
    let err = Vm::new(program).run().unwrap_err();
    assert!(matches!(err, VmError::UnboundVariable { .. }), "got {err:?}");
}

#[test]
fn string_var_coerces_to_float_param() {
    // $af bound as the string "0.5"; the WITH min_af param expects a float and
    // must coerce it. Filter to chr7 so the run is fast.
    let mut vars = VarMap::new();
    vars.insert("bam", RuntimeValue::Str(Arc::from(bam().as_str())));
    vars.insert("af", RuntimeValue::Str(Arc::from("0.5")));
    let program = compile(
        r#"FROM bam $bam WHERE chr = "7" CALL variants WITH min_af = $af LIMIT 5"#,
    )
    .unwrap();
    // Should run without a type error (the Str coerces to f64 at SET_PARAM).
    let out = Vm::new(program).with_vars(vars).run().unwrap();
    assert!(matches!(out, VmOutput::Records(_)));
}
