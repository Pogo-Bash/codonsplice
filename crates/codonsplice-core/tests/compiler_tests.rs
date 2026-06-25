//! Integration tests for the Phase 3 bytecode compiler and VM.
//!
//! Groups mirror the Phase 3 spec:
//!   1. Correct lowering           4. VM expression execution
//!   2. WITH validation            5. Disassembler
//!   3. Constant pool              6. Error display / did-you-mean

use codonsplice_core::{
    compile, compile_and_disassemble, did_you_mean, eval_expr, suggest_param, CompileError, OpCode,
    Program, RuntimeValue, Value, Vm, VmError,
};

// ── Helpers ──────────────────────────────────────────────────────────────────

fn ok(src: &str) -> Program {
    compile(src).unwrap_or_else(|e| panic!("expected compile to succeed for {src:?}: {e}"))
}

fn err(src: &str) -> CompileError {
    compile(src).expect_err(&format!("expected compile error for {src:?}"))
}

/// Decode the main code section (offset 0 up to and including the first HALT)
/// into a list of opcodes, skipping operand bytes.
fn main_opcodes(p: &Program) -> Vec<OpCode> {
    let mut ops = Vec::new();
    let mut pc = 0;
    while pc < p.code.len() {
        let op = OpCode::from_byte(p.code[pc]).expect("valid opcode");
        ops.push(op);
        pc += 1 + op.operand_len();
        if op == OpCode::Halt {
            break;
        }
    }
    ops
}

/// Decode opcodes in an arbitrary byte range (e.g. a predicate sub-program).
fn opcodes_in(p: &Program, start: usize, len: usize) -> Vec<OpCode> {
    let mut ops = Vec::new();
    let mut pc = start;
    while pc < start + len {
        let op = OpCode::from_byte(p.code[pc]).expect("valid opcode");
        ops.push(op);
        pc += 1 + op.operand_len();
    }
    ops
}

/// Read the FILTER instruction's (pred_off, pred_len) from the main section.
fn filter_operands(p: &Program) -> Option<(usize, usize)> {
    let mut pc = 0;
    while pc < p.code.len() {
        let op = OpCode::from_byte(p.code[pc]).unwrap();
        if op == OpCode::Filter {
            let off = u16::from_le_bytes([p.code[pc + 1], p.code[pc + 2]]) as usize;
            let len = u16::from_le_bytes([p.code[pc + 3], p.code[pc + 4]]) as usize;
            return Some((off, len));
        }
        if op == OpCode::Halt {
            break;
        }
        pc += 1 + op.operand_len();
    }
    None
}

// ── Group 1 — Correct lowering ───────────────────────────────────────────────

