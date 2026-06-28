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
| review 1 | ✅ orchestrator-verified: L858R annotation exact; ANNOTATE composes with WHERE; spliceql on named branch; 17 tests |
| 3 — CALL cnv (wire → validate) | UNBLOCKED (rebased on Track 0); dispatching |
| review 3 | pending |
| consolidated report | ✅ below |

## Log
- **0B DONE + 0C manifest (fa7000d), honest.** Caught a brief error: **L858R is T>G** (GRCh37 ref base at 55259515 = T; ClinVar c.2573T>G), NOT A>G. ClinVar slice testdata/clinvar_GRCh37_EGFR.vcf.gz (109 P/LP EGFR records; L858R id 16609 = EGFR oncogenic/drug_response). Gene model testdata/EGFR_region.GRCh37.gff3 (L858R in EGFR exon 21, ENST00000275493). Contig "7" (no rename needed). **CNV validation**: HCC827/H1975 BAMs NOT obtainable (controlled access) → chose depth-ratio NEGATIVE control (testdata/cnv_depth_baseline.bed + scripts/cnv_depth_baseline.sh): NA12878 diploid → CALL cnv should emit ~ZERO CNVs over EGFR. Validates no-spurious-calls, NOT amplification sensitivity (honest caveat; ~6.4x coverage). **Track 2**: EGFR BAM too small to benchmark speedup (~0.7-0.9s) — use as byte-identical fixture only; bigger BAM needed for timing.
  - **GATE for Track 1 — clinical match must include drug_response**: L858R's CLNSIG is `drug_response`/oncogenic, NOT literal `Pathogenic`. Match EGFR + (oncogenic OR drug_response OR P/LP) or it misses L858R. Strict-P target 2bp away: 55259517 GC>G frameshift (CLNSIG=Pathogenic). **L858R variant = chr7:55259515 T>G.**
