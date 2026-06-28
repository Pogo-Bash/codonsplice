//! Region-sharded parallelism (Track 2).
//!
//! Genomic regions are independent: the variants/coverage over `7:55.0M-55.1M`
//! can be computed without ever looking at `7:55.1M-55.2M`. So a query whose
//! `WHERE` pushed down a `chr/pos` region (see [`crate::extract_region`]) can be
//! split into N contiguous sub-regions ("shards"), each run independently, and
//! the per-shard results merged back into the exact serial order/semantics.
//!
//! ## The two halves of this module
//!
//! 1. **The brain (backend-agnostic):** [`split_region`] partitions a region
//!    into boundary-correct shards, and [`shard_and_merge`] runs a producer over
//!    each shard, clamps each shard's output to its INCLUSIVE bounds, and
//!    concatenates in shard order. This logic never changes between backends.
//! 2. **The dispatch (the only thing a backend swaps):** the [`ShardExecutor`]
//!    trait. [`NativeThreadExecutor`] uses scoped OS threads; a future
//!    `WasmExecutor` (Web Workers + SharedArrayBuffer) slots in here WITHOUT
//!    touching the brain. [`SerialExecutor`] is the single-thread reference used
//!    by the equivalence tests and as the always-available fallback.
//!
//! ## Boundary correctness (the #20 danger zone)
//!
//! Shard bounds are 1-based **INCLUSIVE** genomic coordinates. [`split_region`]
//! produces a partition with **no overlap and no gap**: shard `i` ends at `hi_i`
//! and shard `i+1` begins at `hi_i + 1`. A variant at exactly a boundary
//! position belongs to exactly one shard.
//!
//! The seek itself (BAI random access) over-fetches: a shard for `[lo, hi]`
//! fetches every read *overlapping* `[lo, hi]`, so the pileup at any position
//! `p` in `[lo, hi]` sees *all* reads covering `p` — identical to the serial
//! pileup. But that same over-fetch means a shard can also *emit* a call at a
//! position just outside `[lo, hi]` (from a read that straddles the boundary),
//! which the neighbouring shard would emit too. [`shard_and_merge`] therefore
//! **clamps** each shard's output to `[lo, hi]` before merging, so every emitted
//! position lands in exactly one shard: not dropped, not duplicated.

/// One contiguous sub-region of a query's region. `start`/`end` are 1-based
/// **inclusive** genomic coordinates (`start <= end`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shard {
    /// Position of this shard in the split (0-based, ascending by coordinate).
    pub index: usize,
    pub chrom: String,
    /// Inclusive lower bound.
    pub start: i64,
    /// Inclusive upper bound.
    pub end: i64,
}

impl Shard {
    /// The cnvlens-core seek region for this shard. Inclusive `[start, end]`.
    pub fn to_core_region(&self) -> crate::runtime::Region {
        crate::runtime::Region {
            chrom: self.chrom.clone(),
            start: Some(self.start),
            end: Some(self.end),
        }
    }

    /// Whether a 1-based position falls inside this shard's inclusive bounds.
    #[inline]
    pub fn contains(&self, pos: i64) -> bool {
        pos >= self.start && pos <= self.end
    }
}

