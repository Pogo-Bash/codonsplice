// CodonSplice WASM shard worker (Track 2, level 3). One per pool worker; runs a
// single shard's variant pileup over the SharedArrayBuffer-backed BAM/BAI bytes
// and posts the raw result JSON back. Pairs with `shard-pool.js`.
//
// STATUS: BUILT, not browser-run here. Requires a cross-origin-isolated context
// for SharedArrayBuffer. See ../../../docs/design/PARALLELISM_WASM.md.
//
// Each worker has its OWN wasm instance (no shared wasm linear memory, so no
// atomics/nightly build needed). The SharedArrayBuffer only shares the read-only
// INPUT bytes — wasm-bindgen copies them into this worker's heap at the call
// boundary. The shard's pileup over [start, end] is independent of every other
// shard; the main thread merges with the boundary-correct clamp.

let wasm = null;
let bamView = null; // Uint8Array over the shared BAM bytes
let baiView = null; // Uint8Array over the shared BAI bytes

self.onmessage = async (e) => {
  const msg = e.data;
  try {
    if (msg.type === "init") {
      const mod = await import(msg.wasmUrl);
      await mod.default({ module_or_path: msg.wasmUrl });
      wasm = mod;
      bamView = new Uint8Array(msg.sabBam);
      baiView = new Uint8Array(msg.sabBai);
      self.postMessage({ ok: true });
      return;
    }
    if (msg.type === "shard") {
      // Raw JSON string — do NOT parse here; the pool forwards it verbatim to
      // the Rust merge so number formatting (e.g. 999.0) is preserved.
      const json = wasm.call_variants_region(
        bamView, baiView, msg.chrom, msg.start, msg.end, msg.optsJson,
      );
      self.postMessage({ index: msg.index, json });
      return;
    }
    self.postMessage({ error: `unknown message type: ${msg.type}` });
  } catch (err) {
    self.postMessage({ error: String(err && err.stack ? err.stack : err) });
  }
};
