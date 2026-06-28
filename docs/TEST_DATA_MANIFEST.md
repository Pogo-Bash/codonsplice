# Test Data Manifest — EGFR / GRCh37

Track 0B deliverable. Sliced, coordinate-correct reference data for Track 1
(ANNOTATE clinical + gene-model join) and Track 3 (CALL cnv validation), plus a
Track 2 (parallelism) sizing note.

## Coordinate system — READ FIRST

**Everything here is GRCh37 / hg19.** Contig name is bare **`7`** (NOT `chr7`),
matching the sample BAM `cnvlens/public/sample-data/NA12878_EGFR.bam` (built on
`hs37d5` / NCBI37) and `chr7.fa` (`>7`, length 159138663). Both source databases
(Ensembl GRCh37, ClinVar GRCh37 VCF) already use bare `7`, so **no contig
renaming was required** — verified, not assumed.

The EGFR region slice window is **`7:54990000-55300000`** throughout.

### L858R coordinate correction (important)

The canonical EGFR activating mutation **L858R** is, on GRCh37:

> **`7:55259515 T>G`** — `NM_005228.5:c.2573T>G` / `NC_000007.13:g.55259515T>G` /
> p.Leu858Arg (rs121434568).

The dispatch brief stated "A>G". That is **incorrect**. The GRCh37 reference base
at this position is **T** (confirmed: `samtools faidx chr7.fa 7:55259515` → `T`),
and ClinVar's authoritative record is `T>G`. EGFR is on the **+** strand, so the
genomic change equals the cDNA change. Use **T>G**. (On GRCh38 this position is
55191822; we do NOT use GRCh38.)

---

## Asset table

| Asset | Source + version | Committed path | Contig | Verified |
|---|---|---|---|---|
| EGFR gene model (GFF3 slice) | Ensembl GRCh37 release-87, `Homo_sapiens.GRCh37.87.chromosome.7.gff3.gz` | `testdata/EGFR_region.GRCh37.gff3` | `7` | ✅ L858R in EGFR exon 21 |
| ClinVar clinical slice | NCBI ClinVar `vcf_GRCh37/clinvar.vcf.gz`, fileDate **2026-06-27** | `testdata/clinvar_GRCh37_EGFR.vcf.gz` (+`.tbi`) | `7` | ✅ L858R = EGFR, Oncogenic/Tier I |
| CNV depth-ratio baseline | Derived from `NA12878_EGFR.bam` (negative control) | `testdata/cnv_depth_baseline.bed` | `7` | ✅ flat ~CN2 (diploid normal) |
| CNV baseline generator | This repo | `scripts/cnv_depth_baseline.sh` | — | ✅ reproducible |
| dbSNP / gnomAD GRCh37 slices | — | NOT obtained | — | ❌ see "Not obtained" |
| Cell-line CNV truth (HCC827/H1975 BAM) | — | NOT obtained | — | ❌ controlled access |

Reference assets already present in the repo (not produced here, listed for context):
`chr7.fa` (+`.fai`), `cnvlens/public/sample-data/NA12878_EGFR.bam` (+`.bai`),
`cnvlens/public/sample-data/EGFR_region.fa` (slice `7:54990000-55300100`).

---

## 1. EGFR gene model — `testdata/EGFR_region.GRCh37.gff3`

**What:** Ensembl GRCh37 gene/transcript/exon/CDS annotations overlapping the EGFR
region, for Track 1's interval (gene-model) join.

**Source:** Ensembl GRCh37 archive, release-87 (the final GRCh37 release).
`https://ftp.ensembl.org/pub/grch37/current/gff3/homo_sapiens/Homo_sapiens.GRCh37.87.chromosome.7.gff3.gz`
(`current` → release-87). Per-chromosome file; contig already named `7`.

**Slice command** (download then overlap-filter; header preserved):
```bash
curl -s -o chr7.gff3.gz \
  "https://ftp.ensembl.org/pub/grch37/current/gff3/homo_sapiens/Homo_sapiens.GRCh37.87.chromosome.7.gff3.gz"
zcat chr7.gff3.gz | awk -F'\t' 'BEGIN{OFS="\t"}
  /^#/ {print; next}
  $1=="7" && $4<=55300000 && $5>=54990000 {print}' \
  > testdata/EGFR_region.GRCh37.gff3
```
357 feature records. Genes in slice: **EGFR** (`7:55086714-55324313`, + strand)
and **EGFR-AS1** (antisense). Contig naming: bare `7` (no normalization needed).

