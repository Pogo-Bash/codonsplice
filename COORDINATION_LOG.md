# Coordination Log — CIGAR-aware indel calling (2026-06-28)

Orchestrator-maintained. **First session that WRITES into the cnvlens-core submodule.**

## Branches
- **Superproject:** `feat/cigar-indel-calling` (off `main`). Holds the plan, this log, codonsplice tests, and the final submodule pointer bump.
- **Submodule (`cnvlens`):** `feat/cigar-indel-calling` (off `cbff4aa`). ALL cnvlens-core edits land here, serialized.

## Plan
`docs/superpowers/plans/2026-06-28-cigar-indel-calling.md` (operationalizes `docs/design/CIGAR_INDEL_CALLING.md`; design decisions D1–D9 are settled).

## ⚠ Submodule discipline (the main risk this session)
- Both tracks edit cnvlens-core → **serialized on ONE submodule branch**, NEVER concurrent on the submodule working tree.
- **Order: Track 2 (keep_read) FIRST, then Track 1 (CIGAR) on top.** Track 2 changes which reads enter the pileup, so Track 1's tests/GIAB baselines are taken AFTER Track 2.
- cnvlens-core commits → submodule branch. Superproject pointer bump → ONE dedicated commit at the very end (Task 1.5), only after submodule is committed.
- **NO push to the cnvlens remote, NO tag/release.** Local submodule commits + local pointer bump are expected/allowed. Stop before any push.

## CIGAR correctness policing (the bug class)
Cursor arithmetic + indel anchoring is where off-by-ones hide (cf. the CALL-reads 0/1-based bug). Code review MUST, at the Task 1.2 and 1.5 checkpoints, demand:
- dual cursors advance correctly for EVERY op (M/I/D/=/X/S/N/H/P + leading/trailing clips), not just M/I/D;
- indel POSITION matches samtools exactly (VCF anchors to the base BEFORE the event → anchor = ref_cur−1; POS = anchor+1);
- a REAL samtools/bcftools differential (indel pos+allele concordance), not just unit tests;
- region-boundary reads (the half-open fix) interact correctly with CIGAR walking.

## Submodule state tracking
| event | submodule HEAD | superproject pointer |
|---|---|---|
| start | cbff4aa (branch feat/cigar-indel-calling created) | cbff4aa |
| Task 2 (keep_read) | 89283ad | cbff4aa (not yet bumped — correct) |
| 1.1 decode CIGAR | 2a4532a | cbff4aa |
| 1.2 walk_cigar | 96ac298 | cbff4aa |
| 1.3 build_indel_alleles | e1a816d | cbff4aa |
| 1.4 CIGAR pileup+indels | 2f818f58 | cbff4aa |

## Status
| track/task | status |
|---|---|
| 2 — keep_read completion (supplementary/qcfail) | ✅ done (submodule 89283ad), TDD red→green, both builds green |
| review 2 | ✅ done (orchestrator verify: clean diff, no over-reach) |
| 1.1 — decode CIGAR into AlnRecord | ✅ done (submodule 2a4532a), both crates green |
| 1.2 — pure walk_cigar (dual cursor) + review | ✅ done (96ac298) + review APPROVE (cursor/anchor/op-coverage verified) |
| 1.3 — pure build_indel_alleles | ✅ done (e1a816d), 5 tests |
| 1.4 — wire pileup + emit indels + SNV regression + review | ✅ done (2f818f58) + review APPROVE: SNV precision 0.036→1.000 (214 FP removed, 0 TP lost); indels exact |
| 1.5 — pointer bump + e2e + bcftools differential + GIAB recall + review | ✅ done (superproject 6984209, pointer→2f818f58) |
| consolidated report (incl. GIAB before/after) | ✅ done (below) |

---

# CONSOLIDATED REPORT

## Submodule + pointer state (NOTHING PUSHED)
- **cnvlens submodule** `feat/cigar-indel-calling` — 5 clean logical commits on top of `cbff4aa`:
  `89283ad` keep_read (supp/qcfail) · `2a4532a` decode CIGAR · `96ac298` walk_cigar · `e1a816d` build_indel_alleles · `2f818f58` CIGAR pileup + indels.
- **Superproject** `feat/cigar-indel-calling` — `3cfe865` (plan/log) · `6984209` (pointer bump → `2f818f58` + e2e indel test).
- Both branches have NO upstream; no `git push` run in superproject or submodule. Pointer recorded == submodule HEAD == `2f818f58`. ✓

## Track 2 (keep_read) — DONE
`keep_read` now excludes supplementary (0x800) + qcfail (0x200), completing the analysis-ready mask. Landed FIRST (submodule `89283ad`), so all Track-1 baselines are on the corrected read set. Zero effect on this BAM (0 supp/0 qcfail); correctness for split-read/long-read data.

## Track 1 (CIGAR indel calling) — DONE, all checkpoints APPROVED
Dual-cursor CIGAR walk (`walk_cigar_full` → `CigarSpan`; `walk_cigar` is a filter over it, so SNV placement and indel anchors share ONE cursor — no divergence). Indels emit as ordinary `Variant`s (kind INS/DEL, longer REF/ALT) through the unchanged `records_to_vcf`. Anchored per VCF 4.2 (`ref_cur-1`, POS=anchor+1) — indel positions match truth EXACTLY (55010562 GA>G, 55074837 T>TAC; no off-by-one).

**Two wins, both verified against samtools/bcftools/GIAB (not just unit tests):**
1. **Latent SNV bug fixed (precision).** The old ungapped loop mis-piled soft-clipped bases at wrong ref positions, over-calling massively. OLD vs NEW SNV concordance:
   - default min_depth: OLD 222 calls / 8 TP / 214 FP (precision 0.036) → NEW 8 calls / 8 TP / **0 FP (precision 1.000)**; the 8 TPs byte-identical.
   - **No recall regression** (orchestrator re-verified at matched params min_depth=2,min_var_reads=2,min_af=0.2): OLD and NEW BOTH give **94 SNV TP**, but NEW FP=21 vs OLD ~200+. Same recall, far higher precision.
