//! Track 2 — region-sharded parallelism: serial-equivalence gate.
//!
//! The non-negotiable contract: a `CALL variants` query run **sharded** across
//! threads must be **byte-identical** to the same query run **serially**. These
//! tests prove it over the real EGFR sample BAM, and hammer the shard-boundary
//! interaction (#20 danger zone): a variant landing exactly on a shard boundary
//! must appear exactly once — not dropped, not duplicated.

use std::collections::HashMap;
use std::path::PathBuf;

use cnvlens_core::model::{Region, Variant, VariantOptions};
use cnvlens_core::variants::call_variants_region;
use codonsplice_core::shard::{
    shard_and_merge, split_region, split_region_bai, NativeThreadExecutor, SerialExecutor, Shard,
};

const CHROM: &str = "7";
// The whole EGFR window the sample BAM/reference cover.
const START: i64 = 54_990_000;
const END: i64 = 55_300_100;
// A known het variant in the sample (depth 340) — our boundary target.
const BOUNDARY_VAR: i64 = 55_220_177;

fn data_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../cnvlens/public/sample-data")
        .join(name)
}

fn parse_fasta(bytes: &[u8]) -> HashMap<String, String> {
    let text = String::from_utf8_lossy(bytes);
    let mut map = HashMap::new();
    let mut name = String::new();
    let mut seq = String::new();
    for line in text.lines() {
        if let Some(h) = line.strip_prefix('>') {
            if !name.is_empty() {
                map.insert(std::mem::take(&mut name), std::mem::take(&mut seq));
            }
            name = h.split_whitespace().next().unwrap_or("").to_string();
        } else {
            seq.push_str(line.trim());
        }
    }
    if !name.is_empty() {
        map.insert(name, seq);
    }
    map
}

struct Fixture {
    bam: Vec<u8>,
    bai: Vec<u8>,
    opts: VariantOptions,
}

fn fixture() -> Fixture {
    let bam = std::fs::read(data_path("NA12878_EGFR.bam")).unwrap();
    let bai = std::fs::read(data_path("NA12878_EGFR.bam.bai")).unwrap();
    let reference = std::fs::read(data_path("EGFR_region.fa")).unwrap();
    let mut opts = VariantOptions::default();
    opts.reference_seqs = Some(parse_fasta(&reference));
    Fixture { bam, bai, opts }
}

/// Serialize a variant list to newline-delimited JSON — exactly the byte stream
/// the CLI/`record_to_json` would emit, so "byte-identical" means byte-identical.
fn to_ndjson(vars: &[Variant]) -> String {
    let mut s = String::new();
    for v in vars {
        s.push_str(&serde_json::to_string(v).unwrap());
        s.push('\n');
    }
    s
}

/// Serial baseline: one pileup over the whole region, clamped to `[START, END]`
/// (the same clamp the query's `WHERE pos` predicate applies).
fn serial(fx: &Fixture) -> Vec<Variant> {
    let region = Region::with_bounds(CHROM, Some(START), Some(END));
    let mut vars = call_variants_region(&fx.bam, &fx.bai, &region, &fx.opts).unwrap();
    vars.retain(|v| v.pos >= START && v.pos <= END);
    vars
}

/// Sharded run: split into shards, call per shard, merge with the boundary-correct
/// clamp. `exec` lets us run the SAME brain serially or natively-threaded.
fn sharded<E: codonsplice_core::shard::ShardExecutor>(
    fx: &Fixture,
    exec: &E,
    shards: &[Shard],
) -> Vec<Variant> {
    shard_and_merge(
        exec,
        shards,
        |s: &Shard| call_variants_region(&fx.bam, &fx.bai, &s.to_core_region().to_core(), &fx.opts),
        |v: &Variant| v.pos,
    )
    .unwrap()
}

#[test]
fn native_sharded_is_byte_identical_to_serial() {
    let fx = fixture();
    let baseline = to_ndjson(&serial(&fx));

    // Sanity: the baseline actually called the variants we expect to shard around.
    assert!(baseline.contains("55220177"), "baseline must contain boundary variant");
    let var_count = serial(&fx).len();
    assert!(var_count >= 4, "expected several variants, got {var_count}");

    for n in [2usize, 3, 4, 8, 16] {
        let shards = split_region(CHROM, START, END, n);
        let native = to_ndjson(&sharded(&fx, &NativeThreadExecutor, &shards));
        assert_eq!(
            native, baseline,
            "native-sharded into {n} shards must be byte-identical to serial"
        );
        // The serial executor over the same shards must also match (proves the
        // merge brain is backend-agnostic; only the dispatch differs).
        let serial_sharded = to_ndjson(&sharded(&fx, &SerialExecutor, &shards));
        assert_eq!(serial_sharded, baseline, "serial-executor sharded must match too");
    }
}