**Verification — L858R falls in EGFR exon 21:**
The exon overlapping `7:55259515` in the canonical EGFR transcript
**ENST00000275493** is **rank=21** (exon 21):
```
7  ensembl_havana  exon  55259412  55259567  .  +  .  Parent=transcript:ENST00000275493;Name=ENSE00001681524;...;rank=21
```
(`55259412 <= 55259515 <= 55259567`.) ✅ This is the textbook "L858R is in EGFR
exon 21" fact, confirmed against real Ensembl coordinates.

---

## 2. ClinVar clinical slice — `testdata/clinvar_GRCh37_EGFR.vcf.gz`

**What:** ClinVar GRCh37 records in the EGFR region, for Track 1's clinical join
(the verifiable annotation target).

**Source:** `https://ftp.ncbi.nlm.nih.gov/pub/clinvar/vcf_GRCh37/clinvar.vcf.gz`
(`##fileDate=2026-06-27`, `##reference=GRCh37`). Full file is ~192 MB; sliced
**remotely via tabix** (no full download):

**Slice command:**
```bash
tabix -h "https://ftp.ncbi.nlm.nih.gov/pub/clinvar/vcf_GRCh37/clinvar.vcf.gz" \
  7:54990000-55300000 > clinvar_GRCh37_EGFR.vcf
bgzip clinvar_GRCh37_EGFR.vcf
tabix -p vcf clinvar_GRCh37_EGFR.vcf.gz
```
4087 lines (incl. header); **109 Pathogenic/Likely_pathogenic** records in slice.
Contig naming: bare `7`.

**Verification — L858R is EGFR + clinically actionable:**
```
7  55259515  16609  T  G  GENEINFO=EGFR:1956;
   CLNHGVS=NC_000007.13:g.55259515T>G; ALLELEID=31648; RS=121434568;
   CLNSIG=drug_response; CLNREVSTAT=reviewed_by_expert_panel;
   ONC=Oncogenic; ONCREVSTAT=criteria_provided,_single_submitter;
   SCI=Tier_I_-_Strong; MC=SO:0001583|missense_variant;
   CLNDN=...gefitinib_response...|Nonsmall_cell_lung_cancer,_response_to_TKI...
```
✅ `GENEINFO=EGFR:1956`. **Honest nuance on "pathogenic":** L858R's *germline*
ClinVar significance is **`drug_response`** (reviewed by expert panel) — NOT the
literal string `Pathogenic` — because it is a somatic, drug-response (gefitinib /
TKI) oncogenic variant. Its *oncogenicity* classification is **`Oncogenic`** and
its *somatic clinical impact* is **`Tier_I_-_Strong`**. So Track 1's join target
should match on **EGFR + (oncogenic OR drug_response OR pathogenic/likely-path)**,
not `CLNSIG=Pathogenic` alone, or it will miss L858R. The slice independently
contains 109 true `Pathogenic/Likely_pathogenic` EGFR-region records if a strict
P/LP target is preferred (e.g. the frameshift `7:55259517 GC>G` `CLNSIG=Pathogenic`
two bases away).

---

## 3. CNV validation — chosen plan + committed measuring stick

**Constraint:** CNV has no GIAB. NA12878 is a **germline normal** → it is
copy-number **diploid (CN=2)** across EGFR, so it carries no positive CNV truth.

**Options researched:**
- **(a) EGFR-amplified cell line** (HCC827 = EGFR-amplified ~75 copies + exon-19
  E746_A750del; H1975 = L858R+T790M). Copy number is well documented, but the
  WGS/WES **BAMs are CCLE/dbGaP controlled-access** — not openly streamable or
  tabix-sliceable in this environment; ATCC distributes purified DNA, not public
  alignments. **Not obtainable here.** (See sources below.)
- **(b) A CNV truth set for an accessible sample** — none available for this BAM.
- **(c) CHOSEN — depth-ratio baseline from the sample BAM** as a measuring stick.

**What was committed:**
- `scripts/cnv_depth_baseline.sh` — generator. Builds 1 kb windows across
  `7:54990000-55300000`, computes mean depth per window via `samtools bedcov`,
  normalizes to the region median, emits `depth_ratio` + `log2ratio` +
  `expected_cn=2` + `expected_call=neutral` per window.
