//! Stack-based virtual machine for SpliceQL bytecode.
//!
//! Phase 4 wires the pipeline opcodes (`OPEN_SOURCE … WRITE_INTO`, `CALL_*`) to
//! [`cnvlens_core`]: a query opens a BAM/VCF/FASTA, scans it (optionally seeking
//! via BAI when the `WHERE` clause pins a chromosome), runs the per-record
//! predicate, and materializes records — calling cnvlens-core's coverage /
//! variant / read entry points for the `CALL_*` opcodes.
//!
//! The expression interpreter from Phase 3 is unchanged except that
//! `LOAD_FIELD` / `GET_FIELD` are now record-aware (resolving against the
//! current record during predicate / projection / order evaluation).

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

use cnvlens_core::error::CoreError;
use cnvlens_core::model::{CoverageOptions, VariantOptions};
use cnvlens_core::{bam, coverage, reference_list, variants, vcf, AlnRecord};
use spliceql::ast::Format;

use crate::compiler::{OpCode, Program, Value};
use crate::materialize::materialize;
use crate::runtime::{
    AlnRow, Cursor, Dataset, DatasetInner, ProjItem, QueryOptions, Record, RuntimeValue, VarMap,
};

pub use crate::runtime::RuntimeValue as Runtime; // (kept for any external imports)

// ── Host I/O abstraction ─────────────────────────────────────────────────────

/// Host I/O: the native CLI reads/writes the filesystem; the WASM build serves
/// files from the JS-provided map. Keeps `std::fs` out of the core's hot path.
pub trait Io {
    fn read_file(&self, path: &str) -> io::Result<Vec<u8>>;
    /// The co-located index for `path` (`path + ".bai"`), if available.
    fn read_sibling_index(&self, path: &str) -> Option<Vec<u8>>;
    fn write_file(&mut self, path: &str, bytes: &[u8]) -> io::Result<()>;
}

/// Filesystem-backed I/O for the native CLI.
#[cfg(not(target_arch = "wasm32"))]
pub struct FsIo;

#[cfg(not(target_arch = "wasm32"))]
impl Io for FsIo {
    fn read_file(&self, path: &str) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }
    fn read_sibling_index(&self, path: &str) -> Option<Vec<u8>> {
        std::fs::read(format!("{path}.bai")).ok()
    }
    fn write_file(&mut self, path: &str, bytes: &[u8]) -> io::Result<()> {
        std::fs::write(path, bytes)
    }
}

/// A no-op I/O used for record/expression evaluation (predicate, order key),
/// which never touches files.
pub struct NoIo;
impl Io for NoIo {
    fn read_file(&self, _: &str) -> io::Result<Vec<u8>> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "no I/O in eval mode"))
    }
    fn read_sibling_index(&self, _: &str) -> Option<Vec<u8>> {
        None
    }
    fn write_file(&mut self, _: &str, _: &[u8]) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "no I/O in eval mode"))
    }
}

// ── Runtime value re-export & output ─────────────────────────────────────────

/// The result of running a program.
#[derive(Debug)]
pub enum VmOutput {
    /// The pipeline reached `HALT` with no record stream to emit (e.g. after
    /// `WRITE_INTO`, or an expression-only program).
    Ready(Program),
    /// Textual output (`CALL_HEADER`, or a `WRITE_INTO` summary).
    Text(String),
    /// A materialized record stream — `CALL_*` queries with no `SELECT`.
    Records(Vec<Record>),
    /// Projected rows — `CALL_*` queries with an explicit column `SELECT`.
    Rows(Vec<Record>),
}

/// A runtime error.
#[derive(Debug, Clone, PartialEq)]
pub enum VmError {
    UnknownOpcode(u8, usize),
    StackUnderflow(usize),
    TypeMismatch {
        expected: &'static str,
        got: &'static str,
        pc: usize,
    },
    /// File open / read / write failed.
    Io(String),
    /// A cnvlens-core data-plane error (BAM parse, region, …).
    Core(String),
    /// A `CALL_*` opcode was applied to a dataset of the wrong format.
    SourceFormat {
        expected: &'static str,
        got: &'static str,
    },
    /// `INTO` a format with no writer yet.
    UnsupportedInto(String),
    /// A `$variable` referenced by the query has no binding in the VarMap.
    UnboundVariable { name: String, pc: usize },
    /// A still-stubbed opcode (kept for any remaining unwired paths).
    NotYetImplemented(String),
    /// A builtin function call failed: unknown name, wrong arity, or a
    /// badly-typed argument. Carries a ready-to-print message.
    Builtin(String),
}

impl VmError {
    fn io(e: io::Error) -> Self {
        VmError::Io(e.to_string())
    }
    fn core(e: CoreError) -> Self {
        VmError::Core(e.to_string())
    }
}

impl std::fmt::Display for VmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VmError::UnknownOpcode(b, pc) => write!(f, "unknown opcode 0x{b:02X} at pc {pc}"),
            VmError::StackUnderflow(pc) => write!(f, "stack underflow at pc {pc}"),
            VmError::TypeMismatch { expected, got, pc } => {
                write!(f, "type mismatch at pc {pc}: expected {expected}, got {got}")
            }
            VmError::Io(m) => write!(f, "io error: {m}"),
            VmError::Core(m) => write!(f, "{m}"),
            VmError::SourceFormat { expected, got } => {
                write!(f, "wrong source format: expected {expected}, got {got}")
            }
            VmError::UnsupportedInto(fmt) => write!(f, "no writer for INTO {fmt}"),
            VmError::UnboundVariable { name, pc } => {
                write!(f, "unbound variable ${name} at pc {pc}")
            }
            VmError::NotYetImplemented(op) => write!(f, "opcode {op} is not implemented"),
            VmError::Builtin(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for VmError {}

fn format_from_byte(b: u8) -> Format {
    match b {
        0 => Format::Bam,
        1 => Format::Vcf,
        2 => Format::Fasta,
        3 => Format::Bed,
        4 => Format::Cram,
        5 => Format::Json,
        6 => Format::Tsv,
        _ => Format::Cram,
    }
}

fn format_label(f: &Format) -> &'static str {
    match f {
        Format::Bam => "bam",
        Format::Vcf => "vcf",
        Format::Fasta => "fasta",
        Format::Bed => "bed",
        Format::Cram => "cram",
        Format::Json => "json",
        Format::Tsv => "tsv",
    }
}

/// Which `CALL_*` operation is running.
#[derive(Clone, Copy)]
enum CallKind {
    Variants,
    Cnv,
    Coverage,
    Reads,
}

// ── The VM ───────────────────────────────────────────────────────────────────

/// The bytecode virtual machine.
pub struct Vm {
    program: Program,
    stack: Vec<RuntimeValue>,
    pc: usize,
    /// The record in scope while a per-record sub-program runs.
    current_record: Option<Arc<Record>>,
    /// Host I/O backend.
    io: Box<dyn Io>,
    /// Accumulated `SET_PARAM` key/value pairs, consumed by the next `CALL_*`.
    pending_params: Vec<(Arc<str>, RuntimeValue)>,
    /// Set by `WRITE_INTO`; turns `HALT` into a textual summary.
    wrote: Option<String>,
    /// `$name` template variable bindings, resolved by `LOAD_VAR`.
    vars: VarMap,
}

