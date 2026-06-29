# CodonSplice / SpliceQL

**A SQL-like query language over genomic files, on a self-contained engine.**
Write [SpliceQL](https://github.com/Pogo-Bash/spliceql), point it at a BAM/VCF,
and get variants, coverage, somatic calls, or annotated records back — natively,
embedded, or in the browser via WebAssembly, with **no external runtime on the
query path**.

```sql
FROM bam "tumor.bam"
WHERE chr = "7" AND pos >= 55000000 AND pos <= 55300000 AND depth > 30
CALL variants
WITH min_af = 0.05, min_base_quality = 20, reference = "chr7.fa"
INTO vcf "egfr.vcf"
```

```sh
$ splice query "$(cat egfr.spq)"
wrote 274 record(s) to egfr.vcf (vcf)
```

CodonSplice is the **engine**; SpliceQL is the **language**. They are developed as
separate crates with a hard boundary:

```text
spliceql  (language)        →   codonsplice  (engine)
Lexer → Parser → AST        →   Compiler → Bytecode → VM → cnvlens-core
```

`spliceql` turns source into an AST. `codonsplice-core` compiles that AST to a
compact stack-machine bytecode and executes it against real genomic data via
[`cnvlens-core`](https://github.com/Pogo-Bash/cnvlens). The `splice` binary wraps
both in a CLI + TUI, and the same engine compiles to WebAssembly for the browser.

---

## What it is — and what it is not

SpliceQL is **not a bcftools replacement**. It is a **common-80% engine** with
**verified parity** on the operations it covers, plus reach that bcftools
structurally lacks:

- **Verified parity** on the common path — variant calling, VCF set operations,
  multi-allelic normalization, tumor/normal somatic pairing, and annotation —
  each checked against a named oracle (see [Verified parity](#verified-parity)).
- **Reach bcftools lacks** — the whole engine runs **in the browser** and
  **embedded**, with no external runtime on the query path, and adds
  **parallel CNV calling**.

Use SpliceQL for those. Reach for bcftools/samtools for the long tail of the
full VCF/BCF surface. The project's premise is **verified honesty**: every parity
claim names its oracle, and every scope limit is stated plainly below.

---

## Headline: one query vs. a bcftools pipeline

A common task: take a tumor and a normal VCF, keep the tumor-private (somatic)
variants, annotate them with gene, HGVS, and ClinVar significance, then filter to
the pathogenic ones. In SpliceQL that is a single declarative query:

```sql
FROM vcf "tumor.vcf.gz" PAIRED WITH vcf "normal.vcf.gz"
CALL variants WITH reference = "chr7.fa"
ANNOTATE WITH genes = "refGene.gff3", clinvar = "clinvar.vcf.gz"
WHERE clinvar_significance = "Pathogenic"
SELECT chrom, pos, gene, aa_change, clinvar_significance
INTO vcf "somatic_pathogenic.vcf"
```

The equivalent bcftools route is roughly nine steps of bgzip / tabix / isec /
csq / annotate, with intermediate files at each stage:

```sh
# normalize + index both inputs
bgzip -c tumor.vcf  > tumor.vcf.gz   && tabix -p vcf tumor.vcf.gz
bgzip -c normal.vcf > normal.vcf.gz  && tabix -p vcf normal.vcf.gz
# tumor-private (somatic) set
bcftools isec -p isec_out tumor.vcf.gz normal.vcf.gz
bgzip -c isec_out/0000.vcf > somatic.vcf.gz && tabix -p vcf somatic.vcf.gz
# HGVS / consequence, then ClinVar significance
bcftools csq -f chr7.fa -g refGene.gff3 somatic.vcf.gz -Oz -o csq.vcf.gz && tabix -p vcf csq.vcf.gz
bcftools annotate -a clinvar.vcf.gz -c INFO/CLNSIG csq.vcf.gz -Oz -o ann.vcf.gz
# filter to pathogenic
bcftools view -i 'INFO/CLNSIG="Pathogenic"' ann.vcf.gz -o somatic_pathogenic.vcf
```

Same answer. The SpliceQL somatic set is verified byte-identical to the bcftools
`isec` private-A partition.

---

## Verified parity

Each claim is backed by a test that compares SpliceQL's output to a named oracle.

| Operation | Oracle & result |
| --- | --- |
| **variant calling** | Differential vs the **GIAB** truth set and **samtools/bcftools** on NA12878 — concordance is *measured*, not assumed by construction. |
| **ISEC / set ops** | Byte-identical to **bcftools `isec`** partitions (`0000`–`0003`) on an exact `(chrom, pos, ref, alt)` key; live-bcftools differential test. |
| **PAIRED WITH (somatic)** | Somatic set byte-identical to **bcftools `isec`** private-A; germline to the shared partition. |
| **SPLIT (multi-allelic)** | Record-set identical to **bcftools `norm -m -`**, with per-allele AF apportioned and indels left-trimmed. |
| **ANNOTATE (HGVS)** | EGFR **L858R → p.Leu858Arg** / `c.2573T>G`, *derived from the genetic code* and verified against the real chr7 reference (forward strand). |
| **parallel CNV** | Byte-identical to the **serial** caller across 2–8 shards, with a positive control: one amplification spanning a shard boundary, emitted as a **single** call. |

---

## Honest scope limits

Stated plainly, not buried:

- **A common-80% engine, not the full bcftools/BCF surface.**
- **CNV amplification sensitivity is unvalidated on real tumors.** Correctness is
  proven (no false calls on flat diploid); a sensitivity number awaits an
  amplified-tumor truth set.
- **HGVS is verified on the forward strand (EGFR) only.** Reverse-strand output
  is implemented but not yet verified end-to-end against a reference.
- **BAQ (base alignment quality) is not implemented** — this accounts for the
  residual precision-margin gap vs bcftools.
- **`INTO bam` / `cram` are unsupported sinks**; CRAM input is planned.

---

## Install

`spliceql` and `cnvlens` are git submodules, so build from source with a
recursive clone:

```sh
git clone --recursive https://github.com/Pogo-Bash/codonsplice
cd codonsplice
# if you forgot --recursive:
git submodule update --init --recursive

cargo build --release            # binary at target/release/splice
# …or install it onto your PATH:
cargo install --path crates/splice-cli
```

Prebuilt-binary installers (a `curl | sh` script, an `@codonsplice/cli` npm
package, and a Windows PowerShell one-liner) pull the matching platform binary
when published; see the project's releases page. `splice update` self-updates;
`splice uninstall` removes it.

---

## The language

A query is a `FROM` clause followed by any of the optional clauses below, in any
order (`FROM` must be first):

| Clause | Purpose | Example |
| --- | --- | --- |
| `FROM <fmt> <path>` | the input source (required) | `FROM bam "x.bam"` |
| `SELECT <expr> [AS name], …` | project columns (omit for whole records) | `SELECT chr, pos, depth` |
| `WHERE <expr>` | per-record predicate | `WHERE chr = "7" AND depth > 30` |
| `CALL <op>` | the genomic operation to run | `CALL variants` |
| `WITH <key> = <val>, …` | tune the `CALL` | `WITH min_af = 0.05` |
| `ORDER BY <expr> [ASC\|DESC], …` | sort the results | `ORDER BY depth DESC` |
| `LIMIT <n>` | cap the row count | `LIMIT 100` |
| `INTO <fmt> <path>` | write results to a file | `INTO vcf "out.vcf"` |
| `ISEC <fmt> <path> [MODE …]` | two-input VCF set operation | `FROM vcf "a" ISEC vcf "b" MODE shared` |
| `PAIRED WITH <fmt> <path> [MODE …]` | tumor/normal somatic pairing | `FROM vcf "t" PAIRED WITH vcf "n"` |
| `SPLIT` | normalize multi-allelic records | `FROM vcf "x" SPLIT` |
| `ANNOTATE WITH <key> = <path>, …` | join local gene / ClinVar / HGVS | `ANNOTATE WITH genes = "g.gff3"` |

### Sources & sinks

| Format | `FROM` (input) | `INTO` (output) |
| --- | --- | --- |
| `bam` | yes (with `.bai` region seek) | — |
| `vcf` | yes | yes |
| `bed` | yes | yes |
| `fasta` | yes | yes (JSON array, legacy) |
| `json` | — | yes (NDJSON, one object per record) |
| `tsv` | — | yes (header row + tab-separated values) |
| `cram` | planned | — |

`FROM bam` with a `chr`/`pos` range in `WHERE` is recognized at compile time and
turned into a BAI-indexed region seek instead of a full scan.

### Operations (`CALL`) and their `WITH` parameters

| Operation | Parameters |
| --- | --- |
| `variants` | `min_depth`, `min_base_quality`, `min_mapping_quality`, `min_variant_reads`, `min_allele_freq` (alias `min_af`), `min_strand_bias`, `reference` |
| `cnv` / `coverage` | `window_size`, `amp_threshold`, `del_threshold`, `min_windows`, `segmentation_method` |
| `reads` | *(none)* |
| `header` | *(none)* |

An unknown parameter is a compile error with a "did you mean" hint (Levenshtein +
shared-token ranking over the known names).

### Functions

Functions are implemented and run at execution time (they are no longer no-ops).
Usable in `WHERE` / `SELECT` / `ORDER BY`; unknown names, wrong arity, and
string-arg type mismatches are caught at `splice check`.

- **scalar / math** — `abs`, `round(x[, n])`, `floor`, `ceil`, `sqrt`,
  `pow(b, e)`, `log(x[, base])`, `min(…)`, `max(…)`, `coalesce(…)`
- **string** — `len`, `upper`, `lower`, `concat(…)`, `contains(s, sub)`,
  `starts_with` / `ends_with`, `substr(s, start[, len])`
- **genomic** — `gc(seq)`, `revcomp(seq)`, `translate(seq[, frame])` (NCBI table
  1, `*` = stop), `codon_at(seq, i)`

```sql
FROM bam "tumor.bam"
WHERE chr = "7" AND abs(af - 0.5) < 0.1          -- functions in WHERE
CALL variants
SELECT chrom, pos, ref, alt,
       gc(ref) AS gc,                            -- genomic fns as columns
       revcomp(alt) AS alt_rc
```

### Fields (usable in `WHERE` / `SELECT` / `ORDER BY`)

- **variants**: `chr`/`chrom`, `pos`, `ref`, `alt`, `qual`, `depth`, `ref_count`,
  `alt_count`, `af`/`allele_freq`, `strand_bias`, `kind`, `filter`, `id`
- **reads**: `chr`/`chrom`, `pos`, `mapq`, `flag`, `depth`, `strand`,
  `is_reverse`, `is_duplicate`, `is_secondary`
- **coverage windows**: `chrom`, `start`, `end`, `coverage`, `normalized`
- **annotation** (added by `ANNOTATE WITH`): `gene`, `transcript`, `exon`,
  `exon_id`, `region`, `consequence`, `aa_change`, `hgvs_c`,
  `clinvar_significance`, `clinvar_oncogenic`, `clinvar_id`, `rsid`

---

## Somatic, set operations & annotation

### PAIRED WITH — tumor / normal somatic

`FROM vcf "tumor" PAIRED WITH vcf "normal"` keeps variants present in the tumor
but not the normal (default `MODE somatic`); `MODE germline` keeps the shared
set. The match key is the exact `(chrom, pos, ref, alt)` tuple — the same engine
as `ISEC`.

```sql
FROM vcf "tumor.vcf.gz" PAIRED WITH vcf "normal.vcf.gz" MODE somatic
INTO vcf "somatic.vcf"
```

### ISEC — VCF set operations

`FROM vcf "a" ISEC vcf "b" MODE …` computes a two-input set operation with
bcftools `isec` semantics: `private_a` / `private_b` (records unique to one
input), `shared` / `shared_b` (intersection, taking A's or B's record), or
`union`.

```sql
FROM vcf "a.vcf.gz" ISEC vcf "b.vcf.gz" MODE private_a
INTO vcf "only_in_a.vcf"
```

### SPLIT — multi-allelic normalization

`SPLIT` decomposes multi-allelic records (comma-separated ALTs) into one
biallelic record per ALT, with per-allele AF apportioned and indels left-trimmed
— the semantics of `bcftools norm -m -`.

```sql
FROM vcf "multiallelic.vcf" SPLIT
CALL variants
INTO vcf "biallelic.vcf"
```

### ANNOTATE WITH — gene, ClinVar, HGVS (all local files)

`ANNOTATE WITH genes = "…", clinvar = "…"` joins each variant against local
annotation databases — a GFF3 gene model and a ClinVar VCF. **Every source is a
local file; there are no live API calls**, so annotation works offline and in the
browser (privacy, and a requirement for WASM).

```sql
FROM vcf "egfr.vcf"
CALL variants WITH reference = "chr7.fa"
ANNOTATE WITH genes = "refGene.gff3", clinvar = "clinvar.vcf.gz"
SELECT chrom, pos, gene, exon, aa_change, hgvs_c, clinvar_significance
INTO vcf "annotated.vcf"
```

> **Note.** `ANNOTATE WITH` accepts only `genes` and `clinvar`. The reference for
> HGVS is supplied on the `CALL` clause — `CALL variants WITH reference = "…"` —
> and the annotator picks it up from there.

**Looked-up vs. computed.** ClinVar significance is a *lookup* — a variant gets a
clinical interpretation only if it is already in the ClinVar file. HGVS is the
opposite: `aa_change` (e.g. `p.Leu858Arg`) and `hgvs_c` (e.g. `c.2573T>G`) are
*derived* from the genetic code and the reference codon, so they are produced
even for novel variants never seen before.

### Referencing — `WITH reference`, and why it matters

Pass a reference FASTA with `CALL variants WITH reference = "chr7.fa"`. It is what
makes `REF` the *actual* reference base at each position. Without it, `REF` is
inferred as the pileup-majority base — a coin-flip at balanced heterozygous
sites, and **invisible** for homozygous variants (where nearly every read differs
from the reference). A reference is therefore **required** to call indels and
homozygous variants at all, for valid VCF, for truth-set concordance, for indel
normalization, and for HGVS codon translation. FASTA contig names must match the
input (e.g. `>7` ↔ `7`).

---

## `.spq` scripts

A `.spq` file is a reusable, parameterized query with a typed CLI interface
declared in `--` directives:

```sql
#!/usr/bin/env splice
-- @name: egfr-variant-caller
-- @input: bam required "Input BAM file"
-- @input: min_af optional float 0.05 "Minimum allele frequency"
-- @output: vcf "Variant calls"

FROM bam $bam
WHERE chr = "7" AND pos >= 55000000 AND pos <= 55300000 AND depth > 30
CALL variants
WITH min_af = $min_af, min_depth = 10
INTO vcf $output
```

`$name` template variables bind from `--flag value` arguments at run time:

```sh
splice new caller                                 # scaffold caller.spq
splice run caller.spq --bam tumor.bam --output out.vcf --min-af 0.03
splice build caller.spq --release                 # → self-contained ./caller binary
./caller --bam tumor.bam --output out.vcf         # same flags as `run`
```

`splice build` produces a ~22 MB self-contained native binary (or `--wasm` for a
`.wasm` module). Flags: `-o <name>`, `--release`, `--target <triple>`, `--wasm`.

---

## The CLI

```text
splice                          launch the interactive TUI
splice query   "FROM bam …"     compile + run a one-liner
splice compile "FROM bam …"     compile + print disassembled bytecode
splice check   "FROM bam …"     parse + type-check only, no execution
splice new     <name>           scaffold <name>.spq
splice run     <file.spq> …     run a script, binding $vars from --flag value
splice build   <file.spq> …     compile a script to a native binary or .wasm
splice create  [framework] …    scaffold a web app wired to the WASM engine
splice update | uninstall       self-update / remove the binary
```

> Scripts run via `splice run <file.spq>` (or one-liners via
> `splice query "…"`); there is no inline `-e` flag.

`splice compile` shows exactly what the VM runs — pipeline opcodes inline, with
per-record sub-programs (the `WHERE` predicate, `SELECT` items, `ORDER BY` keys)
appended after `HALT`.

### The TUI

Launching `splice` with no subcommand opens a three-pane educational editor
(query on the left; bytecode / results / errors on the right).

| Key | Action |
| --- | --- |
| `Ctrl+Enter` / `F5` | compile + run the current query |
| `Ctrl+D` | disassemble bytecode |
| `Ctrl+A` | pretty-print the parsed AST |
| `Tab` | switch focus between editor and output |
| `F1` | toggle the keybindings help |
| `Ctrl+Q` | quit |

---

## Browser / npm

CodonSplice compiles to WebAssembly and runs entirely client-side — no server, no
genomic data leaving the browser. The fastest start is the scaffolder:

```sh
splice create                  # interactive menu — react / vue / svelte / astro
splice create react my-app     # …or non-interactively
```

It generates a Vite/Astro app pre-wired to `@codonsplice/wasm` with a live
SpliceQL playground (type-checks and compiles to bytecode as you type) plus a BAM
upload to run a real query. To wire it up by hand, the ergonomic helpers live at
`@codonsplice/wasm/helpers`:

```js
import { execute, compile, check } from "@codonsplice/wasm/helpers";

const result = await execute({
  query: 'FROM bam "sample.bam" WHERE chr = "7" CALL variants',
  files: { "sample.bam": bamBytes },   // name → File | ArrayBuffer | Uint8Array
});
```

Framework wrappers add idiomatic state (`useSpliceQL` for react/vue,
`createSpliceQL` for svelte) and re-export the core tooling, so an app imports
everything from one package — `@codonsplice/{react,vue,svelte,astro}`. The worker
needs cross-origin isolation (`Cross-Origin-Opener-Policy: same-origin`,
`Cross-Origin-Embedder-Policy: require-corp`). See
[docs/NPM_PACKAGE.md](docs/NPM_PACKAGE.md).

A live, in-browser playground for all of the above is at
[swapdoesbioandis-a.dev/splice](https://swapdoesbioandis-a.dev/splice).

---

## Workspace layout

```text
codonsplice/
├── crates/
│   ├── spliceql/           the language: lexer + parser + AST  (submodule, read-only path dep)
│   ├── codonsplice-core/   compiler + bytecode + VM + execution + annotation + HGVS + materialization
│   ├── splice-cli/         the `splice` binary: CLI + ratatui TUI + .spq + installer
│   ├── codonsplice-wasm/   wasm32 bindings (wasm-bindgen cdylib)
│   └── spliceql-grammar/   TextMate grammar + Linguist assets + VS Code manifest
├── cnvlens/                cnvlens-core: BAM/VCF readers, pileup, variant/coverage/CNV callers (submodule)
├── pkg/                    built WASM + npm packages (@codonsplice/*)
├── scripts/                install.sh, build-wasm.sh, build-cli-packages.sh
├── templates/              `splice build` Cargo project template
└── docs/                   per-phase API + design docs
```

`spliceql` and `cnvlens` are git submodules — clone with `git clone --recursive`,
or run `git submodule update --init --recursive`.

### Build & test

```sh
cargo test  --workspace          # compiler / VM / disassembler / execution / parity tests
cargo clippy --workspace
cargo run   -p splice-cli        # launch the TUI
bash scripts/build-wasm.sh       # build the WASM npm package
```

---

## How it runs (engine internals)

`codonsplice-core` is three layers — see [docs/PHASE4_API.md](docs/PHASE4_API.md)
and [docs/PHASE5_API.md](docs/PHASE5_API.md) for the full as-built API.

1. **Compile** — AST → `Program { consts, code, debug, region }`. `extract_region`
   statically lifts a `chr/pos` `WHERE` into a `Region` for BAI seeking.
2. **Execute** (`Vm::run`) — a stack machine walks the bytecode: `OPEN_SOURCE`
   builds a `Dataset`, `SCAN` wraps a `Cursor`, `FILTER`/`SET_PARAM`/`CALL_*`
   configure it, and `materialize` applies the `WHERE` predicate, `SELECT`
   projection, `ORDER BY` sort, and `LIMIT` to produce the record stream.
3. **Serialize** (`WRITE_INTO`) — records → VCF / BED / JSON / TSV bytes via the
   `Io` trait (filesystem natively; an in-memory map in WASM).

A `Program` serializes to a compact `.spq.bc` (`Program::to_bytes` /
`from_bytes`) — the format compiled binaries embed.

---

## Editor support

`crates/spliceql-grammar` ships a TextMate grammar (scope `source.spq`), a VS
Code manifest, and GitHub Linguist assets so `.spq` files highlight on GitHub and
in editors.

---

## License

MIT.
</content>
</invoke>
