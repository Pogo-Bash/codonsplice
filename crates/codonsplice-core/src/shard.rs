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
    fn merge_clamps_overfetch_and_matches_serial() {
        let shards = split_region("7", 10, 30, 4);

        // Serial reference: one shard over the whole range, same clamp.
        let serial = shard_and_merge(&SerialExecutor, &[Shard { index: 0, chrom: "7".into(), start: 10, end: 30 }], toy_produce, |v: &V| v.pos).unwrap();

        let native = shard_and_merge(&NativeThreadExecutor, &shards, toy_produce, |v: &V| v.pos).unwrap();
        let serial_sharded = shard_and_merge(&SerialExecutor, &shards, toy_produce, |v: &V| v.pos).unwrap();

        // Every in-bounds position exactly once, no over-fetch leakage, sorted.
        let expected: Vec<V> = (10..=30).map(|p| V { pos: p }).collect();
        assert_eq!(serial, expected);
        assert_eq!(serial_sharded, expected, "serial-sharded == serial");
        assert_eq!(native, expected, "native-sharded == serial (byte-identical)");
    }
}
