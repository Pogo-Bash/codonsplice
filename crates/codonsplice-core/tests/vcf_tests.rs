//! Phase 5 — VCF input (cnvlens-core reader + VM `FROM vcf`).

use std::io::Write;

use cnvlens_core::model::Region;
use cnvlens_core::vcf;
use codonsplice_core::{compile, Vm, VmOutput};

const VCF: &str = "\
##fileformat=VCFv4.2
#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO
7\t55086000\trs1\tA\tG\t60.0\tPASS\tDP=100;AF=0.5
7\t55090000\t.\tC\tT\t20.0\tLowQual\tDP=30;AF=0.05
1\t1000\t.\tG\tA\t99.0\tPASS\tDP=50;AF=0.9
";

fn write_vcf() -> std::path::PathBuf {
    // Unique per call: the two tests run in parallel and each removes its file,
    // so a shared `cs_vcf_<pid>.vcf` raced (one deleted the other's mid-read).
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!("cs_vcf_{}_{}.vcf", std::process::id(), n));
    let mut f = std::fs::File::create(&p).unwrap();
    f.write_all(VCF.as_bytes()).unwrap();
    p
}

#[test]
fn stream_vcf_maps_columns() {
    let vars: Vec<_> = vcf::stream_vcf(VCF.as_bytes(), None)
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(vars.len(), 3);
    let v = &vars[0];
    assert_eq!(v.chrom, "7");
    assert_eq!(v.pos, 55086000);
    assert_eq!(v.ref_base, "A");
    assert_eq!(v.alt, "G");
    assert_eq!(v.qual, 60.0);
    assert_eq!(v.filter.as_deref(), Some("PASS"));
    assert_eq!(v.id.as_deref(), Some("rs1"));
    assert_eq!(v.depth, 100); // INFO/DP
    assert_eq!(v.allele_freq, 0.5); // INFO/AF
}

#[test]
fn stream_vcf_region_filter() {
    // Restrict to chr7, 55.08–55.087 Mb: only the first record.
    let region = Region::with_bounds("7", Some(55_080_000), Some(55_087_000));
    let vars: Vec<_> = vcf::stream_vcf(VCF.as_bytes(), Some(&region))
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(vars.len(), 1);
    assert_eq!(vars[0].pos, 55086000);
}

#[test]
fn vm_from_vcf_where_af_filters() {
    let path = write_vcf();
    let src = format!("FROM vcf \"{}\" WHERE af > 0.1 CALL variants", path.display());
    let program = compile(&src).unwrap();
    match Vm::new(program).run().unwrap() {
        VmOutput::Records(recs) => {
            // AF 0.5 and 0.9 pass; 0.05 is dropped.
            assert_eq!(recs.len(), 2);
        }
        other => panic!("expected Records, got {other:?}"),
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn vm_from_vcf_filter_field() {
    let path = write_vcf();
    let src = format!(
        "FROM vcf \"{}\" WHERE filter = \"PASS\" CALL variants",
        path.display()
    );
    let program = compile(&src).unwrap();
    match Vm::new(program).run().unwrap() {
        VmOutput::Records(recs) => assert_eq!(recs.len(), 2), // two PASS rows
        other => panic!("expected Records, got {other:?}"),
    }
    let _ = std::fs::remove_file(&path);
}