impl Vm {
    /// Default constructor: filesystem-backed I/O natively, no-op I/O on wasm
    /// (the WASM build supplies a JS-backed I/O via [`Vm::with_io`]).
    pub fn new(program: Program) -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        {
            Self::with_io(program, Box::new(FsIo))
        }
        #[cfg(target_arch = "wasm32")]
        {
            Self::with_io(program, Box::new(NoIo))
        }
    }

    /// Construct with an explicit I/O backend (used by the WASM build).
    pub fn with_io(program: Program, io: Box<dyn Io>) -> Self {
        Self {
            program,
            stack: Vec::new(),
            pc: 0,
            current_record: None,
            io,
            pending_params: Vec::new(),
            wrote: None,
            vars: VarMap::default(),
        }
    }

    /// Attach template-variable bindings (resolved by `LOAD_VAR`).
    pub fn with_vars(mut self, vars: VarMap) -> Self {
        self.vars = vars;
        self
    }

    /// Construct an evaluation-only VM over a (predicate / order-key) program.
    /// No file I/O is performed.
    pub fn eval_only(program: Program) -> Self {
        Self::with_io(program, Box::new(NoIo))
    }

    /// Run the main program from `pc = 0`.
    pub fn run(&mut self) -> Result<VmOutput, VmError> {
        let code = self.program.code.clone();
        loop {
            if self.pc >= code.len() {
                return self.finish();
            }
            let byte = code[self.pc];
            let op = OpCode::from_byte(byte).ok_or(VmError::UnknownOpcode(byte, self.pc))?;
            match op {
                OpCode::Halt => return self.finish(),
                OpCode::OpenSource => self.op_open_source()?,
                OpCode::Scan => self.op_scan()?,
                OpCode::Filter => self.op_filter()?,
                OpCode::Project => self.op_project()?,
                OpCode::SetParam => self.op_set_param()?,
                OpCode::OrderBy => self.op_order_by()?,
                OpCode::Limit => self.op_limit()?,
                OpCode::WriteInto => self.op_write_into()?,
                OpCode::CallVariants => self.op_call(CallKind::Variants)?,
                OpCode::CallCnv => self.op_call(CallKind::Cnv)?,
                OpCode::CallCoverage => self.op_call(CallKind::Coverage)?,
                OpCode::CallReads => self.op_call(CallKind::Reads)?,
                OpCode::CallHeader => self.op_call_header()?,
                _ => self.exec_expr(op)?,
            }
        }
    }

    /// What to return at `HALT`.
    fn finish(&mut self) -> Result<VmOutput, VmError> {
        if let Some(summary) = self.wrote.take() {
            return Ok(VmOutput::Text(summary));
        }
        match self.stack.last().cloned() {
            Some(RuntimeValue::Cursor(cursor)) => {
                // A non-wildcard SELECT projects to Rows; otherwise Records.
                let proj_active = {
                    let g = cursor.lock().unwrap();
                    g.projection
                        .as_ref()
                        .map(|items| !items.iter().any(|i| i.wildcard))
                        .unwrap_or(false)
                };
                let records = materialize(cursor, "").map_err(VmError::core)?;
                if proj_active {
                    Ok(VmOutput::Rows(records))
                } else {
                    Ok(VmOutput::Records(records))
                }
            }
            Some(RuntimeValue::Record(r)) => match &*r {
                Record::Header(refs) => Ok(VmOutput::Text(format_header(refs))),
                _ => Ok(VmOutput::Ready(self.program.clone())),
            },
            _ => Ok(VmOutput::Ready(self.program.clone())),
        }
    }

    /// Evaluate an expression-only program and return the top-of-stack value.
    pub fn eval_expr(&mut self) -> Result<RuntimeValue, VmError> {
        let code = self.program.code.clone();
        loop {
            if self.pc >= code.len() {
                break;
            }
            let byte = code[self.pc];
            let op = OpCode::from_byte(byte).ok_or(VmError::UnknownOpcode(byte, self.pc))?;
            match op {
                OpCode::Halt | OpCode::RetPred => break,
                OpCode::OpenSource
                | OpCode::Scan
                | OpCode::Filter
                | OpCode::Project
                | OpCode::SetParam
                | OpCode::OrderBy
                | OpCode::Limit
                | OpCode::WriteInto
                | OpCode::CallVariants
                | OpCode::CallCnv
                | OpCode::CallCoverage
                | OpCode::CallReads
                | OpCode::CallHeader => {
                    return Err(VmError::NotYetImplemented(op.name().to_string()))
                }
                _ => self.exec_expr(op)?,
            }
        }
        self.stack
            .last()
            .cloned()
            .ok_or(VmError::StackUnderflow(self.pc))
    }

    /// Evaluate the (already-loaded) program against `record` from `pc = 0`,
    /// returning the top-of-stack value. Used by [`crate::materialize`] for the
    /// per-record predicate and order-key sub-programs.
    pub fn eval_record(&mut self, record: Arc<Record>) -> Result<RuntimeValue, VmError> {
        self.pc = 0;
        self.stack.clear();
        self.current_record = Some(record);
        let v = self.eval_expr();
        self.current_record = None;
        v
    }

    // ── Pipeline opcodes ─────────────────────────────────────────────────────

    fn op_open_source(&mut self) -> Result<(), VmError> {
        let pc0 = self.pc;
        self.pc += 1;
        let fmt_byte = self.read_u8();
        // The high bit carries the `SPLIT` request (multi-allelic split); the
        // low 7 bits are the format code.
        let split = fmt_byte & crate::compiler::SPLIT_FLAG != 0;
        let fmt = format_from_byte(fmt_byte & !crate::compiler::SPLIT_FLAG);
        let path_idx = self.read_u16();
        let path = self.resolve_path(&self.const_str(path_idx), pc0)?;
        let bytes = self.io.read_file(&path).map_err(VmError::io)?;

        let data = match fmt {
            Format::Bam => {
                let bai = self.io.read_sibling_index(&path).map(Arc::new);
                DatasetInner::Bam {
                    bytes: Arc::new(bytes),
                    bai,
                }
            }
            Format::Vcf => DatasetInner::Vcf {
                bytes: Arc::new(bytes),
                split,
            },
            Format::Bed => DatasetInner::Bed {
                bytes: Arc::new(bytes),
            },
            Format::Fasta => DatasetInner::Fasta {
                seqs: parse_fasta(&bytes),
            },
            Format::Cram => {
                return Err(VmError::UnsupportedInto("cram (no reader)".to_string()))
            }
            // Output-only sinks — not valid as a FROM source.
            Format::Json | Format::Tsv => {
                return Err(VmError::SourceFormat {
                    expected: "bam/vcf/bed/fasta",
                    got: format_label(&fmt),
                })
            }
        };
        let dataset = Dataset {
            format: fmt,
            path,
            data,
        };
        self.stack.push(RuntimeValue::Dataset(Arc::new(dataset)));
        Ok(())
    }

    fn op_scan(&mut self) -> Result<(), VmError> {
        let pc0 = self.pc;
        self.pc += 1;
        let ds = match self.pop(pc0)? {
            RuntimeValue::Dataset(d) => d,
            other => {
                return Err(VmError::TypeMismatch {
                    expected: "dataset",
                    got: other.type_name(),
                    pc: pc0,
                })
            }
        };
        let region = self.program.region.clone();
        let mut cursor = Cursor::new(ds, region);
        cursor.vars = self.vars.clone();
        self.stack
            .push(RuntimeValue::Cursor(Arc::new(Mutex::new(cursor))));
        Ok(())
    }

    fn op_filter(&mut self) -> Result<(), VmError> {
        let pc0 = self.pc;
        self.pc += 1;
        let off = self.read_u16() as usize;
        let len = self.read_u16() as usize;
        let pred = self.extract_subprogram(off, len);
        let cursor = self.peek_cursor(pc0)?;
        cursor.lock().unwrap().predicate = Some(pred);
        Ok(())
    }

    fn op_project(&mut self) -> Result<(), VmError> {
        let pc0 = self.pc;
        self.pc += 1;
        let table_off = self.read_u16() as usize;

        // Table layout (from the compiler's append_projection):
        //   [u16 count][per item: u16 expr_off, u16 expr_len, u16 name_idx]
        // Collect the triples first (immutable borrow of code), then build the
        // sub-programs (which also borrow self).
        let code = &self.program.code;
        let count = u16::from_le_bytes([code[table_off], code[table_off + 1]]) as usize;
        let mut triples = Vec::with_capacity(count);
        let mut p = table_off + 2;
        for _ in 0..count {
            let expr_off = u16::from_le_bytes([code[p], code[p + 1]]) as usize;
            let expr_len = u16::from_le_bytes([code[p + 2], code[p + 3]]) as usize;
            let name_idx = u16::from_le_bytes([code[p + 4], code[p + 5]]);
            triples.push((expr_off, expr_len, name_idx));
            p += 6;
        }

        let items: Vec<ProjItem> = triples
            .into_iter()
            .map(|(off, len, name_idx)| {
                let name = self.const_str(name_idx).to_string();
                ProjItem {
                    prog: self.extract_subprogram(off, len),
                    wildcard: name == "*",
                    name,
                }
            })
            .collect();

        let cursor = self.peek_cursor(pc0)?;
        cursor.lock().unwrap().projection = Some(items);
        Ok(())
    }

    fn op_set_param(&mut self) -> Result<(), VmError> {
        let pc0 = self.pc;
        self.pc += 1;
        let key_idx = self.read_u16();
        let key = self.const_str(key_idx);
        let value = self.pop(pc0)?;
        self.pending_params.push((key, value));
        Ok(())
    }

    fn op_order_by(&mut self) -> Result<(), VmError> {
        let pc0 = self.pc;
        self.pc += 1;
        let table_off = self.read_u16() as usize;
        // Table: [u16 count][per item: u16 expr_off, u16 expr_len, u8 dir].
        // Phase 4 sorts by the first key.
        let code = &self.program.code;
        let count = u16::from_le_bytes([code[table_off], code[table_off + 1]]) as usize;
        if count > 0 {
            let item = table_off + 2;
            let expr_off = u16::from_le_bytes([code[item], code[item + 1]]) as usize;
            let expr_len = u16::from_le_bytes([code[item + 2], code[item + 3]]) as usize;
            let desc = code[item + 4] != 0;
            let key_prog = self.extract_subprogram(expr_off, expr_len);
            let cursor = self.peek_cursor(pc0)?;
            cursor.lock().unwrap().order = Some((key_prog, desc));
        }
        Ok(())
    }

    fn op_limit(&mut self) -> Result<(), VmError> {
        let pc0 = self.pc;
        self.pc += 1;
        let count = match self.pop(pc0)? {
            RuntimeValue::Int(n) => n,
            other => {
                return Err(VmError::TypeMismatch {
                    expected: "int",
                    got: other.type_name(),
                    pc: pc0,
                })
            }
        };
        let cursor = self.peek_cursor(pc0)?;
        cursor.lock().unwrap().limit = Some(count);
        Ok(())
    }

    fn op_call(&mut self, kind: CallKind) -> Result<(), VmError> {
        let pc0 = self.pc;
        self.pc += 1;
        let cursor = self.peek_cursor(pc0)?;
        let (ds, region) = {
            let c = cursor.lock().unwrap();
            (c.dataset.clone(), c.region.clone())
        };

        match kind {
            // Variant calling is deferred to materialization (so the resolved
            // LIMIT can short-circuit the pileup) and dispatches by source
            // format: a VCF passes its already-called variants straight through;
            // a BAM runs the streaming pileup.
            CallKind::Variants => {
                let is_vcf = matches!(ds.data, DatasetInner::Vcf { .. });
                if !is_vcf && ds.bam_bytes().is_none() {
                    return Err(VmError::SourceFormat {
                        expected: "bam or vcf",
                        got: format_label(&ds.format),
                    });
                }
                let mut opts = self.build_variant_options();
                // `WITH reference = "ref.fa"` makes REF the actual reference base
                // at each position instead of the pileup-majority guess (which is
                // a coin-flip at balanced het sites). Load it here, where the Io
                // backend is available.
                opts.reference_seqs = self.load_reference_seqs()?;
                let core_region = region.as_ref().map(|r| r.to_core());
                let producer: crate::runtime::RecordProducer = Box::new(move |limit| {
                    sharded_variant_producer(&ds, &opts, core_region.as_ref(), is_vcf, limit)
                });
                let mut c = cursor.lock().unwrap();
                c.producer = Some(producer);
            }
            CallKind::Coverage => {
                let opts = self.build_coverage_options();
                let windows = self.compute_coverage_windows(&ds, &region, &opts)?;
                let records = windows.into_iter().map(Record::CoverageWindow).collect();
                let mut c = cursor.lock().unwrap();
                c.records = Some(records);
                c.options = QueryOptions::Coverage(opts);
            }
            CallKind::Cnv => {
                // `CALL cnv` runs copy-number DETECTION over the coverage windows
                // (unlike `CALL coverage`, which streams the raw bins): it emits
                // amplification/deletion call records, not windows.
                let opts = self.build_coverage_options();
                let windows = self.compute_coverage_windows(&ds, &region, &opts)?;
                let cnvs = detect_cnvs(&windows, &opts);
                let records = cnvs
                    .into_iter()
                    .map(|v| Record::Cnv(normalize_cnv(v)))
                    .collect();
                let mut c = cursor.lock().unwrap();
                c.records = Some(records);
                c.options = QueryOptions::Coverage(opts);
            }
            CallKind::Reads => {
                let bytes = self.require_bam(&ds, "reads")?;
                let recs = collect_read_records(bytes, ds.bai_bytes(), &region)?;
                let mut c = cursor.lock().unwrap();
                c.records = Some(recs);
                c.options = QueryOptions::Reads;
            }
        }
        Ok(())
    }

    fn op_call_header(&mut self) -> Result<(), VmError> {
        let pc0 = self.pc;
        self.pc += 1;
        let ds = match self.pop(pc0)? {
            RuntimeValue::Cursor(c) => c.lock().unwrap().dataset.clone(),
            RuntimeValue::Dataset(d) => d,
            other => {
                return Err(VmError::TypeMismatch {
                    expected: "dataset",
                    got: other.type_name(),
                    pc: pc0,
                })
            }
        };
        let bytes = self.require_bam(&ds, "header")?;
        let header = bam::read_header(bytes).map_err(VmError::io)?;
        let refs = reference_list(&header);
        self.stack
            .push(RuntimeValue::Record(Arc::new(Record::Header(refs))));
        Ok(())
    }

    fn op_write_into(&mut self) -> Result<(), VmError> {
        let pc0 = self.pc;
        self.pc += 1;
        let fmt = format_from_byte(self.read_u8());
        let path_idx = self.read_u16();
        let path = self.resolve_path(&self.const_str(path_idx), pc0)?;

        let cursor = match self.pop(pc0)? {
            RuntimeValue::Cursor(c) => c,
            other => {
                return Err(VmError::TypeMismatch {
                    expected: "cursor",
                    got: other.type_name(),
                    pc: pc0,
                })
            }
        };
        // Capture the source contigs (for VCF `##contig=<ID=…,length=…>`) before
        // materializing consumes the cursor.
        let contigs = {
            let c = cursor.lock().unwrap();
            contigs_from_dataset(&c.dataset)
        };
        let records = materialize(cursor, "").map_err(VmError::core)?;
        let bytes = serialize_records(fmt.clone(), &records, &contigs)?;
        self.io.write_file(&path, &bytes).map_err(VmError::io)?;
        self.wrote = Some(format!(
            "wrote {} record(s) to {} ({})",
            records.len(),
            path,
            format_label(&fmt)
        ));
        Ok(())
    }

    // ── Option building from SET_PARAM accumulator ───────────────────────────

    fn build_variant_options(&self) -> VariantOptions {
        let mut o = VariantOptions::default();
        for (key, val) in &self.pending_params {
            match key.as_ref() {
                "min_depth" => o.min_depth = as_i64(val).unwrap_or(o.min_depth),
                "min_base_quality" => {
                    o.min_base_quality = as_i64(val).unwrap_or(o.min_base_quality as i64) as u8
                }
                "min_mapping_quality" => {
                    o.min_mapping_quality = as_i64(val).unwrap_or(o.min_mapping_quality as i64) as u8
                }
                "min_variant_reads" => {
                    o.min_variant_reads = as_i64(val).unwrap_or(o.min_variant_reads)
                }
                "min_allele_freq" | "min_af" => {
                    o.min_allele_freq = as_f64(val).unwrap_or(o.min_allele_freq)
                }
                "min_strand_bias" => o.min_strand_bias = as_f64(val).unwrap_or(o.min_strand_bias),
                _ => {}
            }
        }
        o
    }

    /// Load the `WITH reference = "ref.fa"` FASTA into a per-contig sequence map,
    /// if the parameter was given. Keyed by the FASTA contig name, which must
    /// match the BAM/VCF contig names (e.g. `>7` ↔ BAM `7`). Returns `Ok(None)`
    /// when no reference was requested (the caller then falls back to the
    /// inferred-from-reads REF base).
    fn load_reference_seqs(&self) -> Result<Option<HashMap<String, String>>, VmError> {
        let path = self.pending_params.iter().find_map(|(k, v)| {
            if k.as_ref() == "reference" {
                match v {
                    RuntimeValue::Str(s) => Some(s.to_string()),
                    _ => None,
                }
            } else {
                None
            }
        });
        let path = match path {
            Some(p) => p,
            None => return Ok(None),
        };
        let bytes = self.io.read_file(&path).map_err(|e| {
            VmError::Io(format!("reference FASTA {path:?}: {e}"))
        })?;
        let seqs = parse_fasta(&bytes);
        if seqs.is_empty() {
            return Err(VmError::Io(format!(
                "reference FASTA {path:?} contained no sequences (expected `>name` records)"
            )));
        }
        Ok(Some(seqs))
    }

    /// Bin the BAM into coverage windows over the (optional) region, seeking via
    /// BAI when one is present. Shared by `CALL coverage` (which surfaces the
    /// windows) and `CALL cnv` (which runs detection over them).
    fn compute_coverage_windows(
        &self,
        ds: &Dataset,
        region: &Option<crate::runtime::Region>,
        opts: &CoverageOptions,
    ) -> Result<Vec<cnvlens_core::model::CoverageWindow>, VmError> {
        let bytes = self.require_bam(ds, "coverage")?;
        match (region, ds.bai_bytes()) {
            (Some(r), Some(bai)) => {
                coverage::analyze_coverage_region(bytes, bai, &r.to_core(), opts)
            }
            _ => coverage::coverage_windows(bytes, None, opts),
        }
        .map_err(VmError::core)
    }

    fn build_coverage_options(&self) -> CoverageOptions {
        let mut o = CoverageOptions::default();
        o.window_size = 10_000;
        for (key, val) in &self.pending_params {
            match key.as_ref() {
                "window_size" => {
                    if let Some(n) = as_i64(val) {
                        o.window_size = n.max(1) as u32;
                    }
                }
                "amp_threshold" => {
                    o.amp_threshold = as_f64(val);
                    o.use_manual_thresholds = true;
                }
                "del_threshold" => {
                    o.del_threshold = as_f64(val);
                    o.use_manual_thresholds = true;
                }
                "min_windows" => o.min_windows = as_i64(val).map(|n| n.max(0) as usize),
                "segmentation_method" => {
                    if let RuntimeValue::Str(s) = val {
                        o.segmentation_method = Some(s.to_string());
                    }
                }
                _ => {}
            }
        }
        o
    }

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn require_bam<'a>(
        &self,
        ds: &'a Dataset,
        op: &'static str,
    ) -> Result<&'a [u8], VmError> {
        let _ = op;
        ds.bam_bytes().ok_or(VmError::SourceFormat {
            expected: "bam",
            got: format_label(&ds.format),
        })
    }

    fn peek_cursor(&self, pc: usize) -> Result<Arc<Mutex<Cursor>>, VmError> {
        match self.stack.last() {
            Some(RuntimeValue::Cursor(c)) => Ok(c.clone()),
            Some(other) => Err(VmError::TypeMismatch {
                expected: "cursor",
                got: other.type_name(),
                pc,
            }),
            None => Err(VmError::StackUnderflow(pc)),
        }
    }

    fn const_str(&self, idx: u16) -> Arc<str> {
        match self.program.consts.get(idx as usize) {
            Some(Value::Str(s)) => Arc::from(s.as_ref()),
            _ => Arc::from(""),
        }
    }

    /// Resolve a `FROM`/`INTO` path. A `$name` path is looked up in the VarMap
    /// (the bound value is stringified); any other path is used verbatim.
    fn resolve_path(&self, raw: &str, pc: usize) -> Result<String, VmError> {
        if let Some(name) = raw.strip_prefix('$') {
            match self.vars.get(name) {
                Some(RuntimeValue::Str(s)) => Ok(s.to_string()),
                Some(RuntimeValue::Int(n)) => Ok(n.to_string()),
                Some(RuntimeValue::Float(x)) => Ok(x.to_string()),
                Some(other) => Err(VmError::TypeMismatch {
                    expected: "string path",
                    got: other.type_name(),
                    pc,
                }),
                None => Err(VmError::UnboundVariable {
                    name: name.to_string(),
                    pc,
                }),
            }
        } else {
            Ok(raw.to_string())
        }
    }

    /// Extract a per-record sub-program (offsets relative to its own start) into
    /// a standalone [`Program`] sharing this program's constant pool, runnable
    /// from `pc = 0`.
    fn extract_subprogram(&self, off: usize, len: usize) -> Program {
        let end = (off + len).min(self.program.code.len());
        Program {
            consts: self.program.consts.clone(),
            code: self.program.code[off..end].to_vec(),
            debug: Vec::new(),
            region: None,
        }
    }

    // ── Expression interpreter (Phase 3, now record-aware) ───────────────────

    fn exec_expr(&mut self, op: OpCode) -> Result<(), VmError> {
        let pc0 = self.pc;
        self.pc += 1;
        match op {
            OpCode::LoadConst => {
                let idx = self.read_u16();
                let v = self
                    .program
                    .consts
                    .get(idx as usize)
                    .map(from_const)
                    .unwrap_or(RuntimeValue::Null);
                self.stack.push(v);
            }
            OpCode::LoadTrue => self.stack.push(RuntimeValue::Bool(true)),
            OpCode::LoadFalse => self.stack.push(RuntimeValue::Bool(false)),
            OpCode::LoadField => {
                let idx = self.read_u16();
                let v = match (&self.current_record, self.const_str(idx)) {
                    (Some(rec), name) => rec.get_field(&name),
                    (None, _) => RuntimeValue::Null,
                };
                self.stack.push(v);
            }
            OpCode::GetField => {
                let idx = self.read_u16();
                let name = self.const_str(idx);
                let obj = self.pop(pc0)?;
                let v = match obj {
                    RuntimeValue::Record(r) => r.get_field(&name),
                    _ => RuntimeValue::Null,
                };
                self.stack.push(v);
            }
            OpCode::Index => {
                let _key = self.pop(pc0)?;
                let _obj = self.pop(pc0)?;
                self.stack.push(RuntimeValue::Null);
            }
            OpCode::LoadWildcard => self.stack.push(RuntimeValue::Null),
            OpCode::LoadVar => {
                let idx = self.read_u16();
                let name = self.const_str(idx);
                match self.vars.get(&name) {
                    Some(v) => self.stack.push(v.clone()),
                    None => {
                        return Err(VmError::UnboundVariable {
                            name: name.to_string(),
                            pc: pc0,
                        })
                    }
                }
            }
            OpCode::Neg => {
                let a = self.pop(pc0)?;
                let r = match a {
                    RuntimeValue::Int(n) => RuntimeValue::Int(-n),
                    RuntimeValue::Float(x) => RuntimeValue::Float(-x),
                    other => {
                        return Err(VmError::TypeMismatch {
                            expected: "number",
                            got: other.type_name(),
                            pc: pc0,
                        })
                    }
                };
                self.stack.push(r);
            }
            OpCode::Not => {
                let a = self.pop(pc0)?;
                match a {
                    RuntimeValue::Bool(b) => self.stack.push(RuntimeValue::Bool(!b)),
                    RuntimeValue::Null => self.stack.push(RuntimeValue::Bool(true)),
                    other => {
                        return Err(VmError::TypeMismatch {
                            expected: "bool",
                            got: other.type_name(),
                            pc: pc0,
                        })
                    }
                }
            }
            OpCode::Add | OpCode::Sub | OpCode::Mul | OpCode::Div => {
                let b = self.pop(pc0)?;
                let a = self.pop(pc0)?;
                self.stack.push(arith(op, a, b, pc0)?);
            }
            OpCode::Eq | OpCode::Ne | OpCode::Lt | OpCode::Gt | OpCode::Le | OpCode::Ge => {
                let b = self.pop(pc0)?;
                let a = self.pop(pc0)?;
                self.stack.push(compare(op, a, b, pc0)?);
            }
            OpCode::And => {
                let jmp = self.read_u16() as usize;
                let top = self.peek(pc0)?;
                if !top.is_truthy() {
                    self.pc = jmp;
                } else {
                    self.pop(pc0)?;
                }
            }
            OpCode::Or => {
                let jmp = self.read_u16() as usize;
                let top = self.peek(pc0)?;
                if top.is_truthy() {
                    self.pc = jmp;
                } else {
                    self.pop(pc0)?;
                }
            }
            OpCode::JumpIfFalse => {
                let target = self.read_u16() as usize;
                let c = self.pop(pc0)?;
                if !c.is_truthy() {
                    self.pc = target;
                }
            }
            OpCode::Jump => {
                let target = self.read_u16() as usize;
                self.pc = target;
            }
            OpCode::CallFn => {
                let name_idx = self.read_u16();
                let argc = self.read_u8() as usize;
                let name = self.const_str(name_idx);
                // Args were compiled left-to-right, so the last argument sits on
                // top of the stack. Pop them and flip back into call order.
                let mut args = Vec::with_capacity(argc);
                for _ in 0..argc {
                    args.push(self.pop(pc0)?);
                }
                args.reverse();
                let v = call_builtin(name.as_ref(), &args, pc0)?;
                self.stack.push(v);
            }
            other => return Err(VmError::NotYetImplemented(other.name().to_string())),
        }
        Ok(())
    }

    fn read_u8(&mut self) -> u8 {
        let v = self.program.code[self.pc];
        self.pc += 1;
        v
    }

    fn read_u16(&mut self) -> u16 {
        let v = u16::from_le_bytes([self.program.code[self.pc], self.program.code[self.pc + 1]]);
        self.pc += 2;
        v
    }

    fn pop(&mut self, pc: usize) -> Result<RuntimeValue, VmError> {
        self.stack.pop().ok_or(VmError::StackUnderflow(pc))
    }

    fn peek(&self, pc: usize) -> Result<RuntimeValue, VmError> {
        self.stack.last().cloned().ok_or(VmError::StackUnderflow(pc))
    }
}