2. **Indels now callable (recall).** GIAB EGFR indel recall **0/37 → 8/37** (TP=8, FP=0) at min_depth=2. Whole-region recall 0.237 → 0.265. The 29 missed indels are at depth 0–1 sites or homopolymer/STR regions the design (§9) flagged as the at-risk tail — not a calling bug; at default min_depth=10 indel recall is 0/37 (a data property of this sparse BAM).

## Cross-track outcome
Serialization held: Track 2 landed first; Track 1 built on the corrected read set; one submodule branch, no races; one pointer bump at the end. The CIGAR-correctness review gates (op-coverage matrix at 1.2, real GIAB differential at 1.4/1.5) caught nothing broken but validated the dramatic SNV-precision change as genuine.

## What's left before this can ship
1. **Review + decide release** (yours): the work is on branches, not pushed. Merging needs your call.
2. **Indel error model / QUAL** — v1 reuses the SNV binomial (design D9); homopolymer indels want a context-aware model (design §9). Non-blocking for shipping the counter.
3. **In-engine left-align** (design §5 fast-follow) — currently rely on documented `bcftools norm`; concordance already normalizes both sides, so this is ergonomics, not numbers.
4. **Default `min_depth`** — indels (and many low-coverage SNVs) need a lower default on sparse BAMs; consider auto-scaling to coverage. Separate decision.

## Recommendation
Ship the CIGAR pileup — it is a strict improvement (SNV precision 0.04→1.00 with no recall loss) even before indels, and indels add a real 8/37 with zero FP. Then iterate on the indel error model + default min_depth for the homopolymer/low-depth tail.

## Log
- 1.4 review APPROVE (real GIAB differential): OLD ungapped pileup precision 0.036 (222 calls, 214 soft-clip FP); NEW CIGAR pileup precision 1.000 (8 calls, 8 TP, 0 FP), the 8 TPs byte-identical OLD↔NEW — pure precision gain, no recall loss. Indels exact vs truth (55010562 GA>G, 55074837 T>TAC). NOTE for 1.5: GIAB indel sites are depth 1–7 < default min_depth=10, so the GIAB indel-recall measurement MUST use a low min_depth (e.g. 2) to actually call them.
- 1.4 BIG finding: the old ungapped loop over-called (280 SNVs on EGFR BAM, soft-clip-misplacement FPs). CIGAR-correct pileup → 5 clean het SNVs (worker claims all GIAB TPs). walk generalized to walk_cigar_full (Aligned match-runs + Indel spans); walk_cigar now a filter over it → no cursor divergence, 1.2 tests intact. Superproject streaming_tests.rs `>5` relaxed to `>=5` (encoded buggy over-calling). NEEDS rigorous review: validate the SNV set is BETTER (precision up, recall not down) vs a regression, + real indel differential.
- 1.2 review APPROVE. Two NON-BLOCKING hardening items to fold into Task 1.4 (same file): (a) optional `if read_cur+len <= seq.len()` guard on the Ins slice in walk_cigar (pub fn, panics on malformed CIGAR-vs-seq; unreachable on real BAMs but cheap to guard); (b) add a Pad-op test and a trailing-insertion test.
- (init) Plan written from the design doc; current code confirmed (AlnRecord has no cigar; call_from_pileup:392 ungapped loop; OffsetData at ~290; keep_read at variants.rs:104). Superproject + submodule branches created on feat/cigar-indel-calling; pointer still cbff4aa. Dispatching Track 2 first.

---

# ADDENDUM — in-engine indel left-alignment (drops bcftools norm)

**Goal (ship-blocker for embeddable/WASM):** normalize indel positions inside the
engine so users don't pipe through `bcftools norm`.

- **Submodule** `feat/cigar-indel-calling` += `fcf9d88` — `left_align_indel`
  (standard roll-left while `REF.last()==ALT.last()` and `anchor0>0`, using the
  reference) wired into both indel emission sites; `pos=anchor+1` re-derived
  after the shift (anchor convention preserved). 5 pure unit tests (homopolymer
  del/ins shift, already-aligned no-op, reference-start boundary, non-repetitive
  no-op).
- **Superproject** `feat/cigar-indel-calling` += `f407748` — pointer bump → fcf9d88.
- **Branch-discipline fix:** a prior worker had left the submodule detached at
  2f818f58; the left-align commit landed on detached HEAD. Orchestrator
  fast-forwarded the branch to fcf9d88 and re-attached HEAD (clean ff, full
  6-commit chain intact).

**Verification (the key requirement — differential vs bcftools, not unit tests alone):**
- `bcftools norm -f chr7.fa -m-` on the engine output: **realigned 0** lines; the
  indel POS/REF/ALT diff vs the norm'd output is **empty (byte-identical)** across
  all indels, including STR/homopolymer sites (AACAC>A 55159540, GAGA>G 55193108,
  TTTTG>T 55147281). Code-review APPROVE on the shift logic + an independent
  byte-identical differential.
- **GIAB indel recall identical with vs without bcftools norm: 16/37 both** —
  the engine is self-sufficient; the bcftools norm step can be dropped.
- SNV path + already-aligned indels (55010562 GA>G) unchanged; all cnvlens-core +
  codonsplice-core tests green.

**Submodule state:** branch feat/cigar-indel-calling @ fcf9d88 (6 commits off
cbff4aa), on-branch (not detached). Superproject pointer = fcf9d88. NOTHING PUSHED.
