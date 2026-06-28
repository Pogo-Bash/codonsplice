# CIGAR-Aware Pileup + Indel Calling — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:test-driven-development (failing test first, every task) and superpowers:systematic-debugging (root-cause; if a CIGAR cursor piece needs 3+ attempts, STOP and write the architectural question to COORDINATION_LOG.md — do not thrash). Steps use checkbox (`- [ ]`).

**Goal:** Make cnvlens-core's variant pileup CIGAR-aware so it calls insertions and deletions (anchored to the reference) through the existing VCF writer, and complete the analysis-ready read filter — closing the GIAB indel recall gap.

**Architecture:** Replace the ungapped inner loop in `call_from_pileup` (`variants.rs:392`, `pos = read_start + i`) with a dual-cursor CIGAR walk (read-cursor + ref-cursor) that feeds both the existing SNV accumulators and new per-anchor insertion/deletion accumulators on `OffsetData`. Indels are emitted as ordinary `Variant`s (longer REF/ALT, `kind="INS"/"DEL"`), so `records_to_vcf` is unchanged. Implementation is entirely in the **cnvlens-core submodule**; codonsplice-core only bumps the pointer.

**Tech Stack:** Rust, noodles (CIGAR decode), samtools/bcftools 1.16 (differential oracle), GIAB NA12878 EGFR truth set, reference `chr7.fa`.

**Reference design:** `docs/design/CIGAR_INDEL_CALLING.md` (decisions D1–D9 are settled; follow them). This plan operationalizes it as TDD tasks.

## Global Constraints

- **Submodule serialization (the main risk):** Both tracks edit cnvlens-core. Do them on ONE submodule branch `feat/cigar-indel-calling`, **serialized**: Track 2 (`keep_read`) lands FIRST, then Track 1 builds on top. Never run two workers on the submodule working tree at once. Keep cnvlens-core history clean: `keep_read` and each CIGAR piece are separate logical commits.
- **Pointer discipline:** cnvlens-core changes commit to the submodule's own branch; the codonsplice superproject `feat/cigar-indel-calling` branch bumps the submodule pointer in a dedicated commit AFTER the submodule work is committed. Never commit a pointer bump that references an uncommitted submodule state.
- **Nothing pushed to the cnvlens remote, no tag, no release.** Local submodule commits + local pointer bump are expected and allowed. STOP before any push/release for review.
- **Coordinate convention:** the pileup is 0-based internally; reported `pos = anchor+1` (1-based VCF), identical to the SNV path. VCF anchors an indel to the base BEFORE the event → anchor `= ref_cur - 1`. This is the classic off-by-one; the samtools/bcftools differential at each checkpoint is the guard.
- **Baselines after Track 2:** Track 2 changes which reads enter the pileup. Track 1's tests/GIAB baselines are taken AFTER Track 2 lands, so the numbers are consistent.
- **Differential at checkpoints:** code review MUST demand a real samtools/bcftools differential (indel pos+allele concordance), not just unit tests — unit tests can encode the same wrong cursor assumption as the code.
- Sample BAM `cnvlens/public/sample-data/NA12878_EGFR.bam`; reference repo-root `chr7.fa`; truth `giab_truth_egfr.norm.vcf.gz`; eval `eval.bed`. Run binary with `SPLICE_NO_UPDATE_CHECK=1`.

---

## File Structure

| File | Track/Task | Responsibility |
|---|---|---|
| `cnvlens/rust/cnvlens-core/src/lib.rs` | 2, 1.1 | `AlnRecord`: add `is_supplementary`/`is_qcfail`; add `cigar` field + `CigarOp` enum |
| `cnvlens/rust/cnvlens-core/src/variants.rs` | 2, 1.2–1.4 | `keep_read` mask; pure `walk_cigar`/`build_indel_alleles`; dual-cursor pileup + indel emission |
| `cnvlens/rust/cnvlens-core/src/bam.rs` | 1.1 | `decode_full`: populate `cigar` from `record.cigar()` |
| codonsplice superproject pointer + `crates/codonsplice-core/tests/indel_e2e_tests.rs` (new) | 1.5 | pointer bump + end-to-end VCF/indel test |
| `COORDINATION_LOG.md` | orchestrator | status, submodule commit/pointer state, cross-track + GIAB numbers |

---

# TRACK 2 — keep_read completion (lands FIRST)

