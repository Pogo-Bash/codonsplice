//! #21: SELECT/ORDER BY/LIMIT on CALL header must be a clean compile error, not
//! a runtime "expected cursor, got record" crash.
use codonsplice_core::compile;

#[test]
fn limit_on_header_is_a_compile_error() {
    let e = compile(r#"FROM bam "x.bam" CALL header LIMIT 2"#).unwrap_err();
    let msg = e.to_string();
    assert!(msg.to_lowercase().contains("header"), "names the cause: {msg}");
    assert!(!msg.contains("expected cursor"), "must not leak the VM error: {msg}");
}

#[test]
fn select_on_header_is_a_compile_error() {
    assert!(compile(r#"FROM bam "x.bam" SELECT chr CALL header"#).is_err());
}

#[test]
fn order_by_on_header_is_a_compile_error() {
    assert!(compile(r#"FROM bam "x.bam" CALL header ORDER BY chr"#).is_err());
}

#[test]
fn bare_call_header_still_compiles() {
    assert!(compile(r#"FROM bam "x.bam" CALL header"#).is_ok());
}
