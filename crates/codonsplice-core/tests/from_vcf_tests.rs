//! Track 0A — `FROM vcf` as INPUT: the regression suite for the VCF gateway
//! that downstream annotation (Track 1) depends on.
//!
//! Covers the supported surface end-to-end through the compiler + VM:
//!   * plain `.vcf` load (`FROM vcf "x" CALL variants`),
//!   * `.vcf.gz` load via the in-Rust BGZF inflater (no external tool),
//!   * `WHERE` / `SELECT` / `ORDER BY` / `LIMIT` composition on VCF input,
//!   * `INTO vcf` round-trip preservation.
//!
//! Fixtures live under `tests/data/` and are addressed by an absolute path from
//! `CARGO_MANIFEST_DIR`, so the suite is self-contained (it does not depend on
//! the large, untracked repo-root data files).

use std::path::PathBuf;

use codonsplice_core::vm::record_to_json;
use codonsplice_core::{compile, Vm, VmOutput};
use serde_json::Value;

/// Absolute path to a fixture under `tests/data/`.
fn fixture(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

/// Compile + run a query, returning the emitted records as JSON values.
fn run_records(src: &str) -> Vec<Value> {
    let program = compile(src).unwrap_or_else(|e| panic!("compile failed for `{src}`: {e:?}"));
    match Vm::new(program).run().unwrap_or_else(|e| panic!("run failed for `{src}`: {e:?}")) {
        // `CALL_*` with no SELECT -> Records; with a SELECT projection -> Rows.
        VmOutput::Records(recs) | VmOutput::Rows(recs) => recs.iter().map(record_to_json).collect(),
        other => panic!("expected a record stream, got {other:?}"),
    }
}

#[test]
fn plain_vcf_loads_all_variants() {
    let recs = run_records(&format!(
        "FROM vcf \"{}\" CALL variants",
        fixture("egfr_impact.vcf")
    ));
    // Eight data rows in the fixture (7×chr7 + 1×chr12).
    assert_eq!(recs.len(), 8, "plain VCF should pass through every record");
    assert_eq!(recs[0]["chrom"], "7");
    assert_eq!(recs[0]["pos"], 55019021);
    assert_eq!(recs[0]["ref"], "G");
    assert_eq!(recs[0]["alt"], "A");
    assert_eq!(recs[0]["qual"], 60.0);
    assert_eq!(recs[0]["depth"], 100); // INFO/DP
    assert_eq!(recs[0]["allele_freq"], 0.5); // INFO/AF
    assert_eq!(recs[0]["filter"], "PASS");
    assert_eq!(recs[0]["id"], "rs1");
}

#[test]
fn gz_vcf_loads_via_in_rust_bgzf() {
    // No external tool: the .gz is inflated by the in-Rust BGZF reader.
    let recs = run_records(&format!(
        "FROM vcf \"{}\" CALL variants",
        fixture("egfr_impact.vcf.gz")
    ));
    assert_eq!(recs.len(), 8, "gz VCF should yield the same records as the plain VCF");
    assert_eq!(recs[0]["pos"], 55019021);
}

#[test]
fn indel_record_preserved() {
    // The fixture carries an insertion (A -> ATTG); it must survive the read as
    // an INDEL with the full ALT allele intact.
    let recs = run_records(&format!(
        "FROM vcf \"{}\" WHERE pos = 55181378 CALL variants",
        fixture("egfr_impact.vcf")
    ));
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0]["ref"], "A");
    assert_eq!(recs[0]["alt"], "ATTG");
    assert_eq!(recs[0]["type"], "INDEL");
}

#[test]
fn where_filters_on_vcf_input() {
    // chr7 EGFR window 55.14M–55.20M -> 55142285, 55174772, 55181378, 55191822.
    let recs = run_records(&format!(
        "FROM vcf \"{}\" WHERE chr = \"7\" AND pos >= 55140000 AND pos <= 55200000 CALL variants",
        fixture("egfr_impact.vcf")
    ));
    assert_eq!(recs.len(), 4);
    for r in &recs {
        assert_eq!(r["chrom"], "7");
        let p = r["pos"].as_i64().unwrap();
        assert!((55140000..=55200000).contains(&p), "pos {p} out of window");
    }
}

#[test]
fn where_filters_on_af_and_filter_columns() {
    let recs = run_records(&format!(
        "FROM vcf \"{}\" WHERE af >= 0.4 AND filter = \"PASS\" CALL variants",
        fixture("egfr_impact.vcf")
    ));
    // AF>=0.4 & PASS: 0.50(rs1), 0.45(cosm6240), 0.49(rs3) on chr7, 0.35 KRAS excluded.
    assert_eq!(recs.len(), 3);
    assert!(recs.iter().all(|r| r["filter"] == "PASS"));
}