// ── Free helpers ─────────────────────────────────────────────────────────────

fn from_const(v: &Value) -> RuntimeValue {
    match v {
        Value::Int(n) => RuntimeValue::Int(*n),
        Value::Float(x) => RuntimeValue::Float(*x),
        Value::Str(s) => RuntimeValue::Str(Arc::from(s.as_ref())),
        Value::Bool(b) => RuntimeValue::Bool(*b),
        Value::Null => RuntimeValue::Null,
    }
}

// `as_i64`/`as_f64` also parse string-typed variable values, so `$min_af`
// supplied as the string "0.05" (e.g. an untyped CLI arg) coerces to a number
// where a numeric param is expected (the SET_PARAM coercion rule).
fn as_i64(v: &RuntimeValue) -> Option<i64> {
    match v {
        RuntimeValue::Int(n) => Some(*n),
        RuntimeValue::Float(x) => Some(*x as i64),
        RuntimeValue::Str(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn as_f64(v: &RuntimeValue) -> Option<f64> {
    match v {
        RuntimeValue::Int(n) => Some(*n as f64),
        RuntimeValue::Float(x) => Some(*x),
        RuntimeValue::Str(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Region-sharded wrapper around [`variant_producer`] (Track 2).
///
/// When the query pushed down a bounded BAM region big enough to be worth it
/// (see [`crate::shard::plan_shard_count`]) and no `LIMIT` is in play, this
/// splits the region into boundary-correct shards and runs the per-shard pileup
/// across native threads, merging back to the **exact same** record stream the
/// serial path would produce. Every other case — a VCF, a `LIMIT` (whose
/// early-exit must stay serial), an unbounded/small region, a single core, or
/// `SPLICE_SHARDS=1` — falls straight through to the serial producer. Threading
/// is a speed enhancement here, never load-bearing.
fn sharded_variant_producer(
    ds: &Dataset,
    opts: &VariantOptions,
    region: Option<&cnvlens_core::model::Region>,
    is_vcf: bool,
    limit: Option<usize>,
) -> Result<Vec<Record>, CoreError> {
    let serial = || variant_producer(ds, opts, region, is_vcf, limit);

    // LIMIT keeps the serial early-exit; VCF has its own streaming path. Sharding
    // also needs a BAM + BAI to do per-shard random access.
    if is_vcf || limit.is_some() {
        return serial();
    }
    let (Some(bam), Some(bai)) = (ds.bam_bytes(), ds.bai_bytes()) else {
        return serial();
    };
    let shards = match plan_variant_shards(region) {
        Some(s) => s,
        None => return serial(),
    };

    // Shard over plain `Variant` payloads (which are `Send`), not `Record`
    // (which can wrap a non-`Send` `Cursor`). The boundary-correct clamp keeps
    // each variant in exactly one shard; we wrap into records after merging.
    //
    // Executor by target: native uses scoped OS threads; wasm32 has no in-module
    // threads, so it uses the serial `WasmShardExecutor` (parallelism in the
    // browser is the JS worker pool re-entering the exported per-shard function —
    // see `crates/codonsplice-wasm/js/shard-pool.js`, never `std::thread`). This
    // also means `std::thread::scope` is never *reached* on wasm at runtime.
    #[cfg(not(target_arch = "wasm32"))]
    let executor = crate::shard::NativeThreadExecutor;
    #[cfg(target_arch = "wasm32")]
    let executor = crate::shard::WasmShardExecutor::default();
    let merged = crate::shard::shard_and_merge(
        &executor,
        &shards,
        |s: &crate::shard::Shard| {
            variants::call_variants_region(bam, bai, &s.to_core_region().to_core(), opts)
        },
        |v: &cnvlens_core::model::Variant| v.pos,
    )?;
    Ok(merged.into_iter().map(Record::Variant).collect())
}

/// Plan the shard split for a variant region, honouring the `SPLICE_SHARDS`
/// override (`0`/`1` forces serial; `N` requests up to N-way). Returns `None`
/// when the region is unbounded, empty, or too small to shard — i.e. run serial.
fn plan_variant_shards(
    region: Option<&cnvlens_core::model::Region>,
) -> Option<Vec<crate::shard::Shard>> {
    let r = region?;
    let (start, end) = (r.start?, r.end?);
    if end <= start {
        return None;
    }
    // In-module parallelism. Native: env override or core count. Wasm32: a lone
    // wasm instance is single-threaded, so in-module sharding is always serial
    // (parallelism is the JS worker pool calling the exported per-shard function,
    // outside this VM instance). Forcing `available = 1` here makes
    // `plan_shard_count` return 1 → serial, the load-bearing fallback.
    #[cfg(not(target_arch = "wasm32"))]
    let available = match std::env::var("SPLICE_SHARDS").ok().and_then(|s| s.parse::<usize>().ok()) {
        Some(n) => n,
        None => std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
    };
    #[cfg(target_arch = "wasm32")]
    let available = 1usize;
    let n = crate::shard::plan_shard_count(end - start + 1, available);
    if n <= 1 {
        return None;
    }
    Some(crate::shard::split_region(&r.chrom, start, end, n))
}

/// Produce variant records for `CALL_VARIANTS`, dispatching by source format.
/// A VCF streams its already-called variants (region-filtered); a BAM runs the
/// streaming pileup with the early-exit `limit`.
fn variant_producer(
    ds: &Dataset,
    opts: &VariantOptions,
    region: Option<&cnvlens_core::model::Region>,
    is_vcf: bool,
    limit: Option<usize>,
) -> Result<Vec<Record>, CoreError> {
    if is_vcf {
        let (bytes, split) = match &ds.data {
            DatasetInner::Vcf { bytes, split } => (bytes, *split),
            _ => unreachable!("is_vcf implies a VCF dataset"),
        };
        let mut out = Vec::new();
        for v in vcf::stream_vcf(bytes, region, split) {
            out.push(Record::Variant(v?));
            if let Some(l) = limit {
                if out.len() >= l {
                    break;
                }
            }
        }
        Ok(out)
    } else {
        let bytes = ds.bam_bytes().expect("BAM checked at CALL time");
        let vars: Vec<_> = variants::stream_variants(bytes, ds.bai_bytes(), opts, region, limit)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(vars.into_iter().map(Record::Variant).collect())
    }
}

/// Collect read records, optionally BAI-seeked, annotating each with its
/// chromosome name and a pileup depth (number of collected reads covering its
/// start position).
fn collect_read_records(
    bytes: &[u8],
    bai: Option<&[u8]>,
    region: &Option<crate::runtime::Region>,
) -> Result<Vec<Record>, VmError> {
    let header = bam::read_header(bytes).map_err(VmError::io)?;
    let refs = reference_list(&header);

    let mut reads: Vec<AlnRecord> = Vec::new();
    match (region, bai) {
        (Some(r), Some(bai_bytes)) => {
            bam::for_each_region_full(bytes, bai_bytes, &r.to_core(), |a| reads.push(a))
                .map_err(VmError::io)?;
        }
        _ => {
            bam::for_each_full(bytes, |a| reads.push(a)).map_err(VmError::io)?;
        }
    }

    // Pileup depth at each (ref, position) over the collected read set.
    let mut depth: HashMap<(i32, i64), i64> = HashMap::new();
    for a in &reads {
        let span = a.seq.len() as i64;
        for p in a.pos..a.pos + span {
            *depth.entry((a.ref_id, p)).or_default() += 1;
        }
    }

    Ok(reads
        .into_iter()
        .map(|a| {
            let chrom = refs
                .get(a.ref_id as usize)
                .map(|(n, _)| n.clone())
                .unwrap_or_default();
            let d = *depth.get(&(a.ref_id, a.pos)).unwrap_or(&0);
            Record::Alignment(AlnRow {
                aln: a,
                chrom,
                depth: d,
            })
        })
        .collect())
}

/// Parse the contig name and 0-based genomic offset from a FASTA header token.
///
/// A `samtools faidx`-style region slice has a `contig:start-end` header (e.g.
/// `>7:54990000-55300100`); the engine indexes the reference by *absolute*
/// genomic position to match the BAM, so such a slice maps to contig `7` with
/// the sequence offset to 0-based `start-1`. A plain `>7` (full contig) is offset
/// 0. Returns `(contig, offset)`.
fn parse_fasta_header(token: &str) -> (String, usize) {
    if let Some((contig, range)) = token.rsplit_once(':') {
        if let Some((start, end)) = range.split_once('-') {
            let start = start.replace(',', "");
            let end = end.replace(',', "");
            if start.chars().all(|c| c.is_ascii_digit())
                && end.chars().all(|c| c.is_ascii_digit())
                && !start.is_empty()
            {
                if let Ok(s) = start.parse::<usize>() {
                    if s >= 1 {
                        return (contig.to_string(), s - 1);
                    }
                }
            }
        }
    }
    (token.to_string(), 0)
}

/// Parse a FASTA byte buffer into `contig -> sequence`, where the sequence is
/// indexed by absolute 0-based genomic position. A `contig:start-end` region
/// slice (from `samtools faidx`) is left-padded with `N` up to `start-1` so a
/// small slice (e.g. ~300 KB over the EGFR region) stays coordinate-correct
/// against the BAM — identical calls to the full-chromosome reference, without
/// shipping the 161 MB chr7.fa. (The padding inflates a region slice in memory;
/// a future optimization could offset-index in cnvlens-core to avoid it.)
fn parse_fasta(bytes: &[u8]) -> HashMap<String, String> {
    let mut seqs = HashMap::new();
    let text = String::from_utf8_lossy(bytes);
    let mut name = String::new();
    let mut offset: usize = 0;
    let mut seq = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix('>') {
            if !name.is_empty() {
                let full = pad_to_offset(offset, std::mem::take(&mut seq));
                seqs.insert(std::mem::take(&mut name), full);
            }
            let token = rest.split_whitespace().next().unwrap_or("");
            let (contig, off) = parse_fasta_header(token);
            name = contig;
            offset = off;
        } else {
            seq.push_str(line.trim());
        }
    }
    if !name.is_empty() {
        seqs.insert(name, pad_to_offset(offset, seq));
    }
    seqs
}

/// Left-pad `seq` with `offset` `N` bases so it is indexed by absolute genomic
/// position (offset 0 → unchanged).
fn pad_to_offset(offset: usize, seq: String) -> String {
    if offset == 0 {
        return seq;
    }
    let mut full = String::with_capacity(offset + seq.len());
    full.extend(std::iter::repeat('N').take(offset));
    full.push_str(&seq);
    full
}

fn format_header(refs: &[(String, usize)]) -> String {
    let mut out = String::from("reference sequences:\n");
    for (name, len) in refs {
        out.push_str(&format!("  {name}\t{len}\n"));
    }
    out
}

/// Serialize materialized records into the target output format's bytes.
/// The `(name, length)` contigs of a dataset, for VCF `##contig` headers. Reads
/// them from the BAM header when the source is a BAM; empty otherwise (the writer
/// then emits `##contig=<ID=…>` without a length).
fn contigs_from_dataset(ds: &Dataset) -> Vec<(String, u64)> {
    match ds.bam_bytes() {
        Some(bytes) => match bam::read_header(bytes) {
            Ok(header) => reference_list(&header)
                .into_iter()
                .map(|(name, len)| (name, len as u64))
                .collect(),
            Err(_) => Vec::new(),
        },
        None => Vec::new(),
    }
}

fn serialize_records(
    fmt: Format,
    records: &[Record],
    contigs: &[(String, u64)],
) -> Result<Vec<u8>, VmError> {
    let label = format_label(&fmt);
    match fmt {
        Format::Vcf => Ok(records_to_vcf(records, contigs).into_bytes()),
        Format::Bed => Ok(records_to_bed(records).into_bytes()),
        // Real sinks: NDJSON (one object per line) and TSV (header + rows).
        Format::Json => Ok(records_to_ndjson(records).into_bytes()),
        Format::Tsv => Ok(records_to_tsv(records).into_bytes()),
        // `INTO fasta` predates the `json` token and is kept as the JSON-array
        // sink for backward compatibility.
        Format::Fasta => Ok(records_to_json(records).into_bytes()),
        Format::Bam | Format::Cram => Err(VmError::UnsupportedInto(label.to_string())),
    }
}

/// Records → newline-delimited JSON (NDJSON / JSON Lines): one JSON object per
/// line, so large streams can be written without buffering a whole array.
pub fn records_to_ndjson(records: &[Record]) -> String {
    let mut out = String::new();
    for r in records {
        out.push_str(&record_to_json(r).to_string());
        out.push('\n');
    }
    out
}

/// Records → TSV: a header row of column names, then one tab-separated row per
/// record. Columns are the first-seen union of keys across records; missing
/// values render empty. Tabs/newlines inside string values are sanitized to
/// spaces so the grid stays well-formed.
pub fn records_to_tsv(records: &[Record]) -> String {
    use serde_json::Value as J;

    // Flatten each record to a JSON object (non-objects get a single "value" key).
    let objs: Vec<serde_json::Map<String, J>> = records
        .iter()
        .map(|r| match record_to_json(r) {
            J::Object(m) => m,
            other => {
                let mut m = serde_json::Map::new();
                m.insert("value".to_string(), other);
                m
            }
        })
        .collect();

    // First-seen union of column names.
    let mut cols: Vec<String> = Vec::new();
    for o in &objs {
        for k in o.keys() {
            if !cols.iter().any(|c| c == k) {
                cols.push(k.clone());
            }
        }
    }

    let cell = |v: Option<&J>| -> String {
        let s = match v {
            Some(J::String(s)) => s.clone(),
            Some(J::Null) | None => String::new(),
            Some(other) => other.to_string(),
        };
        s.replace('\t', " ").replace('\n', " ")
    };

    let mut out = String::new();
    if cols.is_empty() {
        return out;
    }
    out.push_str(&cols.join("\t"));
    out.push('\n');
    for o in &objs {
        let row: Vec<String> = cols.iter().map(|c| cell(o.get(c))).collect();
        out.push_str(&row.join("\t"));
        out.push('\n');
    }
    out
}

/// Run copy-number detection over coverage windows, dispatching the same way
/// cnvlens-core does for its UI: manual thresholds when the user supplied
/// `amp_threshold`/`del_threshold`, otherwise CBS-lite or the adaptive
/// threshold scan keyed on the region's coverage class. Returns one JSON object
/// per CNV call (see [`normalize_cnv`]).
fn detect_cnvs(
    windows: &[cnvlens_core::model::CoverageWindow],
    opts: &CoverageOptions,
) -> Vec<serde_json::Value> {
    use cnvlens_core::cnv;
    if opts.use_manual_thresholds {
        cnv::detect_cnvs_manual(
            windows,
            opts.amp_threshold.unwrap_or(1.5),
            opts.del_threshold.unwrap_or(0.5),
            opts.min_windows.unwrap_or(3),
        )
    } else {
        let class = coverage_class(windows);
        match opts.segmentation_method.as_deref() {
            Some("cbs_lite") | Some("cbs") => cnv::detect_cnvs_cbs_lite(windows, class, 3, 3.0),
            _ => cnv::detect_cnvs_adaptive(windows, class),
        }
    }
}

/// Coverage class from window depths — mirrors `cnvlens_core::coverage`'s
/// median-based banding so adaptive/CBS detection picks the same thresholds.
fn coverage_class(windows: &[cnvlens_core::model::CoverageWindow]) -> &'static str {
    let nonzero: Vec<f64> = windows
        .iter()
        .filter(|w| w.coverage > 0)
        .map(|w| w.coverage as f64)
        .collect();
    if nonzero.is_empty() {
        return "high";
    }
    let med = cnvlens_core::stats::median(&nonzero);
    if med < 15.0 {
        "low"
    } else if med < 30.0 {
        "medium"
    } else {
        "high"
    }
}

/// Normalize a cnvlens CNV summary to the snake_case schema the rest of the
/// engine speaks (the UI JSON mixes `copyNumber`/`avgCoverage` camelCase with
/// snake_case keys). Keys absent from a given detector (e.g. `t_statistic`) are
/// dropped rather than emitted as null.
fn normalize_cnv(v: serde_json::Value) -> serde_json::Value {
    use serde_json::Value as J;
    let mut out = serde_json::Map::new();
    let mut put = |out_key: &str, src: Option<&J>| {
        if let Some(val) = src {
            if !val.is_null() {
                out.insert(out_key.to_string(), val.clone());
            }
        }
    };
    put("chrom", v.get("chromosome"));
    put("start", v.get("start"));
    put("end", v.get("end"));
    put("length", v.get("length"));
    put("type", v.get("type"));
    put("copy_number", v.get("copyNumber"));
    put("avg_coverage", v.get("avgCoverage"));
    put("confidence", v.get("confidence"));
    put("num_windows", v.get("num_windows"));
    put("t_statistic", v.get("t_statistic"));
    J::Object(out)
}

/// Records → JSON array (lossless; used for roundtrip + the WASM bridge).
pub fn records_to_json(records: &[Record]) -> String {
    let arr: Vec<serde_json::Value> = records.iter().map(record_to_json).collect();
    serde_json::Value::Array(arr).to_string()
}

pub fn record_to_json(r: &Record) -> serde_json::Value {
    use serde_json::json;
    match r {
        Record::Variant(v) => serde_json::to_value(v).unwrap_or(json!({})),
        Record::CoverageWindow(w) => serde_json::to_value(w).unwrap_or(json!({})),
        Record::Cnv(v) => v.clone(),
        Record::Alignment(a) => json!({
            "chrom": a.chrom,
            "pos": a.pos_1based(), // 1-based (SAM POS), #19
            "mapq": a.aln.mapq,
            "flag": a.aln.flag,
            "depth": a.depth,
        }),
        Record::Header(refs) => {
            let list: Vec<serde_json::Value> = refs
                .iter()
                .map(|(n, l)| json!({ "name": n, "length": l }))
                .collect();
            json!({ "references": list })
        }
        Record::Row(cols) => {
            let mut map = serde_json::Map::new();
            for (k, v) in cols {
                map.insert(k.clone(), runtime_to_json(v));
            }
            serde_json::Value::Object(map)
        }
    }
}

/// Convert a [`RuntimeValue`] scalar to JSON (handles map to null).
fn runtime_to_json(v: &RuntimeValue) -> serde_json::Value {
    use serde_json::Value as J;
    match v {
        RuntimeValue::Int(n) => J::from(*n),
        RuntimeValue::Float(x) => serde_json::Number::from_f64(*x)
            .map(J::Number)
            .unwrap_or(J::Null),
        RuntimeValue::Str(s) => J::String(s.to_string()),
        RuntimeValue::Bool(b) => J::Bool(*b),
        _ => J::Null,
    }
}

fn records_to_vcf(records: &[Record], contigs: &[(String, u64)]) -> String {
    // Native fast path: a homogeneous variant stream renders as a standard VCF.
    if records.iter().all(|r| matches!(r, Record::Variant(_))) {
        let mut out = String::from("##fileformat=VCFv4.2\n");
        // ##contig lines for every contig referenced by the records, in
        // first-seen order, with `length` from the source header when known.
        // bcftools norm requires these (and the ##INFO declarations below) — an
        // output that uses DP/AF in INFO without declaring them is not spec
        // compliant and breaks downstream tools.
        let mut seen: Vec<&str> = Vec::new();
        for r in records {
            if let Record::Variant(v) = r {
                if !seen.contains(&v.chrom.as_str()) {
                    seen.push(v.chrom.as_str());
                }
            }
        }
        for c in &seen {
            match contigs.iter().find(|(n, _)| n == c) {
                Some((n, len)) => out.push_str(&format!("##contig=<ID={n},length={len}>\n")),
                None => out.push_str(&format!("##contig=<ID={c}>\n")),
            }
        }
        out.push_str(
            "##INFO=<ID=DP,Number=1,Type=Integer,Description=\"Total read depth at the site\">\n",
        );
        out.push_str(
            "##INFO=<ID=AF,Number=A,Type=Float,Description=\"Alternate allele frequency\">\n",
        );
        out.push_str("#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO\n");
        for r in records {
            if let Record::Variant(v) = r {
                // Preserve the ID and FILTER columns the reader captured (a
                // VCF->VCF round-trip must not lose them); pileup-called
                // variants leave both `None`, rendering as the VCF defaults
                // "." and "PASS".
                let id = v.id.as_deref().filter(|s| !s.is_empty()).unwrap_or(".");
                let filter = v.filter.as_deref().filter(|s| !s.is_empty()).unwrap_or("PASS");
                out.push_str(&format!(
                    "{}\t{}\t{}\t{}\t{}\t{:.1}\t{}\tDP={};AF={:.4}\n",
                    v.chrom, v.pos, id, v.ref_base, v.alt, v.qual, filter, v.depth, v.allele_freq
                ));
            }
        }
        return out;
    }
    // Anything else — projected `SELECT` rows, coverage windows (CALL coverage),
    // alignments (CALL reads), or a mixed batch — is serialized via each record's
    // natural columns as a custom-FORMAT VCF, so no record kind is silently
    // dropped while still being counted in `wrote N record(s)`. See #1, #3.
    let rows: Vec<Record> = records.iter().cloned().map(Record::into_row).collect();
    projected_rows_to_vcf(&rows)
}

/// Column names that map to one of the eight fixed VCF fields. Anything else in
/// a projected row is carried in `INFO` as a declared `KEY=value` pair.
fn vcf_canonical(col: &str) -> bool {
    matches!(
        col,
        "chrom" | "chr" | "pos" | "id" | "ref" | "ref_base" | "alt" | "qual" | "filter"
    )
}

/// Serialize projected `SELECT` rows (`Record::Row`) as a custom-FORMAT VCF:
/// canonical columns fill the eight fixed VCF fields, every other projected
/// column is declared with an `##INFO` line and packed into `INFO`. Lossless for
/// arbitrary projections, so the body row count matches the `wrote N` summary.
fn projected_rows_to_vcf(records: &[Record]) -> String {
    // Column order comes from the first projected row; rows missing a column
    // emit "." for it.
    let columns: Vec<String> = records
        .iter()
        .find_map(|r| match r {
            Record::Row(cols) => Some(cols.iter().map(|(k, _)| k.clone()).collect()),
            _ => None,
        })
        .unwrap_or_default();
    let info_cols: Vec<String> = columns.into_iter().filter(|c| !vcf_canonical(c)).collect();

    let mut out = String::from("##fileformat=VCFv4.2\n");
    out.push_str("##source=SpliceQL projected SELECT\n");
    for col in &info_cols {
        out.push_str(&format!(
            "##INFO=<ID={col},Number=1,Type={},Description=\"SpliceQL projected column\">\n",
            info_type(records, col)
        ));
    }
    out.push_str("#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO\n");

    for r in records {
        let cols = match r {
            Record::Row(cols) => cols,
            _ => continue,
        };
        let field = |names: &[&str], default: &str| -> String {
            names
                .iter()
                .find_map(|n| cols.iter().find(|(k, _)| k == n).map(|(_, v)| fmt_field(v)))
                .unwrap_or_else(|| default.to_string())
        };
        let info = if info_cols.is_empty() {
            ".".to_string()
        } else {
            info_cols
                .iter()
                .map(|c| {
                    let v = cols.iter().find(|(k, _)| k == c).map(|(_, v)| fmt_field(v));
                    format!("{c}={}", v.unwrap_or_else(|| ".".to_string()))
                })
                .collect::<Vec<_>>()
                .join(";")
        };
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{info}\n",
            field(&["chrom", "chr"], "."),
            field(&["pos"], "."),
            field(&["id"], "."),
            field(&["ref", "ref_base"], "."),
            field(&["alt"], "."),
            field(&["qual"], "."),
            field(&["filter"], "PASS"),
        ));
    }
    out
}

/// Infer the VCF `INFO` Type for a projected column from the first row that
/// carries a non-null value (defaults to `String`).
fn info_type(records: &[Record], col: &str) -> &'static str {
    for r in records {
        if let Record::Row(cols) = r {
            if let Some((_, v)) = cols.iter().find(|(k, _)| k == col) {
                match v {
                    RuntimeValue::Int(_) => return "Integer",
                    RuntimeValue::Float(_) => return "Float",
                    RuntimeValue::Str(_) | RuntimeValue::Bool(_) => return "String",
                    _ => continue,
                }
            }
        }
    }
    "String"
}

/// Format a projected scalar for a VCF field. `Null`/non-scalar → ".".
fn fmt_field(v: &RuntimeValue) -> String {
    match v {
        RuntimeValue::Int(n) => n.to_string(),
        RuntimeValue::Float(x) => {
            if x.is_finite() && *x == x.trunc() {
                format!("{x:.1}")
            } else {
                let s = format!("{x:.4}");
                s.trim_end_matches('0').trim_end_matches('.').to_string()
            }
        }
        RuntimeValue::Str(s) => s.to_string(),
        RuntimeValue::Bool(b) => b.to_string(),
        _ => ".".to_string(),
    }
}

fn records_to_bed(records: &[Record]) -> String {
    // Native fast path: a stream of coverage windows / variants.
    if records
        .iter()
        .all(|r| matches!(r, Record::CoverageWindow(_) | Record::Variant(_)))
    {
        let mut out = String::new();
        for r in records {
            match r {
                Record::CoverageWindow(w) => out.push_str(&format!(
                    "{}\t{}\t{}\t{:.4}\n",
                    w.chromosome, w.start, w.end, w.normalized
                )),
                Record::Variant(v) => {
                    out.push_str(&format!("{}\t{}\t{}\t{}\n", v.chrom, v.pos - 1, v.pos, v.alt))
                }
                _ => {}
            }
        }
        return out;
    }
    // Projected `SELECT` rows, alignments (CALL reads), or a mixed batch: derive
    // chrom/start/end from each record's natural columns instead of silently
    // dropping them. See #1, #3.
    let rows: Vec<Record> = records.iter().cloned().map(Record::into_row).collect();
    projected_rows_to_bed(&rows)
}

/// Column names consumed to build the three fixed BED fields. Anything else in a
/// projected row is appended as an extra (named in the `#` comment header).
fn bed_consumed(col: &str) -> bool {
    matches!(col, "chrom" | "chr" | "start" | "end" | "pos")
}

/// Serialize projected rows as BED: `chrom`/`chr` → col 1, `start` (or `pos`-1,
/// 0-based) → col 2, `end` (or `pos`) → col 3, and every remaining projected
/// column appended as an extra field. A leading `#` comment names the columns.
fn projected_rows_to_bed(records: &[Record]) -> String {
    let columns: Vec<String> = records
        .iter()
        .find_map(|r| match r {
            Record::Row(cols) => Some(cols.iter().map(|(k, _)| k.clone()).collect()),
            _ => None,
        })
        .unwrap_or_default();
    let extra: Vec<String> = columns.into_iter().filter(|c| !bed_consumed(c)).collect();

    let mut out = String::from("#chrom\tstart\tend");
    for c in &extra {
        out.push('\t');
        out.push_str(c);
    }
    out.push('\n');

    for r in records {
        let cols = match r {
            Record::Row(cols) => cols,
            _ => continue,
        };
        let get = |names: &[&str]| -> Option<&RuntimeValue> {
            names
                .iter()
                .find_map(|n| cols.iter().find(|(k, _)| k == n).map(|(_, v)| v))
        };
        let chrom = get(&["chrom", "chr"]).map(fmt_field).unwrap_or_else(|| ".".to_string());
        // BED is 0-based half-open: derive [pos-1, pos) when only `pos` is present.
        let (start, end) = match (get(&["start"]), get(&["end"])) {
            (Some(s), Some(e)) => (fmt_field(s), fmt_field(e)),
            (Some(s), None) => (fmt_field(s), ".".to_string()),
            (None, Some(e)) => (".".to_string(), fmt_field(e)),
            (None, None) => match get(&["pos"]).and_then(as_i64) {
                Some(p) => ((p - 1).to_string(), p.to_string()),
                None => (".".to_string(), ".".to_string()),
            },
        };
        out.push_str(&format!("{chrom}\t{start}\t{end}"));
        for c in &extra {
            let v = cols
                .iter()
                .find(|(k, _)| k == c)
                .map(|(_, v)| fmt_field(v))
                .unwrap_or_else(|| ".".to_string());
            out.push('\t');
            out.push_str(&v);
        }
        out.push('\n');
    }
    out
}

/// Arithmetic: integer when both operands are integers, otherwise float.
fn arith(op: OpCode, a: RuntimeValue, b: RuntimeValue, pc: usize) -> Result<RuntimeValue, VmError> {
    use RuntimeValue::{Float, Int};
    match (&a, &b) {
        (Int(x), Int(y)) => {
            let r = match op {
                OpCode::Add => x.wrapping_add(*y),
                OpCode::Sub => x.wrapping_sub(*y),
                OpCode::Mul => x.wrapping_mul(*y),
                OpCode::Div => {
                    if *y == 0 {
                        return Err(VmError::TypeMismatch {
                            expected: "nonzero divisor",
                            got: "zero",
                            pc,
                        });
                    }
                    x.wrapping_div(*y)
                }
                _ => unreachable!(),
            };
            Ok(Int(r))
        }
        _ => {
            let (x, y) = match (a.as_f64(), b.as_f64()) {
                (Some(x), Some(y)) => (x, y),
                _ => {
                    let bad = if a.as_f64().is_none() { &a } else { &b };
                    return Err(VmError::TypeMismatch {
                        expected: "number",
                        got: bad.type_name(),
                        pc,
                    });
                }
            };
            let r = match op {
                OpCode::Add => x + y,
                OpCode::Sub => x - y,
                OpCode::Mul => x * y,
                OpCode::Div => x / y,
                _ => unreachable!(),
            };
            Ok(Float(r))
        }
    }
}

/// Comparison: `EQ`/`NE` work on any matching pair; ordering operators require
/// numbers.
fn compare(
    op: OpCode,
    a: RuntimeValue,
    b: RuntimeValue,
    pc: usize,
) -> Result<RuntimeValue, VmError> {
    use RuntimeValue::Bool;

    if matches!(op, OpCode::Eq | OpCode::Ne) {
        let eq = a == b;
        return Ok(Bool(if op == OpCode::Eq { eq } else { !eq }));
    }

    let (x, y) = match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => (x, y),
        _ => {
            let bad = if a.as_f64().is_none() { &a } else { &b };
            return Err(VmError::TypeMismatch {
                expected: "number",
                got: bad.type_name(),
                pc,
            });
        }
    };
    let r = match op {
        OpCode::Lt => x < y,
        OpCode::Gt => x > y,
        OpCode::Le => x <= y,
        OpCode::Ge => x >= y,
        _ => unreachable!(),
    };
    Ok(Bool(r))
}

