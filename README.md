# CodonSplice

**A small, SQL-like query language for genomic files — compiled to bytecode and
run on a stack VM.** Write [SpliceQL](https://github.com/Pogo-Bash/spliceql),
point it at a BAM/VCF, and get variants, coverage, or reads back.

```sql
FROM bam "tumor.bam"
WHERE chr = "7" AND pos >= 55000000 AND pos <= 55300000 AND depth > 30
CALL variants
WITH min_af = 0.05, min_base_quality = 20
INTO vcf "egfr.vcf"
```

```sh
$ splice run egfr.spq --bam tumor.bam --output egfr.vcf
wrote 274 record(s) to egfr.vcf (vcf)
```

CodonSplice is the **engine**; SpliceQL is the **language**. The two are
developed as separate crates with a hard boundary:

```text
spliceql  (language)        →   codonsplice  (engine)
Lexer → Parser → AST        →   Compiler → Bytecode → VM → cnvlens-core
```

`spliceql` turns source into an AST. `codonsplice-core` compiles that AST to a
compact stack-machine bytecode and executes it against real genomic data via
[`cnvlens-core`](https://github.com/Pogo-Bash/cnvlens). The `splice` binary wraps
both in a CLI + TUI.

---

## Install

**Prebuilt binaries are the primary path** — Linux (x86_64/aarch64), macOS
(Apple Silicon **and** Intel), and Windows all ship a native `splice` binary, so
no Rust toolchain is needed.

```sh
# Guided installer (Linux / macOS, incl. Apple Silicon) — downloads the
# right prebuilt binary for your OS/arch; installs to ~/.local/bin (no sudo).
curl -fsSL https://github.com/Pogo-Bash/codonsplice/releases/latest/download/install.sh | sh

# npm (cross-platform; pulls the matching platform binary, incl. darwin-arm64)
npm install -g @codonsplice/cli

# Windows (winget)
winget install Pogo-Bash.CodonSplice
```

> **Build from source — fallback only.** If no prebuilt binary fits your
> platform, build it yourself. Run this as your **normal user, never under
> `sudo`/root**, or a root-owned `~/.cargo`/`target/` will break later builds:
>
> ```sh
> cargo install --git https://github.com/Pogo-Bash/codonsplice splice-cli
> ```

`splice update` self-updates to the latest release; `splice uninstall` removes
it. Current release: **v0.2.5**.

---

## The language

A query is a `FROM` clause followed by any of the optional clauses below, in any
order (`FROM` must be first):

| Clause | Purpose | Example |
| --- | --- | --- |
| `FROM <fmt> <path>` | the input source (required) | `FROM bam "x.bam"` |
| `SELECT <expr> [AS name], …` | project columns (omit for whole records) | `SELECT chr, pos, depth * af AS alt_reads` |
| `WHERE <expr>` | per-record predicate | `WHERE chr = "7" AND depth > 30` |
| `CALL <op>` | the genomic operation to run | `CALL variants` |
| `WITH <key> = <val>, …` | tune the `CALL` | `WITH min_af = 0.05` |
| `ORDER BY <expr> [ASC\|DESC], …` | sort the results | `ORDER BY depth DESC` |
| `LIMIT <n>` | cap the row count | `LIMIT 100` |
| `INTO <fmt> <path>` | write results to a file | `INTO vcf "out.vcf"` |

### Sources & sinks

| Format | `FROM` (input) | `INTO` (output) |
| --- | --- | --- |
| `bam` | ✅ (with `.bai` region seek) | — |
| `vcf` | ✅ | ✅ (native, or custom-FORMAT for projections) |
| `bed` | ✅ | ✅ |
| `fasta` | ✅ | ✅ — repurposed as the **JSON** sink (no `json` token yet) |
| `cram` | planned (Phase 6) | — |

`FROM bam` with a `WHERE chr = … AND pos >= … AND pos <= …` is recognized at
compile time and turned into a BAI-indexed region seek instead of a full scan.

### Operations (`CALL`) and their `WITH` parameters

| Operation | Parameters |
| --- | --- |
| `variants` | `min_depth`, `min_base_quality`, `min_mapping_quality`, `min_variant_reads`, `min_allele_freq` (alias `min_af`), `min_strand_bias` |
| `cnv` / `coverage` | `window_size`, `amp_threshold`, `del_threshold`, `min_windows`, `segmentation_method` |
| `reads` | *(none)* |
| `header` | *(none)* |

An unknown parameter is a compile error with a "did you mean" hint (Levenshtein +
shared-token ranking over the known names):

```sh
$ splice check 'FROM bam "x.bam" CALL variants WITH min_freq = 0.05'
error[E001]: unknown parameter "min_freq"
  --> query:1:48
   |
 1 | FROM bam "x.bam" CALL variants WITH min_freq = 0.05
   |                                                ^^^^ did you mean "min_allele_freq"?
```

### Fields (usable in `WHERE` / `SELECT` / `ORDER BY`)

- **variants**: `chr`/`chrom`, `pos`, `ref`, `alt`, `qual`, `depth`, `ref_count`,
  `alt_count`, `af`/`allele_freq`, `strand_bias`, `kind`, `filter`, `id`
- **reads**: `chr`/`chrom`, `pos`, `mapq`, `flag`, `depth`, `strand`,
  `is_reverse`, `is_duplicate`, `is_secondary`
- **coverage windows**: `chrom`, `start`, `end`, `coverage`, `normalized`

### Expressions

Arithmetic (`+ - * /`), comparison (`= != < > <= >=`), boolean
(`AND` / `OR` with short-circuit jumps, `NOT`), grouping with parentheses,
dotted field access (`reads.depth`), string subscript (`info["DP"]`), and the
`*` wildcard. Function calls parse and compile (`abs(af - 0.5)`) but
see [Known limitations](#known-limitations).

---

## `.spq` scripts

A `.spq` file is a reusable, parameterized query with a typed CLI interface
declared in `--` directives:

```sql
#!/usr/bin/env splice
-- @name: egfr-variant-caller
-- @version: 1.0.0
-- @description: Call variants in the EGFR gene region
-- @input: bam required "Input BAM file"
-- @input: min_af optional float 0.05 "Minimum allele frequency"
-- @output: vcf "Variant calls"

FROM bam $bam
WHERE chr = "7" AND pos >= 55000000 AND pos <= 55300000 AND depth > 30
CALL variants
WITH min_af = $min_af, min_depth = 10
INTO vcf $output
```

`$name` template variables are bound from `--flag value` arguments at run time:

```sh
splice new caller                                 # scaffold caller.spq
splice run caller.spq --bam tumor.bam --output out.vcf --min-af 0.03
splice build caller.spq --release                 # → self-contained ./caller binary
./caller --bam tumor.bam --output out.vcf         # same flags as `run`
```

`splice build` produces a ~22 MB self-contained native binary (or `--wasm` for a
`.wasm` module). Flags: `-o <name>`, `--release`, `--target <triple>`,
`--wasm`, `--emit-bc` (also write the `.spq.bc` bytecode).

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
splice create  [framework] …    scaffold a web app wired to splice-wasm (menu if no args)
splice update | uninstall       self-update / remove the binary
```

`splice compile` shows exactly what the VM runs — pipeline opcodes inline, with
per-record sub-programs (the `WHERE` predicate, `SELECT` items, `ORDER BY` keys)
appended after `HALT`:

```sh
$ splice compile 'FROM bam "x.bam" WHERE depth > 30 CALL variants WITH min_af = 0.05'
0000  OPEN_SOURCE  bam "x.bam"
0004  SCAN
0005  FILTER       pred@0012 len=8
000A  LOAD_CONST   0.05
000D  SET_PARAM    "min_af"
0010  CALL_VARIANTS
0011  HALT

; predicate @ 0012 (len 8)
0012  LOAD_FIELD   "depth"
0015  LOAD_CONST   30
0018  GT
0019  RET_PRED
```

### The TUI

Launching `splice` with no subcommand opens a three-pane educational editor:

```text
┌─────────────────────────────────────────────────┐
│  CodonSplice  │  SpliceQL query engine           │
├──────────────────────┬──────────────────────────┤
│  EDITOR              │  OUTPUT                  │
│  FROM bam "x.bam"    │  bytecode / results /    │
│  WHERE depth > 30    │  errors render here      │
│  CALL variants       │                          │
├──────────────────────┴──────────────────────────┤
│  spliceql (language)  →  codonsplice (engine)    │
└─────────────────────────────────────────────────┘
```

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

CodonSplice compiles to WebAssembly and runs entirely client-side — no server,
no genomic data leaving the browser. The fastest start is the scaffolder:

```sh
splice create                  # interactive menu — pick react / vue / svelte / astro
splice create react my-app     # …or non-interactively
```

It generates a Vite/Astro app pre-wired to `@codonsplice/wasm` with a live
SpliceQL playground — the query type-checks and compiles to bytecode as you
type, plus a BAM upload to run a real query.

To wire it up by hand, the ergonomic helpers live at `@codonsplice/wasm/helpers`:

```js
import { execute, compile, check } from "@codonsplice/wasm/helpers";

const result = await execute({
  query: 'FROM bam "sample.bam" WHERE chr = "7" CALL variants',
  files: { "sample.bam": bamBytes },   // name → File | ArrayBuffer | Uint8Array
});
```

Framework wrappers add idiomatic state (`useSpliceQL` for react/vue,
`createSpliceQL` for svelte) and re-export the core tooling, so an app imports
everything from one package:

```js
import { useSpliceQL, compile, check } from "@codonsplice/react";
```

`@codonsplice/{react,vue,svelte,astro}`. See [docs/NPM_PACKAGE.md](docs/NPM_PACKAGE.md).

---

## Workspace layout

```text
codonsplice/
├── crates/
│   ├── spliceql/           the language: lexer + parser + AST  (submodule, read-only path dep)
│   ├── codonsplice-core/   compiler + bytecode + VM + execution + materialization
│   ├── splice-cli/         the `splice` binary: CLI + ratatui TUI + .spq + installer
│   ├── codonsplice-wasm/   wasm32 bindings (wasm-bindgen cdylib)
│   └── spliceql-grammar/   TextMate grammar + Linguist assets + VS Code manifest
├── cnvlens/                cnvlens-core: BAM/VCF readers, pileup, variant/coverage callers (submodule)
├── pkg/                    built WASM + npm packages (@codonsplice/*)
├── scripts/                install.sh, build-wasm.sh, build-cli-packages.sh
├── templates/              `splice build` Cargo project template
└── docs/                   per-phase API + design docs
```

`spliceql` and `cnvlens` are git submodules — clone with
`git clone --recursive`, or run `git submodule update --init --recursive`.

### Build & test

```sh
cargo test  --workspace          # compiler / VM / disassembler / execution tests
cargo clippy --workspace
cargo run   -p splice-cli        # launch the TUI
bash scripts/build-wasm.sh       # build the WASM npm package
```

---

## How it runs (engine internals)

`codonsplice-core` is three layers — see [docs/PHASE4_API.md](docs/PHASE4_API.md)
and [docs/PHASE5_API.md](docs/PHASE5_API.md) for the full as-built API.

1. **Compile** (`compile`) — AST → `Program { consts, code, debug, region }`.
   `extract_region` statically lifts a `chr/pos` `WHERE` into a `Region` for BAI
   seeking.
2. **Execute** (`Vm::run`) — a stack machine walks the bytecode: `OPEN_SOURCE`
   builds a `Dataset`, `SCAN` wraps a `Cursor`, `FILTER`/`SET_PARAM`/`CALL_*`
   configure it, and `materialize` applies the `WHERE` predicate, `SELECT`
   projection, `ORDER BY` sort, and `LIMIT` to produce the record stream.
3. **Serialize** (`WRITE_INTO`) — records → VCF / BED / JSON bytes via the `Io`
   trait (filesystem natively; an in-memory map in WASM).

### Bytecode instruction set

| Range | Opcodes |
| --- | --- |
| `0x01–0x08` | `LOAD_CONST` `LOAD_TRUE` `LOAD_FALSE` `LOAD_FIELD` `GET_FIELD` `INDEX` `LOAD_WILDCARD` `LOAD_VAR` |
| `0x10–0x21` | `NEG` `NOT` `ADD` `SUB` `MUL` `DIV` `EQ` `NE` `LT` `GT` `LE` `GE` `AND` `OR` `CALL_FN` `RET_PRED` `JUMP_IF_FALSE` `JUMP` |
| `0x40–0x4F` | `OPEN_SOURCE` `SCAN` `FILTER` `PROJECT` `SET_PARAM` `ORDER_BY` `LIMIT` `WRITE_INTO` `HALT` |
| `0x50–0x54` | `CALL_VARIANTS` `CALL_CNV` `CALL_COVERAGE` `CALL_READS` `CALL_HEADER` |

A `Program` serializes to a compact `.spq.bc` (`Program::to_bytes` /
`from_bytes`) — the format compiled binaries embed.

---

## Editor support

`crates/spliceql-grammar` ships a TextMate grammar (scope `source.spq`), a VS
Code manifest, and GitHub Linguist assets so `.spq` files highlight on GitHub and
in editors.

---

## Roadmap

- **Phase 1** — spliceql lexer ✅
- **Phase 2** — spliceql parser + AST ✅
- **Phase 3** — compiler + VM + disassembler + CLI/TUI ✅
- **Phase 4** — [cnvlens-core execution bridge](docs/PHASE4_DESIGN.md) (real BAM/VCF execution, BAI seeking) ✅
- **Phase 5** — VCF input, `$variable` templating, `SELECT` projection, streaming, `.spq` files, compiled binaries, grammar crate ✅
- **Phase 6** — [indel calling, tumor/normal pairs, VCF annotation, CRAM support](docs/PHASE6_SCOPE.md) (planned)

---

## Known limitations

- **Builtin functions are not implemented.** `abs(...)` etc. compile to `CALL_FN`
  but evaluate to `null` at runtime
  ([#3](https://github.com/Pogo-Bash/codonsplice/issues/3)).
- **`INTO bam` / `cram`** are unsupported sinks (`UnsupportedInto`).
- **Compiled `.spq.bc` binaries** don't carry the static `region`, so they
  fall back to full-scan + predicate filtering (correct, just without the
  BAI-seek optimization).

---

## License

MIT.
