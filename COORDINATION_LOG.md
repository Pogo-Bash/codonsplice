# Coordination Log — Four-Track (FROM vcf · ANNOTATE · Parallelism · CALL cnv)

Orchestrator-maintained. Off `main` @ bf83cf9 (v0.4.2 local). **Nothing pushed/tagged.** Local branches + submodule commits + pointer bumps OK.

## Plan
`docs/superpowers/plans/2026-06-28-four-track-vcf-annotate-parallel-cnv.md`

## Dependency graph (ENFORCED)
```
TRACK 0 (feat/vcf-input-and-test-data) ─ FROM vcf + annotation DBs ─► TRACK 1 (feat/annotate) validation
                                       ─ CNV validation data ───────► TRACK 3 (feat/call-cnv) validation
TRACK 2 (feat/parallelism) ─ independent, runs from the start
```
- Tracks 1 & 3 may design/scaffold in parallel but **GATE validation on Track 0 deliverables**.
- Track 2 independent.

## Grounding (from investigation)
- **FROM vcf largely EXISTS** (grammar Format::Vcf, VM DatasetInner::Vcf, cnvlens-core vcf.rs BGZF). Track 0A = verify closure + gap-fill, not build-from-scratch.
- **CALL cnv half-wired**: opcode + VM path exist, but `CallKind::Cnv` arm (vm.rs:551) emits coverage windows — `detect_cnvs_*` (cnv.rs) NOT invoked. Track 3 = wire the detection.
- **Two submodules**: `crates/spliceql` (grammar — ANNOTATE, any FROM-vcf gap) + `cnvlens/rust/cnvlens-core` (CALL cnv, parallelism compute). Submodule discipline applies to BOTH; serialize edits; no detached HEAD.
- Network up (Ensembl + NCBI 200) → Track 0 can fetch DBs.

## Submodule state tracking
| submodule | branch | HEAD | superproject pointer |
|---|---|---|---|
| cnvlens | feat/cigar-indel-calling | c328749 | c328749 (v0.4.2 base) |
| crates/spliceql | (main) | ae6e0b9 | ae6e0b9 |

## Cross-track risks to watch
- FROM vcf shape (Track 0) → Track 1 must design against the FINISHED behavior, not assumed.
- Sharding core (Track 2) → if it lands, CALL cnv (Track 3) must work under serial AND sharded, and be in the byte-identical test.
- **Half-open boundary class (#20)**: parallelism shard boundaries, CALL cnv windowing, ANNOTATE interval joins all must use INCLUSIVE boundaries + be boundary-tested.

## Status
| track / phase | status |
|---|---|
| 0A — FROM vcf verify/gap-fill | ✅ DONE (b66a14c fix ID/FILTER, f672927 docs, 10 tests); closure verified |
| 0B — slice test data (DBs, CNV) | ✅ DONE (fa7000d): GRCh37 GFF + ClinVar + CNV baseline + manifest |
| 0C — manifest | ✅ DONE (docs/TEST_DATA_MANIFEST.md) |
| review 0 | ✅ orchestrator-verified: FROM vcf closure green; L858R=T>G in ClinVar (EGFR, oncogenic/drug_response), exon 21, ref base T |
| 2 — parallelism (profile → shard → native → wasm-fallback) | dispatching (independent) |
| review 2 (sharding/merge + byte-identical) | pending |
| 1 — ANNOTATE (design → impl → L858R) | UNBLOCKED (rebased on Track 0); pending dispatch |
| review 1 | pending |
| 3 — CALL cnv (wire → validate) | UNBLOCKED (rebased on Track 0); dispatching |
| review 3 | pending |
| consolidated report | pending |

## Log
- **0B DONE + 0C manifest (fa7000d), honest.** Caught a brief error: **L858R is T>G** (GRCh37 ref base at 55259515 = T; ClinVar c.2573T>G), NOT A>G. ClinVar slice testdata/clinvar_GRCh37_EGFR.vcf.gz (109 P/LP EGFR records; L858R id 16609 = EGFR oncogenic/drug_response). Gene model testdata/EGFR_region.GRCh37.gff3 (L858R in EGFR exon 21, ENST00000275493). Contig "7" (no rename needed). **CNV validation**: HCC827/H1975 BAMs NOT obtainable (controlled access) → chose depth-ratio NEGATIVE control (testdata/cnv_depth_baseline.bed + scripts/cnv_depth_baseline.sh): NA12878 diploid → CALL cnv should emit ~ZERO CNVs over EGFR. Validates no-spurious-calls, NOT amplification sensitivity (honest caveat; ~6.4x coverage). **Track 2**: EGFR BAM too small to benchmark speedup (~0.7-0.9s) — use as byte-identical fixture only; bigger BAM needed for timing.
  - **GATE for Track 1 — clinical match must include drug_response**: L858R's CLNSIG is `drug_response`/oncogenic, NOT literal `Pathogenic`. Match EGFR + (oncogenic OR drug_response OR P/LP) or it misses L858R. Strict-P target 2bp away: 55259517 GC>G frameshift (CLNSIG=Pathogenic). **L858R variant = chr7:55259515 T>G.**
- **TRACK 0 COMPLETE** (0A+0B+0C). feat/annotate + feat/call-cnv rebased onto it (have FROM vcf fix + testdata). Tracks 1 & 3 unblocked.
- **0A DONE** (honest): FROM vcf already ~95% there. All clauses compose (WHERE/SELECT/ORDER BY/LIMIT) on .vcf + .vcf.gz (in-Rust BGZF). Fixed a real round-trip gap (records_to_vcf hardcoded ID=. FILTER=PASS → now preserves them, b66a14c). 10 regression tests. No spliceql submodule change. docs/FROM_VCF.md.
  - **GATE for Track 1 — FROM vcf INFO-map limitation (architectural ceiling):** only INFO/DP→depth and INFO/AF→af are readable; `Variant` has NO generic INFO map, so pre-existing INFO annotations read as null. Track 1 MUST design ANNOTATE around this: annotations flow as SpliceQL *projected columns* (land in INFO via SELECT…INTO vcf), OR extend `Variant` with a generic INFO map (a larger cnvlens-core change — flag, don't silently assume).
  - **GATE for Track 1 — L858R is SOMATIC, not in NA12878/GIAB** (germline normal). The L858R verification target = annotate a variant at chr7:55259515 A>G **against sliced ClinVar** (which carries L858R as pathogenic) + the gene model (EGFR exon 21), NOT a sample variant. Track 0B's ClinVar slice MUST include 55259515. Coordinates are **GRCh37** (sample BAM + chr7.fa are GRCh37; L858R = chr7:55259515 on GRCh37, 55191822 on GRCh38) — slice GRCh37 data.
  - Minor follow-up: the v0.4.2 no-reference warning false-fires on FROM vcf (pre-called variants need no reference) — should gate on FROM bam only. Cosmetic, logged.
- (init) Plan written. Grounded: FROM vcf exists (verify), CALL cnv half-wired, both submodules, network up. 4 branches created off bf83cf9. Dispatching Track 0 (critical path) + Track 2 (independent).
