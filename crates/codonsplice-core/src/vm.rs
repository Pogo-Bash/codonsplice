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
        let fmt = format_from_byte(self.read_u8());
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
                let opts = self.build_variant_options();
                let core_region = region.as_ref().map(|r| r.to_core());
                let producer: crate::runtime::RecordProducer =
                    Box::new(move |limit| variant_producer(&ds, &opts, core_region.as_ref(), is_vcf, limit));
                let mut c = cursor.lock().unwrap();
                c.producer = Some(producer);
            }
            CallKind::Cnv | CallKind::Coverage => {
                let opts = self.build_coverage_options();
                let bytes = self.require_bam(&ds, "coverage")?;
                let windows = match (&region, ds.bai_bytes()) {
                    (Some(r), Some(bai)) => {
                        coverage::analyze_coverage_region(bytes, bai, &r.to_core(), &opts)
                    }
                    _ => coverage::coverage_windows(bytes, None, &opts),
                }
                .map_err(VmError::core)?;
                let records = windows.into_iter().map(Record::CoverageWindow).collect();
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
        let records = materialize(cursor, "").map_err(VmError::core)?;
        let bytes = serialize_records(fmt.clone(), &records)?;
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
                return Err(VmError::NotYetImplemented(op.name().to_string()))
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
        let bytes = match &ds.data {
            DatasetInner::Vcf { bytes } => bytes,
            _ => unreachable!("is_vcf implies a VCF dataset"),
        };
        let mut out = Vec::new();
        for v in vcf::stream_vcf(bytes, region) {
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

/// Parse a (small) FASTA byte buffer into `name -> sequence`.
fn parse_fasta(bytes: &[u8]) -> HashMap<String, String> {
    let mut seqs = HashMap::new();
    let text = String::from_utf8_lossy(bytes);
    let mut name = String::new();
    let mut seq = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix('>') {
            if !name.is_empty() {
                seqs.insert(std::mem::take(&mut name), std::mem::take(&mut seq));
            }
            name = rest.split_whitespace().next().unwrap_or("").to_string();
        } else {
            seq.push_str(line.trim());
        }
    }
    if !name.is_empty() {
        seqs.insert(name, seq);
    }
    seqs
}

fn format_header(refs: &[(String, usize)]) -> String {
    let mut out = String::from("reference sequences:\n");
    for (name, len) in refs {
        out.push_str(&format!("  {name}\t{len}\n"));
    }
    out
}

/// Serialize materialized records into the target output format's bytes.
fn serialize_records(fmt: Format, records: &[Record]) -> Result<Vec<u8>, VmError> {
    let label = format_label(&fmt);
    match fmt {
        Format::Vcf => Ok(records_to_vcf(records).into_bytes()),
        Format::Bed => Ok(records_to_bed(records).into_bytes()),
        // JSON is the lossless default; `INTO fasta` is repurposed as the JSON
        // sink for record streams (the parser has no `json` format token yet).
        Format::Fasta => Ok(records_to_json(records).into_bytes()),
        Format::Bam | Format::Cram => Err(VmError::UnsupportedInto(label.to_string())),
    }
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
        Record::Alignment(a) => json!({
            "chrom": a.chrom,
            "pos": a.aln.pos,
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

fn records_to_vcf(records: &[Record]) -> String {
    // A projected `SELECT` produces `Record::Row`s whose columns don't fit the
    // native variant schema. Emit a custom-FORMAT VCF that preserves every
    // projected column instead of silently dropping the rows (which would leave
    // the `wrote N record(s)` count disagreeing with an empty body). See #1.
    if records.iter().any(|r| matches!(r, Record::Row(_))) {
        return projected_rows_to_vcf(records);
    }
    let mut out = String::from(
        "##fileformat=VCFv4.2\n#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO\n",
    );
    for r in records {
        if let Record::Variant(v) = r {
            out.push_str(&format!(
                "{}\t{}\t.\t{}\t{}\t{:.1}\tPASS\tDP={};AF={:.4}\n",
                v.chrom, v.pos, v.ref_base, v.alt, v.qual, v.depth, v.allele_freq
            ));
        }
    }
    out
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
    let mut out = String::new();
    for r in records {
        match r {
            Record::CoverageWindow(w) => out.push_str(&format!(
                "{}\t{}\t{}\t{:.4}\n",
                w.chromosome, w.start, w.end, w.normalized
            )),
            Record::Variant(v) => out.push_str(&format!(
                "{}\t{}\t{}\t{}\n",
                v.chrom,
                v.pos - 1,
                v.pos,
                v.alt
            )),
            _ => {}
        }
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
