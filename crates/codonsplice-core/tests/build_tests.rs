//! Phase 5 — `.spq.bc` bytecode serialization roundtrip.

use codonsplice_core::{compile, BytecodeError, Program};

#[test]
fn roundtrip_preserves_program() {
    // A query exercising Int, Float, and Str constants.
    let program = compile(
        r#"FROM bam "s.bam" WHERE depth > 30 AND af > 0.5 AND chr = "7" CALL variants WITH min_af = 0.05"#,
    )
    .unwrap();

    let bytes = program.to_bytes();
    let back = Program::from_bytes(&bytes).unwrap();

    assert_eq!(program.code, back.code);
    assert_eq!(program.consts.len(), back.consts.len());
    for (a, b) in program.consts.iter().zip(back.consts.iter()) {
        assert_eq!(a, b);
    }
    assert_eq!(program.debug.len(), back.debug.len());
}

#[test]
fn each_value_type_roundtrips() {
    use codonsplice_core::Value;
    // Build a program by hand-roundtripping a known const pool through bytecode.
    let program = compile(r#"FROM bam "s.bam" WHERE depth > 30 AND af > 1.5 CALL variants"#).unwrap();
    let back = Program::from_bytes(&program.to_bytes()).unwrap();
    // Int and Float constants survive bit-exact.
    assert!(back.consts.iter().any(|v| matches!(v, Value::Int(30))));
    assert!(back
        .consts
        .iter()
        .any(|v| matches!(v, Value::Float(x) if (*x - 1.5).abs() < 1e-12)));
}

#[test]
fn invalid_magic_rejected() {
    let err = Program::from_bytes(b"NOPE\x01\x00\x00\x00\x00").unwrap_err();
    assert!(matches!(err, BytecodeError::InvalidMagic), "got {err:?}");
}

#[test]
fn truncated_rejected() {
    let program = compile(r#"FROM bam "s.bam" CALL variants"#).unwrap();
    let bytes = program.to_bytes();
    let err = Program::from_bytes(&bytes[..bytes.len() - 4]).unwrap_err();
    assert!(matches!(err, BytecodeError::Truncated), "got {err:?}");
}

#[test]
fn empty_input_rejected() {
    assert!(matches!(
        Program::from_bytes(&[]).unwrap_err(),
        BytecodeError::Truncated
    ));
}
