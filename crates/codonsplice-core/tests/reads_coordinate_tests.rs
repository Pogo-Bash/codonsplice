//! #19/#20: CALL reads must report 1-based positions (== samtools POS) and a
//! single-base `pos>=D AND pos<=D` window must return reads whose 1-based start == D.
use std::path::PathBuf;
use codonsplice_core::{compile, Vm, VmOutput, Record, RuntimeValue};
use codonsplice_core::vm::records_to_json;

fn bam() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../cnvlens/public/sample-data/NA12878_EGFR.bam")
        .to_string_lossy().into_owned()
}
fn run(src: &str) -> Vec<Record> {
    let program = compile(src).expect("compiles");
    match Vm::new(program).run().expect("runs") {
        VmOutput::Records(r) | VmOutput::Rows(r) => r,
        other => panic!("expected records, got {other:?}"),
    }
}
fn positions(recs: &[Record]) -> Vec<i64> {
    recs.iter().map(|r| match r.get_field("pos") {
        RuntimeValue::Int(n) => n,
        v => panic!("pos not int: {v:?}"),
    }).collect()
}

#[test]
fn reads_pos_is_one_based_matches_samtools() {
    let src = format!(
        "FROM bam \"{}\" WHERE chr=\"7\" AND pos>=55086040 AND pos<=55086060 CALL reads",
        bam()
    );
    let mut got = positions(&run(&src));
    got.sort();
    assert_eq!(got, vec![55086042, 55086051, 55086051], "reads.pos must equal 1-based SAM POS");
}

#[test]
fn reads_json_display_path_is_one_based() {
    // #19: the JSON/display sink (records_to_json -> CLI render, INTO json/tsv/fasta)
    // must agree with get_field / INTO vcf and report 1-based SAM POS.
    let src = format!(
        "FROM bam \"{}\" WHERE chr=\"7\" AND pos>=55086040 AND pos<=55086060 CALL reads",
        bam()
    );
    let json = records_to_json(&run(&src));
    assert!(json.contains("55086042"), "json must show 1-based pos 55086042: {json}");
    assert!(json.contains("55086051"), "json must show 1-based pos 55086051: {json}");
    assert!(
        !json.contains("55086050"),
        "json must NOT show 0-based pos 55086050: {json}"
    );
}

#[test]
fn single_base_window_returns_reads_at_that_start() {
    let src = format!(
        "FROM bam \"{}\" WHERE chr=\"7\" AND pos>=55086051 AND pos<=55086051 CALL reads",
        bam()
    );
    assert_eq!(run(&src).len(), 2, "single-base window must not silently drop edge reads");
}