/// The density-aware path proper: cuts placed from the REAL BAI linear index
/// (`split_region_bai`), not a uniform grid. Two things must hold together —
/// (1) the cuts are genuinely *non-uniform* (else the gate is vacuous / silently
/// fell back to uniform), and (2) those uneven cuts still produce output
/// byte-identical to serial. "Moving the cuts cannot change the answer."
#[test]
fn density_split_from_bai_is_byte_identical_to_serial() {
    let fx = fixture();
    let baseline = to_ndjson(&serial(&fx));

    let mut saw_nonuniform = false;
    for n in [2usize, 3, 4, 8, 16] {
        let dshards = split_region_bai(CHROM, START, END, n, &fx.bam, &fx.bai);
        // Did density actually shift the cuts off the uniform grid? (Targeted-
        // capture coverage clusters around the EGFR exons, so it should.)
        let dbounds: Vec<i64> = dshards.iter().map(|s| s.start).collect();
        let ubounds: Vec<i64> = split_region(CHROM, START, END, n)
            .iter()
            .map(|s| s.start)
            .collect();
        if dbounds != ubounds {
            saw_nonuniform = true;
        }

        let native = to_ndjson(&sharded(&fx, &NativeThreadExecutor, &dshards));
        assert_eq!(
            native, baseline,
            "density-split ({n} shards, BAI-placed cuts) must be byte-identical to serial"
        );
    }
    assert!(
        saw_nonuniform,
        "density split must move cuts off the uniform grid for the EGFR BAM — \
         else this gate is vacuous (silent fallback to uniform)"
    );
}

#[test]
fn variant_on_shard_boundary_appears_exactly_once() {
    let fx = fixture();
    let baseline = to_ndjson(&serial(&fx));

    // Case A: boundary variant is the INCLUSIVE END of shard 0.
    let shards_a = vec![
        Shard { index: 0, chrom: CHROM.into(), start: START, end: BOUNDARY_VAR },
        Shard { index: 1, chrom: CHROM.into(), start: BOUNDARY_VAR + 1, end: END },
    ];
    // Case B: boundary variant is the INCLUSIVE START of shard 1.
    let shards_b = vec![
        Shard { index: 0, chrom: CHROM.into(), start: START, end: BOUNDARY_VAR - 1 },
        Shard { index: 1, chrom: CHROM.into(), start: BOUNDARY_VAR, end: END },
    ];

    for (label, shards) in [("end-of-shard0", shards_a), ("start-of-shard1", shards_b)] {
        let out = sharded(&fx, &NativeThreadExecutor, &shards);
        let hits = out.iter().filter(|v| v.pos == BOUNDARY_VAR).count();
        assert_eq!(hits, 1, "[{label}] boundary variant must appear exactly once, got {hits}");
        assert_eq!(
            to_ndjson(&out),
            baseline,
            "[{label}] boundary split must still be byte-identical to serial"
        );
    }
}

/// End-to-end through the public `Vm`: the same `CALL variants` query run with
/// sharding forced ON (`SPLICE_SHARDS=8`) must produce byte-identical output to
/// the serial path (`SPLICE_SHARDS=1`). This exercises the real VM dispatch
/// (`stream_variants` serial vs `call_variants_region` sharded), not just the
/// cnvlens producer in isolation.
#[test]
fn vm_query_is_identical_with_and_without_sharding() {
    use codonsplice_core::{compile, Vm, VmOutput};

    let bam = data_path("NA12878_EGFR.bam");
    let reference = data_path("EGFR_region.fa");
    let query = format!(
        "FROM bam \"{}\" WHERE chr = \"7\" AND pos >= {START} AND pos <= {END} \
         CALL variants WITH reference = \"{}\"",
        bam.to_string_lossy(),
        reference.to_string_lossy(),
    );

    let run = |shards: &str| -> String {
        std::env::set_var("SPLICE_SHARDS", shards);
        let program = compile(&query).unwrap();
        let out = Vm::new(program).run().unwrap();
        std::env::remove_var("SPLICE_SHARDS");
        let recs = match out {
            VmOutput::Records(r) | VmOutput::Rows(r) => r,
            other => panic!("expected records, got {other:?}"),
        };
        recs.iter()
            .map(|r| codonsplice_core::vm::record_to_json(r).to_string())
            .collect::<Vec<_>>()
            .join("\n")
    };

    let serial = run("1");
    let sharded = run("8");
    assert!(serial.contains("55220177"), "query should find the boundary variant");
    assert_eq!(serial, sharded, "VM output must be identical sharded vs serial");
}