- **TRACK 0 COMPLETE** (0A+0B+0C). feat/annotate + feat/call-cnv rebased onto it (have FROM vcf fix + testdata). Tracks 1 & 3 unblocked.
- **0A DONE** (honest): FROM vcf already ~95% there. All clauses compose (WHERE/SELECT/ORDER BY/LIMIT) on .vcf + .vcf.gz (in-Rust BGZF). Fixed a real round-trip gap (records_to_vcf hardcoded ID=. FILTER=PASS → now preserves them, b66a14c). 10 regression tests. No spliceql submodule change. docs/FROM_VCF.md.
  - **GATE for Track 1 — FROM vcf INFO-map limitation (architectural ceiling):** only INFO/DP→depth and INFO/AF→af are readable; `Variant` has NO generic INFO map, so pre-existing INFO annotations read as null. Track 1 MUST design ANNOTATE around this: annotations flow as SpliceQL *projected columns* (land in INFO via SELECT…INTO vcf), OR extend `Variant` with a generic INFO map (a larger cnvlens-core change — flag, don't silently assume).
  - **GATE for Track 1 — L858R is SOMATIC, not in NA12878/GIAB** (germline normal). The L858R verification target = annotate a variant at chr7:55259515 A>G **against sliced ClinVar** (which carries L858R as pathogenic) + the gene model (EGFR exon 21), NOT a sample variant. Track 0B's ClinVar slice MUST include 55259515. Coordinates are **GRCh37** (sample BAM + chr7.fa are GRCh37; L858R = chr7:55259515 on GRCh37, 55191822 on GRCh38) — slice GRCh37 data.
  - Minor follow-up: the v0.4.2 no-reference warning false-fires on FROM vcf (pre-called variants need no reference) — should gate on FROM bam only. Cosmetic, logged.
- (init) Plan written. Grounded: FROM vcf exists (verify), CALL cnv half-wired, both submodules, network up. 4 branches created off bf83cf9. Dispatching Track 0 (critical path) + Track 2 (independent).


---

# CONSOLIDATED REPORT — four tracks (NOTHING PUSHED/TAGGED)

All four landed honestly-verified. Each on its own local branch; **not integrated, not pushed**. Both submodules respected (named branches, no detached HEAD).

## Per-track status
| Track | Branch | Status | Key differential |
|---|---|---|---|
| 0 — FROM vcf + data | feat/vcf-input-and-test-data | ✅ FROM vcf verified + ID/FILTER round-trip fixed; GRCh37 GFF+ClinVar+CNV-baseline sliced; manifest | FROM vcf closure on GIAB; L858R=T>G in ClinVar(EGFR,exon21) |
| 1 — ANNOTATE | feat/annotate (+ spliceql c247dad) | ✅ ANNOTATE clause, gene/exon/clinvar/rsid join | **chr7:55259515 T>G → EGFR exon21 drug_response rs121434568; EXACT bcftools-annotate parity** |
| 2 — Parallelism | wt/parallelism (worktree) | ✅ native sharding wired; WASM fallback designed | **serial==8-shard byte-identical** (50 vars, boundary-hardened); 2.33x@16 |
| 3 — CALL cnv | feat/call-cnv | ✅ Record::Cnv, detection wired, composes | flat intronic→**0 calls (clean neg-control)**; honest capture-bias caveat |

## Honest scope / limits (per the honesty rule)
- **Track 0**: dbSNP/gnomAD slices not done (ClinVar sufficed for L858R). No amplified-tumor BAM obtainable (controlled access) → CNV validation is a NEGATIVE control only.
- **Track 1**: aa_change/HGVS (p.Leu858Arg) NOT computed (needs codon/strand translation) — `consequence=missense_variant` from ClinVar MC instead. Coverage = EGFR GRCh37 slice only.
- **Track 2**: native parallel + serial-equivalence PROVEN; full WASM worker threading DESIGNED not built; speedup sublinear (2.33x@16, uniform coord split vs read-density skew — density-aware split is the follow-up). WASM single-thread fallback works.
- **Track 3**: validates "no spurious calls on flat diploid," NOT amplification sensitivity (no panel-of-normals; targeted-capture exon peaks read as amps without bias correction — honest).

## Submodule state (local, unpushed)
- spliceql: branch feat/annotate @ c247dad (ANNOTATE grammar). Superproject feat/annotate records it.
- cnvlens: unchanged this session (c328749, v0.4.2 base). Tracks 2/3 needed NO cnvlens-core change.

## Cross-track INTEGRATION risks (the tracks are NOT merged — this is the next step)
1. **Tracks 1 & 3 both add a Record variant** (AnnotatedVariant / Cnv) + touch runtime.rs/vm.rs/materialize — merging needs care (both extend the same enum + match arms).
2. **Track 2 shards the CALL-variants producer in vm.rs** — merging with 1/3's vm.rs changes needs care. **CALL cnv-under-sharding is UNTESTED** (Track 2 sharded variants only; Track 3's CNV path isn't in the byte-identical test). On integration, add CALL cnv (and ANNOTATE) to the serial==sharded gate.
3. **WASM**: both 2 & 3 hit the raw-`cargo build --target wasm32` zlib_rs link error — NOT a real blocker (wasm-pack provides the allocator, builds fine per the prior audit). Re-verify CNV + sharding-fallback via wasm-pack on integration.
4. Boundary class (#20) respected in all three (sharding inclusive split, CNV inclusive windows, ANNOTATE inclusive interval join) — keep it in the integration tests.

## Recommended next step + release plan
1. **Integrate on a branch** in dependency order: Track 0 base → merge Track 3 (Record::Cnv) → merge Track 1 (AnnotatedVariant) [resolve the shared runtime.rs/vm.rs enum+match] → merge Track 2 (sharding) [resolve vm.rs producer]. Add ANNOTATE + CALL cnv to the byte-identical serial-vs-sharded gate.
2. **Verify the integrated build under wasm-pack** (CNV + ANNOTATE + sharding-fallback).
3. **Version/publish dance** (when ready): codonsplice-core bumps (FROM-vcf fix + Record kinds + sharding + annotate join all live there); spliceql bumps + republish (ANNOTATE grammar — it's published on crates.io); push both submodule branches first, then pointer bumps, then tag. cnvlens-core unchanged → idempotent skip (0.4.1 automation).
4. **Follow-ups**: aa_change/HGVS translation; density-aware shard split; dbSNP/gnomAD slices; an amplified-tumor BAM for CNV amp-sensitivity; gate the v0.4.2 no-ref warning to FROM bam only.


---

## NEXT SESSION (queued, not started) — Integration + Ship
Per the user: **integration is its own session** — done in dependency order with the byte-identical gate extended, then ship. Plan: `docs/superpowers/plans/2026-06-28-integration-and-ship.md`.
- Merge order: Track 0 base → Track 3 (Cnv) → Track 1 (AnnotatedVariant + spliceql c247dad) → Track 2 (sharding last, cross-cutting). The two Record-enum extenders resolved together; all conflicts are ADDITIVE unions (exact resolution code in the plan).
- The real work = Task 5: prove CALL cnv + ANNOTATE serial==sharded (or honestly route CNV serial-only if depth-segmentation isn't shard-safe at seams — boundary class #20). This is the "CALL cnv under sharding" gap, closed honestly.
- Guarded hazard: Track 2 edits `records_to_vcf` where Track 0's ID/FILTER fix lives → explicit preserve+re-test step.
- Ship (Task 7) is GATED on explicit user approval. Submodule push order: spliceql first (published on crates.io), then codonsplice-core; cnvlens-core unchanged → idempotent skip.


---

## NEW TRACK (done, verified) — Correct Parallel CNV (global-segmentation-first)
Spec: `docs/.../Correct Parallel CNV`. Submodule: **cnvlens-core `feat/parallel-cnv` @ e11edd9** (named branch, not detached). Supersedes the integration plan's interim "CNV serial-only" decision — **CNV is now proven shard-safe.**

**Task 0 (research+code):** Confirmed (cited): per-window depth counting is embarrassingly parallel; segmentation (threshold/CBS/HMM) is order-dependent and must stay global; shard-then-stitch's failure modes (wrong-merge / split-at-seam) are real → global-segmentation-first is the correct default at our single-contig scale. Code finding: the two stages were ALREADY separated (`compute_coverage_windows` → `detect_cnvs`), and `detect_cnvs_*` is already pure+global. The real seam is *inside* `compute_coverage`: only the read-counting is parallelizable; median/GC/mask are global reductions.

**Tasks 1–4 (DONE, cnvlens-core):** Refactored out `finalize_coverage` (shared global stage) + lifted `Slot`; added `compute_coverage_region_parallel` — shards only the counting across `std::thread::scope`, each thread into a PRIVATE map merged by a clean integer sum after join (**race-free by construction**, borrow-checked; the reviewer's hazard explicitly addressed). Window-ownership filtering ⇒ each read counted exactly once across seams; boundary shards open-ended to absorb region-edge overhang. **Verified (tests/parallel_cnv.rs, 5 green):** serial characterization; **parallel==serial byte-identical at 2/3/4/8/16 shards**; total-coverage conservation across seams; shards=1==serial; and **the key positive control — a 3x amp straddling the real 8-shard boundary is emitted as ONE whole call, not two fragments.**

**Task 5 (WASM):** `#[cfg(target_arch="wasm32")]` → serial fallback (threads are enhancement, never load-bearing). `cargo check --target wasm32-unknown-unknown` passes (cfg correctly excludes thread::scope). HONEST LANDING (spec-permitted): WASM CNV runs single-threaded; equivalence proven on native; full Web-Worker+SAB CNV remains design-only (PARALLELISM_WASM.md), same status as variant WASM threading.

**Task 6 (gate integration) — DEFERRED to the integration session (anti-tangling):** the unified serial-vs-sharded gate + `SPLICE_SHARDS` plumbing live across Track2/Track3, which merge at integration. Deferring loses NO correctness proof (engine gate proves byte-identical windows; detect is a pure fn of windows). Integration plan Task 5 UPDATED with the exact wiring + pointer bump (e11edd9).

**Submodule discipline:** committed on named branch feat/parallel-cnv; superproject pointer bump intentionally NOT made on Track 0's branch — it belongs on the integration/feat/call-cnv branch (logged for integration).
