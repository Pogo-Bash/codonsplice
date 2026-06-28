//! HGVS/protein annotation against the **real** GRCh37 chr7 reference.
//!
//! This is the genuine end-to-end oracle gate for EGFR L858R: it reads the
//! committed GFF3 gene model and the full `chr7.fa` reference, extracts the
//! reference codon at chr7:55259515 *independently* (asserting it is CTG = Leu
//! before any variant is applied), then runs the annotator and asserts
//! `p.Leu858Arg` / `c.2573T>G`.
//!
//! `chr7.fa` is the 161 MB GRCh37 contig kept outside this worktree at
//! `/home/swap/lang/codonsplice/chr7.fa`. If it is absent the test is skipped
//! (so CI without the big file still passes) — the synthetic-reference unit test
//! in `annotate_tests.rs` covers the same assertion hermetically.

use std::path::{Path, PathBuf};

use cnvlens_core::model::Variant;
use codonsplice_core::annotate::Annotator;
use codonsplice_core::codon::{self, CdsModel, CdsSegment, Strand};

const CHR7_FA: &str = "/home/swap/lang/codonsplice/chr7.fa";

fn testdata(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata")
        .join(name)
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

fn col<'a>(ann: &'a [(String, String)], key: &str) -> &'a str {
    ann.iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .unwrap()
}

/// Read just the contig-7 bases of `chr7.fa` (full-contig FASTA, no region
/// header) into an absolute-0-based string (base at 1-based `pos` is `[pos-1]`).
fn load_chr7() -> String {
    let text = std::fs::read_to_string(CHR7_FA).expect("read chr7.fa");
    let mut seq = String::new();
    for line in text.lines() {
        if line.starts_with('>') {
            continue;
        }
        seq.push_str(line.trim());
    }
    seq
}

#[test]
fn l858r_on_real_chr7_reference() {
    if !Path::new(CHR7_FA).exists() {
        eprintln!("skipping: {CHR7_FA} not present");
        return;
    }

    let seq = load_chr7();
    let bytes = seq.as_bytes();
    let ref_at = |p: i64| bytes.get((p - 1) as usize).copied();

    // 1. Independent codon extraction BEFORE applying the variant. The canonical
    //    EGFR transcript's exon-21 CDS block is 55259412..55259567, phase 0, +.
    let cds = CdsModel::new(
        Strand::Plus,
        vec![CdsSegment { start: 55259412, end: 55259567, phase: 0 }],
    );
    let hit = codon::codon_at_genomic(&cds, 55259515, ref_at).expect("codon");
    assert_eq!(hit.codon, "CTG", "reference codon at L858 must be CTG (Leu)");
    assert_eq!(hit.frame, 1, "variant base is the 2nd base of the codon");
    assert_eq!(codon::codon_to_aa(hit.codon.as_bytes()), 'L');

    // 2. Full annotator against the real reference → HGVS protein + cDNA. The
    //    codon NUMBER (858) comes from the GFF's full CDS model, so we use the
    //    committed GFF here rather than the single synthetic segment above.
    let gff = std::fs::read(testdata("EGFR_region.GRCh37.gff3")).unwrap();
    let fasta = std::fs::read(CHR7_FA).unwrap();
    let ann = Annotator::from_sources(Some(&gff), None, Some(&fasta)).expect("annotator");
    let cols = ann.annotate(&variant("7", 55259515, "T", "G"));
    assert_eq!(col(&cols, "aa_change"), "p.Leu858Arg");
    assert_eq!(col(&cols, "hgvs_c"), "c.2573T>G");
    assert_eq!(col(&cols, "transcript"), "ENST00000275493");
}