/// Partition the inclusive region `[start, end]` on `chrom` into at most `n`
/// contiguous shards with **no overlap and no gap**.
///
/// Guarantees (the boundary contract):
/// * `shards[0].start == start` and `shards.last().end == end`.
/// * `shards[i+1].start == shards[i].end + 1` (gap-free, overlap-free).
/// * Every integer position in `[start, end]` is contained by exactly one shard.
///
/// Degenerate inputs collapse to a single shard: `n <= 1`, `start >= end`, or a
/// span with fewer positions than `n` (so no empty shards are produced).
pub fn split_region(chrom: &str, start: i64, end: i64, n: usize) -> Vec<Shard> {
    let span = end - start + 1; // inclusive count of positions
    let n = n.max(1).min(span.max(1) as usize);
    if n <= 1 || start >= end {
        return vec![Shard {
            index: 0,
            chrom: chrom.to_string(),
            start,
            end,
        }];
    }

    let base = span / n as i64;
    let rem = span % n as i64; // first `rem` shards get one extra position
    let mut shards = Vec::with_capacity(n);
    let mut lo = start;
    for i in 0..n {
        let width = base + if (i as i64) < rem { 1 } else { 0 };
        // INCLUSIVE upper bound: width positions means [lo, lo + width - 1].
        let hi = lo + width - 1;
        shards.push(Shard {
            index: i,
            chrom: chrom.to_string(),
            start: lo,
            end: hi,
        });
        lo = hi + 1; // next shard starts exactly one past this one — no gap/overlap.
    }
    debug_assert_eq!(shards.last().unwrap().end, end);
    shards
}

/// A cheap per-window estimate of how much *work* (read density) a region
/// carries, used to place shard cuts on equal **work** rather than equal
/// coordinate span. `weights[i]` is the estimated work for the genomic window
/// `[first_window_start + i*window_size, first_window_start + (i+1)*window_size - 1]`
/// (1-based, inclusive). The unit of `weights` is arbitrary and only relative
/// magnitudes matter — the BAI estimator uses *compressed BGZF bytes per 16 kb
/// linear-index window*, a direct proxy for reads-per-window that needs zero
/// read scanning.
#[derive(Debug, Clone, PartialEq)]
pub struct DensityProfile {
    /// Genomic span (in positions) each weight covers. For the BAI linear index
    /// this is 16384.
    pub window_size: i64,
    /// 1-based genomic position where `weights[0]`'s window begins.
    pub first_window_start: i64,
    /// Per-window work estimate.
    pub weights: Vec<u64>,
}

impl DensityProfile {
    /// Work estimate (interpolated, positions assumed uniformly weighted within a
    /// window) for the inclusive coordinate range `[lo, hi]`.
    pub fn weight_in(&self, lo: i64, hi: i64) -> f64 {
        if hi < lo || self.window_size <= 0 || self.weights.is_empty() {
            return 0.0;
        }
        let per = |w: usize| self.weights.get(w).copied().unwrap_or(0) as f64 / self.window_size as f64;
        let start = lo.max(self.first_window_start);
        if hi < start {
            return 0.0; // entirely before the first window -> zero work
        }
        let w_first = ((start - self.first_window_start) / self.window_size) as usize;
        let w_last = ((hi - self.first_window_start) / self.window_size) as usize;
        let mut total = 0.0;
        for w in w_first..=w_last {
            let win_lo = self.first_window_start + w as i64 * self.window_size;
            let win_hi = win_lo + self.window_size - 1;
            let clamp_lo = win_lo.max(start);
            let clamp_hi = win_hi.min(hi);
            if clamp_hi >= clamp_lo {
                total += per(w) * (clamp_hi - clamp_lo + 1) as f64;
            }
        }
        total
    }

    /// The smallest 1-based position `p` in `[start, end]` whose cumulative work
    /// from `start` reaches `target`. Cuts are placed by inverting the cumulative
    /// work curve here. Returns `end` if `target` exceeds the total.
    fn pos_at_cumulative(&self, start: i64, end: i64, target: f64) -> i64 {
        if target <= 0.0 {
            return start;
        }
        let per = |w: usize| self.weights.get(w).copied().unwrap_or(0) as f64 / self.window_size as f64;
        let scan_start = start.max(self.first_window_start);
        if end < scan_start {
            return end;
        }
        let w_first = ((scan_start - self.first_window_start) / self.window_size) as usize;
        let w_last = ((end - self.first_window_start) / self.window_size) as usize;
        let mut acc = 0.0;
        for w in w_first..=w_last {
            let win_lo = self.first_window_start + w as i64 * self.window_size;
            let win_hi = win_lo + self.window_size - 1;
            let lo = win_lo.max(start);
            let hi = win_hi.min(end);
            if hi < lo {
                continue;
            }
            let per_pos = per(w);
            let win_weight = per_pos * (hi - lo + 1) as f64;
            if per_pos > 0.0 && acc + win_weight >= target {
                let need = target - acc;
                // Number of positions into this window to cover `need`.
                let k = (need / per_pos).ceil() as i64;
                return (lo + k - 1).clamp(lo, hi);
            }
            acc += win_weight;
        }
        end
    }

