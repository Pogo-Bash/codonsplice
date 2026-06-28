# Region-Sharded Parallelism — WASM Backend (Track 2)

Status: **native backend built + serial-equivalence proven; WASM single-thread
fallback VERIFIED byte-identical in a real `wasm-pack` build; WASM worker pool +
SharedArrayBuffer wiring BUILT and compiling; parallel-in-browser byte-identity
DESIGNED/BUILT-not-run (needs a cross-origin-isolated host — steps in §5/§6).**

This document is the contract the WASM worker-pool implementation satisfies; it
slots in *without* rewriting the sharding/merge brain.

## 0. What changed (this branch, `feat/wasm-threads`)

- **The bare-`wasm32` link no longer fails.** The old blocker (`zlib_rs`
  `malloc`/`free` undefined in a bare `wasm32-unknown-unknown` link) does **not**
  reproduce under `wasm-pack` 0.13.1 / Rust 1.96 with the current `noodles`
  stack: `wasm-pack build crates/codonsplice-wasm --target {web,nodejs}` links
  clean (894 KB `.wasm`). `wasm-pack` supplies the allocator; that resolves it.
- **New WASM exports** (`crates/codonsplice-wasm/src/lib.rs`) make the Rust
  sharding brain callable from JS: `crossorigin_isolated()`, `shard_parallelism()`,
  `plan_shards(...)` (reuses `split_region`/`split_region_bai`),
  `call_variants_region(...)` (the per-shard producer), and `merge_shards(...)`
  (the boundary-correct clamp + shard-ordered concat).
- **`WasmShardExecutor`** added to `shard.rs`: the in-module executor, serial by
  construction (one wasm instance == one thread), unit-tested byte-identical to
  `SerialExecutor`. The VM (`sharded_variant_producer`) now picks it via
  `cfg(target_arch = "wasm32")` so `std::thread::scope` is never *reached* on wasm.
- **JS worker pool** (`crates/codonsplice-wasm/js/shard-pool.js` +
  `shard-worker.js`): copies BAM/BAI once into a `SharedArrayBuffer`, fans shards
  to Web Workers, merges via `merge_shards`. Falls back to a single sequential
  call when not isolated.
- **Byte-identity proven** by running the real wasm in Node
  (`crates/codonsplice-wasm/test/byte_identity.mjs`): the single whole-region call
  AND the full sharded pipeline (`plan_shards` → per-shard `call_variants_region`
  → `merge_shards`, at 2/4/6 shards) are byte-identical to the native serial
  `call_variants_region` baseline.

## 1. What already exists (the backend-agnostic brain)

All of the boundary-correct logic lives in `crates/codonsplice-core/src/shard.rs`
and is completely backend-independent:

- `split_region(chrom, start, end, n)` — partitions an **inclusive** `[start,
  end]` region into `n` shards with **no gap and no overlap** (`shards[i+1].start
  == shards[i].end + 1`). This is the #20 half-open-boundary danger zone, handled
  once, here.
- `plan_shard_count(span, available)` — decides the shard count. Returns **1
  (serial)** for small spans (`< MIN_SHARD_SPAN`, 50 kb) or a single core. This
  is the *load-bearing single-thread rule*: parallelism is a speed enhancement,
  never required for correctness.
- `shard_and_merge(executor, shards, produce, pos_of)` — runs `produce` per
  shard, **clamps** each shard's output to its inclusive bounds (so a feature on
  a boundary is emitted by exactly one shard — not dropped, not duplicated), and
  concatenates in shard order. Because each shard's producer returns
  position-sorted output and the shards partition the region in ascending order,
  the concatenation is globally sorted and **byte-identical to the serial
  producer over the whole region**.
- `trait ShardExecutor` — **the only thing a backend swaps.** It runs a closure
  over each shard and returns results *in shard order*.

Implemented executors:

| Executor | Mechanism | Use |
|---|---|---|
| `SerialExecutor` | maps in-process, no threads | always-available fallback / equivalence baseline |
| `NativeThreadExecutor` | `std::thread::scope`, one worker per shard | native CLI (built, tested) |
| `WasmShardExecutor` | serial in-module (one wasm instance == one thread) | wasm in-module dispatch (built, tested) |
| JS worker pool (`js/shard-pool.js`) | Web Workers + SharedArrayBuffer, re-enters the exported `call_variants_region` per shard | browser parallel (built; parallel run needs a COI host) |

