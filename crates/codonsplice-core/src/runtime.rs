//! Phase 4 runtime handles: the concrete `Dataset` / `Cursor` / `Record` values
//! that replace the Phase 3 `RuntimeValue::Pending` placeholder.
//!
//! A query executes as a small dataflow: `OPEN_SOURCE` produces a [`Dataset`],
//! `SCAN` wraps it in a [`Cursor`], `FILTER`/`PROJECT`/`SET_PARAM`/`CALL_*`
//! refine that cursor, and materialization ([`crate::materialize`]) pulls
//! [`Record`]s out applying the per-record predicate, ordering, and limit.
//!
//! Handles are reference-counted (`Arc`) so the VM operand stack stays cheap to
//! clone. The cursor is wrapped in a `Mutex` because materialization mutates it
//! (draining its record buffer) behind a shared handle.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use cnvlens_core::model::{CoverageWindow, Variant};
use cnvlens_core::AlnRecord;
use spliceql::ast::Format;

use crate::compiler::Program;

/// A value on the VM operand stack. Scalars carry data inline; the three handle
/// variants point at reference-counted runtime objects.
#[derive(Clone)]
pub enum RuntimeValue {
    Int(i64),
    Float(f64),
    Str(Arc<str>),
    Bool(bool),
    Null,
    Dataset(Arc<Dataset>),
    Cursor(Arc<Mutex<Cursor>>),
    Record(Arc<Record>),
}

impl std::fmt::Debug for RuntimeValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeValue::Int(n) => write!(f, "Int({n})"),
            RuntimeValue::Float(x) => write!(f, "Float({x})"),
            RuntimeValue::Str(s) => write!(f, "Str({s:?})"),
            RuntimeValue::Bool(b) => write!(f, "Bool({b})"),
            RuntimeValue::Null => write!(f, "Null"),
            RuntimeValue::Dataset(d) => write!(f, "Dataset({}, {})", fmt_format(&d.format), d.path),
            RuntimeValue::Cursor(_) => write!(f, "Cursor(..)"),
            RuntimeValue::Record(r) => write!(f, "Record({})", r.kind_name()),
        }
    }
}

/// Scalar equality; handles compare by pointer identity, cross-type is `false`.
/// This backs the VM's `EQ`/`NE` opcodes (only ever applied to scalars in
/// practice — comparing two cursors is not expressible in SpliceQL).
impl PartialEq for RuntimeValue {
    fn eq(&self, other: &Self) -> bool {
        use RuntimeValue::*;
        match (self, other) {
            (Int(a), Int(b)) => a == b,
            (Float(a), Float(b)) => a == b,
            (Int(a), Float(b)) | (Float(b), Int(a)) => (*a as f64) == *b,
            (Str(a), Str(b)) => a == b,
            (Bool(a), Bool(b)) => a == b,
            (Null, Null) => true,
            (Dataset(a), Dataset(b)) => Arc::ptr_eq(a, b),
            (Cursor(a), Cursor(b)) => Arc::ptr_eq(a, b),
            (Record(a), Record(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl RuntimeValue {
    pub fn type_name(&self) -> &'static str {
        match self {
            RuntimeValue::Int(_) => "int",
            RuntimeValue::Float(_) => "float",
            RuntimeValue::Str(_) => "string",
            RuntimeValue::Bool(_) => "bool",
            RuntimeValue::Null => "null",
            RuntimeValue::Dataset(_) => "dataset",
            RuntimeValue::Cursor(_) => "cursor",
            RuntimeValue::Record(_) => "record",
        }
    }

    pub fn is_truthy(&self) -> bool {
        match self {
            RuntimeValue::Bool(b) => *b,
            RuntimeValue::Int(n) => *n != 0,
            RuntimeValue::Float(x) => *x != 0.0,
            RuntimeValue::Str(s) => !s.is_empty(),
            RuntimeValue::Null => false,
            // Handles are always truthy (they exist).
            _ => true,
        }
    }

    /// Numeric coercion for arithmetic/ordering, if this value is a number.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            RuntimeValue::Int(n) => Some(*n as f64),
            RuntimeValue::Float(x) => Some(*x),
            _ => None,
        }
    }
}

fn fmt_format(f: &Format) -> &'static str {
    match f {
        Format::Bam => "bam",
        Format::Vcf => "vcf",
        Format::Fasta => "fasta",
        Format::Bed => "bed",
        Format::Cram => "cram",
    }
}

// ── Dataset ──────────────────────────────────────────────────────────────────

/// An opened genomic source. Phase 4 loads the whole file into memory (matching
/// cnvlens-core, which operates on `&[u8]`); streaming I/O is a later
/// optimization.
#[derive(Debug)]
pub struct Dataset {
    pub format: Format,
    pub path: String,
    pub data: DatasetInner,
}

#[derive(Debug)]
pub enum DatasetInner {
    Bam {
        bytes: Arc<Vec<u8>>,
        bai: Option<Arc<Vec<u8>>>,
    },
    Vcf {
        bytes: Arc<Vec<u8>>,
    },
    Fasta {
        seqs: HashMap<String, String>,
    },
    Bed {
        bytes: Arc<Vec<u8>>,
    },
}

impl Dataset {
    /// The BAM byte buffer, if this is a BAM dataset.
    pub fn bam_bytes(&self) -> Option<&[u8]> {
        match &self.data {
            DatasetInner::Bam { bytes, .. } => Some(bytes),
            _ => None,
        }
    }

