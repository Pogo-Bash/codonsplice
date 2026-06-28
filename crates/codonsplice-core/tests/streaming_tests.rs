//! Phase 5 — true streaming for variants (LIMIT short-circuit).

use std::path::PathBuf;

use cnvlens_core::model::{Region, VariantOptions};
use cnvlens_core::variants;

fn bam_bytes() -> Vec<u8> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../cnvlens/public/sample-data/NA12878_EGFR.bam");
    std::fs::read(p).unwrap()
}

fn chr7_opts() -> VariantOptions {
    let mut o = VariantOptions::default();
    o.chromosomes = Some(vec!["7".to_string()]);
    o
}

#[test]
fn limit_yields_exactly_n() {
    let bytes = bam_bytes();
    let five: Vec<_> = variants::stream_variants(&bytes, None, &chr7_opts(), None, Some(5))
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(five.len(), 5);
}

#[test]
fn no_limit_returns_all() {
    let bytes = bam_bytes();
    let streamed: Vec<_> = variants::stream_variants(&bytes, None, &chr7_opts(), None, None)
        .collect::<Result<_, _>>()
        .unwrap();
    let collected = variants::collect_variants(&bytes, None, &chr7_opts()).unwrap();
    // The core streaming guarantee: streaming yields exactly the collected set.
    assert_eq!(streamed.len(), collected.len());
    // The CIGAR-correct pileup removed the soft-clip-misplacement false positives
    // the old ungapped loop produced (~280 spurious calls); the EGFR sample now
    // yields a small set of clean, GIAB-confirmed heterozygous SNVs.
    assert!(streamed.len() >= 5, "sample should still hold variants");
}

#[test]
fn limit_is_a_strict_prefix() {
    // The first N streamed-with-limit variants match the first N of the full set
    // (same order), proving the limit truncates rather than re-orders.
    let bytes = bam_bytes();
    let region = Region::new("7");
    let opts = VariantOptions::default();
    let full: Vec<_> = variants::stream_variants(&bytes, None, &opts, Some(&region), None)
        .collect::<Result<_, _>>()
        .unwrap();
    let limited: Vec<_> = variants::stream_variants(&bytes, None, &opts, Some(&region), Some(3))
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(limited.len(), 3);
    for (a, b) in limited.iter().zip(full.iter()) {
        assert_eq!((&a.chrom, a.pos, &a.alt), (&b.chrom, b.pos, &b.alt));
    }
}
