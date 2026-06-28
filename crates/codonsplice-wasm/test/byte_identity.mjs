// Byte-identity harness: prove the WASM variant-calling pipeline reproduces the
// NATIVE serial `call_variants_region` bytes — via (1) a single whole-region
// call (the no-SAB / single-thread fallback), and (2) the full sharded pipeline
// run sequentially in-process (plan_shards -> per-shard call_variants_region ->
// merge_shards). In Node there is no crossOriginIsolated context, so this is the
// FALLBACK path; it must still be byte-identical.
//
// Usage: node byte_identity.mjs <pkg-node-dir> <scratch-dir> <bam> <bai> \
//          <chrom> <start> <end>
//
// Exit 0 + "PASS" on byte-identity for every shard count; non-zero on any diff.

import { readFileSync } from "node:fs";
import { join } from "node:path";

const [, , pkgDir, scratch, bamPath, baiPath, chrom, startS, endS] = process.argv;
const start = Number(startS);
const end = Number(endS);

const wasm = await import(join(process.cwd(), pkgDir, "codonsplice_wasm.js"));

const bam = new Uint8Array(readFileSync(bamPath));
const bai = new Uint8Array(readFileSync(baiPath));
const optsJson = readFileSync(join(scratch, "opts.json"), "utf8");
const serial = readFileSync(join(scratch, "serial.json"), "utf8");

let failures = 0;
const check = (label, got) => {
  const ok = got === serial;
  console.log(`${ok ? "PASS" : "FAIL"}  ${label}`);
  if (!ok) {
    failures++;
    console.log(`   expected ${serial.length}B: ${serial.slice(0, 120)}`);
    console.log(`   got      ${got.length}B: ${got.slice(0, 120)}`);
  }
};

// Capability detection (Node: not isolated -> serial fallback).
console.log(`crossorigin_isolated() = ${wasm.crossorigin_isolated()}`);
console.log(`shard_parallelism()    = ${wasm.shard_parallelism()}`);

// (1) Single whole-region call — the literal single-thread fallback path.
const whole = wasm.call_variants_region(bam, bai, chrom, start, end, optsJson);
check("single whole-region call_variants_region == native serial", whole);

// (2) Full sharded pipeline, sequential (one wasm instance == one thread, which
// is exactly what each worker would run). Force several shard counts via the
// `available` argument so plan_shards actually carves the region.
for (const available of [2, 4, 8, 16]) {
  const shardsJson = wasm.plan_shards(chrom, start, end, available, bam, bai);
  const shards = JSON.parse(shardsJson);
  // Keep each shard's result as the RAW JSON string the worker returned — do not
  // JSON.parse it (JS would collapse 999.0 -> 999). merge_shards reparses in Rust.
  const perShardStrings = shards.map((s) =>
    wasm.call_variants_region(bam, bai, s.chrom, s.start, s.end, optsJson),
  );
  const merged = wasm.merge_shards(JSON.stringify(perShardStrings), shardsJson);
  check(`sharded pipeline (available=${available}, ${shards.length} shards) == native serial`, merged);
}

if (failures > 0) {
  console.error(`\n${failures} byte-identity failure(s)`);
  process.exit(1);
}
console.log("\nALL PASS — wasm pipeline byte-identical to native serial");
