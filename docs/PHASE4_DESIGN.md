# Phase 4 — cnvlens-core execution bridge

Phase 3 compiles every SpliceQL query to bytecode and runs the expression
opcodes, but the pipeline opcodes (`OPEN_SOURCE … WRITE_INTO`, `CALL_*`) stop at
`VmError::NotYetImplemented`. Phase 4 wires those opcodes to
[`cnvlens-core`](https://github.com/Pogo-Bash/cnvlens) (`rust/cnvlens-core`) so a
query actually parses BAM/VCF/FASTA bytes and produces records.

This document specifies exactly how each pipeline opcode is implemented against
the existing cnvlens-core functions, how runtime handles are represented, how the
`WHERE` predicate runs per record, how BAI random access is wired up, and how
streaming replaces the current monolithic JSON return.

## 0. Dependency wiring

`codonsplice-core/Cargo.toml` gains:

```toml
[dependencies]
spliceql     = { path = "../spliceql" }
cnvlens-core = { path = "../../../cnvlens/rust/cnvlens-core" }  # or git dep
```

cnvlens-core already builds natively (its wasm-bindgen shim is gated to
`cfg(target_arch = "wasm32")`), so the native VM links the plain functions:

| Module | Function (native) |
| --- | --- |
| `bam` | `read_header(&[u8]) -> io::Result<Header>` |
| `bam` | `for_each_core(&[u8], FnMut(AlnRecord)) -> io::Result<u64>` |
| `bam` | `for_each_full(&[u8], FnMut(AlnRecord)) -> io::Result<u64>` |
| `coverage` | `analyze_coverage(&[u8], Option<&[u8]>, &CoverageOptions) -> serde_json::Value` |
| `variants` | `call_variants(&[u8], Option<&[u8]>, &VariantOptions) -> serde_json::Value` |
| `cnv` | `detect_cnvs_manual / _adaptive / _cbs_lite(...)` |

## 1. Runtime handles: `Dataset`, `Cursor`, `Record`

The Phase 3 placeholder `RuntimeValue::Pending` is replaced by three concrete
handle variants. Handles are reference-counted so the stack stays cheap to clone.

```rust
pub enum RuntimeValue {
    Int(i64), Float(f64), Str(Rc<str>), Bool(bool), Null,
    Dataset(Rc<Dataset>),     // an opened source (OPEN_SOURCE result)
    Cursor(Rc<RefCell<Cursor>>), // an active scan (SCAN result)
    Record(Rc<Record>),       // the current row inside a per-record sub-program
}

/// An opened, memory-resident source. Phase 4 loads the whole file into linear
/// memory (matching cnvlens-core, which operates on &[u8]); streaming I/O is a
/// later optimization.
pub struct Dataset {
    format: Format,           // Bam | Vcf | Fasta | Bed | Cram
    path:   Rc<str>,
    bytes:  Rc<[u8]>,         // file contents
    bai:    Option<Rc<[u8]>>, // sibling .bai/.csi index if present
    header: Option<Header>,   // SAM header for BAM (ref names ↔ ref_id)
    params: CallParams,       // accumulated SET_PARAM values
}

/// A lazy iterator over records of a Dataset. Variant per format keeps the
/// concrete reader (noodles BAM reader, VCF line reader, …) internal.
pub enum Cursor {
    Bam   { reader: BamScan, region: Option<Region> },
    Vcf   { reader: VcfScan },
    Fasta { reader: FastaScan },
}

/// One record exposed to the WHERE/SELECT/ORDER expression interpreter as a set
/// of named fields. Backed by an AlnRecord (BAM) or a parsed VCF/FASTA row.
pub struct Record {
    fields: RecordView,  // resolves names → RuntimeValue lazily
}
```

`OpenSource` pushes a `Dataset`; `Scan` consumes a `Dataset` and pushes a
`Cursor`. `CallParams` is a small struct mirroring `CoverageOptions` /
`VariantOptions` — `SET_PARAM` writes into it before the `CALL_*` opcode reads
it.

### Field resolution (`RecordView`)

The compiler already lowers `WHERE depth > 30` to
`LOAD_FIELD "depth"` / `LOAD_CONST 30` / `GT`. In Phase 4 `LOAD_FIELD name`
calls `record.fields.get(name)`:

| Field name | BAM source | VCF source |
| --- | --- | --- |
| `chr` / `chrom` | `header.reference_sequences()[ref_id]` | `CHROM` column |
| `pos` | `aln.pos` (0-based) | `POS` (1-based) |
| `mapq` | `aln.mapq` | — |
| `depth` | pileup depth at `pos` (computed by the scan) | `INFO/DP` |
| `qual` | mean base quality | `QUAL` column |
| `af` | `alt_count / depth` | `INFO/AF` |
| `strand` | `aln.is_reverse()` → `"-"`/`"+"` | — |
| `flag` | `aln.flag` | — |

`GET_FIELD` (`reads.mapq`) and `INDEX` (`info["DP"]`) resolve nested access the
same way. An unknown field resolves to `RuntimeValue::Null`, so predicates over
absent fields are well-defined (`Null` is falsey) rather than a crash.

## 2. Pipeline opcode implementations

### `OPEN_SOURCE(format, path_idx)`

```rust
OpCode::OpenSource => {
    let format = decode_format(self.read_u8());
    let path   = self.const_str(self.read_u16());
    let bytes  = self.io.read_file(&path)?;            // host file loader (CLI)
    let bai    = self.io.read_sibling_index(&path);    // .bai / .csi if present
    let header = if format == Format::Bam {
        Some(bam::read_header(&bytes).map_err(VmError::io)?)
    } else { None };
    self.push(RuntimeValue::Dataset(Rc::new(Dataset { format, path, bytes, bai, header, params: CallParams::default() })));
}
```

The VM is parameterized over an `Io` trait so the native CLI reads from the
filesystem and the WASM build reads from the JS-provided `files` map
(see [NPM_PACKAGE.md](NPM_PACKAGE.md)). No `std::fs` leaks into the core.

### `SCAN`

Pops the `Dataset`, builds the format-specific reader, pushes a `Cursor`. For
BAM this constructs a pileup-aware iterator (so `depth`/`af` fields are
available) using the same noodles reader cnvlens-core uses internally. If the
query carried a `WHERE chr = "chrN"` whose left operand is a constant chromosome
equality, the scan's `region` is set so BAI random access (§4) can skip blocks.

### `FILTER(pred_off, pred_len)`

The compiler emits the predicate as a sub-program after `HALT`. Phase 4 runs it
per record by saving the main `pc`, seeding the stack with the current `Record`,
and executing from `pred_off` until `RET_PRED`:

```rust
OpCode::Filter => {
    let (off, len) = (self.read_u16(), self.read_u16());
    let cursor = self.peek_cursor()?;
    cursor.borrow_mut().retain(|record| {
        self.run_subprogram(off, len, record)   // pushes Record fields, runs to RET_PRED
            .map(|v| v.is_truthy())
            .unwrap_or(false)
    });
}
```

`run_subprogram` reuses the **existing** Phase 3 expression interpreter
(`exec_expr`) unchanged — only `LOAD_FIELD`/`GET_FIELD`/`INDEX`/`LOAD_WILDCARD`
gain record-aware behavior. Jumps inside the predicate are already relative to
`off`, so the only change is seeding `pc = off` and bounding execution at
`off + len`. `PROJECT` (SELECT) and `ORDER_BY` run their sub-programs the same
way, per record / per key.

### `SET_PARAM(key_idx)` then `CALL_*`

`SET_PARAM` pops a constant and stores it into the `Dataset`/`Cursor`'s
`CallParams` under the interned key. The compiler has already type-checked and
coerced the value (Phase 3 `compile_with`), so the VM just assigns. `CALL_*`
materializes the cursor and dispatches to cnvlens-core:

```rust
OpCode::CallVariants => {
    let opts: VariantOptions = self.params.to_variant_options();
    let value = variants::call_variants(&ds.bytes, ds.bai.as_deref(), &opts);
    self.emit_records(value);     // stream rows out (see §5)
}
OpCode::CallCnv | OpCode::CallCoverage => {
    let opts: CoverageOptions = self.params.to_coverage_options();
    let value = coverage::analyze_coverage(&ds.bytes, ds.bai.as_deref(), &opts);
    self.emit_records(value);
}
OpCode::CallHeader => {
    let header = bam::read_header(&ds.bytes)?;
    self.output = VmOutput::Text(format_header(&header));
}
OpCode::CallReads => { /* stream AlnRecords through for_each_full, post-FILTER */ }
```

`CallParams::to_variant_options()` / `to_coverage_options()` map the WITH keys
exactly as the Phase 3 compiler validated them (e.g. `min_allele_freq → f64`,
`window_size → u32`, `segmentation_method → String`).

### `WRITE_INTO(format, path_idx)`

Consumes the record stream and serializes it in the target format:

```rust
OpCode::WriteInto => {
    let format = decode_format(self.read_u8());
    let path   = self.const_str(self.read_u16());
    let mut sink = self.io.create(&path)?;
    match format {
        Format::Vcf => write_vcf(&mut sink, &header, self.drain_records())?,
        Format::Bam => write_bam(&mut sink, &header, self.drain_records())?,
        Format::Bed => write_bed(&mut sink, self.drain_records())?,
        _ => return Err(VmError::unsupported_into(format)),
    }
}
```

When `INTO` is absent, `HALT` is reached with records still in the stream and the
VM returns them as `VmOutput::Records(...)` (a new variant) for the CLI to print
or the WASM layer to hand back to JS.

## 3. Putting it together — execution order

For `FROM bam "s.bam" WHERE depth > 30 CALL variants WITH min_allele_freq = 0.05`:

```text
OPEN_SOURCE bam "s.bam"   → Dataset{bytes, header}
SCAN                      → Cursor over pileup records
FILTER pred@… len=8       → cursor.retain(depth > 30)        [per-record subprog]
LOAD_CONST 0.05 / SET_PARAM "min_allele_freq"  → params.min_allele_freq = 0.05
CALL_VARIANTS             → variants::call_variants(bytes, bai, opts) → stream rows
HALT                      → VmOutput::Records(stream)
```

The compiler's existing opcode ordering already matches the execution semantics
(FILTER before CALL, SET_PARAM before CALL), so no bytecode layout changes are
needed — Phase 4 is purely the VM-side interpretation of opcodes that currently
return `NotYetImplemented`.

## 4. BAI / CSI random access

Today cnvlens-core accepts `bai: Option<&[u8]>` but ignores it (`_bai`) and does
a full sequential scan. Phase 4 fixes this where a query constrains the
chromosome:

1. `OPEN_SOURCE` loads the sibling `.bai` (BAM) or `.csi` index alongside the
   data file into `Dataset.bai`.
2. When `SCAN` sees a region predicate (a `WHERE` clause that includes
   `chr = "chrN"` and optionally `pos` bounds, recognized by a small analysis
   over the predicate sub-program), it resolves `chrN → ref_id` via the header
   and asks the index for the covering chunks.
3. cnvlens-core's pileup/variant functions gain a region-restricted entry point
   (`call_variants_region(bytes, index, region, opts)`); internally this uses
   noodles' `csi`/`bai` chunk offsets to seek the BGZF reader to the first block
   rather than scanning from byte 0.
4. Without a usable index or region constraint, behavior is unchanged (full
   scan) — random access is an optimization, never a correctness requirement.

This is the one change that reaches **into** cnvlens-core (adding region-aware
entry points and actually dereferencing the index). spliceql remains untouched.

## 5. Streaming output replaces monolithic JSON

cnvlens-core currently builds a whole `serde_json::Value` (all windows / all
variants) and returns it at once. For interactive queries and `LIMIT`, Phase 4
moves to a pull-based record stream:

- cnvlens-core grows callback/iterator variants, e.g.
  `call_variants_each(bytes, bai, opts, &mut FnMut(Variant) -> ControlFlow)`,
  built on the existing `for_each_full` scan it already uses.
- The VM wraps that in a `RecordStream` that `PROJECT`, `ORDER_BY`, `LIMIT`, and
  `WRITE_INTO` consume lazily. `LIMIT n` short-circuits the callback with
  `ControlFlow::Break` after `n` rows, so `... LIMIT 100` stops the pileup early
  instead of computing every variant.
- `ORDER_BY` is the one materializing operator (it must buffer to sort); it
  buffers only the projected keys + row handles, not full records.
- `VmOutput` gains `Records(RecordStream)`; the CLI prints rows as a table and
  the WASM `engine.stream({ onRecord, onDone })` API (see NPM_PACKAGE.md) forwards
  each record to JS as it is produced.

The monolithic `analyze_coverage`/`call_variants` JSON functions stay for the
existing CNVLens UI; the new streaming entry points are additive.

## 6. New/changed error variants

`VmError` gains data-plane errors that Phase 3 never needed:

```rust
Io(String),                    // file open / read / write failed
SourceFormat { expected, got },// e.g. CALL_VARIANTS on a FASTA dataset
UnsupportedInto(Format),       // INTO a format with no writer yet
// NotYetImplemented is retired for the wired opcodes (kept for any remaining stubs)
```

## Summary of what changes where

| Component | Phase 4 change |
| --- | --- |
| `spliceql` | none (read-only) |
| `codonsplice-core/compiler.rs` | none — bytecode layout already correct |
| `codonsplice-core/vm.rs` | implement pipeline/CALL opcodes; add `Dataset`/`Cursor`/`Record`, `Io` trait, record-aware `LOAD_FIELD`, streaming `VmOutput::Records` |
| `cnvlens-core` | add region-aware + callback/streaming entry points; actually use the BAI/CSI index |
| `splice-cli` | print record tables; accept file paths/regions |
