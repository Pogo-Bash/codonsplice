# Four-Track Integration + Ship — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. This is a **dedicated integration session** — the four tracks were built in isolation and verified separately (see `COORDINATION_LOG.md`); this plan merges them in dependency order and does NOT begin until that session starts.

**Goal:** Merge the four verified track branches (FROM vcf + data, CALL cnv, ANNOTATE, region-sharding) into one integration branch, resolve the known additive conflicts, and — for the first time — prove CALL cnv + ANNOTATE are byte-identical under sharded vs serial execution, then ship.

**Architecture:** One integration branch off the Track 0 base (`feat/vcf-input-and-test-data`, which already carries the FROM-vcf fix + GRCh37 test data). Merge Track 3 then Track 1 (the two `Record`-enum extenders — resolved together once) then Track 2 (sharding — the cross-cutting producer wrapper, merged last so its equivalence gate can cover every producer). The conflicts are all *additive* (each branch adds a new enum variant + a new arm to the same `match`); resolution is union, not rewrite.

**Tech Stack:** Rust, codonsplice-core (VM/compiler), two submodules — `crates/spliceql` (ANNOTATE grammar, at `c247dad` on `feat/annotate`) and `cnvlens/rust/cnvlens-core` (unchanged this cycle). noodles, samtools/bcftools as oracle, wasm-pack for the WASM gate.

## Global Constraints

