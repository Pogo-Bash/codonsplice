// CodonSplice WASM worker pool — true parallel region-sharded variant calling
// in the browser (Track 2, level 3). Pairs with `shard-worker.js`.
//
// STATUS: BUILT, COMPILES (the wasm exports it drives are verified byte-identical
// to native serial — see ../test/byte_identity.mjs). The *parallel* path requires
// a cross-origin-isolated browser context (SharedArrayBuffer); see verify-coi.html
// and ../../../docs/design/PARALLELISM_WASM.md §3 for the COOP/COEP headers a host
// must send and the manual verification steps. Run in a non-isolated context, the
// pool transparently falls back to a single sequential call — the load-bearing
// single-thread rule — which IS verified.
//
// Design (matches PARALLELISM_WASM.md §2.2): the BAM + BAI bytes are copied ONCE
// into a SharedArrayBuffer that every worker reads (no per-worker postMessage copy
// of the inputs). Each worker has its OWN wasm instance and calls the exported
// `call_variants_region(bam, bai, chrom, start, end, opts)` over its shard's
// slice of the shared bytes. The main thread then reuses the Rust
// `merge_shards(...)` (boundary-correct clamp + shard-ordered concat) so the
// result is byte-identical to the serial whole-region call.

import initWasm, {
  crossorigin_isolated,
  shard_parallelism,
  plan_shards,
  call_variants_region,
  merge_shards,
} from "../pkg/codonsplice_wasm.js";

/**
 * Run `CALL variants` over [start, end] on `chrom`, parallelised across Web
 * Workers when the page is cross-origin isolated, else sequentially. Returns the
 * variant array (parsed JSON) — byte-identical either way.
 *
 * @param {object} cfg
 * @param {string} cfg.wasmUrl   URL of codonsplice_wasm.js (the worker imports it)
 * @param {Uint8Array} cfg.bam   BAM bytes
 * @param {Uint8Array} cfg.bai   BAI bytes (may be empty -> uniform split)
 * @param {string} cfg.chrom
 * @param {number} cfg.start     1-based inclusive
 * @param {number} cfg.end       1-based inclusive
 * @param {string} cfg.optsJson  snake_case VariantOptions JSON
 * @param {number} [cfg.maxWorkers]  cap on worker count (default: hardwareConcurrency)
 * @returns {Promise<{variants: object[], mode: "parallel"|"serial", workers: number, shards: number}>}
 */
export async function callVariantsSharded(cfg) {
  await initWasm({ module_or_path: cfg.wasmUrl });

  const isolated = crossorigin_isolated();
  // shard_parallelism() == 1 when not isolated -> plan_shards returns 1 shard ->
  // pure serial. This is the single-thread fallback, identical output.
  let available = isolated ? shard_parallelism() : 1;
  if (cfg.maxWorkers) available = Math.min(available, cfg.maxWorkers);

  const shardsJson = plan_shards(cfg.chrom, cfg.start, cfg.end, available, cfg.bam, cfg.bai);
  const shards = JSON.parse(shardsJson);

  // ── Fallback: not isolated, or planner chose a single shard. One sequential
  // call over the whole region — load-bearing, always available, byte-identical.
  if (!isolated || shards.length <= 1) {
    const json = call_variants_region(
      cfg.bam, cfg.bai, cfg.chrom, cfg.start, cfg.end, cfg.optsJson,
    );
    return { variants: JSON.parse(json), mode: "serial", workers: 1, shards: shards.length };
  }

  // ── Parallel: copy inputs ONCE into SharedArrayBuffers, fan shards out.
  const sabBam = toShared(cfg.bam);
  const sabBai = toShared(cfg.bai);

  const workerCount = Math.min(shards.length, available);
  const workers = Array.from({ length: workerCount }, () =>
    new Worker(new URL("./shard-worker.js", import.meta.url), { type: "module" }),
  );
  // Hand each worker the wasm module URL + the shared inputs up front (one init).
  await Promise.all(
    workers.map((w) => postAwait(w, { type: "init", wasmUrl: cfg.wasmUrl, sabBam, sabBai })),
  );

  // Dispatch shards to workers (a worker takes the next shard when it finishes).
  const results = new Array(shards.length); // raw JSON strings, in shard order
  let next = 0;
  await Promise.all(
    workers.map((w) => (async function pump() {
      while (next < shards.length) {
        const i = next++;
        const s = shards[i];
        const res = await postAwait(w, {
          type: "shard",
          index: i,
          chrom: s.chrom,
          start: s.start,
          end: s.end,
          optsJson: cfg.optsJson,
        });
        results[res.index] = res.json; // raw string — must NOT JSON.parse here
      }
    })()),
  );
  workers.forEach((w) => w.terminate());

  // Boundary-correct merge in Rust (clamp + shard-ordered concat). Pass the raw
  // per-shard strings so serde_json reparses numbers (keeps 999.0 as 999.0).
  const mergedJson = merge_shards(JSON.stringify(results), shardsJson);
  return {
    variants: JSON.parse(mergedJson),
    mode: "parallel",
    workers: workerCount,
    shards: shards.length,
  };
}

/** Copy a Uint8Array into a SharedArrayBuffer-backed Uint8Array. */
function toShared(bytes) {
  const sab = new SharedArrayBuffer(bytes.byteLength);
  new Uint8Array(sab).set(bytes);
  return sab;
}

/** postMessage + await the matching response (one in-flight request per worker). */
function postAwait(worker, msg) {
  return new Promise((resolve, reject) => {
    worker.onmessage = (e) => (e.data && e.data.error ? reject(new Error(e.data.error)) : resolve(e.data));
    worker.onerror = (e) => reject(e);
    worker.postMessage(msg);
  });
}