    /// Total work over `[start, end]`.
    fn total_weight(&self, start: i64, end: i64) -> f64 {
        self.weight_in(start, end)
    }
}

/// Density-aware analogue of [`split_region`]: partitions `[start, end]` into `n`
/// boundary-correct shards whose **estimated work is balanced** (equal read
/// density) rather than equal coordinate span. Cut points are placed where the
/// cumulative work curve from `profile` crosses `k/n` of the total.
///
/// Honours the exact same boundary contract as [`split_region`] (no gap, no
/// overlap, `shards[0].start == start`, `shards.last().end == end`, every
/// position in exactly one shard). When the profile carries no usable signal
/// (empty or all-zero weights), this falls back to a uniform [`split_region`],
/// so a missing/degenerate index can never change correctness — only speed.
pub fn split_region_density(
    chrom: &str,
    start: i64,
    end: i64,
    n: usize,
    profile: &DensityProfile,
) -> Vec<Shard> {
    let span = end - start + 1;
    let n = n.max(1).min(span.max(1) as usize);
    if n <= 1 || start >= end {
        return vec![Shard {
            index: 0,
            chrom: chrom.to_string(),
            start,
            end,
        }];
    }

    let total = profile.total_weight(start, end);
    if total <= 0.0 {
        // No density signal — uniform split is the honest fallback.
        return split_region(chrom, start, end, n);
    }

    // Place n-1 interior cuts. cut[k] is the INCLUSIVE end of shard k. Each cut is
    // clamped so every shard stays non-empty (boundary discipline): shard k needs
    // room on both sides, so cut[k] in [start+k-1, end-(n-k)] and strictly above
    // the previous cut.
    let mut cuts = Vec::with_capacity(n - 1);
    let mut prev = start - 1;
    for k in 1..n {
        let target = total * k as f64 / n as f64;
        let raw = profile.pos_at_cumulative(start, end, target);
        let lo_bound = (start + k as i64 - 1).max(prev + 1);
        let hi_bound = end - (n as i64 - k as i64);
        let c = raw.clamp(lo_bound, hi_bound);
        cuts.push(c);
        prev = c;
    }

    let mut shards = Vec::with_capacity(n);
    let mut lo = start;
    for i in 0..n {
        let hi = if i + 1 < n { cuts[i] } else { end };
        shards.push(Shard {
            index: i,
            chrom: chrom.to_string(),
            start: lo,
            end: hi,
        });
        lo = hi + 1;
    }
    debug_assert_eq!(shards.first().unwrap().start, start);
    debug_assert_eq!(shards.last().unwrap().end, end);
    shards
}

/// The genomic span (bp) each BAI linear-index entry covers — fixed by the
/// SAM/BAM spec at 16 kb.
pub const BAI_WINDOW_SIZE: i64 = 1 << 14;

