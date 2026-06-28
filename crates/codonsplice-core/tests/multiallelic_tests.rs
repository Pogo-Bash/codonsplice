//! Multi-allelic variant splitting — the `SPLIT` clause on `FROM vcf`.
//!
//! `SPLIT` decomposes each multi-allelic VCF record (one REF, comma-separated
//! ALTs) into one biallelic record per ALT, reproducing `bcftools norm -m -`
//! semantics: per-allele REF/ALT reduced to minimal representation (shared
//! suffix then prefix trimmed, POS advanced on a left trim) and per-allele
//! INFO/AF apportioned positionally.
//!
//! Two layers of verification:
//!   1. A self-contained golden set (the exact `(pos,ref,alt)` records bcftools
//!      1.16 emits for `tests/data/multiallelic.vcf` against GRCh37 chr7) — runs
//!      everywhere, no external tools.
//!   2. A live differential against `bcftools norm -m -` — runs only when
//!      `SPLICE_BCFTOOLS_REF` points at a chr7 FASTA (and bcftools/bgzip/tabix
//!      are on PATH), so CI without bcftools is unaffected.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

use codonsplice_core::vm::record_to_json;
use codonsplice_core::{compile, Vm, VmOutput};
use serde_json::Value;

fn fixture(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

fn run_records(src: &str) -> Vec<Value> {
    let program = compile(src).unwrap_or_else(|e| panic!("compile failed for `{src}`: {e:?}"));
    match Vm::new(program).run().unwrap_or_else(|e| panic!("run failed for `{src}`: {e:?}")) {
        VmOutput::Records(recs) | VmOutput::Rows(recs) => recs.iter().map(record_to_json).collect(),
        other => panic!("expected a record stream, got {other:?}"),
    }
}

/// A `(chrom, pos, ref, alt)` identity tuple — the hard equality gate.
fn ident(r: &Value) -> (String, i64, String, String) {
    (
        r["chrom"].as_str().unwrap().to_string(),
        r["pos"].as_i64().unwrap(),
        r["ref"].as_str().unwrap().to_string(),
        r["alt"].as_str().unwrap().to_string(),
    )
}

/// The biallelic record set bcftools 1.16 produces for `multiallelic.vcf`
/// (`bcftools norm -m - -f chr7.fa`), verified against the live tool.
fn bcftools_golden() -> BTreeSet<(String, i64, String, String)> {
    [
        ("7", 55019021, "C", "A"),
        ("7", 55019021, "C", "G"),
        ("7", 55019021, "C", "T"),
        ("7", 55019024, "A", "C"),
        ("7", 55019024, "A", "T"),
        ("7", 55181380, "G", "A"),
        ("7", 55249060, "GC", "G"),
        ("7", 55249060, "G", "GT"),
    ]
    .iter()
    .map(|(c, p, r, a)| (c.to_string(), *p, r.to_string(), a.to_string()))
    .collect()
}

#[test]
fn without_split_multiallelics_keep_first_alt_only() {
    // Baseline: the historical behaviour reads only the first ALT, so a
    // tri-allelic record yields ONE row. Four input records -> four rows.
    let recs = run_records(&format!("FROM vcf \"{}\" CALL variants", fixture("multiallelic.vcf")));
    assert_eq!(recs.len(), 4, "no SPLIT -> one row per source line");
    assert_eq!(ident(&recs[0]), ("7".into(), 55019021, "C".into(), "A".into()));
    // The C->A,G,T record contributes only its first ALT (A).
    assert!(!recs.iter().any(|r| ident(r) == ("7".into(), 55019021, "C".into(), "G".into())));
}

#[test]
fn split_matches_bcftools_record_set() {
    // The hard gate: SPLIT output's (chrom,pos,ref,alt) set == bcftools norm -m -.
    let recs = run_records(&format!(
        "FROM vcf \"{}\" SPLIT CALL variants",
        fixture("multiallelic.vcf")
    ));
    let got: BTreeSet<_> = recs.iter().map(ident).collect();
    assert_eq!(got.len(), recs.len(), "no duplicate biallelic records");
    assert_eq!(got, bcftools_golden(), "SPLIT record set must equal bcftools norm -m -");

    // Indel-specific spot checks: the deletion stays anchored and the insertion
    // is trimmed to minimal representation (GTC -> GT at the same POS).
    assert!(got.contains(&("7".into(), 55249060, "GC".into(), "G".into())), "anchored deletion");
    assert!(got.contains(&("7".into(), 55249060, "G".into(), "GT".into())), "trimmed insertion");
}

#[test]
fn split_apportions_per_allele_af() {
    // INFO/AF (Number=A) is split positionally; shared INFO/DP is copied.
    let recs = run_records(&format!(
        "FROM vcf \"{}\" SPLIT CALL variants",
        fixture("multiallelic.vcf")
    ));
    let find = |pos: i64, alt: &str| {
        recs.iter()
            .find(|r| r["pos"].as_i64() == Some(pos) && r["alt"].as_str() == Some(alt))
            .unwrap_or_else(|| panic!("missing record {pos}:{alt}"))
    };
    assert_eq!(find(55019021, "A")["allele_freq"], 0.3);
    assert_eq!(find(55019021, "G")["allele_freq"], 0.2);
    assert_eq!(find(55019021, "T")["allele_freq"], 0.1);
    assert_eq!(find(55249060, "GT")["allele_freq"], 0.35);
    assert_eq!(find(55249060, "G")["depth"], 120);
}

/// Live oracle: regenerate the bcftools split for the fixture and assert our
/// SPLIT engine matches it exactly. Skipped unless `SPLICE_BCFTOOLS_REF` names a
/// chr7 FASTA and bcftools/bgzip/tabix are available.
#[test]
fn split_differential_vs_live_bcftools() {
    let Ok(reference) = std::env::var("SPLICE_BCFTOOLS_REF") else {
        eprintln!("skipping live bcftools differential (set SPLICE_BCFTOOLS_REF=/path/chr7.fa)");
        return;
    };
    if Command::new("bcftools").arg("--version").output().is_err() {
        eprintln!("skipping: bcftools not on PATH");
        return;
    }

    // bgzip + tabix the fixture into a temp dir.
    let dir = std::env::temp_dir().join(format!("cs_ma_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let gz = dir.join("ma.vcf.gz");
    let raw = std::fs::read(fixture("multiallelic.vcf")).unwrap();
    {
        let out = Command::new("bgzip").arg("-c").stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
        use std::io::Write;
        out.stdin.as_ref().unwrap().write_all(&raw).unwrap();
        let o = out.wait_with_output().unwrap();
        std::fs::write(&gz, o.stdout).unwrap();
    }
    assert!(Command::new("tabix").arg("-p").arg("vcf").arg(&gz).status().unwrap().success());

    let out = Command::new("bcftools")
        .args(["norm", "-m", "-", "-f", &reference])
        .arg(&gz)
        .output()
        .unwrap();
    assert!(out.status.success(), "bcftools failed: {}", String::from_utf8_lossy(&out.stderr));
    let bcf_set: BTreeSet<(String, i64, String, String)> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            (c[0].to_string(), c[1].parse().unwrap(), c[3].to_string(), c[4].to_string())
        })
        .collect();

    let recs = run_records(&format!(
        "FROM vcf \"{}\" SPLIT CALL variants",
        fixture("multiallelic.vcf")
    ));
    let got: BTreeSet<_> = recs.iter().map(ident).collect();

    assert_eq!(got, bcf_set, "live bcftools norm -m - differential");
    // And the committed golden must track the live tool.
    assert_eq!(bcf_set, bcftools_golden(), "golden drifted from live bcftools");

    let _ = std::fs::remove_dir_all(&dir);
}
