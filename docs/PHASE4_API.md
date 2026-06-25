# Phase 4 — codonsplice-core public API

Phase 4 turns the stubbed pipeline opcodes into a real execution engine over
`cnvlens-core`. This document is the as-built public API after Phase 4, covering
the new runtime handles, the execution VM, region extraction, and
materialization. (Phase 1–3 lexer/parser/compiler API is unchanged.)

## Crate exports (`codonsplice_core`)

```rust
// Compilation (Phase 3, + extract_region)
pub fn compile(source: &str) -> Result<Program, CompileError>;
pub fn compile_and_disassemble(source: &str) -> Result<String, CompileError>;
pub fn compile_expr(expr_source: &str) -> Result<Program, CompileError>;
pub fn eval_expr(expr_source: &str) -> Result<RuntimeValue, EvalError>;
pub use compiler::{disassemble, extract_region, OpCode, Program, Value, CompileError, ...};

// Runtime handles (Part 1)
pub use runtime::{
    RuntimeValue, Dataset, DatasetInner, Cursor, Record, AlnRow, QueryOptions, Region,
};

// Execution (Part 2)
pub use vm::{Vm, VmError, VmOutput, Io};

// Materialization (Part 3)
pub use materialize::{materialize, materialize_streaming};
```

### `Program` (changed)

```rust
pub struct Program {
    pub consts: Vec<Value>,
    pub code:   Vec<u8>,
    pub debug:  Vec<DebugInfo>,
    pub region: Option<runtime::Region>, // NEW: statically-extracted WHERE region
}
```

`region` is populated by `extract_region(&Expr)` during `compile()` and drives
BAI seeking at execution time.

### `RuntimeValue` (replaces `Pending`)

```rust
pub enum RuntimeValue {
    Int(i64), Float(f64), Str(Arc<str>), Bool(bool), Null,
    Dataset(Arc<Dataset>),
    Cursor(Arc<Mutex<Cursor>>),
    Record(Arc<Record>),
}
```

### Handles

```rust
pub struct Dataset { pub format: Format, pub path: String, pub data: DatasetInner }
pub enum DatasetInner {
    Bam { bytes: Arc<Vec<u8>>, bai: Option<Arc<Vec<u8>>> },
    Vcf { bytes: Arc<Vec<u8>> },
    Fasta { seqs: HashMap<String, String> },
    Bed  { bytes: Arc<Vec<u8>> },
}

pub struct Cursor {
    pub dataset:    Arc<Dataset>,
    pub predicate:  Option<Program>,        // compiled WHERE sub-program
    pub projection: Option<Program>,        // compiled SELECT (stored; identity-applied)
    pub region:     Option<Region>,
    pub options:    QueryOptions,
    pub order:      Option<(Program, bool)>, // ORDER BY key + descending  (added)
    pub limit:      Option<i64>,             // LIMIT count                (added)
    pub records:    Option<Vec<Record>>,     // filled by CALL_*           (added)
}

pub struct Region { pub chrom: String, pub start: Option<i64>, pub end: Option<i64> }
impl Region { pub fn to_core(&self) -> cnvlens_core::model::Region; }

pub enum QueryOptions {
    Variant(cnvlens_core::model::VariantOptions),
    Coverage(cnvlens_core::model::CoverageOptions),
    Reads,
    Header,
}

pub enum Record {
    Alignment(AlnRow),                              // see deviation note
    Variant(cnvlens_core::model::Variant),
    CoverageWindow(cnvlens_core::model::CoverageWindow),
    Header(Vec<(String, usize)>),
}
impl Record { pub fn get_field(&self, name: &str) -> RuntimeValue; }

// Enriched alignment: a bare AlnRecord cannot answer `chr`/`depth`, so the
// header-resolved chromosome and a pileup depth are attached.
pub struct AlnRow { pub aln: cnvlens_core::AlnRecord, pub chrom: String, pub depth: i64 }
```

### VM

```rust
pub trait Io {
    fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>>;
    fn read_sibling_index(&self, path: &str) -> Option<Vec<u8>>;
    fn write_file(&mut self, path: &str, bytes: &[u8]) -> std::io::Result<()>;
}
pub struct FsIo;  // native filesystem backend (cfg(not(wasm32)))
pub struct NoIo;  // no-op backend (predicate/order eval, wasm default)

impl Vm {
    pub fn new(program: Program) -> Self;                       // FsIo (native) / NoIo (wasm)
    pub fn with_io(program: Program, io: Box<dyn Io>) -> Self;  // custom backend (WASM uses MapIo)
    pub fn eval_only(program: Program) -> Self;                 // NoIo, for sub-program eval
    pub fn run(&mut self) -> Result<VmOutput, VmError>;
    pub fn eval_expr(&mut self) -> Result<RuntimeValue, VmError>;
    pub fn eval_record(&mut self, record: Arc<Record>) -> Result<RuntimeValue, VmError>;
}

pub enum VmOutput { Ready(Program), Text(String), Records(Vec<Record>) }  // Records is NEW

pub enum VmError {
    UnknownOpcode(u8, usize), StackUnderflow(usize),
    TypeMismatch { expected: &'static str, got: &'static str, pc: usize },
    Io(String),                                   // NEW
    Core(String),                                 // NEW (wraps cnvlens_core::CoreError)
    SourceFormat { expected: &'static str, got: &'static str }, // NEW
    UnsupportedInto(String),                      // NEW
    NotYetImplemented(String),
}

// Record serialization helpers (used by the CLI table view + WASM bridge)
pub fn vm::records_to_json(records: &[Record]) -> String;
pub fn vm::record_to_json(record: &Record) -> serde_json::Value;
```