/// Estimate per-window read density over `chrom` from the **BAI linear index**,
/// without scanning a single read.
///
/// The BAI linear index stores one BGZF *virtual offset* per 16 kb window: the
/// file offset of the first record that overlaps that window. The
/// **compressed-byte delta** between consecutive window offsets is the volume of
/// BGZF data whose records begin in that window — a cheap, monotone proxy for
/// reads-per-window (denser pileups ⇒ more bytes). This is the "use the .bai
/// linear index to approximate per-region density cheaply" path.
///
/// Returns `None` — and the caller falls back to a uniform split, never a wrong
/// answer — when the header or index can't be parsed, the chromosome is absent,
/// or the index has too few windows to carry a signal.
pub fn estimate_density_from_bai(bam: &[u8], bai: &[u8], chrom: &str) -> Option<DensityProfile> {
    let header = cnvlens_core::bam::read_header(bam).ok()?;
    let target = chrom.as_bytes();
    let ref_id = header.reference_sequences().keys().position(|k| {
        let name: &[u8] = k.as_ref();
        name == target
    })?;

    let index = cnvlens_core::bam::read_bai_index(bai).ok()?;
    let ref_seq = index.reference_sequences().get(ref_id)?;
    let linear = ref_seq.index(); // &Vec<bgzf::VirtualPosition>, one per 16 kb window
    if linear.len() < 2 {
        return None; // not enough windows to balance anything
    }

    let mut weights = Vec::with_capacity(linear.len());
    for pair in linear.windows(2) {
        // Compressed-offset delta == BGZF bytes for records starting in this
        // window. Saturating: virtual offsets are monotone, but never trust it.
        weights.push(pair[1].compressed().saturating_sub(pair[0].compressed()));
    }
    // The final window has no successor offset; reuse the previous window's weight
    // so a dense tail isn't biased toward zero work.
    if let Some(&last) = weights.last() {
        weights.push(last);
    }

    Some(DensityProfile {
        window_size: BAI_WINDOW_SIZE,
        first_window_start: 1, // linear window i covers 1-based [i*16384+1, (i+1)*16384]
        weights,
    })
}

/// Density-aware region split driven by a real BAM+BAI: estimate read density
/// from the BAI linear index ([`estimate_density_from_bai`]) and place cuts on
/// equal work ([`split_region_density`]). Falls back to the uniform
/// [`split_region`] when no density signal is available, so correctness never
/// depends on the index — only the load balance does.
pub fn split_region_bai(
    chrom: &str,
    start: i64,
    end: i64,
    n: usize,
    bam: &[u8],
    bai: &[u8],
) -> Vec<Shard> {
    match estimate_density_from_bai(bam, bai, chrom) {
        Some(profile) => split_region_density(chrom, start, end, n, &profile),
        None => split_region(chrom, start, end, n),
    }
}

/// Minimum region span (count of inclusive positions) worth sharding. Below
/// this, the per-shard BAI re-seek + thread-spawn overhead outweighs the
/// parallelism win. Measured on the EGFR sample: a ~310kb region is a ~100ms
/// CPU-bound core and 2-way sharding already pays off (~1.4x), so a 50kb floor
/// is conservative — smaller queries simply run single-threaded.
pub const MIN_SHARD_SPAN: i64 = 50_000;

/// Decide how many shards a region of `span` inclusive positions should split
/// into, given `available` parallelism.
///
/// Returns **1 (run serially)** when only one core is available or the span is
/// below [`MIN_SHARD_SPAN`]. This encodes the load-bearing single-thread rule:
/// threading is a *speed enhancement*, never required for correctness, so the
/// default for anything small or single-core is plain serial execution. The
/// same function is what a WASM backend calls after detecting
/// `crossOriginIsolated` (passing `available = 1` when SAB is unavailable, which
/// forces serial).
pub fn plan_shard_count(span: i64, available: usize) -> usize {
    if available <= 1 || span < MIN_SHARD_SPAN {
        return 1;
    }
    // Don't carve shards thinner than MIN_SHARD_SPAN, and never exceed cores.
    let by_span = (span / MIN_SHARD_SPAN).max(1) as usize;
    by_span.min(available)
}

/// The dispatch backend: runs a closure over each shard and returns the results
/// **in shard order** (result `i` corresponds to `shards[i]`). This is the ONLY
/// surface a parallel backend implements; the merge brain
/// ([`shard_and_merge`]) is backend-agnostic.
///
/// `f` must be `Sync` (shared by reference across workers); `T` must be `Send`
/// (moved back from workers). Both are trivially satisfied by the native and
/// serial backends and are exactly the bounds a Web Worker backend needs.
pub trait ShardExecutor {
    fn run_shards<T, F>(&self, shards: &[Shard], f: F) -> Vec<T>
    where
        T: Send,
        F: Fn(&Shard) -> T + Sync;
}

