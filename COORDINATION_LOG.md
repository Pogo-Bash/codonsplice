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

## Status
| track/task | status |
|---|---|
| 2 — keep_read completion (supplementary/qcfail) | pending (lands first) |
| review 2 | pending |
| 1.1 — decode CIGAR into AlnRecord | pending |
| 1.2 — pure walk_cigar (dual cursor) + review | pending |
| 1.3 — pure build_indel_alleles | pending |
| 1.4 — wire pileup + emit indels + SNV regression + review | pending |
| 1.5 — pointer bump + e2e + bcftools differential + GIAB recall + review | pending |
| consolidated report (incl. GIAB before/after) | pending |

## Log
- (init) Plan written from the design doc; current code confirmed (AlnRecord has no cigar; call_from_pileup:392 ungapped loop; OffsetData at ~290; keep_read at variants.rs:104). Superproject + submodule branches created on feat/cigar-indel-calling; pointer still cbff4aa. Dispatching Track 2 first.