#[test]
fn basic_call_variants_exact_bytecode() {
    let p = ok(r#"FROM bam "sample.bam" CALL variants"#);
    // OPEN_SOURCE bam, "sample.bam"(idx 0); SCAN; CALL_VARIANTS; HALT
    assert_eq!(
        p.code,
        vec![
            0x40, 0x00, 0x00, 0x00, // OPEN_SOURCE fmt=bam path=0
            0x41, // SCAN
            0x50, // CALL_VARIANTS
            0x4F, // HALT
        ]
    );
    assert_eq!(p.consts, vec![Value::Str("sample.bam".into())]);
}

#[test]
fn call_cnv_with_params_emits_set_param_then_call() {
    let p = ok(r#"FROM bam "t.bam" CALL cnv WITH window_size = 10000, amp_threshold = 1.5"#);
    let ops = main_opcodes(&p);
    assert_eq!(
        ops,
        vec![
            OpCode::OpenSource,
            OpCode::Scan,
            OpCode::LoadConst, // window_size value
            OpCode::SetParam,
            OpCode::LoadConst, // amp_threshold value
            OpCode::SetParam,
            OpCode::CallCnv,
            OpCode::Halt,
        ]
    );
    // window_size stays an int; amp_threshold is coerced/kept float.
    assert!(p.consts.contains(&Value::Int(10000)));
    assert!(p.consts.contains(&Value::Float(1.5)));
}

#[test]
fn where_wraps_predicate_correctly() {
    let p = ok(r#"FROM bam "x.bam" WHERE depth > 30 CALL variants"#);
    let ops = main_opcodes(&p);
    assert_eq!(
        ops,
        vec![
            OpCode::OpenSource,
            OpCode::Scan,
            OpCode::Filter,
            OpCode::CallVariants,
            OpCode::Halt,
        ]
    );
    // The predicate sub-program lives after HALT and is: depth > 30 ; RET_PRED
    let (off, len) = filter_operands(&p).expect("FILTER present");
    assert_eq!(off, p.code.iter().position(|&b| b == 0x4F).unwrap() + 1); // right after HALT
    assert_eq!(
        opcodes_in(&p, off, len),
        vec![
            OpCode::LoadField, // depth
            OpCode::LoadConst, // 30
            OpCode::Gt,
            OpCode::RetPred,
        ]
    );
}

#[test]
fn into_emits_write_into_before_halt() {
    let p = ok(r#"FROM vcf "i.vcf" CALL variants INTO vcf "o.vcf""#);
    let ops = main_opcodes(&p);
    assert_eq!(ops.last(), Some(&OpCode::Halt));
    let n = ops.len();
    assert_eq!(ops[n - 2], OpCode::WriteInto); // WRITE_INTO immediately before HALT
}

#[test]
fn select_star_emits_wildcard_in_projection() {
    let p = ok(r#"FROM bam "x.bam" SELECT * CALL variants"#);
    // PROJECT appears in the main section...
    assert!(main_opcodes(&p).contains(&OpCode::Project));
    // ...and a LOAD_WILDCARD lives in the projection sub-program after HALT.
    assert!(
        p.code.contains(&OpCode::LoadWildcard.byte()),
        "expected LOAD_WILDCARD byte in projection region"
    );
}

#[test]
fn all_five_call_operations_emit_correct_opcode() {
    for (op, want) in [
        ("variants", OpCode::CallVariants),
        ("cnv", OpCode::CallCnv),
        ("coverage", OpCode::CallCoverage),
        ("reads", OpCode::CallReads),
        ("header", OpCode::CallHeader),
    ] {
        let p = ok(&format!(r#"FROM bam "x.bam" CALL {op}"#));
        assert!(main_opcodes(&p).contains(&want), "for CALL {op}");
    }
}

// ── Group 2 — WITH validation ────────────────────────────────────────────────

#[test]
fn unknown_param_is_rejected() {
    let e = err(r#"FROM bam "x.bam" CALL variants WITH min_freq = 0.05"#);
    match e {
        CompileError::UnknownParam { key, .. } => assert_eq!(key, "min_freq"),
        other => panic!("expected UnknownParam, got {other:?}"),
    }
}

#[test]
fn wrong_param_type_is_rejected() {
    // min_allele_freq expects a float; a string is a type mismatch.
    let e = err(r#"FROM bam "x.bam" CALL variants WITH min_allele_freq = "high""#);
    match e {
        CompileError::ParamTypeMismatch { key, expected, got, .. } => {
            assert_eq!(key, "min_allele_freq");
            assert_eq!(expected, "float");
            assert_eq!(got, "string");
        }
        other => panic!("expected ParamTypeMismatch, got {other:?}"),
    }
}

#[test]
fn with_without_call_is_rejected() {
    let e = err(r#"FROM bam "x.bam" WITH window_size = 10000"#);
    assert!(matches!(e, CompileError::ParamWithoutCall { .. }), "got {e:?}");
}

#[test]
fn non_constant_param_is_rejected() {
    let e = err(r#"FROM bam "x.bam" CALL cnv WITH window_size = depth"#);
    assert!(matches!(e, CompileError::NonConstantParam { .. }), "got {e:?}");
}

#[test]
fn valid_params_for_each_call_op_accepted() {
    assert!(compile(
        r#"FROM bam "x.bam" CALL cnv WITH window_size = 10000, amp_threshold = 1.5, del_threshold = 0.5, min_windows = 3, segmentation_method = "cbs_lite""#
    )
    .is_ok());
    assert!(compile(
        r#"FROM bam "x.bam" CALL variants WITH min_depth = 10, min_base_quality = 20, min_mapping_quality = 20, min_variant_reads = 3, min_allele_freq = 0.05, min_strand_bias = 0.1"#
    )
    .is_ok());
}

#[test]
fn int_coerces_to_float_param() {
    // amp_threshold is f64; an IntLit should coerce, not error.
    let p = ok(r#"FROM bam "x.bam" CALL cnv WITH amp_threshold = 2"#);
    assert!(p.consts.contains(&Value::Float(2.0)));
}

#[test]
fn negative_constant_param_is_allowed() {
    // del_threshold = -0.3 ; unary minus is const-folded.
    let p = ok(r#"FROM bam "x.bam" CALL cnv WITH del_threshold = -0.3"#);
    assert!(p.consts.contains(&Value::Float(-0.3)));
}

// ── Group 3 — Constant pool ──────────────────────────────────────────────────

#[test]
fn identical_constants_are_interned_once() {
    let p = ok(r#"FROM bam "x.bam" WHERE a = 5 OR b = 5 CALL variants"#);
    let fives = p.consts.iter().filter(|v| **v == Value::Int(5)).count();
    assert_eq!(fives, 1, "Int(5) should be interned exactly once");
}

#[test]
fn strings_ints_floats_intern() {
    let p = ok(r#"FROM bam "x.bam" WHERE a = "z" AND b = "z" AND c = 1.5 AND d = 1.5 CALL variants"#);
    assert_eq!(
        p.consts.iter().filter(|v| **v == Value::Str("z".into())).count(),
        1
    );
    assert_eq!(
        p.consts.iter().filter(|v| **v == Value::Float(1.5)).count(),
        1
    );
    // distinct field names a,b,c,d are interned separately
    for name in ["a", "b", "c", "d"] {
        assert!(p.consts.contains(&Value::Str(name.into())), "missing {name}");
    }
}

// ── Group 4 — VM expression execution ────────────────────────────────────────

#[test]
fn vm_arithmetic_precedence() {
    // Precedence is already encoded in the AST: 2 + 3 * 4 = 14
    assert_eq!(eval_expr("2 + 3 * 4").unwrap(), RuntimeValue::Int(14));
}

#[test]
fn vm_comparison() {
    assert_eq!(eval_expr("5 > 3").unwrap(), RuntimeValue::Bool(true));
    assert_eq!(eval_expr("5 <= 3").unwrap(), RuntimeValue::Bool(false));
}

#[test]
fn vm_and_short_circuits() {
    // `depth` would be a LOAD_FIELD (Phase-4 NotYetImplemented); short-circuit
    // means it is never executed, so this evaluates to false cleanly.
    assert_eq!(eval_expr("false AND depth").unwrap(), RuntimeValue::Bool(false));
}

#[test]
fn vm_or_short_circuits() {
    assert_eq!(eval_expr("true OR depth").unwrap(), RuntimeValue::Bool(true));
}

#[test]
fn vm_not() {
    assert_eq!(eval_expr("NOT true").unwrap(), RuntimeValue::Bool(false));
}

#[test]
fn vm_pipeline_opcode_is_wired_in_phase4() {
    // Phase 4 implements the pipeline/CALL opcodes. `OPEN_SOURCE` now actually
    // tries to load the file, so a nonexistent path surfaces as an I/O error
    // rather than the old `NotYetImplemented` stub.
    let p = ok(r#"FROM bam "x.bam" CALL variants"#);
    let res = Vm::new(p).run();
    assert!(matches!(res, Err(VmError::Io(_))), "got {res:?}");
}

// ── Group 5 — Disassembler ───────────────────────────────────────────────────

#[test]
fn disassembly_is_non_empty_and_named() {
    let asm = compile_and_disassemble(r#"FROM bam "x.bam" CALL variants"#).unwrap();
    assert!(!asm.is_empty());
    assert!(asm.contains("OPEN_SOURCE"));
    assert!(asm.contains("CALL_VARIANTS"));
    assert!(asm.contains("HALT"));
    // mnemonics, not raw hex bytes for the opcodes
    assert!(!asm.contains("0x50"));
}

#[test]
fn disassembly_exact_for_simple_query() {
    let asm = compile_and_disassemble(r#"FROM vcf "i.vcf" CALL variants"#).unwrap();
    assert_eq!(
        asm,
        "\
0000  OPEN_SOURCE  vcf \"i.vcf\"
0004  SCAN
0005  CALL_VARIANTS
0006  HALT
"
    );
}

#[test]
fn disassembly_of_phase2_example_query() {
    // The Phase 2 design sketch's byte offsets were illustrative; here we check
    // the real disassembly contains the expected instructions and predicate.
    let asm = compile_and_disassemble(
        r#"FROM bam "sample.bam" WHERE depth > 30 CALL cnv WITH window_size = 10000"#,
    )
    .unwrap();
    assert!(asm.contains("OPEN_SOURCE  bam \"sample.bam\""));
    assert!(asm.contains("SCAN"));
    assert!(asm.contains("FILTER       pred@"));
    assert!(asm.contains("SET_PARAM    \"window_size\""));
    assert!(asm.contains("CALL_CNV"));
    assert!(asm.contains("HALT"));
    // predicate section is decoded too
    assert!(asm.contains("; predicate @"));
    assert!(asm.contains("LOAD_FIELD   \"depth\""));
    assert!(asm.contains("GT"));
    assert!(asm.contains("RET_PRED"));
}

// ── Group 6 — Error display / did-you-mean ───────────────────────────────────

#[test]
fn compile_error_render_includes_line_col() {
    let src = "FROM bam \"x.bam\"\nCALL variants\nWITH min_freq = 0.05";
    let e = compile(src).unwrap_err();
    let rendered = e.render(src, suggest_param("min_freq", "variants").as_deref());
    // line:col coordinate and a caret
    assert!(rendered.contains("query:3:"), "rendered:\n{rendered}");
    assert!(rendered.contains('^'), "rendered:\n{rendered}");
    assert!(rendered.contains("did you mean"), "rendered:\n{rendered}");
    // Display form carries the error code + message.
    assert!(e.to_string().contains("E001"));
    assert!(e.to_string().contains("min_freq"));
}

#[test]
fn did_you_mean_suggests_closest_param() {
    assert_eq!(suggest_param("min_freq", "variants").as_deref(), Some("min_allele_freq"));
    assert_eq!(suggest_param("windo_size", "cnv").as_deref(), Some("window_size"));
    // unrelated key for the op → no suggestion
    assert_eq!(suggest_param("zzzzz", "variants"), None);
}

#[test]
fn did_you_mean_basic_edit_distance() {
    assert_eq!(
        did_you_mean("colur", &["color", "colour", "border"]).as_deref(),
        Some("color")
    );
}
