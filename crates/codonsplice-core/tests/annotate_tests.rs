//! Track 1 — ANNOTATE: variant records joined against local GFF (gene model) and
//! ClinVar VCF databases by genomic position, gaining gene/exon/consequence/
//! clinical fields. The payoff case is EGFR L858R (chr7:55259515 T>G, GRCh37).
//!
//! The annotation databases are the GRCh37 EGFR slices committed by Track 0 at
//! the repo root `testdata/`, addressed relative to `CARGO_MANIFEST_DIR`.

use std::path::PathBuf;

use cnvlens_core::model::Variant;
use codonsplice_core::annotate::Annotator;
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

/// Repo-root committed annotation database (Track 0 GRCh37 EGFR slice).
fn testdata(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

fn gff_bytes() -> Vec<u8> {
    std::fs::read(testdata("EGFR_region.GRCh37.gff3")).expect("read GFF slice")
}
fn clinvar_bytes() -> Vec<u8> {
    std::fs::read(testdata("clinvar_GRCh37_EGFR.vcf.gz")).expect("read ClinVar slice")
}

fn variant(chrom: &str, pos: i64, r: &str, alt: &str) -> Variant {
    Variant {
        chrom: chrom.into(),
        pos,
        ref_base: r.into(),
        alt: alt.into(),
        qual: 60.0,
        kind: "SNV".into(),
        depth: 100,
        ref_count: 50,
        alt_count: 50,
        allele_freq: 0.5,
        strand_bias: 0.0,
        filter: Some("PASS".into()),
        id: None,
    }
}

/// Look up a single annotation column from the annotator output.
fn col<'a>(ann: &'a [(String, String)], key: &str) -> &'a str {
    ann.iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .unwrap_or_else(|| panic!("annotation column `{key}` missing; got {ann:?}"))
}

// ── Unit: the annotator join, directly ────────────────────────────────────────

#[test]
fn l858r_annotates_gene_exon_and_clinical() {
    let ann = Annotator::from_sources(Some(&gff_bytes()), Some(&clinvar_bytes()), None)
        .expect("build annotator");
    // chr7:55259515 T>G (GRCh37) — EGFR L858R, somatic/oncogenic.
    let cols = ann.annotate(&variant("7", 55259515, "T", "G"));
    assert_eq!(col(&cols, "gene"), "EGFR");
    assert_eq!(col(&cols, "exon"), "21");
    assert_eq!(col(&cols, "region"), "exon");
    assert_eq!(col(&cols, "clinvar_significance"), "drug_response");
    assert_eq!(col(&cols, "clinvar_oncogenic"), "Oncogenic");
    assert_eq!(col(&cols, "rsid"), "rs121434568");
    assert_eq!(col(&cols, "consequence"), "missense_variant");
}

/// A minimal reference covering just the L858 codon (genomic 55259514..516 =
/// CTG on the + strand), using a `samtools faidx` style region-slice header so
/// the absolute coordinates stay correct without shipping the whole contig.
fn l858_ref_bytes() -> Vec<u8> {
    b">7:55259514-55259516\nCTG\n".to_vec()
}

#[test]
fn l858r_emits_hgvs_protein_and_cdna() {
    let ann = Annotator::from_sources(
        Some(&gff_bytes()),
        Some(&clinvar_bytes()),
        Some(&l858_ref_bytes()),
    )
    .expect("build annotator");
    // EGFR L858R: chr7:55259515 T>G (GRCh37), exon 21, + strand.
    // Reference codon CTG (Leu858) → CGG (Arg858).
    let cols = ann.annotate(&variant("7", 55259515, "T", "G"));
    assert_eq!(col(&cols, "aa_change"), "p.Leu858Arg");
    assert_eq!(col(&cols, "hgvs_c"), "c.2573T>G");
}

#[test]
fn intronic_egfr_variant_gets_gene_but_no_exon() {
    let ann = Annotator::from_sources(Some(&gff_bytes()), Some(&clinvar_bytes()), None)
        .expect("build annotator");
    let cols = ann.annotate(&variant("7", 55230000, "C", "T"));
    assert_eq!(col(&cols, "gene"), "EGFR");
    assert_eq!(col(&cols, "region"), "intron");
    assert_eq!(col(&cols, "exon"), ".");
}

#[test]
fn unmatched_clinvar_yields_dot_significance() {
    let ann = Annotator::from_sources(Some(&gff_bytes()), Some(&clinvar_bytes()), None)
        .expect("build annotator");
    // Right position, wrong ALT — must not match L858R's clinvar record.
    let cols = ann.annotate(&variant("7", 55259515, "T", "A"));
    assert_eq!(col(&cols, "clinvar_significance"), ".");
    // Gene/exon still resolve from coordinate overlap.
    assert_eq!(col(&cols, "gene"), "EGFR");
}

// ── End-to-end: ANNOTATE through the compiler + VM ────────────────────────────

fn run_records(src: &str) -> Vec<Value> {
    let program = compile(src).unwrap_or_else(|e| panic!("compile failed for `{src}`: {e:?}"));
    match Vm::new(program)
        .run()
        .unwrap_or_else(|e| panic!("run failed for `{src}`: {e:?}"))
    {
        VmOutput::Records(recs) | VmOutput::Rows(recs) => recs.iter().map(record_to_json).collect(),
        other => panic!("expected a record stream, got {other:?}"),
    }
}

#[test]
fn annotate_clause_attaches_columns_visible_to_select() {
    let src = format!(
        r#"FROM vcf "{}" CALL variants ANNOTATE WITH genes="{}", clinvar="{}"
           SELECT chrom, pos, gene, exon, clinvar_significance, rsid"#,
        fixture("annotate_egfr.vcf"),
        testdata("EGFR_region.GRCh37.gff3"),
        testdata("clinvar_GRCh37_EGFR.vcf.gz"),
    );
    let recs = run_records(&src);
    let l858r = recs
        .iter()
        .find(|r| r["pos"] == 55259515)
        .expect("L858R row present");
    assert_eq!(l858r["gene"], "EGFR");
    assert_eq!(l858r["exon"], "21");
    assert_eq!(l858r["clinvar_significance"], "drug_response");
    assert_eq!(l858r["rsid"], "rs121434568");
}

#[test]
fn annotate_columns_filterable_in_where() {
    // WHERE on an annotation column: keep only ClinVar-annotated variants.
    let src = format!(
        r#"FROM vcf "{}" CALL variants ANNOTATE WITH clinvar="{}"
           WHERE clinvar_significance != "." SELECT pos, clinvar_significance"#,
        fixture("annotate_egfr.vcf"),
        testdata("clinvar_GRCh37_EGFR.vcf.gz"),
    );
    let recs = run_records(&src);
    // L858R (55259515) carries CLNSIG=drug_response and is kept. The intronic
    // 55230000 is not in ClinVar; 55086938's ClinVar record has no CLNSIG/ONC, so
    // both resolve to "." and are filtered out (matches `bcftools annotate`).
    let positions: Vec<i64> = recs.iter().map(|r| r["pos"].as_i64().unwrap()).collect();
    assert!(positions.contains(&55259515), "L858R kept: {positions:?}");
    assert!(
        !positions.contains(&55230000),
        "intronic dropped: {positions:?}"
    );
}