    /// The co-located BAI index bytes, if present.
    pub fn bai_bytes(&self) -> Option<&[u8]> {
        match &self.data {
            DatasetInner::Bam { bai, .. } => bai.as_deref().map(|v| v.as_slice()),
            _ => None,
        }
    }
}

// ── Region ───────────────────────────────────────────────────────────────────

/// A statically-extracted region from a `WHERE chr = "x" [AND pos ...]` clause,
/// used to drive BAI seeking. 1-based-ish genomic coordinates as written in the
/// query; treated as a coarse seek hint (the predicate re-filters exactly).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    pub chrom: String,
    pub start: Option<i64>,
    pub end: Option<i64>,
}

impl Region {
    /// Convert to cnvlens-core's region type for the seeking entry points.
    pub fn to_core(&self) -> cnvlens_core::model::Region {
        cnvlens_core::model::Region::with_bounds(self.chrom.clone(), self.start, self.end)
    }
}

// ── Query options ────────────────────────────────────────────────────────────

/// The CALL operation a cursor will run, plus its tuned parameters. Built at
/// `CALL_*` time from the `SET_PARAM` accumulator.
#[derive(Debug)]
pub enum QueryOptions {
    Variant(cnvlens_core::model::VariantOptions),
    Coverage(cnvlens_core::model::CoverageOptions),
    Reads,
    Header,
}

// ── Cursor ───────────────────────────────────────────────────────────────────

/// A lazy query over a dataset. Built by `SCAN`; refined by the pipeline
/// opcodes. After a `CALL_*` opcode runs, `records` holds the raw produced rows
/// and materialization applies `predicate` → `order` → `limit`.
pub struct Cursor {
    pub dataset: Arc<Dataset>,
    /// Compiled `WHERE` sub-program, run per record (top-of-stack truthy keeps).
    pub predicate: Option<Program>,
    /// Compiled `SELECT` projection columns. `None` is SELECT * / no projection.
    /// (Phase 5 makes this real; Phase 4 stored a single `Option<Program>`.)
    pub projection: Option<Vec<ProjItem>>,
    /// Statically-extracted region for BAI seeking.
    pub region: Option<Region>,
    /// The CALL operation + tuned params.
    pub options: QueryOptions,
    /// Compiled `ORDER BY` key sub-program + descending flag.
    pub order: Option<(Program, bool)>,
    /// `LIMIT` row cap.
    pub limit: Option<i64>,
    /// Rows produced eagerly by a `CALL_*` opcode (CNV/coverage/reads/header),
    /// pending predicate/order/limit.
    pub records: Option<Vec<Record>>,
    /// A deferred record producer (used by `CALL_VARIANTS`): run at
    /// materialization time with the resolved `LIMIT`, so the variant pileup can
    /// short-circuit early. Takes an optional row limit and yields records.
    pub producer: Option<RecordProducer>,
    /// Template variable bindings, copied from the VM at `SCAN` so the
    /// per-record predicate/projection/order sub-programs can resolve `$vars`.
    pub vars: VarMap,
}

