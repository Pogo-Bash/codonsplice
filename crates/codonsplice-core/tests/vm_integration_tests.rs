//! Phase 4 end-to-end VM tests against the bundled NA12878 EGFR sample BAM.
//!
//! The sample (GRCh37 names) carries ~31k reads on contig "7" around EGFR
//! (~55 Mb) and nothing elsewhere, with a co-located `.bai`. The native VM's
//! `FsIo` loads the file by path and auto-detects the sibling index.

use std::path::PathBuf;

use codonsplice_core::vm::record_to_json;
use codonsplice_core::{compile, extract_region, Record, Vm, VmOutput};

fn bam_path() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../cnvlens/public/sample-data/NA12878_EGFR.bam")
        .to_string_lossy()
        .into_owned()
}

/// Compile + run a query, expecting a materialized record stream.
fn run_records(source: &str) -> Vec<Record> {
    let program = compile(source).expect("compile");
    match Vm::new(program).run().expect("run") {
        VmOutput::Records(r) => r,
        other => panic!("expected Records, got {other:?}"),
    }
}

fn q(body: &str) -> String {
    format!("FROM bam \"{}\" {body}", bam_path())
}

fn field_f64(r: &Record, name: &str) -> Option<f64> {
    record_to_json(r).get(name).and_then(|v| v.as_f64())
}

fn field_i64(r: &Record, name: &str) -> Option<i64> {
    record_to_json(r).get(name).and_then(|v| v.as_i64())
}

// ── Group 1 — BAI seeking / region extraction ────────────────────────────────

#[test]
fn region_is_statically_extracted_from_where() {
    let program = compile(&q(r#"WHERE chr = "7" CALL variants"#)).unwrap();
    let region = program.region.expect("region extracted");
    assert_eq!(region.chrom, "7");
    assert_eq!(region.start, None);
    assert_eq!(region.end, None);
}

#[test]
fn region_extracts_position_bounds() {
    let r = extract_region(
        &spliceql::parse(&q(r#"WHERE chr = "7" AND pos >= 55000000 AND pos <= 55300000 CALL variants"#))
            .unwrap()
            .filter
            .unwrap(),
    )
    .unwrap();
    assert_eq!(r.chrom, "7");
    assert_eq!(r.start, Some(55_000_000));
    assert_eq!(r.end, Some(55_300_000));
}

#[test]
fn non_region_predicate_extracts_nothing() {
    let program = compile(&q("WHERE depth > 30 CALL variants")).unwrap();
    assert!(program.region.is_none());
}

#[test]
fn region_query_runs_and_returns_chr7_variants() {
    // WHERE chr="7" must seek via BAI and still produce variants.
    let recs = run_records(&q(r#"WHERE chr = "7" CALL variants"#));
    assert!(!recs.is_empty(), "expected variants on chr7");
    for r in &recs {
        assert_eq!(record_to_json(r)["chrom"], "7");
    }
}

// ── Group 2 — Streaming records ──────────────────────────────────────────────

#[test]
fn call_variants_streams_records() {
    let recs = run_records(&q("CALL variants"));
    assert!(!recs.is_empty());
    assert!(matches!(recs[0], Record::Variant(_)));
}

#[test]
fn call_cnv_streams_coverage_windows() {
    let recs = run_records(&q(r#"WHERE chr = "7" CALL cnv WITH window_size = 10000"#));
    assert!(!recs.is_empty());
    assert!(recs.iter().all(|r| matches!(r, Record::CoverageWindow(_))));
}

// ── Group 3 — per-record WHERE predicate ─────────────────────────────────────

#[test]
fn where_depth_filters_alignment_stream() {
    let all = run_records(&q(r#"WHERE chr = "7" CALL reads"#));
    let deep = run_records(&q(r#"WHERE chr = "7" AND depth > 30 CALL reads"#));
    assert!(!all.is_empty());
    assert!(deep.len() < all.len(), "depth filter must drop some reads");
    assert!(!deep.is_empty(), "EGFR has well-covered positions");
    for r in &deep {
        assert!(field_i64(r, "depth").unwrap() > 30);
    }
}

#[test]
fn where_af_band_filters_variant_stream() {
    let banded = run_records(&q(r#"WHERE af > 0.1 AND af < 0.9 CALL variants"#));
    // The band may be empty depending on the sample, but every survivor obeys it.
    for r in &banded {
        let af = field_f64(r, "allele_freq").unwrap();
        assert!(af > 0.1 && af < 0.9, "af {af} out of band");
    }
    // It must be a strict subset of the unfiltered set.
    let unfiltered = run_records(&q("CALL variants"));
    assert!(banded.len() <= unfiltered.len());
}

#[test]
fn where_not_strand_bias_evaluates_per_record() {
    let kept = run_records(&q("WHERE NOT strand_bias > 0.3 CALL variants"));
    for r in &kept {
        assert!(field_f64(r, "strand_bias").unwrap() <= 0.3);
    }
}

// ── Group 4 — full pipeline integration ──────────────────────────────────────

#[test]
fn vm_variants_match_direct_cnvlens_call() {
    use cnvlens_core::model::VariantOptions;
    let bytes = std::fs::read(bam_path()).unwrap();
    let direct = cnvlens_core::variants::collect_variants(&bytes, None, &VariantOptions::default())
        .unwrap();
    let vm = run_records(&q("CALL variants"));
    assert_eq!(
        vm.len(),
        direct.len(),
        "VM variant count must match a direct cnvlens-core call"
    );
}

#[test]
fn limit_truncates_record_stream() {
    let limited = run_records(&q("CALL variants LIMIT 3"));
    assert!(limited.len() <= 3);
}

#[test]
fn write_into_roundtrip_bed() {
    let tmp = std::env::temp_dir().join(format!("cs_rt_{}.bed", std::process::id()));
    let tmp_s = tmp.to_string_lossy().into_owned();

    // Run once to count windows, once to write them out.
    let windows = run_records(&q(r#"WHERE chr = "7" CALL cnv WITH window_size = 50000"#));
    let program = compile(&q(&format!(
        r#"WHERE chr = "7" CALL cnv WITH window_size = 50000 INTO bed "{tmp_s}""#
    )))
    .unwrap();
    match Vm::new(program).run().unwrap() {
        VmOutput::Text(summary) => assert!(summary.contains("wrote")),
        other => panic!("expected write summary, got {other:?}"),
    }

    let written = std::fs::read_to_string(&tmp).unwrap();
    let line_count = written.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(line_count, windows.len(), "every window must be written");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn call_header_returns_reference_text() {
    let program = compile(&q("CALL header")).unwrap();
    match Vm::new(program).run().unwrap() {
        VmOutput::Text(t) => {
            assert!(t.contains('7'), "header should list contig 7");
            assert!(t.contains("reference sequences"));
        }
        other => panic!("expected header text, got {other:?}"),
    }
}
