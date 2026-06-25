//! Phase 5 — SELECT column projection (Record::Row + VmOutput::Rows).

use std::path::PathBuf;

use codonsplice_core::{compile, Record, RuntimeValue, Vm, VmOutput};

fn bam() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../cnvlens/public/sample-data/NA12878_EGFR.bam")
        .to_string_lossy()
        .into_owned()
}

fn rows(body: &str) -> Vec<Record> {
    let src = format!("FROM bam \"{}\" {body}", bam());
    let program = compile(&src).unwrap();
    match Vm::new(program).run().unwrap() {
        VmOutput::Rows(r) => r,
        other => panic!("expected Rows, got {other:?}"),
    }
}

fn records(body: &str) -> Vec<Record> {
    let src = format!("FROM bam \"{}\" {body}", bam());
    let program = compile(&src).unwrap();
    match Vm::new(program).run().unwrap() {
        VmOutput::Records(r) => r,
        other => panic!("expected Records, got {other:?}"),
    }
}

fn col(r: &Record, name: &str) -> RuntimeValue {
    match r {
        Record::Row(cols) => cols
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .unwrap_or(RuntimeValue::Null),
        _ => panic!("not a Row"),
    }
}

#[test]
fn select_columns_produce_rows() {
    let rs = rows(r#"SELECT chrom, pos WHERE chr = "7" CALL variants LIMIT 3"#);
    assert!(!rs.is_empty());
    for r in &rs {
        assert!(matches!(r, Record::Row(_)));
        assert_eq!(col(r, "chrom"), RuntimeValue::Str("7".into()));
        assert!(matches!(col(r, "pos"), RuntimeValue::Int(_)));
        // Un-selected columns are absent.
        assert_eq!(col(r, "depth"), RuntimeValue::Null);
    }
}

#[test]
fn select_expression_with_alias() {
    let rs = rows(r#"SELECT af * 100 AS pct WHERE chr = "7" CALL variants LIMIT 2"#);
    assert!(!rs.is_empty());
    for r in &rs {
        match col(r, "pct") {
            RuntimeValue::Float(x) => assert!(x >= 0.0 && x <= 100.0),
            other => panic!("pct should be a float, got {other:?}"),
        }
    }
}

#[test]
fn select_star_passes_full_records() {
    // SELECT * is identity → Records, not Rows.
    let rs = records(r#"SELECT * WHERE chr = "7" CALL variants LIMIT 2"#);
    assert!(!rs.is_empty());
    assert!(rs.iter().all(|r| matches!(r, Record::Variant(_))));
}

#[test]
fn projection_on_coverage_windows() {
    let rs = rows(r#"SELECT chrom, coverage WHERE chr = "7" CALL cnv WITH window_size = 50000 LIMIT 3"#);
    assert!(!rs.is_empty());
    for r in &rs {
        assert_eq!(col(r, "chrom"), RuntimeValue::Str("7".into()));
        assert!(matches!(col(r, "coverage"), RuntimeValue::Int(_)));
    }
}