#[test]
fn select_projects_columns_from_vcf() {
    // SpliceQL puts SELECT after FROM. Projected rows carry exactly the chosen
    // columns (canonical VCF fields are all readable).
    let recs = run_records(&format!(
        "FROM vcf \"{}\" SELECT chr, pos, ref, alt, qual, filter, id CALL variants LIMIT 2",
        fixture("egfr_impact.vcf")
    ));
    assert_eq!(recs.len(), 2);
    let keys: Vec<&str> = recs[0].as_object().unwrap().keys().map(|s| s.as_str()).collect();
    assert_eq!(keys, vec!["chr", "pos", "ref", "alt", "qual", "filter", "id"]);
    assert_eq!(recs[0]["id"], "rs1");
    assert_eq!(recs[0]["filter"], "PASS");
}

#[test]
fn order_by_and_limit_compose_on_vcf() {
    let recs = run_records(&format!(
        "FROM vcf \"{}\" CALL variants ORDER BY qual DESC LIMIT 3",
        fixture("egfr_impact.vcf")
    ));
    assert_eq!(recs.len(), 3);
    let quals: Vec<f64> = recs.iter().map(|r| r["qual"].as_f64().unwrap()).collect();
    assert_eq!(quals, vec![999.0, 120.0, 80.0], "ORDER BY qual DESC");
}

#[test]
fn info_fields_beyond_dp_af_are_not_readable() {
    // Documented limitation: only INFO/DP and INFO/AF are mapped (-> depth/af).
    // An arbitrary INFO key (GENE) is not exposed; projecting it yields null.
    let recs = run_records(&format!(
        "FROM vcf \"{}\" SELECT chr, pos, gene CALL variants LIMIT 1",
        fixture("egfr_impact.vcf")
    ));
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0]["gene"], Value::Null, "non-DP/AF INFO must read as null");
}

/// Round-trip: load a VCF, write it back with `INTO vcf`, and reload the output.
/// The canonical identity + the DP/AF INFO must be preserved, and (after the
/// Track 0A fix) the ID and FILTER columns too.
fn round_trip(input: &str) -> Vec<Value> {
    // Unique per call: round-trip tests run in parallel and each removes its
    // file, so a shared name (same pid) would race.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut out = std::env::temp_dir();
    out.push(format!("cs_from_vcf_rt_{}_{}.vcf", std::process::id(), n));
    let write_src = format!(
        "FROM vcf \"{}\" CALL variants INTO vcf \"{}\"",
        fixture(input),
        out.display()
    );
    let program = compile(&write_src).unwrap();
    match Vm::new(program).run().unwrap() {
        VmOutput::Text(_) | VmOutput::Ready(_) => {}
        other => panic!("expected a WRITE_INTO summary, got {other:?}"),
    }
    // Reload the freshly written file through the same gateway.
    let recs = run_records(&format!("FROM vcf \"{}\" CALL variants", out.display()));
    let _ = std::fs::remove_file(&out);
    recs
}

#[test]
fn round_trip_preserves_identity_and_info() {
    let src = run_records(&format!("FROM vcf \"{}\" CALL variants", fixture("egfr_impact.vcf")));
    let rt = round_trip("egfr_impact.vcf");
    assert_eq!(rt.len(), src.len(), "record count must be preserved");
    for (a, b) in src.iter().zip(rt.iter()) {
        assert_eq!(a["chrom"], b["chrom"], "CHROM");
        assert_eq!(a["pos"], b["pos"], "POS");
        assert_eq!(a["ref"], b["ref"], "REF");
        assert_eq!(a["alt"], b["alt"], "ALT");
        assert_eq!(a["qual"], b["qual"], "QUAL");
        assert_eq!(a["depth"], b["depth"], "INFO/DP -> depth");
        assert_eq!(a["allele_freq"], b["allele_freq"], "INFO/AF -> allele_freq");
    }
}

#[test]
fn round_trip_preserves_id_and_filter() {
    // Track 0A gap fix: the native VCF writer used to hardcode ID="." and
    // FILTER="PASS", silently dropping both on a VCF->VCF round-trip. They are
    // captured by the reader, so the writer must emit them.
    let rt = round_trip("egfr_impact.vcf");
    // rs1 / PASS at the first record; cosm6240 / PASS; rs2 / LowQual; etc.
    assert_eq!(rt[0]["id"], "rs1", "ID must survive round-trip");
    assert_eq!(rt[0]["filter"], "PASS");
    // The LowQual record (pos 55142285) must keep its non-PASS FILTER.
    let low = rt
        .iter()
        .find(|r| r["pos"] == 55142285)
        .expect("LowQual record present");
    assert_eq!(low["filter"], "LowQual", "non-PASS FILTER must survive round-trip");
    assert_eq!(low["id"], ".", "absent ID stays '.'");
    // And the rs2 INDEL keeps both a real ID and a non-PASS FILTER.
    let rs2 = rt.iter().find(|r| r["pos"] == 55191822).unwrap();
    assert_eq!(rs2["id"], "rs2");
    assert_eq!(rs2["filter"], "LowQual");
}
