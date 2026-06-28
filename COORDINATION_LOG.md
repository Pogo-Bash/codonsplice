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
| 0B — slice test data (DBs, CNV) | dispatching |
| 0C — manifest | pending |
| review 0 (FROM vcf closure + L858R-in-slice) | pending |
| 2 — parallelism | ✅ DONE (worktree wt/parallelism): native sharding, byte-identical, 2.33x@16; WASM fallback designed |
| review 2 | ✅ orchestrator-verified: serial==8-shard byte-identical (50 vars, md5 match); indel survives; boundary tests pass |
| 1 — ANNOTATE (design → impl → L858R) | gated on Track 0 |
| review 1 | pending |
| 3 — CALL cnv (wire → validate) | gated on Track 0 |
| review 3 | ✅ orchestrator-verified: flat intronic→0 calls; SELECT/ORDER BY compose; 16 tests; honest capture-bias caveat |
| consolidated report | pending |

## Log
- **TRACK 2 DONE (honest), reviewed.** PROFILE: CPU-bound (31% BGZF decode + 69% pileup), sharding helps. Generic sharding layer (split_region INCLUSIVE bounds, ShardExecutor trait, shard_and_merge, plan_shard_count single-thread rule) — backend-agnostic brain. Native (std::thread::scope) wired into VM/CLI. **Byte-identical serial==sharded PROVEN** (orchestrator re-verified: 50 vars md5 match @ 8 shards; indel survives; Track 2's 4 equivalence+boundary tests pass). Speedup 2.33x@16 (sublinear — honest, uniform coord split vs non-uniform read density; density-aware split = follow-up). WASM: single-thread fallback IMPLEMENTED (crossOriginIsolated detect → planner→1); full worker threading DESIGNED (docs/design/PARALLELISM_WASM.md, COOP/COEP documented). Commits on wt/parallelism worktree.
- **TRACK 3 DONE (honest), reviewed.** Added Record::Cnv; split CallKind::Cnv from Coverage → runs detect_cnvs; composes with WHERE/SELECT/ORDER BY/LIMIT/INTO. **No cnvlens-core change** (reused detect_cnvs). TDD red→green, 16 tests. Validation vs Track 0 baseline: genuinely-flat intronic 55121000-55177000 → **0 CNV calls (clean negative control)** ✓. HONEST: NA12878 is TARGETED CAPTURE — exonic depth >> intronic median, so naive within-sample depth-ratio reads exon peaks as amps (10 over EGFR incl. the artifact spike); validates no-spurious-on-flat-diploid, NOT amp sensitivity (no panel-of-normals / no amplified BAM). Commit fafb6e5 on feat/call-cnv.
- **WASM cross-track clarification**: both Track 2 & Track 3 hit the same raw-`cargo build --target wasm32` `zlib_rs` malloc/free link error — but per the prior audit, **`wasm-pack` builds fine** (provides the allocator); the raw-cargo failure is a build-tooling artifact, NOT a real WASM blocker. CNV + sharding-fallback are WASM-ready via the real (wasm-pack) build.
- **0A DONE** (honest): FROM vcf already ~95% there. All clauses compose (WHERE/SELECT/ORDER BY/LIMIT) on .vcf + .vcf.gz (in-Rust BGZF). Fixed a real round-trip gap (records_to_vcf hardcoded ID=. FILTER=PASS → now preserves them, b66a14c). 10 regression tests. No spliceql submodule change. docs/FROM_VCF.md.
  - **GATE for Track 1 — FROM vcf INFO-map limitation (architectural ceiling):** only INFO/DP→depth and INFO/AF→af are readable; `Variant` has NO generic INFO map, so pre-existing INFO annotations read as null. Track 1 MUST design ANNOTATE around this: annotations flow as SpliceQL *projected columns* (land in INFO via SELECT…INTO vcf), OR extend `Variant` with a generic INFO map (a larger cnvlens-core change — flag, don't silently assume).
  - **GATE for Track 1 — L858R is SOMATIC, not in NA12878/GIAB** (germline normal). The L858R verification target = annotate a variant at chr7:55259515 A>G **against sliced ClinVar** (which carries L858R as pathogenic) + the gene model (EGFR exon 21), NOT a sample variant. Track 0B's ClinVar slice MUST include 55259515. Coordinates are **GRCh37** (sample BAM + chr7.fa are GRCh37; L858R = chr7:55259515 on GRCh37, 55191822 on GRCh38) — slice GRCh37 data.
  - Minor follow-up: the v0.4.2 no-reference warning false-fires on FROM vcf (pre-called variants need no reference) — should gate on FROM bam only. Cosmetic, logged.
- (init) Plan written. Grounded: FROM vcf exists (verify), CALL cnv half-wired, both submodules, network up. 4 branches created off bf83cf9. Dispatching Track 0 (critical path) + Track 2 (independent).
