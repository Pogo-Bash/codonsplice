//! Track 2 — serial vs region-sharded wall-clock for `CALL variants`.
//!
//! Usage: cargo run --release --example bench_shard -- <bam> <bai> <ref.fa> <chrom> <start> <end> [iters]

use std::time::Instant;

use cnvlens_core::model::{Region, VariantOptions};
use cnvlens_core::variants::call_variants_region;
use codonsplice_core::shard::{shard_and_merge, split_region, NativeThreadExecutor, Shard};

fn parse_fasta(bytes: &[u8]) -> std::collections::HashMap<String, String> {
    let text = String::from_utf8_lossy(bytes);
    let mut map = std::collections::HashMap::new();
    let (mut name, mut seq) = (String::new(), String::new());
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

fn median(mut xs: Vec<f64>) -> f64 {
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    xs[xs.len() / 2]
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (bam_p, bai_p, ref_p) = (&a[1], &a[2], &a[3]);
    let chrom = a[4].clone();
    let start: i64 = a[5].parse().unwrap();
    let end: i64 = a[6].parse().unwrap();
    let iters: usize = a.get(7).and_then(|s| s.parse().ok()).unwrap_or(30);

    let bam = std::fs::read(bam_p).unwrap();
    let bai = std::fs::read(bai_p).unwrap();
    let mut opts = VariantOptions::default();
    opts.reference_seqs = Some(parse_fasta(&std::fs::read(ref_p).unwrap()));

    let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    eprintln!("available_parallelism = {cpus}");

    // Serial baseline.
    let mut serial_ms = Vec::new();
    let mut n_vars = 0;
    for _ in 0..iters {
        let region = Region::with_bounds(chrom.clone(), Some(start), Some(end));
        let t = Instant::now();
        let v = call_variants_region(&bam, &bai, &region, &opts).unwrap();
        serial_ms.push(t.elapsed().as_secs_f64() * 1e3);
        n_vars = v.len();
    }
    let serial = median(serial_ms);
    eprintln!("\nserial: {serial:.2} ms  ({n_vars} variants)");

    for n in [2usize, 4, 8, cpus] {
        let shards = split_region(&chrom, start, end, n);
        let mut ms = Vec::new();
        for _ in 0..iters {
            let t = Instant::now();
            let v = shard_and_merge(
                &NativeThreadExecutor,
                &shards,
                |s: &Shard| call_variants_region(&bam, &bai, &s.to_core_region().to_core(), &opts),
                |v: &cnvlens_core::model::Variant| v.pos,
            )
            .unwrap();
            ms.push(t.elapsed().as_secs_f64() * 1e3);
            assert_eq!(v.len(), n_vars, "sharded variant count must match serial");
        }
        let m = median(ms);
        eprintln!(
            "shards={:<2} ({} actual): {:.2} ms   speedup {:.2}x",
            n,
            shards.len(),
            m,
            serial / m
        );
    }
}