The native path is wired into the VM (`sharded_variant_producer` in `vm.rs`) and
proven byte-identical to serial (`tests/shard_equivalence_tests.rs`, plus a
CLI-level md5 match). A WASM backend only has to implement `ShardExecutor`; the
clamp/merge/ordering guarantees come for free.

## 2. WASM worker-pool design

### 2.1 Why threads need special setup in the browser

WASM threads are `SharedArrayBuffer` (SAB) + the WebAssembly threads proposal:
all workers share one linear memory, and Rust's `std`/`wasm-bindgen-rayon` build
the WASM with `atomics` + `bulk-memory` so `Atomics.wait`/`notify` work. The
browser only exposes SAB when the page is **cross-origin isolated**, which the
server must opt into with two response headers (see §3).

### 2.2 Topology

```
main thread (UI)                         worker pool (N = navigator.hardwareConcurrency)
─────────────────                        ────────────────────────────────────────────
load BAM + BAI bytes  ──┐
                        ├─► copy once into a SharedArrayBuffer (read-only to workers)
split_region(..,N) ─────┘
   │  shard descriptors (chrom,start,end) — tiny, postMessage'd
   ▼
for each shard i ──► worker i: instantiate the SAME wasm module over the SHARED
                     memory, run call_variants_region(shard) → Vec<Variant>
   ▲
   │ results returned (postMessage / written to a per-worker output region)
   ▼
shard_and_merge clamp + concat (on main thread)  ──► identical to serial
```

Key invariant: the **BAM/BAI bytes are shared read-only**; each worker does its
own BAI seek + BGZF inflate + pileup for its shard. No mutation, no locks on the
input — the only synchronization is the join/postMessage at the end. This maps
exactly onto `ShardExecutor::run_shards`: spawn N workers, collect results in
shard order.

### 2.3 Rust side

Two viable implementations, both leaving the brain untouched:

1. **`wasm-bindgen-rayon`** — provides a `rayon` thread pool backed by Web
   Workers. `NativeThreadExecutor` could be generalized to a `RayonExecutor`
   (`shards.par_iter().map(f)`), shared by native and wasm. Requires building
   with `RUSTFLAGS="-C target-feature=+atomics,+bulk-memory,+mutable-globals"`
   and `--target wasm32-unknown-unknown` + `-Z build-std=std,panic_abort`
   (nightly), then `wasm-pack` with the `--target web` output.
2. **Hand-rolled `WasmWorkerExecutor`** — a JS worker pool that the wasm calls
   via imported functions: `run_shards` posts shard descriptors, each worker
   re-enters an exported `run_one_shard(shard_json) -> variants_json` over the
   shared module/memory, and the main thread reassembles. More code, no nightly.

Either way the implementor writes only an `impl ShardExecutor for
WasmWorkerExecutor`; `split_region` / `shard_and_merge` / `plan_shard_count` are
reused verbatim.

### 2.4 Choosing shard count in WASM

```text
available = crossOriginIsolated && sabSupported ? navigator.hardwareConcurrency : 1
n = plan_shard_count(region_span, available)   // n == 1 ⇒ pure serial
```

Passing `available = 1` when isolation/SAB is missing makes the *existing*
`plan_shard_count` return 1, so the engine transparently runs single-threaded.
The small-query threshold (`MIN_SHARD_SPAN`) applies identically in the browser.

## 3. COOP / COEP headers (what builders must serve)

`SharedArrayBuffer` (hence WASM threads) is gated behind cross-origin isolation.
The server hosting the app **must** send, on the top-level document (and ideally
all same-origin responses):

```
Cross-Origin-Opener-Policy: same-origin
Cross-Origin-Embedder-Policy: require-corp
```

- **COOP `same-origin`** severs the browsing-context group from cross-origin
  openers (process isolation).
- **COEP `require-corp`** forces every subresource to explicitly opt in
  (`Cross-Origin-Resource-Policy: cross-origin` or CORS), so no
  non-cooperating cross-origin resource can be embedded.

With both present, `self.crossOriginIsolated === true` and `SharedArrayBuffer`
is available. Cross-origin assets (CDN scripts, fonts) must then carry CORP/CORS
headers or be self-hosted, or they will fail to load — a real deployment cost,
which is exactly why threading must stay *optional*.