/// A deferred producer of records, invoked by materialization with the resolved
/// `LIMIT` (so e.g. variant calling can stop piling up after N variants).
pub type RecordProducer =
    Box<dyn FnOnce(Option<usize>) -> Result<Vec<Record>, cnvlens_core::error::CoreError>>;

/// One projected `SELECT` column: a compiled sub-program plus its output name.
/// `wildcard` marks `SELECT *` (pass the whole record through unchanged).
#[derive(Debug, Clone)]
pub struct ProjItem {
    pub prog: Program,
    pub name: String,
    pub wildcard: bool,
}

impl Cursor {
    /// A fresh scan cursor with no refinements.
    pub fn new(dataset: Arc<Dataset>, region: Option<Region>) -> Self {
        Cursor {
            dataset,
            predicate: None,
            projection: None,
            region,
            options: QueryOptions::Reads,
            order: None,
            limit: None,
            records: None,
            producer: None,
            vars: VarMap::default(),
        }
    }
}

// ── Record ───────────────────────────────────────────────────────────────────

/// An alignment row enriched with the header-resolved chromosome name and a
/// pileup depth, since a bare [`AlnRecord`] can answer neither `chr` nor
/// `depth`. (Spec models this as `Record::Alignment(AlnRecord)`; the enrichment
/// is required for the documented field set.)
#[derive(Debug, Clone)]
pub struct AlnRow {
    pub aln: AlnRecord,
    pub chrom: String,
    pub depth: i64,
}

/// A single genomic record exposed to the expression interpreter as named
/// fields.
#[derive(Debug, Clone)]
pub enum Record {
    Alignment(AlnRow),
    Variant(Variant),
    CoverageWindow(CoverageWindow),
    Header(Vec<(String, usize)>),
    /// A projected output row (from `SELECT col, ...`): an ordered list of
    /// `(column_name, value)` pairs.
    Row(Vec<(String, RuntimeValue)>),
}

impl Record {
    pub fn kind_name(&self) -> &'static str {
        match self {
            Record::Alignment(_) => "alignment",
            Record::Variant(_) => "variant",
            Record::CoverageWindow(_) => "coverage_window",
            Record::Header(_) => "header",
            Record::Row(_) => "row",
        }
    }

    /// Convert this record into a [`Record::Row`] of its natural columns
    /// (already-projected `Row`s are returned unchanged). Used when projection
    /// is absent but a flat row representation is wanted.
    pub fn into_row(self) -> Record {
        match self {
            Record::Row(_) => self,
            Record::Variant(ref v) => Record::Row(vec![
                ("chrom".into(), self.get_field("chrom")),
                ("pos".into(), RuntimeValue::Int(v.pos)),
                ("ref".into(), self.get_field("ref")),
                ("alt".into(), self.get_field("alt")),
                ("qual".into(), RuntimeValue::Float(v.qual)),
                ("depth".into(), RuntimeValue::Int(v.depth)),
                ("allele_freq".into(), RuntimeValue::Float(v.allele_freq)),
            ]),
            Record::CoverageWindow(ref w) => Record::Row(vec![
                ("chrom".into(), self.get_field("chrom")),
                ("start".into(), RuntimeValue::Int(w.start)),
                ("end".into(), RuntimeValue::Int(w.end)),
                ("coverage".into(), RuntimeValue::Int(w.coverage)),
                ("normalized".into(), RuntimeValue::Float(w.normalized)),
            ]),
            Record::Alignment(ref a) => Record::Row(vec![
                ("chrom".into(), RuntimeValue::Str(Arc::from(a.chrom.as_str()))),
                ("pos".into(), RuntimeValue::Int(a.aln.pos)),
                ("mapq".into(), RuntimeValue::Int(a.aln.mapq as i64)),
                ("flag".into(), RuntimeValue::Int(a.aln.flag as i64)),
                ("depth".into(), RuntimeValue::Int(a.depth)),
            ]),
            Record::Header(refs) => Record::Row(
                refs.into_iter()
                    .map(|(n, l)| (n, RuntimeValue::Int(l as i64)))
                    .collect(),
            ),
        }
    }

    /// Resolve a field name to a [`RuntimeValue`] for predicate/projection
    /// evaluation. Unknown fields resolve to `Null` (falsey), so predicates over
    /// absent fields are well-defined rather than a crash.
    pub fn get_field(&self, name: &str) -> RuntimeValue {
        match self {
            Record::Alignment(r) => aln_field(r, name),
            Record::Variant(v) => variant_field(v, name),
            Record::CoverageWindow(w) => window_field(w, name),
            Record::Header(_) => RuntimeValue::Null,
            Record::Row(cols) => cols
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
                .unwrap_or(RuntimeValue::Null),
        }
    }
}