## Task 2: Complete the analysis-ready read mask (#supplementary/#qcfail)

**Files:** `cnvlens/rust/cnvlens-core/src/lib.rs` (add helpers), `cnvlens/rust/cnvlens-core/src/variants.rs` (`keep_read` + test).
**Interfaces:** Produces `AlnRecord::is_supplementary()` (0x800), `AlnRecord::is_qcfail()` (0x200); `keep_read` now also excludes both.

- [ ] **Step 1: Write the failing test** (in `variants.rs` `#[cfg(test)]`)

```rust
#[test]
fn keep_read_excludes_supplementary_and_qcfail() {
    let opts = VariantOptions::default();
    let base = |flag: u16| AlnRecord { ref_id: 0, pos: 100, mapq: 60, flag, seq: vec![b'A'], qual: vec![30] };
    assert!(keep_read(&base(0x2), &opts), "a normal paired read is kept");
    assert!(!keep_read(&base(0x800), &opts), "supplementary (0x800) excluded");
    assert!(!keep_read(&base(0x200), &opts), "qcfail (0x200) excluded");
    assert!(!keep_read(&base(0x400), &opts), "duplicate still excluded");
}
```
(Adapt the `AlnRecord { .. }` literal to its real fields — check lib.rs; if more fields exist, fill them.)

- [ ] **Step 2: Run → FAIL.** `cargo test -p cnvlens-core keep_read_excludes_supplementary_and_qcfail` — fails (helpers undefined / supplementary kept).
- [ ] **Step 3: Implement.** In lib.rs add, mirroring `is_duplicate`:
```rust
#[inline]
pub fn is_supplementary(&self) -> bool { self.flag & 0x800 != 0 }
#[inline]
pub fn is_qcfail(&self) -> bool { self.flag & 0x200 != 0 }
```
In variants.rs `keep_read`, extend the exclusion:
```rust
!(aln.is_unmapped() || aln.is_duplicate() || aln.is_secondary()
    || aln.is_supplementary() || aln.is_qcfail())
    && (aln.mapq as i64) >= opts.min_mapping_quality as i64
```
- [ ] **Step 4: Run → PASS** + full `cargo test -p cnvlens-core` green. Also build the superproject (`cargo build -p codonsplice-core`) to confirm the struct/API change doesn't break consumers.
- [ ] **Step 5: Commit (in the submodule, on branch `feat/cigar-indel-calling`)**
```bash
cd cnvlens/rust/cnvlens-core   # or wherever the submodule git root is
git checkout -b feat/cigar-indel-calling   # first submodule commit creates the branch
git add src/lib.rs src/variants.rs
git commit -m "feat(filter): exclude supplementary (0x800) + qcfail (0x200) from variant pileup

Completes keep_read to the standard analysis-ready mask (UNMAP|SECONDARY|QCFAIL|DUP
+ SUPPLEMENTARY). Zero effect on the NA12878 EGFR BAM (0 supplementary/0 qcfail)
but prevents double-counting split-read/long-read evidence into depth/AF."
```
(Do NOT bump the superproject pointer yet — Track 1 adds more submodule commits; one pointer bump at the end.)

---

# TRACK 1 — CIGAR-aware pileup + indel calling (on top of Track 2)

> All Track 1 submodule commits go on the SAME `feat/cigar-indel-calling` submodule branch, after Task 2.

## Task 1.1: Decode CIGAR into AlnRecord (mechanical)

**Files:** `lib.rs` (`CigarOp` enum + `cigar` field), `bam.rs` (`decode_full`).
**Interfaces:** Produces `pub enum CigarOp { Match, Ins, Del, Skip, SoftClip, HardClip, Pad, SeqMatch, SeqMismatch }` and `AlnRecord.cigar: Vec<(CigarOp, usize)>`. (A LOCAL enum — not noodles `Kind` — so the pure `walk_cigar` tests build CIGAR vectors with no BAM/noodles dependency.)

