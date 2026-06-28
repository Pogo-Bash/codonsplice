//! End-to-end indel calling against the bundled NA12878 EGFR sample BAM.
//!
//! Exercises the CIGAR-aware pileup (cnvlens-core) through the full
//! compile → VM → `Record::Variant` path. The known GIAB high-confidence
//! deletion 7:55010562 GA>G has read depth ~2 in this sample, well below the
//! default `min_depth = 10`, so the query relaxes the depth threshold.
//!
//! Reference-gated: indel realignment needs `chr7.fa` at the repo root. If it
//! is absent the test skips cleanly (the same convention other reference-
//! dependent checks use), so CI without the large FASTA still passes.

use std::path::PathBuf;

use codonsplice_core::{compile, Record, Vm, VmOutput};

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/crates/codonsplice-core
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn bam_path() -> String {
    repo_root()
        .join("cnvlens/public/sample-data/NA12878_EGFR.bam")
        .to_string_lossy()
        .into_owned()
}

fn reference_path() -> PathBuf {
    repo_root().join("chr7.fa")
}

fn run_records(source: &str) -> Vec<Record> {
    let program = compile(source).expect("compile");
    match Vm::new(program).run().expect("run") {
        VmOutput::Records(r) => r,
        other => panic!("expected Records, got {other:?}"),
    }
}

/// The GIAB deletion 7:55010562 GA>G must be emitted as an indel `Variant`
/// when the depth threshold is relaxed below the site's coverage.
#[test]
fn calls_giab_deletion_55010562() {
    let reference = reference_path();
    if !reference.exists() {
        eprintln!(
            "skipping calls_giab_deletion_55010562: reference {} not present",
            reference.display()
        );
        return;
    }

    let query = format!(
        r#"FROM bam "{bam}" WHERE chr = "7" AND pos >= 55010560 AND pos <= 55010565 CALL variants WITH reference = "{reference}", min_depth = 2"#,
        bam = bam_path(),
        reference = reference.to_string_lossy(),
    );

    let recs = run_records(&query);

    let found = recs.iter().find_map(|r| match r {
        Record::Variant(v) if v.pos == 55010562 && v.ref_base == "GA" && v.alt == "G" => Some(v),
        _ => None,
    });

    assert!(
        found.is_some(),
        "expected indel Variant pos=55010562 ref=GA alt=G; got: {:#?}",
        recs.iter()
            .filter_map(|r| match r {
                Record::Variant(v) => Some((v.pos, v.ref_base.clone(), v.alt.clone(), v.depth)),
                _ => None,
            })
            .collect::<Vec<_>>()
    );

    let v = found.unwrap();
    assert_eq!(v.chrom, "7");
    assert!(
        v.ref_base.len() > v.alt.len(),
        "GA>G should be a deletion (ref longer than alt)"
    );
}