- **Nothing pushed, tagged, or released without the user's explicit say-so.** Local branches + local submodule commits + pointer bumps only. Task 7 (ship) is GATED on the user.
- **Submodule discipline (both submodules):** `crates/spliceql` changes stay on its own branch (`feat/annotate` @ `c247dad`); bump the superproject pointer in a dedicated commit AFTER the submodule is committed. **Never leave a submodule in detached HEAD** (it bit us before). `cnvlens` is unchanged — do not touch its working tree.
- **Half-open boundary class (#20)** is the recurring danger: sharding split, CNV windowing, ANNOTATE interval joins all use INCLUSIVE boundaries. Every equivalence test must keep a boundary-straddling case.
- **Differentials at review, not unit tests alone:** each merge task's gate is a real differential proven in the source session — L858R → EGFR/exon21/drug_response, CNV negative-control → 0 calls, serial == sharded byte-identical. Re-run them post-merge; a green `cargo test` alone does not close a task.
- **Run the binary with `SPLICE_NO_UPDATE_CHECK=1`.** Reference + BAM are absolute paths: `/home/swap/lang/codonsplice/chr7.fa`, `/home/swap/lang/codonsplice/cnvlens/public/sample-data/NA12878_EGFR.bam`.
- Base branch: `feat/vcf-input-and-test-data` @ `922eaed`. Track branches: `feat/call-cnv` (`fafb6e5`), `feat/annotate` (`cf9c9f6` + spliceql `c247dad`), `wt/parallelism` (worktree `../codonsplice-parallel`, tip `2930fe2`).

---

## File Structure

Merge touches these shared files (new files — `annotate.rs`, `shard.rs`, test data — merge cleanly and are not listed):

- `crates/codonsplice-core/src/runtime.rs` — `Record` enum + its `kind()`/`into_row()`/`get_field()` match blocks. **Conflict: Track 1 & Track 3 each add a variant + an arm.** Resolution = union.
- `crates/codonsplice-core/src/vm.rs` — `CallKind` dispatch (Track 2 wraps `Variants`, Track 3 splits `Cnv|Coverage`), `record_to_json` (Track 1 + Track 3 each add an arm), `records_to_vcf` (Track 2 edits — **must preserve Track 0's ID/FILTER fix**). Resolution = union + verify the Track 0 fix survives.
- `crates/codonsplice-core/src/lib.rs` — `pub mod` / `pub use` list (Track 1 adds `annotate`, Track 2 adds `shard`). Resolution = keep both lines.
- `crates/codonsplice-core/tests/shard_equivalence_tests.rs` — extended in Task 5 to cover CNV + ANNOTATE (the first real "CALL cnv under sharding" test).

---

## Task 1: Create the integration branch

**Files:**
- None modified — branch creation only.

**Interfaces:**
- Produces: branch `feat/integration` at the Track 0 base, with submodules in a known-good non-detached state.

- [ ] **Step 1: Create the integration worktree off the Track 0 base**

```bash
cd /home/swap/lang/codonsplice
git worktree add -b feat/integration ../codonsplice-integration feat/vcf-input-and-test-data
cd ../codonsplice-integration
git submodule update --init --recursive
```

- [ ] **Step 2: Verify the base builds and the FROM-vcf + data foundation is present**

```bash
SPLICE_NO_UPDATE_CHECK=1 cargo test -p codonsplice-core --test from_vcf_tests
ls testdata/clinvar_GRCh37_EGFR.vcf.gz testdata/EGFR_region.GRCh37.gff3
```
Expected: from_vcf tests PASS (10 tests); both data files exist.

- [ ] **Step 3: Confirm no submodule is detached**

```bash
git submodule foreach 'git symbolic-ref -q HEAD || echo "DETACHED in $name"'
```
Expected: no `DETACHED` line. (cnvlens may print its branch; spliceql at base is fine.)

- [ ] **Step 4: Commit the integration-session marker in the log**

```bash
# append a "## Integration session started" line to COORDINATION_LOG.md, then:
git add COORDINATION_LOG.md && git commit -m "integration: branch off Track 0 base; foundation verified"
```

---

## Task 2: Merge Track 3 (CALL cnv) — first Record-enum extender

**Files:**
- Modify: `crates/codonsplice-core/src/runtime.rs` (add `Record::Cnv` + arms)
- Modify: `crates/codonsplice-core/src/vm.rs` (`CallKind::Cnv|Coverage` split, `compute_coverage_windows`, `records_to_tsv`/`record_to_json` Cnv arms)

**Interfaces:**
- Consumes: Track 0 base.
- Produces: `Record::Cnv(serde_json::Value)`; `cnv_field(v: &serde_json::Value, name: &str) -> RuntimeValue` (aliases `chr`→`chrom`, `pos`→`start`, `cn`/`copyNumber`→`copy_number`); VM `CallKind::Cnv` runs `detect_cnvs_*`.

- [ ] **Step 1: Merge the branch**

```bash
git merge --no-ff feat/call-cnv
```
Expected conflicts: `runtime.rs`, `vm.rs` (only if base moved; if it merges clean, skip to Step 4). Track 3 is the FIRST extender, so against the clean base this **merges without conflict** — the conflicts appear in Task 3 when Track 1 lands on top.

- [ ] **Step 2: (If conflicted) resolve runtime.rs — keep Track 3's additions verbatim**

The `Record` enum gains:
```rust
    /// A copy-number call (amplification/deletion) produced by `CALL cnv`,
    /// carried as the normalized snake_case JSON object that
    /// `cnvlens_core::cnv::detect_cnvs_*` emits.
    Cnv(serde_json::Value),
```
`kind()` gains `Record::Cnv(_) => "cnv",`; `into_row()` gains the `Record::Cnv(_) => Record::Row(vec![...])` arm; `get_field()` gains `Record::Cnv(v) => cnv_field(v, name),`; and the free `fn cnv_field(...)` is added. Take all of these as-is.

- [ ] **Step 3: (If conflicted) resolve vm.rs — keep Track 3's `CallKind::Cnv | CallKind::Coverage` split and helpers verbatim.**

- [ ] **Step 4: Build**

```bash
SPLICE_NO_UPDATE_CHECK=1 cargo build -p codonsplice-core
```
Expected: clean build.

- [ ] **Step 5: Re-run the CNV differential (negative control — the real gate)**

```bash
SPLICE_NO_UPDATE_CHECK=1 cargo run -q -- run -e \
'FROM bam "/home/swap/lang/codonsplice/cnvlens/public/sample-data/NA12878_EGFR.bam" \
 WHERE chr = "7" AND pos >= 55200000 AND pos <= 55210000 \
 CALL cnv WITH window_size = 500' --format json
```
Expected: empty result set (flat diploid intron → **0 CNV calls**). This is the differential, not a unit test.

- [ ] **Step 6: Full suite + verify Track 0 fix intact**

```bash
SPLICE_NO_UPDATE_CHECK=1 cargo test -p codonsplice-core
```
Expected: all green, including `from_vcf_tests` (Track 0's ID/FILTER round-trip).

- [ ] **Step 7: Commit**

```bash
git commit  # finalize the merge commit if it paused for resolution; else already committed
```

---

## Task 3: Merge Track 1 (ANNOTATE) — second Record-enum extender + spliceql pointer

**Files:**
- Modify: `crates/codonsplice-core/src/runtime.rs` (**conflict** — union `AnnotatedVariant` arms with Task 2's `Cnv` arms; add `Cursor.annotator`)
- Modify: `crates/codonsplice-core/src/vm.rs` (**conflict** in `record_to_json` — union `AnnotatedVariant` with `Cnv`; `Annotate` opcode handler at the dispatch is non-conflicting)
- Modify: `crates/codonsplice-core/src/lib.rs` (**conflict** — keep `pub mod annotate;`)
- Modify: `crates/codonsplice-core/src/compiler.rs`, `materialize.rs` (Track 1 only — merge clean)
- Submodule: `crates/spliceql` → `c247dad` (ANNOTATE grammar) + superproject pointer bump

**Interfaces:**
- Consumes: Track 2's `Record::Cnv` (so the match unions are visible).
- Produces: `Record::AnnotatedVariant { variant: Variant, annotations: Vec<(String,String)> }`; `Cursor.annotator: Option<Arc<crate::annotate::Annotator>>`; the `ANNOTATE WITH genes=..., clinvar=...` clause end-to-end.

- [ ] **Step 1: Merge the branch**

```bash
git merge --no-ff feat/annotate
```
Expected conflicts: `runtime.rs`, `vm.rs`, `lib.rs`. The spliceql submodule pointer also updates — verify in Step 5.

- [ ] **Step 2: Resolve runtime.rs — union both new variants and both new arms**

The `Record` enum keeps BOTH `Cnv(...)` (from Task 2) and `AnnotatedVariant {...}` (Track 1). In each match block keep both arms side by side, e.g. `kind()`:
```rust
            Record::Cnv(_) => "cnv",
            Record::AnnotatedVariant { .. } => "annotated_variant",
```
`into_row()` keeps both the `Record::Cnv(_) => Record::Row(...)` arm and the `Record::AnnotatedVariant { variant, annotations } => { ... }` arm. `get_field()` keeps both `Record::Cnv(v) => cnv_field(v, name),` and the `Record::AnnotatedVariant { variant, annotations } => match annotations... ` arm. Also keep the `Cursor.annotator` field + its `annotator: None` initializer (no conflict with Cnv — different struct). **No arm is dropped or merged into the other; they are distinct variants.**

- [ ] **Step 3: Resolve vm.rs `record_to_json` — union both arms**

Keep both the `Record::Cnv(...)` JSON arm and the `Record::AnnotatedVariant {...}` JSON arm. The `Annotate` opcode handler (separate match on `OpCode`) is non-conflicting — keep it.

- [ ] **Step 4: Resolve lib.rs — keep both module lines**

```rust
pub mod annotate;   // Track 1
// (pub mod shard;  arrives in Task 4)
```
Keep `pub use annotate::{Annotator, ANNOTATION_FIELDS};` too.

- [ ] **Step 5: Verify the spliceql submodule is on its named branch, not detached**

```bash
git -C crates/spliceql rev-parse --short HEAD          # expect c247dad
git -C crates/spliceql symbolic-ref -q HEAD || echo "DETACHED — fix before commit"
git diff --cached --submodule=log -- crates/spliceql   # pointer bump recorded
```
Expected: `c247dad`, on `feat/annotate` (not detached). If detached: `git -C crates/spliceql checkout feat/annotate`.

- [ ] **Step 6: Build**

```bash
SPLICE_NO_UPDATE_CHECK=1 cargo build -p codonsplice-core
```
Expected: clean build (both `Record` variants compile).

- [ ] **Step 7: Re-run the L858R differential (the payoff gate)**

```bash
SPLICE_NO_UPDATE_CHECK=1 cargo run -q -- run -e \
'FROM vcf "testdata/clinvar_GRCh37_EGFR.vcf.gz" CALL variants \
 ANNOTATE WITH genes="testdata/EGFR_region.GRCh37.gff3", clinvar="testdata/clinvar_GRCh37_EGFR.vcf.gz" \
 WHERE pos = 55259515' --format json
```
Expected: one record with `gene":"EGFR"`, `exon":"21"`, `clinvar_significance":"drug_response"`, `rsid":"rs121434568"`. This is the bcftools-parity differential.

- [ ] **Step 8: Full suite**

```bash
SPLICE_NO_UPDATE_CHECK=1 cargo test -p codonsplice-core
```
Expected: all green — `annotate_tests`, `from_vcf_tests`, and the CNV path from Task 2 coexist.

- [ ] **Step 9: Commit (merge + pointer bump together)**

```bash
git commit
```

---

## Task 4: Merge Track 2 (region-sharding) — the cross-cutting producer wrapper

**Files:**
- Modify: `crates/codonsplice-core/src/vm.rs` (**conflict** — union the sharded `CallKind::Variants` arm with Task 2/3's `Cnv|Coverage` arms; **carefully** resolve `records_to_vcf` to preserve Track 0's ID/FILTER fix)
- Modify: `crates/codonsplice-core/src/lib.rs` (**conflict** — add `pub mod shard;` alongside `pub mod annotate;`)
- Create: `crates/codonsplice-core/src/shard.rs` (merges clean)

**Interfaces:**
- Consumes: `Record::Cnv`, `Record::AnnotatedVariant`.
- Produces: `shard::{split_region, ShardExecutor, SerialExecutor, NativeThreadExecutor, shard_and_merge, plan_shard_count}`; VM honors `SPLICE_SHARDS`. Currently shards the `CALL variants` producer ONLY.

- [ ] **Step 1: Merge the branch from the worktree**

```bash
git merge --no-ff wt/parallelism
```

- [ ] **Step 2: Resolve vm.rs `CallKind` dispatch — keep all three producers**

Keep Track 2's sharded wrapping of `CallKind::Variants` AND Track 3's `CallKind::Cnv => detect_cnvs` / `CallKind::Coverage => stream windows` split. They are sibling arms — neither replaces the other.

- [ ] **Step 3: Resolve vm.rs `records_to_vcf` — Track 0's fix MUST survive**

Track 2 edits `records_to_vcf` (hunk near the contig/record loop). Ensure the emitted line still uses the captured `Variant.id` / `Variant.filter` (falling back to `.` / `PASS` when `None`) — NOT a hardcoded `ID="."`/`FILTER="PASS"`. If Track 2's version reintroduced the hardcode, keep Track 0's field-preserving version.

- [ ] **Step 4: Resolve lib.rs**

```rust
pub mod annotate;   // Track 1
pub mod shard;      // Track 2
```

- [ ] **Step 5: Build**

```bash
SPLICE_NO_UPDATE_CHECK=1 cargo build -p codonsplice-core
```
Expected: clean build.

- [ ] **Step 6: Re-run the EXISTING byte-identical gate (regression — must still hold)**

```bash
SPLICE_NO_UPDATE_CHECK=1 cargo test -p codonsplice-core --test shard_equivalence_tests
```
Expected: PASS — `CALL variants` serial (`SPLICE_SHARDS=1`) == sharded (`SPLICE_SHARDS=8`), byte-identical, boundary variant appears exactly once.

- [ ] **Step 7: Re-verify Track 0's round-trip (guards Step 3)**

```bash
SPLICE_NO_UPDATE_CHECK=1 cargo test -p codonsplice-core --test from_vcf_tests
```
Expected: PASS — ID/FILTER round-trip preserved through the Track 2 `records_to_vcf` edit.

- [ ] **Step 8: Commit**

```bash
git commit
```

---

## Task 5: Extend the byte-identical gate to CALL cnv + ANNOTATE (the real integration work — TDD)

This is the task the user singled out: prove the producers Track 2 did NOT originally shard are *also* serial==sharded, or honestly document which producers run serial-only. Sharding currently wraps `CALL variants` alone; this task either extends it to `CALL cnv` / `ANNOTATE` or asserts they fall back to serial deterministically.

**Files:**
- Modify: `crates/codonsplice-core/tests/shard_equivalence_tests.rs` (add CNV + ANNOTATE equivalence cases)
- Possibly modify: `crates/codonsplice-core/src/vm.rs` (extend sharding to the CNV/ANNOTATE producer, OR make those producers an explicit serial-only path)

**Interfaces:**
- Consumes: `SPLICE_SHARDS` env toggle; the `CALL cnv` and `ANNOTATE` producers.
- Produces: a passing equivalence assertion (or a documented, tested serial-only fallback) for both producers.

- [ ] **Step 1: Write the failing test — CALL cnv under sharding == serial**

```rust
#[test]
fn cnv_sharded_is_byte_identical_to_serial() {
    let bam = "/home/swap/lang/codonsplice/cnvlens/public/sample-data/NA12878_EGFR.bam";
    let q = format!(
        r#"FROM bam "{bam}" WHERE chr = "7" AND pos >= 55200000 AND pos <= 55260000 \
           CALL cnv WITH window_size = 500"#
    );
    std::env::set_var("SPLICE_SHARDS", "1");
    let serial = run_to_json(&q);          // existing helper in this file
    std::env::set_var("SPLICE_SHARDS", "8");
    let sharded = run_to_json(&q);
    std::env::remove_var("SPLICE_SHARDS");
    assert_eq!(serial, sharded, "CALL cnv must be identical sharded vs serial");
}
```

- [ ] **Step 2: Run it — expect FAIL or SKEW**

```bash
SPLICE_NO_UPDATE_CHECK=1 cargo test -p codonsplice-core --test shard_equivalence_tests cnv_sharded -- --nocapture
```
Expected: FAIL if CNV is sharded incorrectly (window boundaries split a CNV), OR PASS if the producer already runs serial-only (CNV needs whole-region depth — sharding a CNV across shard seams is semantically wrong). **Diagnose which before "fixing."** A CNV call spans windows; a naive coordinate split would double-count or clip calls at seams (boundary class #20).

- [ ] **Step 3: Implement the honest resolution**

Decide from Step 2's evidence:
- If CNV detection needs the whole region (likely): route `CallKind::Cnv` through `SerialExecutor` explicitly (`plan_shard_count` returns 1 for CNV) and document "CNV runs serial — depth segmentation is not shard-safe; sharding applies to per-position producers only." The test then asserts serial==serial (trivially identical) and the docstring records the limitation.
- If CNV *can* shard with overlap-and-merge: implement the overlap halo + dedup-at-seam in `shard_and_merge`, keep the test as a true equivalence.

Whichever: **say which in the test name + COORDINATION_LOG.md.** Do not fake a parallel CNV that double-counts.

- [ ] **Step 4: Add the ANNOTATE equivalence case**

```rust
#[test]
fn annotate_sharded_is_byte_identical_to_serial() {
    // ANNOTATE is a per-record map over the variant producer — if CALL variants
    // shards identically, ANNOTATE on top must too. Prove it, don't assume.
    let q = r#"FROM vcf "testdata/clinvar_GRCh37_EGFR.vcf.gz" CALL variants \
        ANNOTATE WITH clinvar="testdata/clinvar_GRCh37_EGFR.vcf.gz""#;
    std::env::set_var("SPLICE_SHARDS", "1");  let serial  = run_to_json(q);
    std::env::set_var("SPLICE_SHARDS", "8");  let sharded = run_to_json(q);
    std::env::remove_var("SPLICE_SHARDS");
    assert_eq!(serial, sharded, "ANNOTATE must be identical sharded vs serial");
}
```

- [ ] **Step 5: Run the full equivalence suite**

```bash
SPLICE_NO_UPDATE_CHECK=1 cargo test -p codonsplice-core --test shard_equivalence_tests
```
Expected: PASS — variants, CNV (sharded or documented-serial), ANNOTATE all byte-identical to serial. Keep the boundary-straddling case green.

- [ ] **Step 6: Commit + log the producer/sharding matrix**

```bash
git add crates/codonsplice-core/tests/shard_equivalence_tests.rs crates/codonsplice-core/src/vm.rs COORDINATION_LOG.md
git commit -m "integration: extend byte-identical gate to CALL cnv + ANNOTATE; document CNV sharding semantics"
```

---

## Task 6: WASM build gate (CNV + ANNOTATE + sharding fallback)

**Files:**
- None modified (build verification); if a target-specific cfg is needed, modify the smallest relevant file.

**Interfaces:**
- Consumes: the integrated tree.
- Produces: a confirmed `wasm-pack` build with the single-thread fallback active.

- [ ] **Step 1: Build the WASM package with wasm-pack (provides the allocator zlib-rs needs)**

```bash
cd cnvlens/rust   # or wherever the wasm crate lives; see prior WASM build in COORDINATION_LOG
wasm-pack build --target web 2>&1 | tail -20
```
Expected: build succeeds. (Raw `cargo build --target wasm32` fails on `zlib_rs` malloc/free — that is a tooling artifact, NOT a real blocker; wasm-pack provides the allocator.)

- [ ] **Step 2: Confirm the single-thread fallback path compiles when `crossOriginIsolated` is false**

Verify `shard.rs`'s WASM dispatch defaults to `SerialExecutor` when SharedArrayBuffer is unavailable (the load-bearing rule: threading is a speed enhancement, never required). Grep the design doc + the cfg:
```bash
grep -n "crossOriginIsolated\|SerialExecutor\|single-thread" crates/codonsplice-core/src/shard.rs docs/design/PARALLELISM_WASM.md
```
Expected: fallback present and documented.

- [ ] **Step 3: Log the WASM result honestly**

Record in COORDINATION_LOG.md: which queries were exercised in WASM (CNV depth-based should port; ANNOTATE reads local files), and whether worker-thread parallelism was actually run or remains design-only (it was DESIGNED, not built — say so).

---

## Task 7: Ship (GATED — do not run any push/tag without explicit user approval)

**Files:**
- Version bumps in the relevant `Cargo.toml` / package manifests; CHANGELOG.

**Interfaces:**
- Consumes: a green integration branch.
- Produces: published submodules + a tagged release — **only after the user says go.**

- [ ] **Step 1: Final consolidated differential re-run on the integration branch**

```bash
SPLICE_NO_UPDATE_CHECK=1 cargo test --workspace
```
Expected: all green. Re-state the four differentials (L858R, CNV negative control, variants/CNV/ANNOTATE serial==sharded) in the ship report.

- [ ] **Step 2: STOP — present the integration result and the release plan to the user. Get explicit approval before any network action.**

- [ ] **Step 3: (After approval) submodule push order — spliceql first**

```bash
git -C crates/spliceql push origin feat/annotate   # ANNOTATE grammar; it is published on crates.io
# bump spliceql version, publish; then bump codonsplice-core (FROM-vcf fix + Record kinds + sharding + annotate join all live there)
# cnvlens-core UNCHANGED this cycle → the 0.4.1 automation idempotently skips it
```

- [ ] **Step 4: (After approval) push the superproject branch, then tag**

```bash
git push origin feat/integration
# open PR / merge per the user's workflow, then:
# git tag vX.Y.Z && git push origin vX.Y.Z   — only when the user names the version
```

- [ ] **Step 5: Re-run the release hunter against the new tag (as in prior releases) and report.**

---

## Self-Review

**1. Spec coverage** (vs the user's directive "dependency order, byte-identical gate extended, then ship"):
- Dependency order → Tasks 2→3→4 (the two enum-extenders resolved together, sharding last). ✓
- Byte-identical gate extended → Task 5 (CALL cnv + ANNOTATE under SPLICE_SHARDS, with honest serial-only fallback for CNV if depth-segmentation isn't shard-safe). ✓
- Ship → Task 7, GATED on user approval, submodule-push-order correct. ✓
- "Don't rush / don't tangle the clean separation" → conflicts pre-identified as additive unions with exact resolution code; the one real hazard (Track 2's `records_to_vcf` vs Track 0's fix) has its own guard step (4.3 + 4.7). ✓

**2. Placeholder scan:** every merge step shows the exact code/command; every verify step has an expected result. The one deliberate decision-point (Task 5 Step 3: shard CNV vs serial-only) is framed as "diagnose then choose," with both branches fully specified — not a TODO.

**3. Type consistency:** `Record::Cnv(serde_json::Value)`, `Record::AnnotatedVariant { variant: Variant, annotations: Vec<(String,String)> }`, `Cursor.annotator: Option<Arc<Annotator>>`, `cnv_field`/`split_region`/`SPLICE_SHARDS` — all match the names verified in the source branches' diffs.

**Cross-track integration risks resolved by this plan:** (a) shared `Record` enum + match arms → additive union, Tasks 2–3; (b) CALL-cnv-under-sharding untested → Task 5 makes it a real gate with an honest fallback; (c) Track 2's `records_to_vcf` vs Track 0's fix → guarded, Tasks 4.3/4.7; (d) spliceql pointer + detached-HEAD → Task 3.5.