/// Single-threaded reference backend. Always available (no threads, no `std`
/// features beyond core) — this is the WASM/`crossOriginIsolated == false`
/// fallback and the serial baseline the equivalence tests compare against.
#[derive(Debug, Clone, Copy, Default)]
pub struct SerialExecutor;

impl ShardExecutor for SerialExecutor {
    fn run_shards<T, F>(&self, shards: &[Shard], f: F) -> Vec<T>
    where
        T: Send,
        F: Fn(&Shard) -> T + Sync,
    {
        shards.iter().map(|s| f(s)).collect()
    }
}

/// Native multi-threaded backend using `std::thread::scope`. Spawns one scoped
/// thread per shard (shard count is chosen to match the desired parallelism, so
/// this is one worker per shard). Scoped threads let `f` borrow from the caller
/// without `'static`, so no `Arc`-cloning of the BAM bytes is needed.
#[derive(Debug, Clone, Copy)]
pub struct NativeThreadExecutor;

impl ShardExecutor for NativeThreadExecutor {
    fn run_shards<T, F>(&self, shards: &[Shard], f: F) -> Vec<T>
    where
        T: Send,
        F: Fn(&Shard) -> T + Sync,
    {
        if shards.len() <= 1 {
            return shards.iter().map(|s| f(s)).collect();
        }
        let f = &f;
        std::thread::scope(|scope| {
            let handles: Vec<_> = shards
                .iter()
                .map(|s| scope.spawn(move || f(s)))
                .collect();
            // join() returns in spawn order == shard order, preserving the
            // serial ordering invariant the merge relies on.
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        })
    }
}

/// WASM shard backend: the executor used **inside a single wasm module
/// instance**. A lone wasm instance has exactly one thread, so this delegates to
/// the serial map — it is, deliberately, byte-for-byte the [`SerialExecutor`].
///
/// The actual cross-shard parallelism in the browser does **not** flow through
/// this trait: a `Fn(&Shard) -> T` closure can't be `postMessage`'d to a Web
/// Worker. Instead the JS worker pool (`js/shard-pool.js`) re-enters the
/// *exported* per-shard function (`call_variants_region` in the `@codonsplice/wasm`
/// bindings) once per shard, each call running this serial executor over its one
/// shard inside its own worker's wasm instance over the SAME
/// `SharedArrayBuffer`-backed input bytes. The boundary-correct clamp/merge then
/// happens once on the main thread (`merge_shards`). So:
///
/// * **In-module dispatch** (this type) is always serial — the load-bearing
///   single-thread guarantee. If the worker pool can't spin up (no
///   `crossOriginIsolated`, no `SharedArrayBuffer`), JS calls the same exported
///   function once over the whole region and the result is identical.
/// * **Cross-worker dispatch** (the JS pool) is the speed enhancement, layered
///   *outside* Rust, reusing [`split_region`]/[`split_region_bai`] (via the
///   exported `plan_shards`) and the same clamp as [`shard_and_merge`].
///
/// `workers` records the parallelism JS detected (`crossOriginIsolated ?
/// hardwareConcurrency : 1`) purely for introspection/telemetry; it never
/// changes the in-module result.
#[derive(Debug, Clone, Copy)]
pub struct WasmShardExecutor {
    /// Parallelism JS detected. `<= 1` means the page is not cross-origin
    /// isolated (or has no SAB) — pure serial fallback.
    pub workers: usize,
}

impl WasmShardExecutor {
    /// Build from the JS-detected parallelism (`crossOriginIsolated ?
    /// navigator.hardwareConcurrency : 1`). `0` is normalised to `1`.
    pub fn from_parallelism(workers: usize) -> Self {
        WasmShardExecutor { workers: workers.max(1) }
    }
}