### Region extraction (compiler.rs)

```rust
pub fn extract_region(filter: &Expr) -> Option<Region>;
```

Recognizes `chr = "x"` / `chrom = "x"` optionally AND-ed with `pos >= a` /
`pos <= b` (and strict variants, either operand order). Anything else → `None`
(full scan + per-record predicate).

### Materialization (materialize.rs)

```rust
pub fn materialize(cursor: Arc<Mutex<Cursor>>, source: &str) -> Result<Vec<Record>, CoreError>;
pub fn materialize_streaming(cursor: Arc<Mutex<Cursor>>)
    -> impl Iterator<Item = Result<Record, CoreError>>;
```

Applies, in order: the WHERE predicate (per record, via an eval-only `Vm`), the
ORDER BY key sort, then the LIMIT truncation.

## cnvlens-core additions (the Phase 3 gaps, now fixed)

```rust
// error.rs
pub enum CoreError {
    Io(std::io::Error), BamParse(String), InvalidRegion(String),
    NoReadsInRegion(String), InsufficientData { min_required: usize, found: usize },
}
impl std::error::Error for CoreError {}

// model.rs
pub struct Region { pub chrom: String, pub start: Option<i64>, pub end: Option<i64> }

// bam.rs — BAI random access
pub fn read_bai_index(bai_bytes: &[u8]) -> io::Result<bai::Index>;
pub fn for_each_region_core(bam, bai, region, f) -> io::Result<u64>;
pub fn for_each_region_full(bam, bai, region, f) -> io::Result<u64>;

// variants.rs — streaming + region
pub fn stream(bam, bai, opts) -> Result<impl Iterator<Item = Variant>, CoreError>;
pub fn collect_variants(bam, bai, opts) -> Result<Vec<Variant>, CoreError>;
pub fn call_variants_region(bam, bai, region, opts) -> Result<Vec<Variant>, CoreError>;
#[deprecated] pub fn call_variants(bam, bai, opts) -> serde_json::Value;  // legacy JSON wrapper

// coverage.rs — streaming + region
pub fn stream(bam, bai, opts) -> Result<impl Iterator<Item = CoverageWindow>, CoreError>;
pub fn coverage_windows(bam, bai, opts) -> Result<Vec<CoverageWindow>, CoreError>;
pub fn analyze_coverage_region(bam, bai, region, opts) -> Result<Vec<CoverageWindow>, CoreError>;
pub fn compute_coverage(bam, bai, opts, region: Option<&Region>) -> Result<CoverageData, CoreError>;
#[deprecated] pub fn analyze_coverage(bam, bai, opts) -> serde_json::Value; // legacy JSON wrapper
```

## Documented deviations from the Phase 4 spec

These are intentional, where the spec's literal shape couldn't answer a required
field or where a feature isn't exercised by the test matrix:

1. **`Record::Alignment(AlnRow)`** rather than `Alignment(AlnRecord)`. A bare
   `AlnRecord` carries only `ref_id`, so it cannot answer `chr` (needs the
   header) or `depth` (needs a pileup). `AlnRow` attaches the resolved
   chromosome name and a per-position depth computed over the streamed read set.
2. **`Cursor` gains `order` / `limit` / `records`** beyond the spec's four
   fields — required to carry ORDER BY / LIMIT metadata and the CALL_* output
   buffer to materialization.
3. **`PROJECT` is identity** (SELECT * semantics). Column projection to an
   arbitrary row type needs a generic `Row` record variant; deferred to Phase 5.
   The opcode is consumed correctly; no test exercises column projection.
4. **`INTO`**: VCF (variants), BED (windows/variants), and JSON (`INTO fasta` is
   repurposed as the JSON sink, since the parser has no `json` token). `INTO bam`
   / `cram` return `UnsupportedInto`.
5. **Streaming** is "compute-then-yield": coverage normalization needs a global
   median, so windows/variants are computed up front and then streamed via an
   iterator. The API shape (`impl Iterator`) matches the spec; true incremental
   yield for variants is a Phase 5 optimization.