// ── builtin functions (CALL_FN) ──────────────────────────────────────────────
//
// SpliceQL function calls (`abs(x)`, `gc(seq)`, …) compile to a `CALL_FN`
// opcode; the VM pops the args (in call order) and dispatches here. Three
// families: scalar/math, string, and genomic. Unknown names, wrong arity, or
// mistyped arguments return a `VmError::Builtin` carrying a printable message.

/// Stringify a value for `concat()` (Null → empty string).
fn display_value(v: &RuntimeValue) -> String {
    match v {
        RuntimeValue::Int(n) => n.to_string(),
        RuntimeValue::Float(x) => x.to_string(),
        RuntimeValue::Str(s) => s.to_string(),
        RuntimeValue::Bool(b) => b.to_string(),
        RuntimeValue::Null => String::new(),
        other => other.type_name().to_string(),
    }
}

/// Fraction of G/C among A/C/G/T bases (other symbols ignored); 0.0 if none.
fn gc_fraction(seq: &str) -> f64 {
    let (mut gc, mut at) = (0u64, 0u64);
    for b in seq.bytes() {
        match b.to_ascii_uppercase() {
            b'G' | b'C' => gc += 1,
            b'A' | b'T' => at += 1,
            _ => {}
        }
    }
    let denom = gc + at;
    if denom == 0 {
        0.0
    } else {
        gc as f64 / denom as f64
    }
}

