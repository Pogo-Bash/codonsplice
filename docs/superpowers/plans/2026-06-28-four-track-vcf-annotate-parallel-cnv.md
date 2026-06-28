# Four-Track: FROM vcf + ANNOTATE + Parallelism + CALL cnv — Orchestration Plan

> **For agentic workers:** Each track is dispatched as its own subagent task (detailed steps in the dispatch prompt). REQUIRED SUB-SKILLS: superpowers:test-driven-development (failing test first), superpowers:systematic-debugging (root-cause; 3+ attempts → stop and write the architectural question). HONESTY RULE: land each track as far as it genuinely reaches (built / verified / designed) and say so — a verified-partial honestly reported beats a fake "done."

**Goal:** Add VCF-as-input, position-join annotation (ANNOTATE), region-sharded parallelism (native + WASM fallback), and finish CALL cnv — each validated by a real differential, not just unit tests.

**Architecture:** codonsplice-core (VM/compiler) + two submodules — `crates/spliceql` (grammar/parser) and `cnvlens/rust/cnvlens-core` (genomic compute). Branches are local only; nothing pushed/tagged.

**Tech Stack:** Rust, noodles (BAM/VCF/BGZF), samtools/bcftools as oracle, GIAB EGFR slice, Ensembl/ClinVar/NCBI data (Track 0).

