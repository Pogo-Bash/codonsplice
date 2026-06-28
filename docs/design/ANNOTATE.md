# ANNOTATE clause — design

Track 1. Joins variant records against **local** downloaded databases by genomic
position, attaching gene / exon / consequence / clinical fields that downstream
`SELECT` and `WHERE` can see. All joins are local file reads — no live APIs (the
WASM/offline thesis).

Status: design agreed, implemented on `feat/annotate` (codonsplice-core) +
`feat/annotate` branch in the `spliceql` submodule.

## Syntax

```
ANNOTATE WITH genes="EGFR_region.GRCh37.gff3", clinvar="clinvar_GRCh37_EGFR.vcf.gz"
```

* `ANNOTATE WITH key="path", key="path", ...` — a comma-separated list of
  `key = string-path` pairs, mirroring the existing `WITH` clause shape so the
  language stays uniform. Paths may also be `$vars` (resolved at runtime), same
  as `FROM`/`INTO`.
* Recognised keys: `genes` (a GFF3 gene model) and `clinvar` (a ClinVar VCF,
  plain or BGZF). Either may be omitted; an unknown key is a compile error with
  a "did you mean" suggestion. At least one key is required.
* **Clause position**: order-independent, like every clause after `FROM`
  (the parser dispatches clauses in a loop). Conventionally written after
  `CALL` / before `SELECT`/`WHERE`, but position does not matter — annotation is
  applied to every record before the predicate and projection run.
* `ANNOTATE` is distinct from `WITH`: `WITH` tunes the upcoming `CALL`
  (min_depth, reference, …); `ANNOTATE WITH` names annotation databases. Keeping
  them separate avoids overloading `WITH`'s CALL-parameter namespace.

## Closure preservation

Variant records in → variant records out, enriched with annotation columns:

| column                 | source                                             |
|------------------------|----------------------------------------------------|
| `gene`                 | GFF `gene` feature `Name=` whose interval covers POS|
| `transcript`           | chosen overlapping transcript id (canonical-first)  |
| `exon`                 | overlapping exon `rank=` in the chosen transcript   |
| `exon_id`              | overlapping exon `exon_id=`                          |
| `region`               | `exon` if an exon covers POS, else `intron` / `.`   |
| `consequence`          | ClinVar `MC=` molecular consequence (so/term)       |
| `clinvar_significance` | ClinVar `CLNSIG=` (falls back to `ONC=`)            |
| `clinvar_oncogenic`    | ClinVar `ONC=`                                       |
| `clinvar_id`           | ClinVar `ID` column                                 |
| `rsid`                 | `rs` + ClinVar `RS=`                                |

Every column is always present; a missing join yields `"."` (not null) so the
documented `WHERE clinvar_significance != "."` filter behaves predictably.

The enriched record is a new `Record::AnnotatedVariant { variant, annotations }`
kind. `get_field` resolves annotation columns first, then delegates to the
inner `Variant` — so the variant's own columns (chrom/pos/ref/alt/af/…) keep
working and the annotation columns are additive. `into_row` / JSON / VCF output
append the annotation columns (VCF puts them in INFO).

This works identically on `FROM vcf` input and freshly-called BAM variants,
because annotation runs in `materialize` on the produced `Record::Variant`
stream regardless of where the variants came from (VCF passthrough or pileup).

## Join semantics

* **Gene model (GFF) — coordinate overlap, INCLUSIVE boundaries.** A variant at
  POS matches feature `[start, end]` when `start <= POS <= end`. A variant
  sitting exactly on an exon boundary is *in* the exon.
  * gene: the `gene` feature whose interval covers POS (`Name=`).
  * exon: among all `exon` features covering POS, pick the one whose parent
    transcript ranks highest by **(has CCDS id, then longest mRNA span, then
    lexicographically smallest transcript id)**. This deterministically selects
    the canonical coding transcript — for EGFR that is `ENST00000275493`
    (CCDS5514.1), whose exon 21 covers the L858R site. Report that exon's
    `rank=` as `exon` and its `exon_id`.
  * `region` = `exon` when an exon covers POS, else `intron` when only the gene
    body covers it, else `.`.