fn aln_field(r: &AlnRow, name: &str) -> RuntimeValue {
    use RuntimeValue::*;
    let flag = r.aln.flag;
    match name {
        "chr" | "chrom" => Str(Arc::from(r.chrom.as_str())),
        "pos" => Int(r.aln.pos),
        "mapq" => Int(r.aln.mapq as i64),
        "flag" => Int(flag as i64),
        "depth" => Int(r.depth),
        "strand" => Str(Arc::from(if flag & 0x10 != 0 { "-" } else { "+" })),
        "is_reverse" => Bool(flag & 0x10 != 0),
        "is_duplicate" => Bool(flag & 0x400 != 0),
        "is_secondary" => Bool(flag & 0x100 != 0),
        _ => Null,
    }
}

fn variant_field(v: &Variant, name: &str) -> RuntimeValue {
    use RuntimeValue::*;
    match name {
        "chr" | "chrom" => Str(Arc::from(v.chrom.as_str())),
        "pos" => Int(v.pos),
        "ref" => Str(Arc::from(v.ref_base.as_str())),
        "alt" => Str(Arc::from(v.alt.as_str())),
        "qual" => Float(v.qual),
        "depth" => Int(v.depth),
        "ref_count" => Int(v.ref_count),
        "alt_count" => Int(v.alt_count),
        "af" | "allele_freq" => Float(v.allele_freq),
        "strand_bias" => Float(v.strand_bias),
        "kind" => Str(Arc::from(v.kind.as_str())),
        // VCF-sourced fields (None for pileup-called variants).
        "filter" => match &v.filter {
            Some(f) => Str(Arc::from(f.as_str())),
            None => Str(Arc::from("PASS")),
        },
        "id" => match &v.id {
            Some(i) => Str(Arc::from(i.as_str())),
            None => Str(Arc::from(".")),
        },
        _ => Null,
    }
}

fn window_field(w: &CoverageWindow, name: &str) -> RuntimeValue {
    use RuntimeValue::*;
    match name {
        "chr" | "chrom" => Str(Arc::from(w.chromosome.as_str())),
        "start" => Int(w.start),
        "end" => Int(w.end),
        "coverage" => Int(w.coverage),
        "normalized" => Float(w.normalized),
        "masked" => Bool(w.masked.unwrap_or(false)),
        _ => Null,
    }
}

// ── Template variables ───────────────────────────────────────────────────────

/// Runtime bindings for `$name` template variables, resolved by `LOAD_VAR`.
/// Populated by `splice run` (from CLI args) or the WASM `vars` parameter.
#[derive(Debug, Clone, Default)]
pub struct VarMap(pub HashMap<String, RuntimeValue>);

impl VarMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, name: impl Into<String>, value: RuntimeValue) {
        self.0.insert(name.into(), value);
    }

    pub fn get(&self, name: &str) -> Option<&RuntimeValue> {
        self.0.get(name)
    }
}