## Global Constraints
- **Nothing pushed, tagged, or released.** Local branches + local submodule commits + pointer bumps only. STOP for review.
- **Submodule discipline (both submodules):** changes to `crates/spliceql` (ANNOTATE grammar, any FROM-vcf grammar gap) and `cnvlens/rust/cnvlens-core` (CALL cnv, parallelism compute) go on the submodule's own branch; bump the superproject pointer in a dedicated commit AFTER the submodule is committed. **Never leave the submodule in detached HEAD** (it bit us before — a worker committed off-branch). Serialize submodule edits across tracks — no concurrent writes to one submodule working tree.
- **Coordinate convention:** the half-open boundary class (#20) is the recurring danger. CALL cnv windowing, ANNOTATE interval joins, and parallelism shard boundaries MUST use inclusive boundaries and be boundary-tested.
- **Differentials at review:** every track's correctness gate is a real differential (vs bcftools / the CNV reference / serial output) — unit tests alone can encode the same wrong assumption.
- Data on disk: sample BAM `cnvlens/public/sample-data/NA12878_EGFR.bam`, reference `chr7.fa` + sliced `cnvlens/public/sample-data/EGFR_region.fa`, GIAB `giab_truth_egfr.vcf.gz` / `.norm.vcf.gz`, `eval.bed`. Run binary with `SPLICE_NO_UPDATE_CHECK=1`.

## Dependency graph (enforced)
```
TRACK 0 (data + FROM vcf)  ──delivers FROM vcf + DBs──►  TRACK 1 (ANNOTATE) validation
                           ──delivers CNV truth data──►  TRACK 3 (CALL cnv) validation
TRACK 2 (parallelism)  ── independent, runs from the start ──
```
Tracks 1 & 3 may design/scaffold in parallel with Track 0 but **gate their validation phases** on Track 0's deliverables. Track 2 is independent. Log the graph + gates in COORDINATION_LOG.md.

---

## TRACK 0 — Prerequisites + test data (RUNS FIRST) — `feat/vcf-input-and-test-data`

**0A — FROM vcf (verify/finish; it largely exists).** Grammar (`Format::Vcf`), VM (`DatasetInner::Vcf`), and cnvlens-core `vcf.rs` (BGZF) exist; `CALL variants` over a VCF passes through already-called variants. TASK: verify the full closure — `FROM vcf "x" WHERE/SELECT/ORDER BY/LIMIT` compose on the loaded variants; `.vcf` AND `.vcf.gz` both load (in-Rust bgzf, no external tool); round-trip `FROM vcf x ... INTO vcf y` preserves variants; load `giab_truth_egfr.vcf.gz` and filter/select. Where a gap exists (e.g. SELECT projection over VCF records, or a clause that doesn't compose), FIX it with TDD. Document the exact supported surface. **This blocks Track 1.**

**0B — slice every test asset, coordinate-correct (true chr7 coords, contig "7", never reset to 1 — same care as the reference FASTA).** Commit each where the dependent track expects it; record source+version.
- Track 1: a sliced EGFR-region **gene model** (Ensembl/RefSeq GFF3/GTF), a sliced **ClinVar** VCF (dbSNP/gnomAD if feasible). Confirm the **L858R site (chr7:55259515 A>G, GRCh37)** carries the expected gene/exon + ClinVar significance in the slice — Track 1's verifiable target.
- Track 3: solve the CNV validation gap (no GIAB for CNV). Research + obtain the best available: a known EGFR-amplified cell line with documented CN (e.g. HCC827/H1975), a CNV-truth set, or — minimum — an established CNV caller's output on the sample BAM to diff against. If an open-access tumor BAM can be sliced to EGFR, fetch it. Document what's realistic.
- Track 2: confirm the NA12878 BAM + reference suffice for the serial-vs-parallel byte-identical test; provide a larger region/BAM if needed to make speedup measurable.

**0C — MANIFEST** (`docs/TEST_DATA_MANIFEST.md`): what was downloaded/sliced, committed paths, source+version of each DB (reproducible/citable), and the chosen CNV validation plan.

**Honest landing:** FROM vcf verified+documented (gaps fixed); as many DBs sliced + the L858R target confirmed as the data sources allow; CNV validation plan chosen (even if "diff vs an established caller" is the realistic option). Report what couldn't be sourced.

**Review checkpoint:** FROM vcf closure (does WHERE/SELECT/ORDER/LIMIT genuinely compose on VCF input?) + the L858R-in-slice confirmation.

---

## TRACK 1 — ANNOTATE clause — `feat/annotate` (gated on Track 0: FROM vcf + DBs)

**Design doc first** (`docs/design/ANNOTATE.md`): the `ANNOTATE WITH genes=..., clinvar=...` clause — closure-preserving (variant records in → variant records + gene/exon/aa_change/clinvar_significance fields out; composes with downstream WHERE/SELECT). All joins against LOCAL files (no live APIs — privacy/offline/WASM thesis). Gene model → coordinate-overlap interval query (inclusive boundaries). Known-variant DBs → position+allele join. Grammar = spliceql submodule; join logic = codonsplice-core (or cnvlens-core if it needs the genomic primitives). Must work on `FROM vcf` input (the real bcftools-annotate workflow) AND freshly-called BAM variants.

**TDD + verify:** annotate the EGFR variants; **a variant at L858R (chr7:55259515) must annotate EGFR + correct exon + (if in ClinVar) pathogenic** — "chr7:55259515 A>G → EGFR L858R pathogenic." Differential vs `bcftools annotate`/VEP if feasible.

**Honest landing:** "ANNOTATE working on the EGFR slice, validated on L858R" is a legitimate result even without full multi-DB coverage.

**Review checkpoint:** the interval-join correctness (boundary inclusivity) + the L858R differential.

---

## TRACK 2 — Parallelism (native + WASM) — `feat/parallelism` (independent)

**Profile first** — profile a representative query to confirm parallelism helps the hotspot (if I/O-bound, threads do little). Report the profile before building.

**Architecture (write sharding ONCE):** generic region-chunking + result-merge in shared Rust; abstract ONLY dispatch behind a backend trait — native → OS threads (rayon/pool), wasm → Web Workers + SharedArrayBuffer + WASM threads. Same sharding brain, two substrates. Do NOT write native-only and bolt WASM on later.

**WASM fallback (load-bearing rule):** SharedArrayBuffer needs cross-origin isolation (COOP same-origin + COEP require-corp); detect via `crossOriginIsolated`. true → worker pool; false/SAB-absent → **automatic single-thread fallback** (engine MUST work single-threaded; threading is a speed enhancement, never load-bearing). Also single-thread for small queries (measure a threshold). Document the COOP/COEP headers for builders, while making clear it works without them.

**Correctness gate (non-negotiable):** parallel output **byte-identical** to serial, on BOTH backends. Shard boundaries are exactly where #20 (half-open) could reappear — boundary-test hard.

**Honest landing:** native parallel + serial-equivalence is the core; "native parallel works + WASM falls back cleanly to single-thread + headers documented" is legitimate if full WASM worker threading needs more time.

**Review checkpoint:** the sharding cursor/merge + boundary interactions; the byte-identical serial-vs-parallel differential (incl. CALL cnv once Track 3 lands).

---

## TRACK 3 — Finish CALL cnv — `feat/call-cnv` (gated on Track 0: CNV data; submodule)

`cnv.rs` has `detect_cnvs_manual/adaptive/cbs_lite` but the VM's `CallKind::Cnv` arm (vm.rs:551) currently emits coverage windows like `CALL coverage` — the CNV detection is NOT invoked. TASK (TDD): wire `CALL cnv WITH amp_threshold/del_threshold/window_size/...` to run `detect_cnvs_*` over the coverage windows and surface amplification/deletion calls as records composing with WHERE/SELECT/ORDER BY/LIMIT/INTO. Confirm it runs in **WASM** (depth-based — should port; verify, don't assume). cnvlens-core changes (if any) on its own branch + pointer bump.

**Validate** against Track 0's CNV dataset (cell-line CN / CNV-truth / established-caller diff). Report what it was validated against and how close. Sanity-level honestly labelled if that's all that's possible.

**Honest landing:** "CALL cnv wired end-to-end, runs native+WASM, sanity/differential-validated against <Track 0 data>" — the SNV+indel+CNV trinity completed in one language.

**Review checkpoint:** the CALL cnv wiring + the CNV differential; CALL cnv under both serial AND sharded (Track 2) execution if both land.

---

## COORDINATION (orchestrator)
- COORDINATION_LOG.md: the dependency graph + gates; submodule branch/HEAD/pointer per change (both submodules); cross-track risks (FROM vcf shape → Track 1; sharding core → Track 3 must work sharded; boundary class across all three); per-track honest status.
- Dispatch Track 0 + Track 2 first; gate Tracks 1 & 3 validation on Track 0; review checkpoints at the risky points (FROM vcf closure, ANNOTATE join, sharding merge, CALL cnv wiring) demanding real differentials.
- Serialize submodule edits; watch for detached HEAD. 3+ attempts on a hard piece → stop + architectural write-up.
- Final consolidated report: per-track (shipped vs built-and-verified vs designed), Track 0 manifest, all differentials/numbers, cross-track issues, recommended next step + release plan. STOP for review.

## Self-Review
- Coverage: FROM vcf + data + manifest → Track 0; ANNOTATE → Track 1; parallelism native+WASM+fallback → Track 2; CALL cnv wiring+validation → Track 3; dependency gates + submodule discipline + differentials + honest landings → Global/Coordination. ✓
- Grounded in investigation (FROM vcf exists/verify; CALL cnv half-wired; both submodules; network up). ✓