- `testdata/cnv_depth_baseline.bed` — the baseline (310 windows). Run:
  ```bash
  ./scripts/cnv_depth_baseline.sh        # defaults to the NA12878 EGFR BAM
  ```

**How Track 3 uses it:** This is a **negative-control truth**. Run `CALL cnv` on
`NA12878_EGFR.bam` over the same region; it should produce **no** amplification or
deletion events (every window is CN=2 / `log2ratio≈0`). Any CNV call is a false
positive. Diff `CALL cnv` segment output against the `expected_call=neutral`
column; the `depth_ratio`/`log2ratio` columns are the continuous signal to
correlate against the caller's per-window ratio.

**Coverage caveat (honest):** the BAM is 1000-Genomes-style low-pass over the
region — mean depth ~6.4×, median 1 kb-window depth ~2.6×, 297/310 windows
covered. A few windows spike (max ~46× the median at `7:55002000-55003000`,
likely a repeat/segdup mapping artifact, **not** a real amplification) — Track 3
should treat isolated high-ratio windows skeptically, and ideally the caller's
own GC/mappability handling. Low coverage means this baseline validates
**"calls no spurious CNV on a flat diploid region"** well, but cannot validate
**amplification sensitivity** — for that, option (a)'s controlled-access data
would be needed.

---

## 4. Track 2 (parallelism) — BAM-size recommendation

**The current sample BAM is TOO SMALL to benchmark serial-vs-parallel speedup.**
Timing `CALL variants` over the full EGFR region:
```
FROM bam "cnvlens/public/sample-data/NA12878_EGFR.bam"
WHERE pos >= 54990000 AND pos <= 55300000
CALL variants WITH reference = "chr7.fa", min_depth = 2
```
→ **~0.7-0.9 s wall, ~0.45 s user** (release binary, repeated). Region = 310 kb,
~31,421 reads, 2.4 MB BAM. At sub-second compute dominated by process startup and
I/O, parallel speedup is unmeasurable / in the noise.

**Recommendation for Track 2:** benchmark on a larger workload — either (i) a
whole-chromosome-7 or multi-region BAM, or (ii) a higher-coverage BAM, or (iii)
synthesize a larger input by replicating/region-expanding so total reads reach
1e6+ and serial runtime is several seconds. Keep the EGFR BAM only as a
correctness/byte-identical fixture (serial == sharded output), not as the speedup
benchmark.

---

## Not obtained (honest)

| Wanted | Why not |
|---|---|
| **HCC827 / H1975 EGFR-amplified BAM** (CNV positive truth) | CCLE/dbGaP **controlled access**; no open, region-sliceable alignment. Documented as plan option (a); fell back to the depth-ratio negative control. |
| **dbSNP GRCh37 EGFR slice** | Deprioritized: ClinVar already supplies the rsIDs / clinical join target needed by Track 1. dbSNP GRCh37 (`GCF_000001405.25`) is tabix-sliceable remotely but uses RefSeq `NC_000007.13` contig names → would need renaming to `7`; not needed for the L858R target, so skipped to avoid a contig-mismatch footgun. Add later only if Track 1 needs population rsIDs beyond ClinVar. |
| **gnomAD GRCh37 EGFR slice** | Deprioritized: large; population AF not required for the L858R clinical-annotation target. Remote-tabix sliceable from gnomAD v2.1.1 sites if a future track needs allele frequencies. |

---

## Reproducibility summary

All slices keep true GRCh37 chr7 coordinates and contig `7`. Verifications:
- `samtools faidx chr7.fa 7:55259515` → `T` (L858R ref base; mutation is T>G).
- gene model: exon `rank=21` of ENST00000275493 spans `55259412-55259567` ⊇ 55259515.
- ClinVar: `tabix testdata/clinvar_GRCh37_EGFR.vcf.gz 7:55259515` → EGFR L858R,
  Oncogenic / Tier I-Strong.
- CNV baseline: 310 windows, median depth-ratio 0.999 (flat diploid).

**Sources (CNV cell-line research):**
- HCC827 EGFR amplification & exon-19 del: [Nature Struct Mol Biol 2025](https://www.nature.com/articles/s41594-025-01685-4),
  [ATCC CRL-2868DQ (EGFR-amplified genomic DNA)](https://www.atcc.org/products/crl-2868dq).
