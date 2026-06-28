# `FROM vcf` — VCF as a SpliceQL INPUT

`FROM vcf "<path>"` opens a VCF file as a record source. Its already-called
variants are passed straight through the pipeline — no pileup/variant calling is
performed — so it is the **gateway for annotating an existing variant set**
(e.g. GIAB truth, a tool's calls). This document is the precise supported
surface: which clauses compose, `.vcf` vs `.vcf.gz`, INFO-field access,
multi-sample status, and round-trip fidelity.

All CLI examples below were run with:

```
SPLICE_NO_UPDATE_CHECK=1 splice query '<source>'
```

## Quick start

A VCF source must be paired with `CALL variants` — that is the producer that
streams the file's variants. A bare `FROM vcf "x"` with no `CALL` produces
**zero records** (it compiles, but there is no producer):

```
FROM vcf "egfr_impact.vcf" CALL variants
```

```json
{"chrom":"7","pos":55019021,"ref":"G","alt":"A","qual":60.0,"type":"SNV","depth":100,"ref_count":0,"alt_count":0,"allele_freq":0.5,"strand_bias":0.0,"filter":"PASS","id":"rs1"}
...
(8 record(s))
```

> Note: a cosmetic `note: no reference provided …` line is printed to **stderr**
> for `CALL variants`. It is irrelevant for VCF input (no calling happens) and
> does not affect the records on stdout.

## File formats: `.vcf` and `.vcf.gz`

| Input | How it is read |
| --- | --- |
| Plain `.vcf` | UTF-8 text, tab-delimited line parser |
| BGZF `.vcf.gz` | **Inflated in-Rust** via `noodles::bgzf` — no external `bgzip`/`zcat` |

Gzip/BGZF is detected by the `1f 8b` magic bytes, so the extension does not have
to be `.gz`. Verified on the GIAB EGFR truth set:

```
FROM vcf "giab_truth_egfr.vcf.gz" CALL variants    ->  50 records
```

## Clause composition (closure)

Every standard query clause composes on top of a VCF source. SpliceQL clause
order is `FROM … [SELECT …] [WHERE …] CALL … [ORDER BY …] [LIMIT …]` (SELECT
comes **after** FROM, not SQL-style before it).

| Clause | Status | Example |
| --- | --- | --- |
| `FROM vcf` + `CALL variants` | works | `FROM vcf "x" CALL variants` |
| `WHERE` | works | `… WHERE chr="7" AND pos>=55259000 AND pos<=55260000 …` |
| `WHERE` on VCF columns | works | `… WHERE af >= 0.4 AND filter = "PASS" …` |
| `SELECT` projection | works | `FROM vcf "x" SELECT chr,pos,ref,alt,qual,filter,id CALL variants` |
| `ORDER BY` | works | `… CALL variants ORDER BY qual DESC` |
| `LIMIT` | works | `… CALL variants LIMIT 5` |
| `INTO vcf` (round-trip) | works | `FROM vcf "x" CALL variants INTO vcf "y"` |
| `INTO tsv` / `INTO json` | works | projected or native columns |

### Readable columns

The reader maps each VCF record to a `Variant`. These names are usable in
`WHERE`/`SELECT`/`ORDER BY`:

| Column | Source |
| --- | --- |
| `chr` / `chrom` | CHROM |
| `pos` | POS (1-based, as in the file) |
| `id` | ID (`.` when absent) |
| `ref` / `ref_base` | REF |
| `alt` | **first** ALT allele only |
| `qual` | QUAL (`.` → `0.0`) |
| `filter` | FILTER (`PASS` when absent) |
| `depth` | **INFO/DP** |
| `af` / `allele_freq` | **INFO/AF** (first value) |
| `kind` / `type` | derived: `SNV` if REF/ALT both length 1, else `INDEL` |
| `ref_count`, `alt_count`, `strand_bias` | always `0` for VCF input (not in VCF) |

Example — `WHERE` over a window:

```
FROM vcf "splice_calls.vcf" WHERE chr="7" AND pos>=55259000 AND pos<=55260000 CALL variants
->  3 record(s)  (55259684, 55259730, 55259732)
```

## INFO-field access — KNOWN LIMITATION

**Only `INFO/DP` and `INFO/AF` are read.** They are surfaced as the `depth` and
`af` columns. **No other INFO key is accessible** — `Variant` has no generic
INFO map, so projecting an arbitrary INFO field returns `null`:

```
FROM vcf "egfr_impact.vcf" SELECT chr, pos, gene CALL variants
{"chr":"7","pos":55019021,"gene":null}   # INFO/GENE=EGFR is NOT read
```

Implication for annotation (Track 1): you can **read** the identity columns
(CHROM/POS/REF/ALT/ID/FILTER/QUAL) and DP/AF, filter/sort/select on them, and
write the set back out — but you **cannot read pre-existing INFO annotations**
(gene, consequence, etc.) from the input, and any new annotation must be
expressed as a SpliceQL projected column (which lands in INFO via the projected
`SELECT … INTO vcf` path), not by merging into the source INFO string.

## Round-trip fidelity (`FROM vcf … INTO vcf`)

A native (non-projected) round-trip preserves:

- CHROM, POS, REF, ALT (first allele), QUAL
- **ID** and **FILTER** (fixed in Track 0A — the writer previously hardcoded
  `.` / `PASS`)
- INFO/DP and INFO/AF (re-declared with spec-compliant `##INFO` headers)
- `##contig` lines (with length when the source header carried one)

```
FROM vcf "in.vcf" CALL variants INTO vcf "out.vcf"
```

```
#CHROM  POS   ID     REF  ALT  QUAL  FILTER   INFO
7       100   rs99   A    G    60.0  LowQual  DP=10;AF=0.5000
7       200   myid   C    T    30.0  PASS     DP=20;AF=0.2500
```

Round-trip **does not** preserve:

- INFO fields other than DP/AF (dropped on read — see limitation above)
- Multiple ALT alleles (only the first is kept)
- Sample/genotype columns (FORMAT + per-sample fields are ignored entirely)
- The original `##INFO`/`##FORMAT`/other meta header lines (a canonical header is
  re-emitted)

A projected round-trip (`SELECT … INTO vcf`) instead emits a custom-FORMAT VCF:
canonical columns fill the eight fixed fields and every other projected column is
declared and packed into INFO.

## Multi-sample status

**Single-record, first-allele only.** The reader takes the first ALT allele and
ignores FORMAT/sample columns. Multi-sample genotypes and multi-allelic sites are
**not** represented — split/normalize upstream if you need per-allele or
per-sample granularity.

## Summary closure table

| Capability | Status |
| --- | --- |
| Plain `.vcf` load | works |
| `.vcf.gz` load (in-Rust BGZF) | works |
| `WHERE` / `SELECT` / `ORDER BY` / `LIMIT` | works |
| Read CHROM/POS/ID/REF/ALT/QUAL/FILTER | works |
| Read INFO/DP, INFO/AF | works |
| Read arbitrary INFO fields | **not supported** |
| `INTO vcf` round-trip of identity + ID/FILTER + DP/AF | works |
| Multi-allelic / multi-sample / FORMAT | **not supported** |

Regression coverage: `crates/codonsplice-core/tests/from_vcf_tests.rs`
(fixtures in `crates/codonsplice-core/tests/data/`).
