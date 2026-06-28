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
