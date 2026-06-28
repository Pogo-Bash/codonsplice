//! Emit the NATIVE serial `call_variants_region` result + the matching
//! `VariantOptions` JSON, so the WASM byte-identity harness
//! (`crates/codonsplice-wasm/test/byte_identity.mjs`) can compare the wasm
//! single-thread and sharded pipelines against the exact same serial bytes.
//!
//! Usage: cargo run --release --example wasm_baseline -- <bam> <bai> <ref.fa> \
//!            <chrom> <start> <end> <out_dir>
//!
//! Writes <out_dir>/opts.json (VariantOptions incl. reference_seqs) and
//! <out_dir>/serial.json (the serial variant array, serde_json compact).

use std::collections::HashMap;

use cnvlens_core::model::{Region, VariantOptions};
use cnvlens_core::variants::call_variants_region;

fn parse_fasta(bytes: &[u8]) -> HashMap<String, String> {
    let text = String::from_utf8_lossy(bytes);
    let mut map = HashMap::new();
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

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (bam_p, bai_p, ref_p) = (&a[1], &a[2], &a[3]);
    let chrom = a[4].clone();
    let start: i64 = a[5].parse().unwrap();
    let end: i64 = a[6].parse().unwrap();
    let out_dir = &a[7];

    let bam = std::fs::read(bam_p).unwrap();
    let bai = std::fs::read(bai_p).unwrap();
    let reference = parse_fasta(&std::fs::read(ref_p).unwrap());

    let mut opts = VariantOptions::default();
    opts.reference_seqs = Some(reference);

    // Serial baseline: one pileup over the whole inclusive region, clamped to
    // [start, end] exactly as the sharded merge clamps each shard.
    let region = Region::with_bounds(&chrom, Some(start), Some(end));
    let mut vars = call_variants_region(&bam, &bai, &region, &opts).unwrap();
    vars.retain(|v| v.pos >= start && v.pos <= end);

    // VariantOptions is Deserialize-only, so emit the JSON the wasm export will
    // re-parse by hand. Only reference_seqs is non-default; everything else falls
    // through #[serde(default)] on both sides — identical config.
    let opts_json = serde_json::json!({
        "reference_seqs": opts.reference_seqs,
    });

    std::fs::create_dir_all(out_dir).unwrap();
    std::fs::write(
        format!("{out_dir}/opts.json"),
        serde_json::to_string(&opts_json).unwrap(),
    )
    .unwrap();
    std::fs::write(
        format!("{out_dir}/serial.json"),
        serde_json::to_string(&vars).unwrap(),
    )
    .unwrap();
    eprintln!(
        "serial baseline: {} variants over {chrom}:{start}-{end} -> {out_dir}/serial.json",
        vars.len()
    );
}