* **ClinVar (VCF) — exact position + REF + ALT match.** Key is
  `(CHROM, POS, REF, ALT)`. Significance comes from `CLNSIG`; when `CLNSIG` is
  absent but `ONC` (oncogenicity) is present, `ONC` is used so somatic/oncogenic
  records (like L858R, `CLNSIG=drug_response` + `ONC=Oncogenic`) are not dropped.
* All joins are **local**: GFF and ClinVar are read through the `Io` trait
  (same path as `FROM`/reference FASTA), so they work in the native CLI and,
  with a WASM `Io` backend, offline in the browser. No network.

## aa_change / HGVS — honest scope

`p.Leu858Arg` requires codon-aware translation (CDS phase, strand, ref base →
amino acid). The GFF slice has CDS features but full HGVS protein notation is
out of scope for this track. `aa_change` is **not** emitted; `consequence`
(ClinVar `MC=`, e.g. `missense_variant`) is provided instead. HGVS p. is future
work and is documented as such rather than approximated.

## Implementation

1. **Grammar (`spliceql` submodule, branch `feat/annotate`)**: `Annotate`
   token + `match_kw_8`, `Query.annotate: Option<AnnotateClause>`,
   `AnnotateClause { params: Vec<(String, Expr)>, span }`, `parse_annotate`
   wired into the clause loop. Mirrors `parse_with`.
2. **Annotator (`codonsplice-core::annotate`)**: parses GFF intervals (genes /
   transcripts / exons) and ClinVar (`(chrom,pos,ref,alt) → fields`) from byte
   buffers (BGZF transparently inflated via `flate2::MultiGzDecoder`, which reads
   concatenated BGZF blocks). `annotate(&Variant) -> Vec<(String, RuntimeValue)>`.
3. **VM**: a new `Annotate` opcode reads the `genes`/`clinvar` paths (stashed as
   reserved `SET_PARAM` keys `__annotate_genes` / `__annotate_clinvar`), loads the
   files via `Io`, builds the `Annotator`, and stores it on the `Cursor`.
4. **materialize**: before the predicate, map each `Record::Variant` through the
   annotator into `Record::AnnotatedVariant` so `WHERE`/`SELECT`/`ORDER BY` and
   the output serializers all see the annotation columns.
5. **Compiler**: `validate_where_fields` admits the annotation column names when
   `ANNOTATE` is present (otherwise `WHERE clinvar_significance` is rejected as an
   unknown variant field).

## Rejected alternatives

* **Adding an `annotations` map to `cnvlens_core::Variant`.** `cnvlens` is a git
  submodule; mutating its model couples this feature to a second submodule bump
  and to cnvlens's own release cadence. A codonsplice-core-local
  `Record::AnnotatedVariant` keeps the change in one crate. (If a *generic* INFO
  map were ever needed on the input side, that would be the cnvlens-core change
  — flagged, not taken here.)
* **Reusing `WITH` for database paths.** Overloads the CALL-parameter namespace
  and makes `min_depth` and `clinvar` siblings; semantically muddy. A dedicated
  `ANNOTATE WITH` reads clearly and validates against its own key set.
* **Reading ClinVar via cnvlens-core `stream_vcf`.** That reader only extracts
  DP/AF from INFO and discards CLNSIG/ONC/RS/MC — exactly the fields ANNOTATE
  needs. A purpose-built INFO parser in the annotator is required.
* **Live API lookups (Ensembl VEP / MyVariant).** Violates the offline/WASM
  thesis. Everything is a local sliced database.
* **Computing `aa_change` (HGVS p.)** — deferred; see "honest scope" above.
* **Annotating in the VM opcode itself (eagerly).** Annotation must see the
  produced record stream, which for variant calling is deferred to materialize
  (LIMIT short-circuit). So the opcode only *builds* the annotator; application
  happens in materialize.