/// Reverse complement of a DNA string (uppercased; N and unknowns pass through).
fn revcomp(seq: &str) -> String {
    seq.chars()
        .rev()
        .map(|c| match c.to_ascii_uppercase() {
            'A' => 'T',
            'T' | 'U' => 'A',
            'C' => 'G',
            'G' => 'C',
            other => other,
        })
        .collect()
}

/// Map a base to its index in T,C,A,G order (the layout of the codon table).
fn base_idx(b: u8) -> Option<usize> {
    match b.to_ascii_uppercase() {
        b'T' | b'U' => Some(0),
        b'C' => Some(1),
        b'A' => Some(2),
        b'G' => Some(3),
        _ => None,
    }
}

/// Single-letter amino acid for a DNA codon (NCBI table 1). `*` = stop, `X` =
/// codon containing a non-ACGT base.
fn codon_aa(c: &[u8]) -> char {
    // Amino acids indexed by base1*16 + base2*4 + base3, each base in T,C,A,G order.
    const AAS: &[u8] = b"FFLLSSSSYY**CC*WLLLLPPPPHHQQRRRRIIIMTTTTNNKKSSRRVVVVAAAADDEEGGGG";
    match (base_idx(c[0]), base_idx(c[1]), base_idx(c[2])) {
        (Some(a), Some(b), Some(d)) => AAS[a * 16 + b * 4 + d] as char,
        _ => 'X',
    }
}

