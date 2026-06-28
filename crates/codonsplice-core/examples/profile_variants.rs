//! Phase-1 profiling harness for Track 2 (region-sharded parallelism).
//!
//! Splits a `CALL variants` over the EGFR region into measurable sub-phases so
//! we can decide whether sharding (CPU-bound pileup) actually helps, or whether
//! the work is dominated by BAM/BGZF decode (I/O-ish, threads do little).
//!
//! Usage: cargo run --release --example profile_variants -- <bam> <bai> <ref.fa> <chrom> <start> <end> [iters]

use std::time::Instant;

use cnvlens_core::model::{Region, VariantOptions};
use cnvlens_core::{bam, reference_list, variants};

fn parse_fasta(bytes: &[u8]) -> std::collections::HashMap<String, String> {
    let text = String::from_utf8_lossy(bytes);
    let mut map = std::collections::HashMap::new();
    let mut name = String::new();
    let mut seq = String::new();
    for line in text.lines() {
        if let Some(h) = line.strip_prefix('>') {
            if !name.is_empty() {
                map.insert(name.clone(), std::mem::take(&mut seq));
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let bam_path = &args[1];
    let bai_path = &args[2];
    let ref_path = &args[3];
    let chrom = args[4].clone();
    let start: i64 = args[5].parse().unwrap();
    let end: i64 = args[6].parse().unwrap();
    let iters: usize = args.get(7).and_then(|s| s.parse().ok()).unwrap_or(20);

    // ---- one-time loads (not part of the per-query hot path we shard) ----
    let t = Instant::now();
    let bam_bytes = std::fs::read(bam_path).unwrap();
    let bai_bytes = std::fs::read(bai_path).unwrap();
    let ref_bytes = std::fs::read(ref_path).unwrap();
    eprintln!(
        "[load] bam {} KB + bai {} KB + ref {} KB read from disk in {:?}",
        bam_bytes.len() / 1024,
        bai_bytes.len() / 1024,
        ref_bytes.len() / 1024,
        t.elapsed()
    );

    let t = Instant::now();
    let ref_seqs = parse_fasta(&ref_bytes);
    eprintln!("[fasta] parsed {} contigs in {:?}", ref_seqs.len(), t.elapsed());

    let mut opts = VariantOptions::default();
    opts.reference_seqs = Some(ref_seqs);
    let region = Region::with_bounds(chrom.clone(), Some(start), Some(end));

    let header = bam::read_header(&bam_bytes).unwrap();
    let refs = reference_list(&header);
    let ref_idx = refs.iter().position(|(n, _)| n == &chrom).unwrap() as i32;

    // ---- Phase A: header read only ----
    let mut acc = std::time::Duration::ZERO;
    for _ in 0..iters {
        let t = Instant::now();
        let _h = bam::read_header(&bam_bytes).unwrap();
        acc += t.elapsed();
    }
    let t_header = acc / iters as u32;

    // ---- Phase B: BAM region decode only (BGZF inflate + record decode) ----
    let mut acc = std::time::Duration::ZERO;
    let mut n_reads = 0usize;
    for _ in 0..iters {
        let mut reads = Vec::new();
        let t = Instant::now();
        bam::for_each_region_full(&bam_bytes, &bai_bytes, &region, |a| {
            if a.ref_id == ref_idx {
                reads.push(a);
            }
        })
        .unwrap();
        acc += t.elapsed();
        n_reads = reads.len();
    }
    let t_decode = acc / iters as u32;

    // ---- Phase B+C: full call (decode + pileup + call) ----
    let mut acc = std::time::Duration::ZERO;
    let mut n_vars = 0usize;
    for _ in 0..iters {
        let t = Instant::now();
        let vars = variants::call_variants_region(&bam_bytes, &bai_bytes, &region, &opts).unwrap();
        acc += t.elapsed();
        n_vars = vars.len();
    }
    let t_total = acc / iters as u32;
    let t_pileup = t_total.saturating_sub(t_decode);

    // ---- Phase D: serialize the variants to JSON ----
    let vars = variants::call_variants_region(&bam_bytes, &bai_bytes, &region, &opts).unwrap();
    let mut acc = std::time::Duration::ZERO;
    for _ in 0..iters {
        let t = Instant::now();
        let mut s = String::new();
        for v in &vars {
            s.push_str(&serde_json::to_string(v).unwrap());
            s.push('\n');
        }
        std::hint::black_box(s);
        acc += t.elapsed();
    }
    let t_ser = acc / iters as u32;

    eprintln!("\n=== PROFILE (mean of {iters} iters) — region {chrom}:{start}-{end} ===");
    eprintln!("reads decoded: {n_reads}, variants called: {n_vars}");
    eprintln!("A header read   : {t_header:?}");
    eprintln!("B BAM decode    : {t_decode:?}   <- BGZF inflate + record decode (I/O-ish)");
    eprintln!("C pileup+call   : {t_pileup:?}   <- CPU-bound (shardable)");
    eprintln!("  (B+C total    : {t_total:?})");
    eprintln!("D serialize     : {t_ser:?}");
    let core = t_decode + t_pileup;
    if core.as_nanos() > 0 {
        eprintln!(
            "\nsplit: decode {:.0}% / pileup {:.0}% of the {:?} shardable core",
            t_decode.as_secs_f64() / core.as_secs_f64() * 100.0,
            t_pileup.as_secs_f64() / core.as_secs_f64() * 100.0,
            core
        );
    }
}