For the cnvlens/splice Oracle deploy (static files behind the web server), this
means adding the two headers to the server/CDN config for the app's routes.

## 4. The fallback path (load-bearing single-thread rule)

The engine MUST work with one thread; threading only ever speeds it up. The
fallback is enforced at three layers, all already in place on the Rust side:

1. **Capability detection (JS / wasm boundary):**
   `crossOriginIsolated && typeof SharedArrayBuffer !== "undefined"`. If false,
   pass `available = 1` to `plan_shard_count` → serial.
2. **Planner (`plan_shard_count`):** returns 1 for single-core *or* small spans,
   so even an isolated page runs small queries serially (no worker overhead).
3. **Executor:** `SerialExecutor` is a complete, always-available
   implementation of `ShardExecutor`. If the worker pool fails to spin up for
   any reason, swap in `SerialExecutor` and the result is identical — the
   equivalence tests cover `SerialExecutor` over the same shard splits.

`SPLICE_SHARDS=1` (native) is the CLI equivalent of "isolation unavailable" and
is what the byte-identical VM test uses as its serial baseline.

## 5. Verification status (what is RUN vs BUILT vs DESIGNED)

**VERIFIED-RUNNING:**

- **Native parallel:** built and wired into the VM. ~2.3x wall-clock on the
  EGFR sample at 16 shards (sublinear; density-aware split now lands the cuts).
- **Serial equivalence (native):** proven byte-identical (2/3/4/8/16 shards,
  boundary variant at `55220177` on both the inclusive end and start of a shard,
  splits AT every known variant position, and an end-to-end VM `SPLICE_SHARDS=1`
  vs `=8` comparison; plus a CLI md5 match). Plus a new `WasmShardExecutor`
  unit test (any worker count ⇒ serial bytes).
- **WASM build:** `wasm-pack build crates/codonsplice-wasm --target nodejs`
  (and `--target web`) **links and runs**. 894 KB `.wasm`.
- **WASM single-thread byte-identity:** `node
  crates/codonsplice-wasm/test/byte_identity.mjs` runs the real wasm and
  confirms BOTH the single whole-region `call_variants_region` AND the full
  sharded pipeline (`plan_shards` → per-shard `call_variants_region` →
  `merge_shards`, sequentially — exactly one worker's worth of work each) are
  **byte-identical** to the native serial baseline. In Node,
  `crossorigin_isolated()` is `false` and `shard_parallelism()` is `1`, so this
  is precisely the FALLBACK path being exercised.

**BUILT-not-run (browser):**

- **WASM worker pool + SAB wiring** (`js/shard-pool.js`, `js/shard-worker.js`):
  written, syntax-valid, and drives only the exports already proven
  byte-identical. The *parallel dispatch itself* has not been executed in a
  cross-origin-isolated browser in this session (no headless COI browser
  available here). The merge it performs is the same `merge_shards` proven above.

**DESIGNED:**

- **Shared-wasm-memory threads** (`wasm-bindgen-rayon`, atomics+bulk-memory,
  nightly `build-std`) — *not* used. The hand-rolled pool here shares only the
  read-only INPUT bytes via SAB and gives each worker its own wasm instance, so
  no nightly/atomics build is needed. The rayon route remains an option (§2.3).

### Manual parallel-in-browser verification (the one step not run here)

1. `wasm-pack build crates/codonsplice-wasm --target web --out-dir pkg`
2. `cargo run --release --example wasm_baseline -- <bam> <bai> <ref.fa> 7
   54990000 55300100 crates/codonsplice-wasm/js` (writes `opts.json` + the
   `serial.json` baseline next to `verify-coi.html`); put the sample
   BAM/BAI under `crates/codonsplice-wasm/js/data/`.
3. `node crates/codonsplice-wasm/js/coi-server.mjs` (sends COOP `same-origin` +
   COEP `require-corp`).
4. Open `http://localhost:8080/crates/codonsplice-wasm/js/verify-coi.html`.
5. Expect `crossOriginIsolated = true`, `mode=parallel`, `workers>1`, and
   `PARALLEL == SERIAL: byte-identical`. If `crossOriginIsolated` is false the
   headers aren't reaching the page and the pool falls back to serial (still
   correct, just not parallel).