/// Translate DNA to a single-letter amino-acid string from `frame` (0/1/2); a
/// trailing partial codon is dropped.
fn translate_dna(seq: &str, frame: usize) -> String {
    let bases: Vec<u8> = seq.bytes().collect();
    let mut aa = String::new();
    let mut i = frame;
    while i + 3 <= bases.len() {
        aa.push(codon_aa(&bases[i..i + 3]));
        i += 3;
    }
    aa
}

/// Dispatch a builtin by name. `args` are in call order; `_pc` is kept for
/// parity with the other VM helpers and future span-aware errors.
fn call_builtin(name: &str, args: &[RuntimeValue], _pc: usize) -> Result<RuntimeValue, VmError> {
    use RuntimeValue::*;
    let bad = |m: String| VmError::Builtin(m);
    let arity = |n: usize| -> Result<(), VmError> {
        if args.len() == n {
            Ok(())
        } else {
            Err(VmError::Builtin(format!(
                "{name}() takes {n} argument(s), got {}",
                args.len()
            )))
        }
    };
    // Numeric argument (coerces ints/floats/numeric strings), else a type error.
    let num = |i: usize| -> Result<f64, VmError> {
        as_f64(&args[i]).ok_or_else(|| {
            VmError::Builtin(format!(
                "{name}(): argument {} must be a number, got {}",
                i + 1,
                args[i].type_name()
            ))
        })
    };
    // String argument (must be an actual string).
    let text = |i: usize| -> Result<std::sync::Arc<str>, VmError> {
        match &args[i] {
            Str(s) => Ok(s.clone()),
            other => Err(VmError::Builtin(format!(
                "{name}(): argument {} must be a string, got {}",
                i + 1,
                other.type_name()
            ))),
        }
    };

    match name {
        // ── scalar / math ────────────────────────────────────────────────
        "abs" => {
            arity(1)?;
            Ok(match &args[0] {
                Int(n) => Int(n.abs()),
                Float(x) => Float(x.abs()),
                other => {
                    return Err(bad(format!(
                        "abs(): argument must be a number, got {}",
                        other.type_name()
                    )))
                }
            })
        }
        "floor" => {
            arity(1)?;
            Ok(Int(num(0)?.floor() as i64))
        }
        "ceil" => {
            arity(1)?;
            Ok(Int(num(0)?.ceil() as i64))
        }
        "sqrt" => {
            arity(1)?;
            Ok(Float(num(0)?.sqrt()))
        }
        "pow" => {
            arity(2)?;
            Ok(Float(num(0)?.powf(num(1)?)))
        }
        "round" => match args.len() {
            1 => Ok(Int(num(0)?.round() as i64)),
            2 => {
                let p = as_i64(&args[1]).unwrap_or(0).max(0) as i32;
                let f = 10f64.powi(p);
                Ok(Float((num(0)? * f).round() / f))
            }
            n => Err(bad(format!("round() takes 1 or 2 arguments, got {n}"))),
        },
        "log" => match args.len() {
            1 => Ok(Float(num(0)?.ln())),
            2 => Ok(Float(num(0)?.log(num(1)?))),
            n => Err(bad(format!("log() takes 1 or 2 arguments, got {n}"))),
        },
        "min" | "max" => {
            if args.is_empty() {
                return Err(bad(format!("{name}() needs at least 1 argument")));
            }
            let want_min = name == "min";
            if args.iter().all(|a| matches!(a, Int(_))) {
                let mut acc = as_i64(&args[0]).unwrap();
                for a in &args[1..] {
                    let v = as_i64(a).unwrap();
                    acc = if want_min { acc.min(v) } else { acc.max(v) };
                }
                Ok(Int(acc))
            } else {
                let mut acc = num(0)?;
                for i in 1..args.len() {
                    let v = num(i)?;
                    acc = if want_min { acc.min(v) } else { acc.max(v) };
                }
                Ok(Float(acc))
            }
        }
        "coalesce" => {
            for a in args {
                if !matches!(a, Null) {
                    return Ok(a.clone());
                }
            }
            Ok(Null)
        }
        // ── string ───────────────────────────────────────────────────────
        "len" => {
            arity(1)?;
            Ok(Int(text(0)?.chars().count() as i64))
        }
        "upper" => {
            arity(1)?;
            Ok(Str(std::sync::Arc::from(text(0)?.to_uppercase())))
        }
        "lower" => {
            arity(1)?;
            Ok(Str(std::sync::Arc::from(text(0)?.to_lowercase())))
        }
        "concat" => {
            let mut s = String::new();
            for a in args {
                s.push_str(&display_value(a));
            }
            Ok(Str(std::sync::Arc::from(s)))
        }
        "contains" => {
            arity(2)?;
            Ok(Bool(text(0)?.contains(&*text(1)?)))
        }
        "starts_with" => {
            arity(2)?;
            Ok(Bool(text(0)?.starts_with(&*text(1)?)))
        }
        "ends_with" => {
            arity(2)?;
            Ok(Bool(text(0)?.ends_with(&*text(1)?)))
        }
        "substr" => {
            if args.len() != 2 && args.len() != 3 {
                return Err(bad(format!(
                    "substr() takes 2 or 3 arguments, got {}",
                    args.len()
                )));
            }
            let chars: Vec<char> = text(0)?.chars().collect();
            let start = (as_i64(&args[1]).unwrap_or(0).max(0) as usize).min(chars.len());
            let end = if args.len() == 3 {
                let len = as_i64(&args[2]).unwrap_or(0).max(0) as usize;
                (start + len).min(chars.len())
            } else {
                chars.len()
            };
            Ok(Str(std::sync::Arc::from(
                chars[start..end].iter().collect::<String>(),
            )))
        }
        // ── genomic ──────────────────────────────────────────────────────
        "gc" => {
            arity(1)?;
            Ok(Float(gc_fraction(&text(0)?)))
        }
        "revcomp" => {
            arity(1)?;
            Ok(Str(std::sync::Arc::from(revcomp(&text(0)?))))
        }
        "translate" => match args.len() {
            1 => Ok(Str(std::sync::Arc::from(translate_dna(&text(0)?, 0)))),
            2 => {
                let frame = as_i64(&args[1]).unwrap_or(0).max(0) as usize;
                Ok(Str(std::sync::Arc::from(translate_dna(&text(0)?, frame))))
            }
            n => Err(bad(format!("translate() takes 1 or 2 arguments, got {n}"))),
        },
        "codon_at" => {
            arity(2)?;
            let chars: Vec<char> = text(0)?.chars().collect();
            let i = as_i64(&args[1]).unwrap_or(-1);
            if i < 0 || (i as usize) + 3 > chars.len() {
                Ok(Null)
            } else {
                let i = i as usize;
                Ok(Str(std::sync::Arc::from(
                    chars[i..i + 3].iter().collect::<String>(),
                )))
            }
        }
        _ => Err(bad(format!("unknown function {name}()"))),
    }
}

