//! Regression for #1 — a projected `SELECT ... INTO vcf` must serialize the
//! projected columns (custom-FORMAT VCF) instead of silently dropping the rows
//! while still counting them in the `wrote N record(s)` summary.

use std::path::PathBuf;

use codonsplice_core::{compile, Vm, VmOutput};

fn bam() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../cnvlens/public/sample-data/NA12878_EGFR.bam")
        .to_string_lossy()
        .into_owned()
}

fn out_path(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("cs_proj_{}_{}.vcf", tag, std::process::id()));
    p
}

/// Run `body` with an `INTO vcf <tmp>` sink and return the file contents.
fn run_into_vcf(tag: &str, body: &str) -> String {
    let path = out_path(tag);
    let src = format!(
        "FROM bam \"{}\" {body} INTO vcf \"{}\"",
        bam(),
        path.display()
    );
    let program = compile(&src).unwrap();
    match Vm::new(program).run().unwrap() {
        VmOutput::Text(_) | VmOutput::Ready(_) => {}
        other => panic!("expected a WRITE_INTO summary, got {other:?}"),
    }
    let contents = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    contents
}

fn data_rows(vcf: &str) -> Vec<&str> {
    vcf.lines().filter(|l| !l.starts_with('#')).collect()
}

#[test]
fn projected_select_writes_data_rows() {
    // The headline bug: with a custom SELECT the body used to be empty.
    let vcf = run_into_vcf(
        "basic",
        r#"SELECT chr, pos, depth WHERE chr = "7" AND depth >= 20 CALL variants WITH min_af = 0.05"#,
    );
    let rows = data_rows(&vcf);
    assert!(
        !rows.is_empty(),
        "projected SELECT produced an empty VCF body:\n{vcf}"
    );
    // depth is non-canonical, so it must be declared + packed into INFO.
    assert!(
        vcf.contains("##INFO=<ID=depth"),
        "missing INFO declaration for projected column `depth`:\n{vcf}"
    );
    assert!(
        rows.iter().all(|r| r.contains("depth=")),
        "every row should carry the projected INFO column `depth`:\n{vcf}"
    );
}

#[test]
fn canonical_columns_fill_fixed_fields() {
    // chr/pos/ref/alt/qual map to the eight fixed columns; only the computed
    // column lands in INFO.
    let vcf = run_into_vcf(
        "canonical",
        r#"SELECT chr, pos, ref, alt, qual, depth * af AS alt_reads WHERE chr = "7" CALL variants WITH min_af = 0.05 LIMIT 5"#,
    );
    let rows = data_rows(&vcf);
    assert_eq!(rows.len(), 5, "LIMIT 5 should yield 5 rows:\n{vcf}");
    assert!(vcf.contains("##INFO=<ID=alt_reads"));
    for r in &rows {
        let f: Vec<&str> = r.split('\t').collect();
        assert_eq!(f.len(), 8, "expected 8 VCF columns, got {}: {r}", f.len());
        assert_eq!(f[0], "7", "CHROM should be filled from `chr`");
        assert_ne!(f[1], ".", "POS should be filled from `pos`");
        assert!(f[7].starts_with("alt_reads="), "INFO should carry alt_reads: {r}");
    }
}

#[test]
fn wildcard_select_still_native() {
    // Sanity: no projection → native variant VCF (unchanged behaviour).
    let vcf = run_into_vcf(
        "native",
        r#"WHERE chr = "7" AND depth >= 20 CALL variants WITH min_af = 0.05 LIMIT 4"#,
    );
    let rows = data_rows(&vcf);
    assert_eq!(rows.len(), 4);
    assert!(vcf.contains("DP=") && vcf.contains("AF="), "native INFO expected:\n{vcf}");
    assert!(!vcf.contains("##source=SpliceQL"), "native path must not use the projected header");
}
