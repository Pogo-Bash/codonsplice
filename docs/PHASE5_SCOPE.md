# Phase 5 — scope

Phase 4 delivered a working execution engine (BAM open → scan/seek → predicate →
CALL → materialize/stream → INTO) plus the WASM build, installer, and npm
packages. What remains, prioritized by **user impact ÷ implementation cost**.

## Tier 1 — high impact, moderate cost (do first)

1. **VCF input (`FROM vcf`)** — currently only BAM is wired for CALL ops; a VCF
   dataset opens but has no reader. Add a noodles VCF line reader producing
   `Record::Variant`, so `FROM vcf "x.vcf" WHERE af > 0.1` works. The field map
   already exists. *Cost: low–moderate. Impact: high* (VCF is the most common
   interchange format).

2. **Column projection (`SELECT`)** — make `PROJECT` real. Add a
   `Record::Row(Vec<(String, RuntimeValue)>)` variant and have materialize run
   each projection sub-program, building rows. Unlocks `SELECT chrom, pos, af`.
   *Cost: moderate. Impact: high* (it's core SQL ergonomics).

3. **True streaming for variants** — yield per chromosome/window through the
   callback instead of buffering, so `LIMIT 100` short-circuits the pileup.
   cnvlens-core already scans per-window; thread a `ControlFlow` break through.
   *Cost: moderate. Impact: high for large BAMs.*

## Tier 2 — high impact, higher cost

4. **Indel calling (CIGAR-aware pileup)** — the SNV caller ignores CIGAR. Real
   indel support means decoding CIGAR ops and a gapped pileup. *Cost: high.
   Impact: high* (indels are clinically important) — but it's a cnvlens-core
   algorithm change, largely independent of the VM.

5. **Multi-file queries (tumor/normal)** — `FROM bam "tumor" PAIRED WITH
   "normal"` for somatic calling. Requires grammar additions (spliceql, Phase
   1/2 — currently frozen), two datasets on the stack, and a paired caller in
   cnvlens-core. *Cost: high (touches the frozen language). Impact: high for
   oncology workflows.*

## Tier 3 — valuable, lower urgency

6. **Annotation** — join called variants against gene models / dbSNP
   (`WITH annotate = "refseq"`). Needs a bundled annotation source and an
   interval index. *Cost: high (data + indexing). Impact: medium–high.*

7. **CRAM support** — `FROM cram`. noodles has a CRAM reader but it needs the
   reference FASTA for decode. *Cost: moderate–high. Impact: medium* (CRAM is
   growing but BAM still dominates).

8. **`INTO bam` writer** — currently unsupported (read-only for BAM). noodles can
   write BAM; needs header reconstruction. *Cost: moderate. Impact: low–medium.*

## Recommended Phase 5 cut

Ship **Tier 1 (VCF input + SELECT projection + true variant streaming)** as
Phase 5 — each is moderate cost, high impact, and none requires re-opening the
frozen spliceql grammar. Indels and tumor/normal pairing become Phase 6, since
they need either heavy algorithm work or grammar changes.