- [ ] **Step 1: Failing test** (bam.rs `#[cfg(test)]`): decode the sample BAM, assert at least one read has a non-empty `cigar` and that summing ref-consuming ops (Match/Del/Skip/SeqMatch/SeqMismatch) for a read equals its reference span. (Use the bundled NA12878 BAM; pick the first read with a non-trivial CIGAR.)
- [ ] **Step 2: Run → FAIL** (field doesn't exist).
- [ ] **Step 3: Implement.** Add `CigarOp` + `cigar` field to `AlnRecord`. In `decode_full`, map `record.cigar().iter()` (`noodles::sam::alignment::record::cigar::op::Kind`) to `CigarOp` and collect `Vec<(CigarOp, usize)>`. Update any `AlnRecord { .. }` struct literals in cnvlens-core (and codonsplice-core tests) to include `cigar` (default `vec![]` where synthetic).
- [ ] **Step 4: Run → PASS**; `cargo test -p cnvlens-core` + `cargo build -p codonsplice-core` green (fix any struct-literal breakage in codonsplice-core).
- [ ] **Step 5: Commit (submodule):** `feat(bam): decode CIGAR into AlnRecord (CigarOp vec)`

## Task 1.2: Pure `walk_cigar` dual-cursor helper (THE correctness core)

**Files:** `variants.rs` (helper + thorough unit tests).
**Interfaces:** Produces
```rust
pub enum CigarEvent { Ins(Vec<u8>), Del(usize) }   // payloads
/// Returns (ref_anchor_0based, event) for each indel, plus the per-ref-base
/// match positions for SNV pileup. Signature the implementer finalizes, but it
/// MUST expose the indel anchors for unit assertion.
fn walk_cigar(read_pos: i64, cigar: &[(CigarOp, usize)], seq: &[u8]) -> Vec<(i64, CigarEvent)>;
```
Anchor rule (D5): I/D anchor at `ref_cur - 1`. Leading I/D (anchor < read_pos) dropped.

- [ ] **Step 1: Failing tests** — hand-built CIGARs, asserting EXACT anchors/payloads. Cover every op and clips:
```rust
#[test]
fn walk_cigar_simple_insertion() {
    // 4M 2I 4M at read_pos=100, seq= AAAA CC GGGG ; insertion CC anchored at last M before it.
    let seq = b"AAAACCGGGG";
    let cig = vec![(CigarOp::Match,4),(CigarOp::Ins,2),(CigarOp::Match,4)];
    let ev = walk_cigar(100, &cig, seq);
    assert_eq!(ev, vec![(103, CigarEvent::Ins(b"CC".to_vec()))]); // anchor = 100+4-1 = 103
}
#[test]
fn walk_cigar_simple_deletion() {
    // 8M 3D 9M at read_pos=200 ; deletion of 3 ref bases anchored at 200+8-1 = 207.
    let seq = b"AAAAAAAACCCCCCCCC"; // 8 + 9 read bases (D consumes ref only)
    let cig = vec![(CigarOp::Match,8),(CigarOp::Del,3),(CigarOp::Match,9)];
    assert_eq!(walk_cigar(200, &cig, seq), vec![(207, CigarEvent::Del(3))]);
}
#[test]
fn walk_cigar_leading_softclip_then_indel() {
    // 5S 20M 1I 4M : softclip consumes read only; ref starts at read_pos; ins anchor=read_pos+20-1.
    let mut seq = vec![b'N';5]; seq.extend_from_slice(&[b'A';20]); seq.push(b'T'); seq.extend_from_slice(&[b'A';4]);
    let cig = vec![(CigarOp::SoftClip,5),(CigarOp::Match,20),(CigarOp::Ins,1),(CigarOp::Match,4)];
    assert_eq!(walk_cigar(300, &cig, &seq), vec![(319, CigarEvent::Ins(b"T".to_vec()))]); // 300+20-1
}
#[test]
fn walk_cigar_skip_and_seqmatch_mismatch_advance_ref() {
    // 5= 10N 5X : =/X advance both, N advances ref only, no indels emitted.
    let seq = b"AAAAACCCCC";
    let cig = vec![(CigarOp::SeqMatch,5),(CigarOp::Skip,10),(CigarOp::SeqMismatch,5)];
    assert_eq!(walk_cigar(0, &cig, seq), vec![]); // no indels; must not panic / mis-advance
}
#[test]
fn walk_cigar_drops_leading_indel() {
    // 2I 10M : insertion as first op has no preceding ref base -> dropped.
    let seq = b"NN AAAAAAAAAA".iter().filter(|&&c| c!=b' ').copied().collect::<Vec<_>>();
    let cig = vec![(CigarOp::Ins,2),(CigarOp::Match,10)];
    assert_eq!(walk_cigar(50, &cig, &seq), vec![]);
}
```
- [ ] **Step 2: Run → FAIL** (helper undefined).
- [ ] **Step 3: Implement** the dual-cursor walk per the design §2 table (ref_cur init `read_pos`, read_cur init 0; M/=/X advance both; I advance read + emit `Ins(seq[read_cur..read_cur+len])` at `ref_cur-1`; D advance ref + emit `Del(len)` at `ref_cur-1`; N advance ref; S advance read; H/P neither; drop events whose anchor `< read_pos`).
- [ ] **Step 4: Run → PASS** (all walk tests).
- [ ] **Step 5: Commit (submodule):** `feat(pileup): pure dual-cursor walk_cigar with full op coverage`

## Task 1.3: Pure `build_indel_alleles` helper

**Interfaces:** Produces `fn build_indel_alleles(ref_seq: &[u8], anchor0: usize, ev: &CigarEvent) -> Option<(String /*REF*/, String /*ALT*/, &'static str /*kind*/)>` per D5/§3: Ins → `(b, b+ins, "INS")`; Del(n) → `(ref_seq[anchor0..=anchor0+n], b, "DEL")`; `None` if out of `ref_seq` bounds.

- [ ] **Step 1: Failing tests**
```rust
#[test]
fn alleles_insertion() {
    let r = b"ACTGACTG"; // anchor0=0 -> b='A'
    assert_eq!(build_indel_alleles(r, 0, &CigarEvent::Ins(b"CC".to_vec())), Some(("A".into(),"ACC".into(),"INS")));
}
#[test]
fn alleles_deletion() {
    let r = b"GATTACA"; // anchor0=0 b='G', del 1 -> REF = r[0..=1]="GA", ALT="G"
    assert_eq!(build_indel_alleles(r, 0, &CigarEvent::Del(1)), Some(("GA".into(),"G".into(),"DEL")));
}
#[test]
fn alleles_out_of_bounds_is_none() {
    let r = b"AC";
    assert_eq!(build_indel_alleles(r, 1, &CigarEvent::Del(5)), None);
}
```
- [ ] **Step 2: Run → FAIL.** **Step 3: Implement.** **Step 4: Run → PASS.**
- [ ] **Step 5: Commit (submodule):** `feat(pileup): build_indel_alleles (VCF 4.2 anchor-prefixed REF/ALT)`

## Task 1.4: Wire the walk into call_from_pileup + emit indel Variants

**Files:** `variants.rs` (`OffsetData` extension, replace the ungapped loop, emission). This is where SNV regression risk lives — guard it.
**Interfaces:** Consumes `walk_cigar`, `build_indel_alleles`. Produces indel `Variant`s (`kind="INS"/"DEL"`, multi-char ref_base/alt) alongside SNVs; `OffsetData` gains `insertions: HashMap<Vec<u8>,[i64;2]>`, `deletions: HashMap<usize,[i64;2]>`.

- [ ] **Step 1: Failing tests** — (a) SNV regression: feed a synthetic ungapped read set to `call_from_pileup` and assert the SAME SNV calls as before (lock current behavior first by capturing it). (b) A synthetic read with `8M3D9M` over a known `ref_seq` produces a `DEL` Variant at the right `pos+1`/REF/ALT/alt_count. (c) An `Ins` read produces an `INS` Variant. (Build `AlnRecord`s with explicit `cigar` + `seq`; provide `reference_seqs` with a known sequence.)
- [ ] **Step 2: Run → FAIL** (no indel Variants emitted yet; SNV test passes — that's the regression guard).
- [ ] **Step 3: Implement** per design §2/§4: extend `OffsetData`; in the per-read loop replace `for i in 0..seq_len { pos = read_start + i }` with the dual-cursor walk that (i) does the existing SNV per-base accumulation on M/=/X bases at the correct `ref_cur` (this also fixes the latent ungapped-SNV bug), and (ii) accumulates I/D into `insertions`/`deletions` at the anchor offset with strand. In the emission loop, after SNV alts, emit one `Variant` per `insertions`/`deletions` key (depth = spanning `counts` sum; alt_count = indel support; allele_freq = alt_count/depth; reuse `binomial_qual_score` per D9; `build_indel_alleles` for REF/ALT; respect `min_variant_reads`/`min_allele_freq`). Deletions require `reference_seqs` (skip + the existing no-FASTA warning extended).
- [ ] **Step 4: Run → PASS** (SNV regression green + indel tests green); full `cargo test -p cnvlens-core` green.
- [ ] **Step 5: Commit (submodule):** `feat(pileup): CIGAR-aware pileup emits indel Variants (fixes ungapped SNV bug too)`

## Task 1.5: Superproject pointer bump + end-to-end + GIAB recall

**Files:** codonsplice superproject (submodule pointer), `crates/codonsplice-core/tests/indel_e2e_tests.rs` (new). NO codonsplice-core engine change expected (indels flow through `records_to_vcf` unchanged).

- [ ] **Step 1: Bump the submodule pointer.** In the superproject on branch `feat/cigar-indel-calling`: `git add cnvlens` (records the new submodule SHA), confirm `git submodule status` shows the `feat/cigar-indel-calling` submodule HEAD. Do NOT push.
- [ ] **Step 2: Failing e2e test** — `FROM bam NA12878_EGFR.bam WHERE chr="7" AND pos>=55010560 AND pos<=55010565 CALL variants WITH reference="<abs chr7.fa>"` should yield the GIAB deletion `7:55010562 GA>G` (assert a record with pos 55010562, ref "GA", alt "G"). Run → FAIL if the pointer/build isn't wired, PASS once it is. (If chr7.fa isn't committed, gate the test on its presence like other reference-dependent checks.)
- [ ] **Step 3: bcftools differential (REQUIRED at review).** Run the §5 concordance script: splice indel calls over the EGFR region → `bcftools norm -f chr7.fa -m-` → `bcftools isec` vs `bcftools view -R eval.bed -v indels giab_truth_egfr.norm.vcf.gz`. Record TP/FP/FN and a few exact pos/REF/ALT matches against `bcftools mpileup|call` on the same region. Confirm indel POSITIONS match samtools exactly (anchor off-by-one guard).
- [ ] **Step 4: GIAB before/after recall (THE number).** Re-run the full EGFR concordance (SNV+indel) and report recall before (SNV-only, ~indels=0/37) vs after (indels included), toward bcftools' 0.60. Capture the table.
- [ ] **Step 5: Commit (superproject):**
```bash
git add cnvlens crates/codonsplice-core/tests/indel_e2e_tests.rs
git commit -m "feat(variants): CIGAR-aware indel calling via cnvlens-core (pointer bump)

Bumps the cnvlens-core submodule to the CIGAR-aware pileup that emits INS/DEL
Variants through the existing VCF writer. Adds an end-to-end indel test
(7:55010562 GA>G). GIAB EGFR indel recall 0/37 -> <N>/37; whole-region recall
<before> -> <after> toward bcftools 0.60. cnvlens-core submodule committed on
feat/cigar-indel-calling (NOT pushed)."
```

---

# COORDINATION (orchestrator)

- [ ] Update `COORDINATION_LOG.md`: submodule branch name + HEAD SHA after each submodule commit; the codonsplice pointer SHA; cross-track note (Track 2 changes the read set → Track 1 baselines taken after it); GIAB before/after.
- [ ] Code-review checkpoint after Task 2, after Task 1.2 (walk_cigar — demand the op-coverage matrix), after Task 1.4 (SNV regression + indel correctness), and after Task 1.5 (**demand the bcftools/samtools differential + exact indel position concordance**, not just unit tests).
- [ ] Verify NO push to the cnvlens remote and NO superproject push/tag. Final consolidated report: Track 1 + GIAB before/after recall, Track 2, submodule commit/pointer state, what's left before ship.

## Self-Review (author)
- Coverage: keep_read → Task 2; CIGAR decode → 1.1; dual-cursor (all ops + clips + leading-indel) → 1.2; REF/ALT anchor → 1.3; pileup wire + SNV regression + indel emit → 1.4; pointer bump + e2e + bcftools differential + GIAB recall → 1.5. Submodule serialization + pointer discipline + differential-at-review → Global Constraints + Coordination. ✓
- Placeholders: test code given for the pure seams (the bug-prone parts); 1.1/1.4/1.5 describe exact edits + commands. The `<N>`/`<before>`/`<after>` in the 1.5 commit are runtime outputs to fill in, not plan gaps. ✓
- Type consistency: `CigarOp`, `CigarEvent`, `walk_cigar`, `build_indel_alleles`, `AlnRecord.cigar`, `OffsetData.insertions/deletions` used consistently across 1.1–1.4. ✓
