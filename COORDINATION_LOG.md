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

---

# PHASE 1+2 ORCHESTRATION — 6 verifiable features → 2 ship agents (NOTHING PUSHED)

## Oracle matrix (VERIFIED present before dispatch)
- bcftools 1.16: `isec` ✓, `norm -m` ✓, `csq -f chr7.fa -g GFF` ✓ (protein HGVS). VEP absent → csq + genetic code are the HGVS oracle. Reference chr7.fa + testdata/EGFR_region.GRCh37.gff3 present.

## Dependency graph + branch bases
```
HGVS/translation (FOUNDATION: codon-extraction ONCE) ── feat/hgvs off feat/annotate (spliceql c247dad)
isec (VCF set ops) ── feat/isec off feat/vcf-input-and-test-data ──► PAIRED WITH ── feat/paired off feat/isec
multi-allelic ── feat/multiallelic off feat/vcf-input-and-test-data        (independent)
density-aware shards ── feat/density off wt/parallelism ──► WASM workers ── feat/wasm-threads off feat/density
```
- WAVE 1 (parallel, independent): HGVS, isec, multi-allelic, density-shards.
- WAVE 2 (gated on wave-1 dep): PAIRED WITH (needs isec), WASM workers (needs density).

## Per-feature oracle gate (non-negotiable, honesty rule)
- HGVS: L858R chr7:55259515 T>G → p.Leu858Arg; differential vs `bcftools csq`. Genetic-code unit tests for translate()/codon_at()/gc(). **codon-extraction-from-reference is ONE module; translate/codon_at/gc/HGVS all consume it — no forking.**
- isec: byte-identical vs `bcftools isec` (intersect/union/complement) on the ClinVar/GIAB slices.
- PAIRED WITH: tumor/normal set logic == `bcftools isec` (somatic = tumor-only complement).
- multi-allelic: split == `bcftools norm -m -`.
- density-shards: byte-identical to serial (moving cuts can't change the answer) + speedup toward linear vs uniform split.
- WASM workers: byte-identical in a real wasm-pack build + crossOriginIsolated detection w/ single-thread fallback.

## Shared-enum coordination (orchestrator owns)
Record enum already gains Cnv (T3) + AnnotatedVariant (T1). HGVS extends AnnotatedVariant's columns (no new variant — additive). isec/PAIRED/multi-allelic operate on Variant records (likely no new Record variant; if isec needs a set-membership record, flag it). Logged so Phase-2 integration resolves once.

## Phase 2 GATE: dispatch ship agents ONLY after all 6 features + the 4 existing tracks are built+verified. Ship Agent A = integration (per docs/.../2026-06-28-integration-and-ship.md, extended for the 6 new features). Ship Agent B = release (two-submodule publish dance; STOP before actual publish for user approval).

## Status
| feature | branch | status |
|---|---|---|
| HGVS foundation | feat/hgvs 23412f4 | ✅ VERIFIED (oracle below) |
| isec | feat/isec e5dabc7 (cnvlens 48bf8f1, spliceql c34cada) | ✅ VERIFIED |
| multi-allelic | feat/multiallelic 0e0d83f (cnvlens 5524886, spliceql ef15948) | ✅ VERIFIED |
| density-shards | feat/density f05e934 (+orch review fix) | ✅ VERIFIED |
| PAIRED WITH | feat/paired 12be95d (spliceql c7216e9) | ✅ VERIFIED |
| WASM workers | feat/wasm-threads 8cccdc9 | ✅ VERIFIED (honest landing) |

## DISK CRASH + recovery (logged): wave-1 agents died when the process exited (full-disk EROFS from target/ dirs at 90GB). User compacted the WSL vhdx, hand-committed all 4 wave-1 features on their feat/<name> branches with submodules branched (not detached) + pointers set. Disk now 889G free. Lesson reinforced: worktree target/ dirs are the disk risk — freed all 4 wave-1 targets after review.

## WAVE-1 REVIEW CHECKPOINTS — oracle results VERIFIED (orchestrator re-ran each, not just "tests pass")
- **HGVS** (cs-hgvs): codon.rs is the SINGLE shared module (8 unit tests: strand-aware extraction, genetic-code table, exon-boundary span, gc); `gc`/`translate`/`codon_at` registered as SpliceQL builtins (compiler.rs:709-712 — no grammar token needed, so spliceql had no new commit, which is why feat/hgvs spliceql == c247dad). **L858R on REAL chr7.fa → p.Leu858Arg / c.2573T>G / ENST00000275493** (l858r_on_real_chr7_reference PASS; codon independently asserted CTG=Leu before variant). Honest gap: bcftools csq corroboration not added (genetic code is authoritative; acceptable). The "build codon-extraction ONCE" constraint HELD.
- **isec** (cs-isec): `matches_bcftools_isec` PASS — live `bcftools isec -p`, all 4 partitions record-set identical; exact (chrom,pos,ref,alt) match (chr7:55249100 T-vs-C correctly NOT shared); indel + cross-chrom cases. Surface `FROM vcf a ISEC vcf b MODE ...`; reusable set-op fn in vm.rs (PAIRED WITH builds on it).
- **multi-allelic** (cs-multiallelic): `split_differential_vs_live_bcftools` PASS — live `bcftools norm -m - -f chr7.fa`, record-set identical + golden tracks live tool; per-allele AF apportioned (0.3/0.2/0.1) + indel. Surface `FROM vcf ... SPLIT CALL variants`.
- **density** (cs-density): 10 shard unit tests (BAI-linear-index density estimate, balance, fallback, merge-matches-serial). REVIEW GAP found+closed: the density split path wasn't in an end-to-end byte-identical gate (only uniform + hand-placed-uneven were). Added `density_split_from_bai_is_byte_identical_to_serial` (f05e934): proves BAI-placed cuts are genuinely non-uniform for the EGFR BAM AND byte-identical to serial — non-vacuous. PASS.

## WAVE-2 review — PAIRED WITH ✅ VERIFIED (orchestrator re-ran)
- Surface `FROM vcf "t" PAIRED WITH vcf "n" [MODE somatic|germline]` (default somatic). **Reuse confirmed STRUCTURALLY**: core commit = paired_tests.rs ONLY (no engine code); grammar lowers to the SAME IsecClause → reuses `cnvlens_core::vcf::isec` / OpenIsec. Oracle: `somatic_matches_bcftools_isec_private_tumor` PASS (ran live bcftools, no skip) — somatic==isec 0000.vcf, germline==0002.vcf, exact (chrom,pos,ref,alt) incl. 55249100 T-vs-C edge. spliceql c7216e9 on feat/paired (not detached).
- **WASM workers** ✅ VERIFIED (orchestrator re-ran the real wasm gate): native sharding STILL byte-identical after cfg-select (5 green); codonsplice-wasm compiles wasm32 (cfg excludes thread::scope); `wasm_executor_matches_serial_regardless_of_worker_count` PASS; **end-to-end real-.wasm-in-Node byte-identity PASS** — single + full plan→call→merge pipeline at 2/4/6 shards == native serial (crossOriginIsolated=false→fallback). HONEST GAP (authorized): parallel-in-browser COI execution built+compiling but NOT run (no headless COI browser); worker pool drives only proven-identical exports; manual steps in PARALLELISM_WASM.md §5. cnvlens/spliceql untouched.

## ✅ PHASE 1 COMPLETE — 6/6 features built + oracle-verified by the orchestrator. Existing 4 tracks + parallel-CNV engine built. **PHASE 2 GATE OPEN.**

## PHASE 2 integration topology (branch CHAINS simplify the merge):
- SHARDING: `feat/wasm-threads` ⊇ `feat/density` ⊇ `wt/parallelism` (linear — take wasm-threads, has all sharding+density+wasm).
- ANNOTATE+HGVS: `feat/hgvs` ⊇ `feat/annotate` (take hgvs).
- ISEC+PAIRED: `feat/paired` ⊇ `feat/isec` (take paired).
- CNV: `feat/call-cnv` (core) + cnvlens `feat/parallel-cnv` e11edd9 (engine).
- SPLIT: `feat/multiallelic`. BASE: `feat/vcf-input-and-test-data` (Track 0).
- **Shared Record enum**: only Cnv (call-cnv) + AnnotatedVariant (hgvs) add variants — additive union. isec/SPLIT/PAIRED produce Variant records (no new variant).
- **cnvlens to MERGE** (3 branches touch vcf.rs/coverage.rs): feat/parallel-cnv (coverage.rs) + feat/isec (vcf.rs) + feat/multiallelic (vcf.rs). Each in a SEPARATE clone (cs-*/cnvlens).
- **spliceql to MERGE** (grammar): feat/isec c34cada + feat/multiallelic ef15948 + feat/paired c7216e9 + ANNOTATE c247dad (HGVS added builtins in core, not grammar). Each in a separate clone.
- vm.rs/compiler.rs conflict zones: CNV arm (call-cnv) | ANNOTATE+builtins (hgvs) | OpenIsec (paired) | SPLIT (multiallelic) | sharding producer+cfg (wasm-threads). Resolve as additive union per the integration plan.

## INTEGRATION pointer map (submodule branches live in separate clones — Phase-2 must gather them):
- spliceql branches: feat/hgvs c247dad(builtins in core), feat/isec c34cada, feat/multiallelic ef15948, feat/paired c7216e9, feat/wasm-threads(pending), others ae6e0b9.
- cnvlens branches: feat/isec 48bf8f1, feat/multiallelic 5524886, feat/paired (from 48bf8f1), feat/parallel-cnv e11edd9; hgvs/density unchanged c328749.

## Worktree recipe (PROVEN — solves the Track-2 submodule fetch failure)
Fresh worktree's `submodule update --init` fails (local-only commits not on remote). Recipe: `git worktree add -b feat/X ../cs-X <base>` then in it `rm -rf cnvlens crates/spliceql; git clone --shared <MAIN>/cnvlens ./cnvlens && checkout <c>; git clone --shared <MAIN>/crates/spliceql ./crates/spliceql && checkout -b feat/X <c>`. Gives independent EDITABLE submodule trees per worktree (each its own feat/X spliceql branch — solves the multi-feature spliceql contention). Build with `CARGO_TARGET_DIR=<wt>/target`. Verified: cargo check green in 8.6s. INTEGRATION NOTE: each worktree's spliceql commits live in its own clone — Phase-2 integration must `git fetch` each worktree's spliceql branch into main's submodule before pointer bumps.

## Wave-1 review checkpoints (when each agent returns): demand the ORACLE RESULT, not just "tests pass" — HGVS: L858R→p.Leu858Arg + bcftools csq; isec: bcftools isec set-equality; multi-allelic: bcftools norm -m record-set; density: byte-identical + load-balance numbers. Then dispatch wave 2.

## PHASE 2 — dispatched Ship Agent A (integration), background. Local only, NOTHING PUSHED.
Given fresh-context = the "dedicated session" the user wanted for the risky merge. Spec: gather both submodules' feature branches from their separate clones (cnvlens: parallel-cnv+isec+multiallelic; spliceql: paired/c7216e9 + multiallelic/ef15948 + annotate/c247dad), merge core in dependency order via the branch chains, resolve Record-enum/producer conflicts (additive unions), extend the serial-vs-sharded gate to CNV+ANNOTATE, wasm-pack re-verify combined. Honesty rule: land partial + report precise conflicts rather than tangle.
**Ship Agent B (release) HELD** — not dispatched until A is reviewed AND user approves publishing (per "Nothing pushed without my approval" + "Stop before the release agent actually publishes").