impl Default for WasmShardExecutor {
    fn default() -> Self {
        WasmShardExecutor { workers: 1 }
    }
}

impl ShardExecutor for WasmShardExecutor {
    fn run_shards<T, F>(&self, shards: &[Shard], f: F) -> Vec<T>
    where
        T: Send,
        F: Fn(&Shard) -> T + Sync,
    {
        // One wasm instance == one thread. Serial, byte-identical to SerialExecutor.
        shards.iter().map(|s| f(s)).collect()
    }
}

/// Run `produce` over every shard, clamp each shard's output to its INCLUSIVE
/// bounds, and concatenate in shard order. This is the boundary-correct merge.
///
/// * `produce(shard)` yields this shard's records (e.g. variants whose seek
///   region is `shard.to_core_region()`); it may legitimately over-produce at
///   the edges (see the module docs).
/// * `pos_of(record)` extracts the record's 1-based genomic position used for
///   the inclusive clamp.
///
/// Because the shards partition `[start, end]` disjointly and the clamp keeps
/// `pos in [shard.start, shard.end]`, every position is emitted by exactly one
/// shard. When each shard's `produce` returns records sorted by position
/// (cnvlens-core sorts variants), the concatenation is globally sorted and
/// therefore **byte-identical** to the serial producer over the whole region.
pub fn shard_and_merge<T, E, F, P, Err>(
    exec: &E,
    shards: &[Shard],
    produce: F,
    pos_of: P,
) -> Result<Vec<T>, Err>
where
    E: ShardExecutor,
    T: Send,
    Err: Send,
    F: Fn(&Shard) -> Result<Vec<T>, Err> + Sync,
    P: Fn(&T) -> i64 + Sync,
{
    let per_shard: Vec<Result<Vec<T>, Err>> = exec.run_shards(shards, |s| {
        let mut items = produce(s)?;
        items.retain(|it| s.contains(pos_of(it)));
        Ok(items)
    });

    let mut merged = Vec::new();
    for shard_result in per_shard {
        merged.extend(shard_result?);
    }
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_partition(shards: &[Shard], start: i64, end: i64) {
        assert_eq!(shards.first().unwrap().start, start, "first starts at start");
        assert_eq!(shards.last().unwrap().end, end, "last ends at end");
        for w in shards.windows(2) {
            // No gap, no overlap: next starts exactly one past prev's inclusive end.
            assert_eq!(w[1].start, w[0].end + 1, "gap/overlap at boundary");
            assert!(w[0].start <= w[0].end, "non-empty shard");
        }
        // Every position covered exactly once.
        for p in start..=end {
            let hits = shards.iter().filter(|s| s.contains(p)).count();
            assert_eq!(hits, 1, "position {p} covered {hits} times (want 1)");
        }
    }

    #[test]
    fn even_split_partitions_cleanly() {
        let shards = split_region("7", 100, 199, 4);
        assert_eq!(shards.len(), 4);
        assert_partition(&shards, 100, 199);
        assert_eq!(shards[0], Shard { index: 0, chrom: "7".into(), start: 100, end: 124 });
        assert_eq!(shards[3].end, 199);
    }

    #[test]
    fn uneven_split_spreads_remainder_to_front() {
        // span = 10, n = 3 -> widths 4, 3, 3 (first `rem` shards get the extra).
        let shards = split_region("7", 1, 10, 3);
        assert_eq!(shards.len(), 3);
        assert_partition(&shards, 1, 10);
        assert_eq!(shards[0], Shard { index: 0, chrom: "7".into(), start: 1, end: 4 });
        assert_eq!(shards[1], Shard { index: 1, chrom: "7".into(), start: 5, end: 7 });
        assert_eq!(shards[2], Shard { index: 2, chrom: "7".into(), start: 8, end: 10 });
    }

    #[test]
    fn boundary_position_lands_in_exactly_one_shard() {
        // Split so a known variant position (55220177) sits ON a boundary edge.
        let shards = split_region("7", 55220177 - 50, 55220177 + 50, 2);
        // The split point: span=101, n=2 -> widths 51, 50. Shard0 = [.. , .+50] ?
        assert_partition(&shards, 55220177 - 50, 55220177 + 50);
        let hits: Vec<usize> = shards
            .iter()
            .filter(|s| s.contains(55220177))
            .map(|s| s.index)
            .collect();
        assert_eq!(hits.len(), 1, "boundary variant in exactly one shard");
    }

    #[test]
    fn degenerate_inputs_collapse_to_single_shard() {
        assert_eq!(split_region("7", 100, 100, 4).len(), 1); // single position
        assert_eq!(split_region("7", 100, 200, 1).len(), 1); // n == 1
        assert_eq!(split_region("7", 200, 100, 4).len(), 1); // start > end
        // n larger than span never makes empty shards.
        let s = split_region("7", 1, 3, 10);
        assert_eq!(s.len(), 3);
        assert_partition(&s, 1, 3);
    }

    #[test]
    fn plan_shard_count_honours_single_thread_rule() {
        // Single core => always serial, regardless of span.
        assert_eq!(plan_shard_count(10_000_000, 1), 1);
        // Small span => serial even with many cores.
        assert_eq!(plan_shard_count(MIN_SHARD_SPAN - 1, 16), 1);
        // Large span scales with cores but is capped by both span and cores.
        assert_eq!(plan_shard_count(MIN_SHARD_SPAN * 4, 16), 4); // span-limited
        assert_eq!(plan_shard_count(MIN_SHARD_SPAN * 100, 8), 8); // core-limited
        assert_eq!(plan_shard_count(0, 16), 1);
    }

    // A toy "variant" to exercise the clamp+merge without a BAM.
    #[derive(Debug, PartialEq, Clone)]
    struct V {
        pos: i64,
    }

    fn toy_produce(s: &Shard) -> Result<Vec<V>, ()> {
        // Simulate BAI over-fetch: emit one position past each edge (which the
        // clamp must drop) plus every in-bounds position.
        let mut v = Vec::new();
        for p in (s.start - 1)..=(s.end + 1) {
            v.push(V { pos: p });
        }
        Ok(v)
    }

    #[test]
    fn density_split_partitions_cleanly_like_uniform() {
        // A flat profile must reproduce a valid partition and stay boundary-correct.
        let profile = DensityProfile {
            window_size: 10,
            first_window_start: 1,
            weights: vec![5; 10], // [1..100], uniform
        };
        let shards = split_region_density("7", 1, 100, 4, &profile);
        assert_eq!(shards.len(), 4);
        assert_partition(&shards, 1, 100);
    }

    #[test]
    fn density_split_shifts_cut_toward_heavy_region() {
        // 10 windows over [1..100]; window 9 (positions 91..100) is 1000x denser.
        // A 2-way density split must give the heavy tail a MUCH narrower coordinate
        // span than the uniform 50/50 split would.
        let mut weights = vec![1u64; 10];
        weights[9] = 1000;
        let profile = DensityProfile {
            window_size: 10,
            first_window_start: 1,
            weights,
        };
        let shards = split_region_density("7", 1, 100, 2, &profile);
        assert_eq!(shards.len(), 2);
        assert_partition(&shards, 1, 100);
        // Uniform would cut at 50. Density pushes the cut deep into the heavy
        // window so shard 1 (the dense tail) is small.
        assert!(
            shards[0].end >= 90,
            "cut should land inside the heavy window (>=90), got {}",
            shards[0].end
        );
        // And the heavy shard must stay non-empty (boundary discipline).
        assert!(shards[1].start <= shards[1].end, "heavy shard non-empty");
    }

    #[test]
    fn density_split_falls_back_to_uniform_when_profile_empty() {
        let profile = DensityProfile {
            window_size: 16384,
            first_window_start: 1,
            weights: vec![],
        };
        let dens = split_region_density("7", 1, 100, 4, &profile);
        let uniform = split_region("7", 1, 100, 4);
        assert_eq!(dens, uniform, "empty profile must equal uniform split");

        // All-zero weights are also degenerate -> uniform fallback.
        let zero = DensityProfile {
            window_size: 10,
            first_window_start: 1,
            weights: vec![0; 10],
        };
        assert_eq!(split_region_density("7", 1, 100, 4, &zero), uniform);
    }

    #[test]
    fn density_split_balances_estimated_work() {
        // Skewed profile: first half light, second half heavy. The density split's
        // per-shard weight spread must be far tighter than the uniform split's.
        let mut weights = vec![1u64; 20];
        for w in weights.iter_mut().skip(10) {
            *w = 50;
        }
        let profile = DensityProfile {
            window_size: 100,
            first_window_start: 1,
            weights,
        };
        let n = 4;
        let dens = split_region_density("7", 1, 2000, n, &profile);
        let uni = split_region("7", 1, 2000, n);
        assert_partition(&dens, 1, 2000);

        let work = |s: &Shard| profile.weight_in(s.start, s.end);
        let spread = |shards: &[Shard]| {
            let w: Vec<f64> = shards.iter().map(work).collect();
            let max = w.iter().cloned().fold(f64::MIN, f64::max);
            let mean = w.iter().sum::<f64>() / w.len() as f64;
            max / mean // 1.0 == perfectly balanced
        };
        let dens_ratio = spread(&dens);
        let uni_ratio = spread(&uni);
        assert!(
            dens_ratio < uni_ratio,
            "density split must balance work better: dens max/mean {dens_ratio:.3} \
             should be < uniform {uni_ratio:.3}"
        );
        assert!(dens_ratio < 1.3, "density split should be near-balanced, got {dens_ratio:.3}");
    }

    #[test]
    fn merge_clamps_overfetch_and_matches_serial() {
        let shards = split_region("7", 10, 30, 4);

        // Serial reference: one shard over the whole range, same clamp.
        let serial = shard_and_merge(&SerialExecutor, &[Shard { index: 0, chrom: "7".into(), start: 10, end: 30 }], toy_produce, |v: &V| v.pos).unwrap();

        let native = shard_and_merge(&NativeThreadExecutor, &shards, toy_produce, |v: &V| v.pos).unwrap();
        let serial_sharded = shard_and_merge(&SerialExecutor, &shards, toy_produce, |v: &V| v.pos).unwrap();
        // The WASM in-module executor is serial by construction; it must match too.
        let wasm = shard_and_merge(&WasmShardExecutor::from_parallelism(8), &shards, toy_produce, |v: &V| v.pos).unwrap();

        // Every in-bounds position exactly once, no over-fetch leakage, sorted.
        let expected: Vec<V> = (10..=30).map(|p| V { pos: p }).collect();
        assert_eq!(serial, expected);
        assert_eq!(serial_sharded, expected, "serial-sharded == serial");
        assert_eq!(native, expected, "native-sharded == serial (byte-identical)");
        assert_eq!(wasm, expected, "wasm-executor sharded == serial (byte-identical)");
    }

    #[test]
    fn wasm_executor_matches_serial_regardless_of_worker_count() {
        // The detected worker count is telemetry only; the in-module result is
        // always the serial map. Fallback (workers<=1) and "isolated" (workers>1)
        // produce identical output — the load-bearing single-thread guarantee.
        let shards = split_region("7", 1, 97, 5);
        for workers in [0usize, 1, 2, 4, 16] {
            let wasm = shard_and_merge(
                &WasmShardExecutor::from_parallelism(workers),
                &shards,
                toy_produce,
                |v: &V| v.pos,
            )
            .unwrap();
            let serial =
                shard_and_merge(&SerialExecutor, &shards, toy_produce, |v: &V| v.pos).unwrap();
            assert_eq!(wasm, serial, "wasm(workers={workers}) must equal serial");
        }
    }
}