#[cfg(test)]
mod fasta_tests {
    use super::*;

    #[test]
    fn header_full_contig_is_offset_zero() {
        assert_eq!(parse_fasta_header("7"), ("7".to_string(), 0));
        // full chr7.fa header has trailing tokens; only the first is used upstream
        assert_eq!(parse_fasta_header("7"), ("7".to_string(), 0));
    }

    #[test]
    fn header_region_slice_offsets_to_start_minus_one() {
        // samtools faidx style; start is 1-based, offset is 0-based start-1
        assert_eq!(
            parse_fasta_header("7:54990000-55300100"),
            ("7".to_string(), 54989999)
        );
        assert_eq!(parse_fasta_header("chrX:100-200"), ("chrX".to_string(), 99));
    }

    #[test]
    fn header_non_region_colon_is_not_treated_as_offset() {
        // a colon without a numeric start-end range → whole token is the contig
        assert_eq!(parse_fasta_header("HLA:weird"), ("HLA:weird".to_string(), 0));
    }

    #[test]
    fn region_slice_is_coordinate_correct() {
        // A 2-base region at 1-based 5-6 → the bases land at 0-based indices 4,5.
        let fasta = b">7:5-6\nAC\n";
        let seqs = parse_fasta(fasta);
        let seq = seqs.get("7").expect("contig 7");
        assert_eq!(seq.len(), 6); // padded to absolute coordinates
        assert_eq!(&seq[4..6], "AC"); // base at 1-based pos 5 (0-based 4) == 'A'
        assert!(seq[..4].chars().all(|c| c == 'N')); // leading padding
    }

