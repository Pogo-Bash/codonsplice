//! v0.1.5 regressions:
//!  * #1/#3 — VCF and BED serializers must not silently drop any record kind
//!    (projected SELECT rows, coverage windows, alignments). The written body
//!    row count must equal the `wrote N record(s)` count.
//!  * #3 — ORDER BY must see the full record set before LIMIT truncates it
//!    (global top-N, not the first-N produced).

use std::path::PathBuf;

use codonsplice_core::{compile, RuntimeValue, Vm, VmOutput};

fn bam() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../cnvlens/public/sample-data/NA12878_EGFR.bam")
        .to_string_lossy()
        .into_owned()
}

fn out_path(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("cs_v015_{}_{}", tag, std::process::id()));
    p
}

/// Run `body` with an `INTO <fmt> <tmp>` sink and return (reported count, file).
fn run_into(tag: &str, fmt: &str, body: &str) -> (usize, String) {
    let path = out_path(tag);
    let src = format!(
        "FROM bam \"{}\" {body} INTO {fmt} \"{}\"",
        bam(),
        path.display()
    );
    let program = compile(&src).unwrap();
    let summary = match Vm::new(program).run().unwrap() {
        VmOutput::Text(s) => s,
        VmOutput::Ready(_) => String::new(),
        other => panic!("expected a WRITE_INTO summary, got {other:?}"),
    };
    // "wrote N record(s) to ..."
    let count = summary
        .split_whitespace()
        .nth(1)
        .and_then(|n| n.parse::<usize>().ok())
        .unwrap_or(0);
    let contents = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    (count, contents)
}

fn data_rows(text: &str) -> Vec<&str> {
    text.lines().filter(|l| !l.starts_with('#')).collect()
}

// ── #1/#3: serializers never silently drop a record kind ────────────────────

#[test]
fn projected_select_into_bed_writes_rows() {
    let (count, bed) = run_into(
        "bed_proj",
        "bed",
        r#"SELECT chr, pos, depth WHERE chr = "7" AND depth >= 20 CALL variants WITH min_af = 0.05"#,
    );
    let rows = data_rows(&bed);
    assert!(count > 0, "expected a non-zero record count");
    assert_eq!(rows.len(), count, "BED body rows must match reported count:\n{bed}");
    assert!(bed.starts_with("#chrom\tstart\tend"), "expected a BED column header:\n{bed}");
    // pos → 0-based half-open [pos-1, pos); depth is the extra column.
    for r in &rows {
        let f: Vec<&str> = r.split('\t').collect();
        assert_eq!(f[0], "7", "chrom col from `chr`");
        let start: i64 = f[1].parse().unwrap();
        let end: i64 = f[2].parse().unwrap();
        assert_eq!(end - start, 1, "BED interval derived from pos: {r}");
        assert!(f.len() >= 4, "depth should be appended as an extra field: {r}");
    }
}

#[test]
fn coverage_into_vcf_does_not_drop() {
    // CoverageWindow records used to fall through records_to_vcf silently.
    let (count, vcf) = run_into(
        "cov_vcf",
        "vcf",
        r#"WHERE chr = "7" CALL coverage WITH window_size = 1000 LIMIT 8"#,
    );
    let rows = data_rows(&vcf);
    assert!(count > 0);
    assert_eq!(rows.len(), count, "VCF body rows must match reported count:\n{vcf}");
    // chrom is canonical; the window columns land in INFO.
    assert!(vcf.contains("##INFO=<ID=normalized") || vcf.contains("##INFO=<ID=coverage"));
}

// ── #3: ORDER BY sees the full set before LIMIT ─────────────────────────────

fn quals(out: VmOutput) -> Vec<f64> {
    let recs = match out {
        VmOutput::Rows(r) | VmOutput::Records(r) => r,
        other => panic!("expected records, got {other:?}"),
    };
    recs.iter()
        .map(|r| match r.get_field("qual") {
            RuntimeValue::Float(x) => x,
            RuntimeValue::Int(n) => n as f64,
            v => panic!("qual not numeric: {v:?}"),
        })
        .collect()
}

fn run_rows(body: &str) -> VmOutput {
    let src = format!("FROM bam \"{}\" {body}", bam());
    Vm::new(compile(&src).unwrap()).run().unwrap()
}

#[test]
fn order_by_limit_is_global_top_n() {
    // No WHERE → the deferred variant producer must NOT be capped by LIMIT, or
    // it would sort only the first-N produced. Compare LIMIT 5 against the
    // top-5 of the full (unlimited) set.
    let limited = quals(run_rows(
        r#"WHERE chr = "7" CALL variants WITH min_af = 0.05 ORDER BY qual DESC LIMIT 5"#,
    ));
    let mut full = quals(run_rows(
        r#"WHERE chr = "7" CALL variants WITH min_af = 0.05"#,
    ));
    full.sort_by(|a, b| b.partial_cmp(a).unwrap());
    let expected: Vec<f64> = full.into_iter().take(5).collect();

    assert_eq!(limited.len(), 5);
    // Descending order in the result.
    for w in limited.windows(2) {
        assert!(w[0] >= w[1], "ORDER BY qual DESC not descending: {limited:?}");
    }
    // And it's the *global* top 5, not the first-5-produced.
    assert_eq!(limited, expected, "LIMIT after ORDER BY must be the global top-N");
}
