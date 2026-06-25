# Phase 6 — design sketch

The four advanced features deferred past Phase 5, each with the concrete
cnvlens-core changes, grammar impact, complexity, and the use case it unlocks.

## 1. Indel calling (CIGAR-aware pileup)

**cnvlens-core:** the SNV caller (`variants.rs`) ignores CIGAR — it assumes
`pos + i` maps a read base to a reference position. Indels need:
- decode the CIGAR string per `AlnRecord` (add `cigar: Vec<(u8 op, u32 len)>` to
  `AlnRecord`, decoded in `bam::decode_full`);
- a gapped pileup that walks CIGAR ops (M/=/X consume both, I consumes read, D
  consumes ref, S/H clip), accumulating insertion/deletion alleles per position;
- emit `Variant { kind: "INS"|"DEL", … }` with the indel sequence in `alt`/`ref`.

**Grammar:** none — `CALL variants` already covers it; optionally a
`WITH indels = true` param (additive WITH key, no grammar change).

**Complexity:** high (pileup rewrite, but self-contained in cnvlens-core).
**Unlocks:** clinically important indels (e.g. EGFR exon-19 deletions, the
canonical NSCLC driver) that the SNV-only caller silently misses.

## 2. Multi-file queries (tumor/normal pairs)

**cnvlens-core:** a somatic caller `call_somatic(tumor: &[u8], normal: &[u8],
opts)` that piles up both and emits variants present in tumor but not normal
(with a normal-VAF threshold). Reuses the existing per-window pileup twice.

**Grammar (requires reopening spliceql):** a second source binding, e.g.
`FROM bam $tumor PAIRED WITH bam $normal`. This needs a new `PAIRED`/`WITH`
secondary-source production in `parse_from` and a `FromClause.paired:
Option<Box<FromClause>>` AST field — a genuine grammar extension. Justified
because tumor/normal is the dominant somatic workflow and cannot be expressed by
composing single-source queries (the two files must be piled up together).

**VM:** `OPEN_SOURCE` twice → two `Dataset`s; a `CALL_SOMATIC` opcode consuming
both. `Cursor` already supports a single dataset; add a `secondary: Option<Arc<Dataset>>`.

**Complexity:** high (touches the frozen grammar + a new opcode + caller).
**Unlocks:** somatic variant calling — the core oncology use case.

## 3. VCF annotation (gene names, dbSNP)

**cnvlens-core:** an interval index over a bundled gene model (GFF/BED) and a
dbSNP lookup (sorted `rsID` by position). `annotate(variants, sources)` joins
each variant to its overlapping gene + known `rsID`, filling `Variant.id` and a
new `gene: Option<String>` field.

**Grammar:** `WITH annotate = "refseq,dbsnp"` (additive WITH key — no grammar
change). Annotation sources resolve from a config path or bundled data.

**Complexity:** high — mostly *data* (shipping + indexing annotation sources),
not algorithm. The interval join itself is medium.
**Unlocks:** human-readable variant reports (gene + rsID) without a separate
annotation tool (VEP/snpEff).

## 4. CRAM support

**cnvlens-core:** `FROM cram` currently errors. noodles has a CRAM reader, but
CRAM is reference-compressed, so decode needs the reference FASTA. Add
`cram::for_each_full(cram, reference_fasta, f)` and a region-seeked variant
(CRAM uses a `.crai` index, analogous to `.bai`). The downstream pileup is
unchanged once reads are decoded.

**Grammar:** none — `Format::Cram` already exists in the AST/lexer; only the VM's
`OPEN_SOURCE`/readers need wiring. A `WITH reference = $ref` param (additive)
supplies the FASTA.

**Complexity:** medium–high (noodles CRAM API + reference plumbing).
**Unlocks:** CRAM-archived datasets (increasingly the default at large
sequencing centers for storage savings) without a `samtools` pre-conversion.

## Recommended ordering

1. **Indel calling** first — highest clinical value, self-contained in
   cnvlens-core, no grammar change.
2. **CRAM** next — medium cost, no grammar change, growing demand.
3. **Annotation** — high value but gated on shipping annotation data.
4. **Tumor/normal** last — highest value for oncology but the only item that
   reopens the frozen spliceql grammar; batch it with any other grammar work.