/// Run a query through the public `Vm` with `SPLICE_SHARDS` forced to `shards`,
/// returning the newline-joined `record_to_json` byte stream — the exact bytes
/// the CLI emits, so equality here is byte-identity.
fn run_query_json(query: &str, shards: &str) -> String {
    use codonsplice_core::{compile, Vm, VmOutput};
    std::env::set_var("SPLICE_SHARDS", shards);
    let program = compile(query).unwrap();
    let out = Vm::new(program).run().unwrap();
    std::env::remove_var("SPLICE_SHARDS");
    let recs = match out {
        VmOutput::Records(r) | VmOutput::Rows(r) => r,
        other => panic!("expected records, got {other:?}"),
    };
    recs.iter()
        .map(|r| codonsplice_core::vm::record_to_json(r).to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// CALL cnv under sharding: the integration's first real "CALL cnv under
/// sharding" gate. `SPLICE_SHARDS=8` routes `compute_coverage_windows` to
/// `coverage::compute_coverage_region_parallel` (global-segmentation-first:
/// only per-window counting is parallel, median/GC/segmentation stay global),
/// so the emitted CNV calls must be byte-identical to the serial path
/// (`SPLICE_SHARDS=1`). The boundary-spanning guarantee (a CNV straddling a
/// shard seam is one call, not two) is proven in cnvlens-core's
/// `amplification_spanning_a_shard_boundary_is_one_call`.
#[test]
fn cnv_query_is_identical_with_and_without_sharding() {
    let bam = data_path("NA12878_EGFR.bam");
    let query = format!(
        "FROM bam \"{}\" WHERE chr = \"7\" AND pos >= 55000000 AND pos <= 55300000 \
         CALL cnv WITH window_size = 500",
        bam.to_string_lossy(),
    );
    let serial = run_query_json(&query, "1");
    let sharded = run_query_json(&query, "8");
    assert_eq!(
        serial, sharded,
        "CALL cnv must be byte-identical sharded vs serial"
    );
}

/// ANNOTATE over a sharded producer: `CALL variants` is region-sharded across
/// threads (SPLICE_SHARDS=8), then `ANNOTATE WITH genes=...` maps each variant
/// to its gene/exon. The shard count must not change the answer — production +
/// annotation must be byte-identical to the serial run. This is the ANNOTATE leg
/// of the unified gate (non-vacuous: the BAM producer genuinely shards here).
#[test]
fn annotate_query_is_identical_with_and_without_sharding() {
    let bam = data_path("NA12878_EGFR.bam");
    let reference = data_path("EGFR_region.fa");
    let gff = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/EGFR_region.GRCh37.gff3");
    let query = format!(
        "FROM bam \"{}\" WHERE chr = \"7\" AND pos >= {START} AND pos <= {END} \
         CALL variants WITH reference = \"{}\" ANNOTATE WITH genes = \"{}\"",
        bam.to_string_lossy(),
        reference.to_string_lossy(),
        gff.to_string_lossy(),
    );
    let serial = run_query_json(&query, "1");
    let sharded = run_query_json(&query, "8");
    assert!(serial.contains("EGFR"), "ANNOTATE must attach the EGFR gene column");
    assert_eq!(
        serial, sharded,
        "ANNOTATE over a sharded producer must be byte-identical to serial"
    );
}

#[test]
fn split_at_every_known_variant_position_stays_identical() {
    let fx = fixture();
    let baseline = to_ndjson(&serial(&fx));
    // Split exactly AT each known variant position — the hardest boundary case.
    for split in [55_003_988i64, 55_214_348, 55_220_177, 55_228_053, 55_249_063] {
        let shards = vec![
            Shard { index: 0, chrom: CHROM.into(), start: START, end: split },
            Shard { index: 1, chrom: CHROM.into(), start: split + 1, end: END },
        ];
        let out = to_ndjson(&sharded(&fx, &NativeThreadExecutor, &shards));
        assert_eq!(out, baseline, "split at variant pos {split} must be identical");
    }
}