    #[test]
    fn full_contig_unpadded() {
        let seqs = parse_fasta(b">7\nACGT\n");
        assert_eq!(seqs.get("7").map(String::as_str), Some("ACGT"));
    }
}

#[cfg(test)]
mod builtin_tests {
    use super::*;
    use std::sync::Arc;

    fn s(v: &str) -> RuntimeValue {
        RuntimeValue::Str(Arc::from(v))
    }
    fn call(name: &str, args: &[RuntimeValue]) -> Result<RuntimeValue, VmError> {
        call_builtin(name, args, 0)
    }

    #[test]
    fn scalar_math() {
        use RuntimeValue::*;
        assert_eq!(call("abs", &[Int(-5)]).unwrap(), Int(5));
        assert_eq!(call("abs", &[Float(-2.5)]).unwrap(), Float(2.5));
        assert_eq!(call("floor", &[Float(3.9)]).unwrap(), Int(3));
        assert_eq!(call("ceil", &[Float(3.1)]).unwrap(), Int(4));
        assert_eq!(call("round", &[Float(2.49)]).unwrap(), Int(2));
        assert_eq!(call("round", &[Float(2.345), Int(2)]).unwrap(), Float(2.35));
        assert_eq!(call("min", &[Int(3), Int(7), Int(1)]).unwrap(), Int(1));
        assert_eq!(call("max", &[Int(3), Float(7.5)]).unwrap(), Float(7.5));
        assert_eq!(call("pow", &[Int(2), Int(10)]).unwrap(), Float(1024.0));
        assert_eq!(call("coalesce", &[Null, Null, Int(9)]).unwrap(), Int(9));
    }

    #[test]
    fn strings() {
        use RuntimeValue::*;
        assert_eq!(call("len", &[s("ACGT")]).unwrap(), Int(4));
        assert_eq!(call("upper", &[s("acgt")]).unwrap(), s("ACGT"));
        assert_eq!(call("contains", &[s("EGFR p.L858R"), s("L858R")]).unwrap(), Bool(true));
        assert_eq!(call("substr", &[s("ACGTACGT"), Int(2), Int(3)]).unwrap(), s("GTA"));
        assert_eq!(call("concat", &[s("chr"), Int(7)]).unwrap(), s("chr7"));
    }

    #[test]
    fn genomic() {
        use RuntimeValue::*;
        assert_eq!(call("gc", &[s("GGCC")]).unwrap(), Float(1.0));
        assert_eq!(call("gc", &[s("ATAT")]).unwrap(), Float(0.0));
        assert_eq!(call("revcomp", &[s("AACG")]).unwrap(), s("CGTT"));
        // ATG AAA TAG → M K *
        assert_eq!(call("translate", &[s("ATGAAATAG")]).unwrap(), s("MK*"));
        // frame 1 of xATGTTT → CV? offset 1: TGT TT -> C
        assert_eq!(call("translate", &[s("ATGTTT"), Int(0)]).unwrap(), s("MF"));
        assert_eq!(call("codon_at", &[s("ATGAAA"), Int(3)]).unwrap(), s("AAA"));
        assert_eq!(call("codon_at", &[s("ATG"), Int(2)]).unwrap(), Null);
    }

    #[test]
    fn errors() {
        // unknown name and arity/type errors surface as Builtin errors.
        assert!(matches!(call("frobnicate", &[]), Err(VmError::Builtin(_))));
        assert!(matches!(call("abs", &[]), Err(VmError::Builtin(_))));
        assert!(matches!(call("gc", &[RuntimeValue::Int(5)]), Err(VmError::Builtin(_))));
    }
}
