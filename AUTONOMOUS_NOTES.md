# Notes — reference-free variant caller fix (`fix/variant-caller-reference`)

Branched off `main`. Two commits, nothing pushed/tagged.

## Root cause
`CALL variants` was reference-free: REF was the pileup-**majority** base, not the
reference base. Correct only where one allele dominates; wrong in two ways:
- **balanced het sites** → coin-flip REF/ALT (≈half backwards). e.g. 7:55220177
  emitted `G->A` when GIAB truth is `A->G`.
- **homozygous variants** (≈100% of reads differ from reference) → *invisible*,
  because the majority **is** the variant base, so no ALT is ever emitted.

cnvlens-core already supported a per-contig reference (`VariantOptions.reference_seqs`)
— nothing wired it from SpliceQL.

## Fix
1. **`WITH reference = "ref.fa"`** — new string param on `CALL variants`
   (compiler.rs). The VM loads the FASTA via its Io backend (`load_reference_seqs`)
   and sets `reference_seqs`, keyed by contig name to match the BAM (`>7` ↔ `7`).
   REF is then the actual reference base.
2. **VCF writer** now emits `##contig=<ID=…,length=…>` (length from the BAM
   header) and `##INFO=<ID=DP,…>` / `##INFO=<ID=AF,…>` — previously absent, which
   made the output non-spec and broke `bcftools norm`.

## Verification (NA12878 EGFR region, GIAB truth over eval.bed, bcftools 1.16)
- 7:55220177 → `A->G` (was `G->A`); matches truth. ✓
- Homozygous 7:55003988 (all-G over ref A) now called (was silently dropped). ✓
- `bcftools norm -f chr7.fa` **succeeds** (was: `Reference allele mismatch at
  7:55220177 REF_SEQ:'A' vs VCF:'G'`). ✓

GIAB concordance (recall):

| config | TP | recall |
|---|---|---|
| buggy, default min_depth=10 | 3 | 0.011 |
| no-ref, min_depth=2 | 16 | 0.057 |
| **WITH ref, min_depth=2** | **94** | **0.337** |
| bcftools baseline | 167 | 0.599 |

At matched params the reference alone is the dominant driver: 0.057 → 0.337
(TP 16 → 94), i.e. the homozygous-detection + correct-REF/ALT effect.

## Residual gap to bcftools' 0.60 (not part of this fix)
1. **splice is SNV-only** — 37/279 truth records in eval.bed are indels it can't
   call, capping recall at 242/279 ≈ 0.87. SNV-only recall here is 94/242 ≈ 0.39.
2. **`min_depth` default = 10** masks recall on this ~7× region (212/242 truth
   SNV sites have depth 1–9). It's a `WITH` param, not a bug — lower it for
   low-coverage data. Could reconsider the cnvlens-core default separately.
3. Caller precision/sensitivity tuning (FP still high) is a separate effort.

## What I'd do next
- Consider lowering the default `min_depth`, or auto-scaling it to coverage.
- Add indel calling to close the SNV-only cap.
- Thread reference contig info into the VCF `##contig` for VCF-source passthrough
  too (currently BAM-source only; non-BAM emits `##contig=<ID=…>` without length).

---

# v0.3.0 release + issue triage (2026-06-28)

## Release status — ALL GREEN
Merged `fix/variant-caller-reference` into `main` (fast-forward), bumped
**splice-cli 0.2.4 → 0.3.0** (minor: new `WITH reference` capability) and
**codonsplice-core 0.1.2 → 0.1.3** (the fix lives here; must bump for crates.io).
spliceql (0.1.1) and cnvlens-core (0.1.0) unchanged — not bumped, no submodule
push. Tagged `v0.3.0`, pushed main + tag.

Release workflow run 28312490837 — **every job success**: build (5 targets),
build-wasm, **publish-crates**, publish-npm, build/publish cli npm packages,
winget, verify, GitHub release. Crates.io proof from the log:
`Uploaded codonsplice-core v0.1.3 … Published codonsplice-core v0.1.3 at registry
crates-io ✓`. spliceql correctly skipped (already at 0.1.1). **The fix reached
crates.io.**

## Issue triage (all reproduced against the released 0.3.0 binary)

### Already fixed → verified + closed with proof
| # | title | status |
|---|---|---|
| #14 | ORDER BY ignores aliased/computed SELECT columns | ✅ fixed (sorts by `sd`), **closed** |
| #15 | Projected INTO vcf emits "." in #CHROM | ✅ fixed (#CHROM=`7`), **closed** |
| #16 | WHERE on absent field silently returns 0 rows | ✅ fixed (E009 + valid-field list), **closed** |
| #17 | String `@input` defaults ignored | ✅ fixed (`$chr` defaults to 7), **closed** |
| #18 | Computed SELECT cols named col<N> | ✅ fixed (`round_qual`/`revcomp_ref`), **closed** |

(These were fixed in v0.2.2 but the issues were never closed; #15 also benefits
from the v0.3.0 ##contig/##INFO work on the variant path.)

### Still valid → fix-batches, by priority

**Batch A — CALL reads coordinate correctness (#20 + #19). FIX TOGETHER.**
Shared root cause: the reads region-seek path's 0-based/1-based handling.
- **#20 (HIGHEST — silent data loss):** `pos>=X AND pos<=X CALL reads` returns
  0 reads; the region is lifted to half-open `[A,B)` so reads whose alignment
  start == the upper bound are never fetched. Single-base windows return nothing,
  the last base of every window vanishes, no error. Confirmed on 0.3.0.
- **#19 (lower — wrong-but-consistent):** `reads.pos` is 0-based (samtools POS−1,
  and inconsistent with 1-based `CALL variants`). No data loss. Confirmed on 0.3.0.
- Why bundle: fixing the reporting (#19, +1 on pos) without fixing the seek bound
  (#20, make end inclusive) — or vice versa — risks shifting the interaction.
  One change, verified jointly against `samtools view 7:D-D`.

**Batch B — CALL header materialize crash (#21). SEPARATE.**
- **#21 (HIGH — crash):** `CALL header LIMIT 2` (or `SELECT …`/`ORDER BY …`)
  passes `splice check` then aborts: `type mismatch at pc N: expected cursor,
  got record`. Header pushes a single record; SELECT/ORDER/LIMIT emit
  cursor-consuming opcodes the type-checker doesn't gate. Independent of the
  reads coordinate bugs. Fix: reject materialize clauses on `CALL header` at
  compile time, or give header a single-row cursor.

**Batch C — audit follow-up (#3). SEPARATE, tracking, lowest.**
- Meta-issue: silent error-swallowing in materialize (predicate→false,
  order/projection→Null), silent stubs (`OpCode::Index`), param-coercion gaps
  (out-of-range U8/U32 wrap, wrong-typed `$var` reverts to default), u16
  truncation, float `/0.0`. Most need a `cnvlens-core` `CoreError` variant to
  propagate `VmError` through `materialize`. Left open as a tracking issue.
  (Note: the unbound-`$var` half of the coercion item was addressed on the
  separate `fix/wasm-skew-and-firstcontact` branch, not on main/0.3.0.)

## Recommendation for the next fix session
Start with **Batch A (#20+#19)** — #20 is silent wrong output, the worst failure
mode, and #19 rides along on the same coordinate fix. Then **Batch B (#21)** —
a crash that escapes the type-checker. **#3** is a longer audit cleanup (needs a
cnvlens-core change) and can trail.
