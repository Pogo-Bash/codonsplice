//! SpliceQL → bytecode compiler.
//!
//! Walks a [`spliceql::ast::Query`] and emits a [`Program`]: a constant pool, a
//! flat byte stream, and a debug table mapping opcode offsets back to source
//! spans.  The instruction set is a stack machine; see [`OpCode`].
//!
//! # Program layout
//!
//! ```text
//! [ main pipeline ... HALT ]      ; linear program the VM executes
//! [ predicate sub-program  ]      ; <expr> RET_PRED   — run per record by FILTER
//! [ projection table       ]      ; referenced by PROJECT
//! [ order-by table         ]      ; referenced by ORDER_BY
//! ```
//!
//! Per-record sub-programs (the `WHERE` predicate, `SELECT` items, `ORDER BY`
//! keys) live *after* `HALT` so they sit out of the main linear flow.  The
//! opcode that references them carries the absolute byte offset (and, for
//! `FILTER`, the length).  Jumps inside a sub-program are encoded relative to
//! that sub-program's start, so the VM runs one by setting `pc = pred_off` and
//! treating offsets as absolute (Phase 4 detail; in Phase 3 only standalone
//! expression programs — which start at offset 0 — are executed).

use std::fmt;
use std::rc::Rc;

use spliceql::ast::*;
use spliceql::error::{byte_offset_to_line_col, ParseError};
use spliceql::token::Span;

// ── Opcodes ──────────────────────────────────────────────────────────────────

/// The full SpliceQL bytecode instruction set.
///
/// Each variant maps to a single `u8` opcode (see [`OpCode::byte`]).  Operands
/// are encoded inline after the opcode byte, little-endian:
/// `u16` = 2 bytes, `u8` = 1 byte.  Constant/field/key references are `u16`
/// indices into [`Program::consts`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpCode {
    // Expression opcodes
    LoadConst,    // u16 const_idx
    LoadTrue,     //
    LoadFalse,    //
    LoadField,    // u16 name_idx   (current-record column)
    GetField,     // u16 name_idx   (field access on stack top)
    Index,        //                (subscript: [obj, key] -> value)
    LoadWildcard, //                (SELECT *)
    LoadVar,      // u16 name_idx   ($name template variable)
    Neg,
    Not,
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    And, // u16 jmp  (short-circuit)
    Or,  // u16 jmp  (short-circuit)
    CallFn, // u16 name_idx, u8 argc
    RetPred,
    JumpIfFalse, // u16 target
    Jump,        // u16 target

    // Pipeline opcodes
    OpenSource, // u8 format, u16 path_idx
    Scan,
    Filter,    // u16 pred_off, u16 pred_len
    Project,   // u16 table_off
    SetParam,  // u16 key_idx
    OrderBy,   // u16 table_off
    Limit,
    WriteInto, // u8 format, u16 path_idx
    Halt,

    // CALL operation opcodes
    CallVariants,
    CallCnv,
    CallCoverage,
    CallReads,
    CallHeader,
}

impl OpCode {
    /// The raw opcode byte emitted into the bytecode stream.
    pub fn byte(self) -> u8 {
        use OpCode::*;
        match self {
            LoadConst => 0x01,
            LoadTrue => 0x02,
            LoadFalse => 0x03,
            LoadField => 0x04,
            GetField => 0x05,
            Index => 0x06,
            LoadWildcard => 0x07,
            LoadVar => 0x08,
            Neg => 0x10,
            Not => 0x11,
            Add => 0x12,
            Sub => 0x13,
            Mul => 0x14,
            Div => 0x15,
            Eq => 0x16,
            Ne => 0x17,
            Lt => 0x18,
            Gt => 0x19,
            Le => 0x1A,
            Ge => 0x1B,
            And => 0x1C,
            Or => 0x1D,
            CallFn => 0x1E,
            RetPred => 0x1F,
            JumpIfFalse => 0x20,
            Jump => 0x21,
            OpenSource => 0x40,
            Scan => 0x41,
            Filter => 0x42,
            Project => 0x43,
            SetParam => 0x44,
            OrderBy => 0x45,
            Limit => 0x46,
            WriteInto => 0x47,
            Halt => 0x4F,
            CallVariants => 0x50,
            CallCnv => 0x51,
            CallCoverage => 0x52,
            CallReads => 0x53,
            CallHeader => 0x54,
        }
    }

    /// Decode an opcode byte.
    pub fn from_byte(b: u8) -> Option<OpCode> {
        use OpCode::*;
        Some(match b {
            0x01 => LoadConst,
            0x02 => LoadTrue,
            0x03 => LoadFalse,
            0x04 => LoadField,
            0x05 => GetField,
            0x06 => Index,
            0x07 => LoadWildcard,
            0x08 => LoadVar,
            0x10 => Neg,
            0x11 => Not,
            0x12 => Add,
            0x13 => Sub,
            0x14 => Mul,
            0x15 => Div,
            0x16 => Eq,
            0x17 => Ne,
            0x18 => Lt,
            0x19 => Gt,
            0x1A => Le,
            0x1B => Ge,
            0x1C => And,
            0x1D => Or,
            0x1E => CallFn,
            0x1F => RetPred,
            0x20 => JumpIfFalse,
            0x21 => Jump,
            0x40 => OpenSource,
            0x41 => Scan,
            0x42 => Filter,
            0x43 => Project,
            0x44 => SetParam,
            0x45 => OrderBy,
            0x46 => Limit,
            0x47 => WriteInto,
            0x4F => Halt,
            0x50 => CallVariants,
            0x51 => CallCnv,
            0x52 => CallCoverage,
            0x53 => CallReads,
            0x54 => CallHeader,
            _ => return None,
        })
    }

    /// Number of operand bytes that follow this opcode in the stream.
    pub fn operand_len(self) -> usize {
        use OpCode::*;
        match self {
            // u16 operand
            LoadConst | LoadField | GetField | LoadVar | SetParam | Project | OrderBy | And | Or
            | JumpIfFalse | Jump => 2,
            // u8 + u16
            OpenSource | WriteInto => 3,
            // u16 + u8
            CallFn => 3,
            // u16 + u16
            Filter => 4,
            // no operands
            _ => 0,
        }
    }

    /// The mnemonic used by the disassembler.
    pub fn name(self) -> &'static str {
        use OpCode::*;
        match self {
            LoadConst => "LOAD_CONST",
            LoadTrue => "LOAD_TRUE",
            LoadFalse => "LOAD_FALSE",
            LoadField => "LOAD_FIELD",
            GetField => "GET_FIELD",
            Index => "INDEX",
            LoadWildcard => "LOAD_WILDCARD",
            LoadVar => "LOAD_VAR",
            Neg => "NEG",
            Not => "NOT",
            Add => "ADD",
            Sub => "SUB",
            Mul => "MUL",
            Div => "DIV",
            Eq => "EQ",
            Ne => "NE",
            Lt => "LT",
            Gt => "GT",
            Le => "LE",
            Ge => "GE",
            And => "AND",
            Or => "OR",
            CallFn => "CALL_FN",
            RetPred => "RET_PRED",
            JumpIfFalse => "JUMP_IF_FALSE",
            Jump => "JUMP",
            OpenSource => "OPEN_SOURCE",
            Scan => "SCAN",
            Filter => "FILTER",
            Project => "PROJECT",
            SetParam => "SET_PARAM",
            OrderBy => "ORDER_BY",
            Limit => "LIMIT",
            WriteInto => "WRITE_INTO",
            Halt => "HALT",
            CallVariants => "CALL_VARIANTS",
            CallCnv => "CALL_CNV",
            CallCoverage => "CALL_COVERAGE",
            CallReads => "CALL_READS",
            CallHeader => "CALL_HEADER",
        }
    }
}

/// Encode a [`Format`] as the single byte stored by `OPEN_SOURCE`/`WRITE_INTO`.
pub fn format_byte(f: &Format) -> u8 {
    match f {
        Format::Bam => 0,
        Format::Vcf => 1,
        Format::Fasta => 2,
        Format::Bed => 3,
        Format::Cram => 4,
        Format::Json => 5,
        Format::Tsv => 6,
    }
}

/// Decode a format byte back to its mnemonic (for disassembly).
pub fn format_name(b: u8) -> &'static str {
    match b {
        0 => "bam",
        1 => "vcf",
        2 => "fasta",
        3 => "bed",
        4 => "cram",
        _ => "???",
    }
}

// ── Values & program ─────────────────────────────────────────────────────────

/// A compile-time constant in the [`Program`] constant pool.
///
/// Runtime-only handles (`Record`, `Cursor`, `Dataset`) are intentionally absent
/// — they appear only in the VM as `RuntimeValue`.
#[derive(Debug, Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    Str(Rc<str>),
    Bool(bool),
    Null,
}

impl Value {
    /// The type name used in diagnostics.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Str(_) => "string",
            Value::Bool(_) => "bool",
            Value::Null => "null",
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a == b,
            // Bit-equality so the constant pool can dedup identical floats.
            (Value::Float(a), Value::Float(b)) => a.to_bits() == b.to_bits(),
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Null, Value::Null) => true,
            _ => false,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(n) => write!(f, "{n}"),
            Value::Float(v) => write!(f, "{v}"),
            Value::Str(s) => write!(f, "{s:?}"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Null => f.write_str("null"),
        }
    }
}

/// Maps a bytecode offset to the source span that produced it.
#[derive(Debug, Clone, PartialEq)]
pub struct DebugInfo {
    pub code_offset: usize,
    pub span: Span,
}

/// A compiled SpliceQL program.
#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub consts: Vec<Value>,
    pub code: Vec<u8>,
    pub debug: Vec<DebugInfo>,
    /// Statically-extracted region from the `WHERE` clause (`chr = "x"` plus
    /// optional `pos` bounds), used to drive BAI seeking at execution time.
    /// `None` when the predicate has no recognizable region constraint.
    pub region: Option<crate::runtime::Region>,
}

impl Program {
    /// Look up the source span associated with a code offset, if recorded.
    pub fn span_at(&self, offset: usize) -> Option<Span> {
        self.debug
            .iter()
            .find(|d| d.code_offset == offset)
            .map(|d| d.span)
    }
}

// ── Errors ───────────────────────────────────────────────────────────────────

/// An error raised while compiling a query.
#[derive(Debug, Clone, PartialEq)]
pub enum CompileError {
    UnknownParam {
        key: String,
        span: Span,
    },
    ParamTypeMismatch {
        key: String,
        expected: &'static str,
        got: &'static str,
        span: Span,
    },
    NonConstantParam {
        span: Span,
    },
    ParamWithoutCall {
        span: Span,
    },
    MultipleFrom {
        span: Span,
    },
    /// A call to a function the VM has no builtin for.
    UnknownFunction {
        name: String,
        span: Span,
    },
    /// A `WHERE` predicate references a field the record type cannot answer.
    /// `valid` is the record kind's full field set, listed in the diagnostic.
    UnknownField {
        name: String,
        kind: &'static str,
        valid: &'static [&'static str],
        span: Span,
    },
    /// A builtin called with the wrong number of arguments. `expected` is a
    /// human description ("1", "1 or 2", "at least 1").
    FunctionArity {
        name: String,
        expected: String,
        got: usize,
        span: Span,
    },
    /// A builtin argument that must be a string was given a different type that
    /// is knowable at compile time (a literal or arithmetic/boolean expression).
    FunctionArgType {
        name: String,
        arg: usize,
        expected: &'static str,
        got: &'static str,
        span: Span,
    },
    ParseError(ParseError),
}

impl From<ParseError> for CompileError {
    fn from(e: ParseError) -> Self {
        CompileError::ParseError(e)
    }
}

impl CompileError {
    /// A short error code, e.g. `E001`, used in rendered diagnostics.
    pub fn code(&self) -> &'static str {
        match self {
            CompileError::UnknownParam { .. } => "E001",
            CompileError::ParamTypeMismatch { .. } => "E002",
            CompileError::NonConstantParam { .. } => "E003",
            CompileError::ParamWithoutCall { .. } => "E004",
            CompileError::MultipleFrom { .. } => "E005",
            CompileError::UnknownFunction { .. } => "E006",
            CompileError::FunctionArity { .. } => "E007",
            CompileError::FunctionArgType { .. } => "E008",
            CompileError::UnknownField { .. } => "E009",
            CompileError::ParseError(_) => "E000",
        }
    }

    /// The source span this error points at.
    pub fn span(&self) -> Span {
        match self {
            CompileError::UnknownParam { span, .. }
            | CompileError::ParamTypeMismatch { span, .. }
            | CompileError::NonConstantParam { span }
            | CompileError::ParamWithoutCall { span }
            | CompileError::MultipleFrom { span }
            | CompileError::UnknownFunction { span, .. }
            | CompileError::FunctionArity { span, .. }
            | CompileError::FunctionArgType { span, .. }
            | CompileError::UnknownField { span, .. } => *span,
            CompileError::ParseError(e) => e.span(),
        }
    }

    /// The bare diagnostic message (no position).
    pub fn message(&self) -> String {
        match self {
            CompileError::UnknownParam { key, .. } => {
                format!("unknown parameter {key:?}")
            }
            CompileError::ParamTypeMismatch {
                key,
                expected,
                got,
                ..
            } => format!("parameter {key:?} expects {expected}, got {got}"),
            CompileError::NonConstantParam { .. } => {
                "WITH parameter value must be a constant".to_string()
            }
            CompileError::ParamWithoutCall { .. } => {
                "WITH clause has no CALL to configure".to_string()
            }
            CompileError::MultipleFrom { .. } => "only one FROM clause is allowed".to_string(),
            CompileError::UnknownFunction { name, .. } => {
                format!("unknown function {name:?}")
            }
            CompileError::UnknownField {
                name, kind, valid, ..
            } => format!(
                "unknown field {name:?} for {kind} records — valid fields: {}",
                valid.join(", ")
            ),
            CompileError::FunctionArity {
                name, expected, got, ..
            } => format!("function {name:?} takes {expected} argument(s), got {got}"),
            CompileError::FunctionArgType {
                name,
                arg,
                expected,
                got,
                ..
            } => format!("function {name:?} argument {arg} must be a {expected}, got {got}"),
            CompileError::ParseError(e) => e.to_string(),
        }
    }

    /// Render a rich, multi-line diagnostic with `query:line:col`, a caret under
    /// the offending span, and an optional "did you mean" hint.  This is the
    /// formatter the CLI/TUI use; [`fmt::Display`] gives the one-line form.
    pub fn render(&self, source: &str, suggestion: Option<&str>) -> String {
        let span = self.span();
        let (line, col) = byte_offset_to_line_col(source, span.start);
        let line_text = source.lines().nth(line - 1).unwrap_or("");
        let caret_len = span.len().max(1);

        let mut out = String::new();
        out.push_str(&format!("error[{}]: {}\n", self.code(), self.message()));
        out.push_str(&format!("  --> query:{line}:{col}\n"));
        out.push_str("   |\n");
        out.push_str(&format!("{line:>2} | {line_text}\n"));
        out.push_str("   | ");
        for _ in 1..col {
            out.push(' ');
        }
        for _ in 0..caret_len {
            out.push('^');
        }
        if let Some(s) = suggestion {
            out.push_str(&format!(" did you mean {s:?}?"));
        }
        out.push('\n');
        out
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let span = self.span();
        write!(
            f,
            "compile error[{}] at {}..{}: {}",
            self.code(),
            span.start,
            span.end,
            self.message()
        )
    }
}

impl std::error::Error for CompileError {}

// ── WITH parameter tables ────────────────────────────────────────────────────

/// The runtime type a `WITH` parameter coerces to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParamType {
    U32,
    Usize,
    I64,
    U8,
    F64,
    Str,
}

impl ParamType {
    /// The type name shown in `ParamTypeMismatch::expected`.
    fn expected_name(self) -> &'static str {
        match self {
            ParamType::U32 | ParamType::Usize | ParamType::I64 | ParamType::U8 => "int",
            ParamType::F64 => "float",
            ParamType::Str => "string",
        }
    }

    /// Validate `v` against this type, coercing `Int → Float` for `F64`.
    /// Returns the coerced constant or `None` on a type mismatch.
    fn coerce(self, v: &Value) -> Option<Value> {
        match (self, v) {
            (ParamType::U32, Value::Int(n))
            | (ParamType::Usize, Value::Int(n))
            | (ParamType::I64, Value::Int(n))
            | (ParamType::U8, Value::Int(n)) => Some(Value::Int(*n)),
            (ParamType::F64, Value::Float(x)) => Some(Value::Float(*x)),
            (ParamType::F64, Value::Int(n)) => Some(Value::Float(*n as f64)), // coerce
            (ParamType::Str, Value::Str(s)) => Some(Value::Str(s.clone())),
            _ => None,
        }
    }
}

struct ParamSpec {
    name: &'static str,
    ty: ParamType,
}

const COVERAGE_PARAMS: &[ParamSpec] = &[
    ParamSpec { name: "window_size", ty: ParamType::U32 },
    ParamSpec { name: "amp_threshold", ty: ParamType::F64 },
    ParamSpec { name: "del_threshold", ty: ParamType::F64 },
    ParamSpec { name: "min_windows", ty: ParamType::Usize },
    ParamSpec { name: "segmentation_method", ty: ParamType::Str },
];

const VARIANT_PARAMS: &[ParamSpec] = &[
    ParamSpec { name: "min_depth", ty: ParamType::I64 },
    ParamSpec { name: "min_base_quality", ty: ParamType::U8 },
    ParamSpec { name: "min_mapping_quality", ty: ParamType::U8 },
    ParamSpec { name: "min_variant_reads", ty: ParamType::I64 },
    ParamSpec { name: "min_allele_freq", ty: ParamType::F64 },
    // `min_af` is the documented short alias for `min_allele_freq`.
    ParamSpec { name: "min_af", ty: ParamType::F64 },
    ParamSpec { name: "min_strand_bias", ty: ParamType::F64 },
    // Path to a reference FASTA. When given, REF is the actual reference base at
    // each position (required for valid VCF + truth-set concordance); without it
    // REF is inferred as the pileup-majority base, which is a coin-flip at
    // balanced heterozygous sites.
    ParamSpec { name: "reference", ty: ParamType::Str },
];

/// The set of valid `WITH` parameter specs for a CALL operation.
fn params_for(operation: &str) -> &'static [ParamSpec] {
    match operation {
        "cnv" | "coverage" => COVERAGE_PARAMS,
        "variants" => VARIANT_PARAMS,
        // `reads` / `header` take no tunable parameters.
        _ => &[],
    }
}

/// The valid parameter names for a CALL operation (used for "did you mean").
pub fn param_names_for(operation: &str) -> Vec<&'static str> {
    params_for(operation).iter().map(|p| p.name).collect()
}

/// Suggest the closest known parameter name to `unknown` for `operation`.
/// Returns `None` if no candidate is reasonably close.
pub fn suggest_param(unknown: &str, operation: &str) -> Option<String> {
    let candidates = param_names_for(operation);
    did_you_mean(unknown, &candidates)
}

// ── builtin function signatures (compile-time validation) ────────────────────
//
// These describe the functions the VM's `call_builtin` (vm.rs) implements, so
// `splice check` can reject unknown names, wrong arity, and obvious argument
// type errors before the VM ever runs. Keep this table in sync with vm.rs.

/// A builtin's static signature: argument count bounds and which argument
/// positions must be strings.
struct BuiltinSig {
    name: &'static str,
    /// Minimum argument count.
    min: usize,
    /// Maximum argument count, or `None` for variadic.
    max: Option<usize>,
    /// Argument positions (0-based) that must be strings.
    str_args: &'static [usize],
}

impl BuiltinSig {
    /// Human-readable arity, e.g. "1", "1 or 2", "2 or 3", "at least 1".
    fn arity_desc(&self) -> String {
        match self.max {
            Some(mx) if mx == self.min => format!("{}", self.min),
            Some(mx) if mx == self.min + 1 => format!("{} or {}", self.min, mx),
            Some(mx) => format!("{} to {}", self.min, mx),
            None => format!("at least {}", self.min),
        }
    }
}

const BUILTINS: &[BuiltinSig] = &[
    // scalar / math
    BuiltinSig { name: "abs", min: 1, max: Some(1), str_args: &[] },
    BuiltinSig { name: "floor", min: 1, max: Some(1), str_args: &[] },
    BuiltinSig { name: "ceil", min: 1, max: Some(1), str_args: &[] },
    BuiltinSig { name: "sqrt", min: 1, max: Some(1), str_args: &[] },
    BuiltinSig { name: "round", min: 1, max: Some(2), str_args: &[] },
    BuiltinSig { name: "log", min: 1, max: Some(2), str_args: &[] },
    BuiltinSig { name: "pow", min: 2, max: Some(2), str_args: &[] },
    BuiltinSig { name: "min", min: 1, max: None, str_args: &[] },
    BuiltinSig { name: "max", min: 1, max: None, str_args: &[] },
    BuiltinSig { name: "coalesce", min: 1, max: None, str_args: &[] },
    // string
    BuiltinSig { name: "len", min: 1, max: Some(1), str_args: &[0] },
    BuiltinSig { name: "upper", min: 1, max: Some(1), str_args: &[0] },
    BuiltinSig { name: "lower", min: 1, max: Some(1), str_args: &[0] },
    BuiltinSig { name: "concat", min: 1, max: None, str_args: &[] },
    BuiltinSig { name: "contains", min: 2, max: Some(2), str_args: &[0, 1] },
    BuiltinSig { name: "starts_with", min: 2, max: Some(2), str_args: &[0, 1] },
    BuiltinSig { name: "ends_with", min: 2, max: Some(2), str_args: &[0, 1] },
    BuiltinSig { name: "substr", min: 2, max: Some(3), str_args: &[0] },
    // genomic
    BuiltinSig { name: "gc", min: 1, max: Some(1), str_args: &[0] },
    BuiltinSig { name: "revcomp", min: 1, max: Some(1), str_args: &[0] },
    BuiltinSig { name: "translate", min: 1, max: Some(2), str_args: &[0] },
    BuiltinSig { name: "codon_at", min: 2, max: Some(2), str_args: &[0] },
];

fn builtin_sig(name: &str) -> Option<&'static BuiltinSig> {
    BUILTINS.iter().find(|b| b.name == name)
}

/// True if `name` is a known builtin function (used to disambiguate a
/// one-string-arg call from a `name["key"]` subscript).
pub fn is_builtin(name: &str) -> bool {
    builtin_sig(name).is_some()
}

/// Suggest the closest known builtin name to `unknown` (the E006 "did you mean").
pub fn suggest_function(unknown: &str) -> Option<String> {
    let names: Vec<&str> = BUILTINS.iter().map(|b| b.name).collect();
    did_you_mean(unknown, &names)
}

/// A statically-knowable value type, for the conservative argument-type check.
#[derive(PartialEq, Clone, Copy)]
enum StaticType {
    Str,
    Num,
    Bool,
}

impl StaticType {
    fn name(self) -> &'static str {
        match self {
            StaticType::Str => "string",
            StaticType::Num => "number",
            StaticType::Bool => "bool",
        }
    }
}

/// The type of an expression when it is knowable at compile time. Returns `None`
/// for fields, `$vars`, subscripts, and function calls (runtime-dependent), so
/// the type check never produces a false positive on e.g. `gc(ref)`.
fn static_type(e: &Expr) -> Option<StaticType> {
    match e {
        Expr::StringLit(..) => Some(StaticType::Str),
        Expr::IntLit(..) | Expr::FloatLit(..) => Some(StaticType::Num),
        Expr::BoolLit(..) => Some(StaticType::Bool),
        Expr::Unary { op: UnaryOp::Neg, .. } => Some(StaticType::Num),
        Expr::Unary { op: UnaryOp::Not, .. } => Some(StaticType::Bool),
        Expr::Binary { op, .. } => match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => Some(StaticType::Num),
            _ => Some(StaticType::Bool), // comparisons + AND/OR
        },
        _ => None, // Ident, Var, FieldAccess, Call, Wildcard
    }
}

/// Validate a builtin call's name, arity, and (conservatively) argument types.
fn validate_builtin_call(name: &str, args: &[Expr], span: Span) -> Result<(), CompileError> {
    let sig = match builtin_sig(name) {
        Some(s) => s,
        None => {
            return Err(CompileError::UnknownFunction {
                name: name.to_string(),
                span,
            })
        }
    };
    let n = args.len();
    if n < sig.min || sig.max.map_or(false, |mx| n > mx) {
        return Err(CompileError::FunctionArity {
            name: name.to_string(),
            expected: sig.arity_desc(),
            got: n,
            span,
        });
    }
    for &pos in sig.str_args {
        if let Some(arg) = args.get(pos) {
            if let Some(t) = static_type(arg) {
                if t != StaticType::Str {
                    return Err(CompileError::FunctionArgType {
                        name: name.to_string(),
                        arg: pos + 1,
                        expected: "string",
                        got: t.name(),
                        span: arg.span(),
                    });
                }
            }
        }
    }
    Ok(())
}

/// Pick the best "did you mean" candidate for `unknown`.
///
/// Genomic parameter names are `snake_case`, so a plain edit distance is a poor
/// fit: `min_freq` is 7 edits from `min_allele_freq` yet is obviously the same
/// intent.  We rank primarily by **shared underscore tokens**, then by
/// Levenshtein distance as a tiebreaker.  A candidate is accepted if it shares
/// at least one token with `unknown`, or is within 3 edits overall.
pub fn did_you_mean(unknown: &str, candidates: &[&str]) -> Option<String> {
    let utoks: Vec<&str> = unknown.split('_').filter(|t| !t.is_empty()).collect();

    let mut scored: Vec<(&str, usize, usize)> = candidates
        .iter()
        .map(|c| {
            let ctoks: Vec<&str> = c.split('_').filter(|t| !t.is_empty()).collect();
            let shared = utoks.iter().filter(|t| ctoks.contains(t)).count();
            (*c, shared, levenshtein(unknown, c))
        })
        .collect();

    // Highest shared-token count first, then smallest edit distance.
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)));

    let (cand, shared, dist) = *scored.first()?;
    if shared >= 1 || dist <= 3 {
        Some(cand.to_string())
    } else {
        None
    }
}

/// Classic Levenshtein edit distance (insertions, deletions, substitutions).
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

// ── Compiler ─────────────────────────────────────────────────────────────────

/// Lowers a [`Query`] into a [`Program`].
pub struct Compiler {
    consts: Vec<Value>,
    code: Vec<u8>,
    debug: Vec<DebugInfo>,
    #[allow(dead_code)]
    source: String,
}

/// A compiled per-record sub-program with offsets relative to its own start.
struct Region {
    code: Vec<u8>,
    debug: Vec<DebugInfo>,
}

/// The field set the `WHERE` predicate can resolve, by record kind. These
/// mirror the runtime resolvers in `runtime.rs` (`variant_field` / `window_field`
/// / `aln_field`) and must be kept in sync. The predicate runs on the CALL's
/// output records (or the raw source records when there is no CALL).
const VARIANT_FIELDS: &[&str] = &[
    "chr", "chrom", "pos", "ref", "alt", "qual", "depth", "ref_count", "alt_count",
    "af", "allele_freq", "strand_bias", "kind", "filter", "id",
];
const COVERAGE_FIELDS: &[&str] = &["chr", "chrom", "start", "end", "coverage", "normalized", "masked"];
const ALIGNMENT_FIELDS: &[&str] = &[
    "chr", "chrom", "pos", "mapq", "flag", "depth", "strand", "is_reverse", "is_duplicate",
    "is_secondary",
];

/// The record kind (and its field set) the `WHERE` predicate is evaluated
/// against. Returns `None` for kinds whose schema we don't model (cnv/header
/// CALLs, fasta/bed sources), leaving those unchecked rather than risking a
/// false-positive error.
fn where_record_kind(query: &Query) -> Option<(&'static str, &'static [&'static str])> {
    if let Some(call) = &query.call {
        return match call.operation.as_str() {
            "variants" => Some(("variant", VARIANT_FIELDS)),
            "coverage" => Some(("coverage", COVERAGE_FIELDS)),
            "reads" => Some(("alignment", ALIGNMENT_FIELDS)),
            _ => None,
        };
    }
    match query.from.format {
        Format::Bam => Some(("alignment", ALIGNMENT_FIELDS)),
        Format::Vcf => Some(("variant", VARIANT_FIELDS)),
        _ => None,
    }
}

/// Collect the bare field identifiers referenced as values in `expr`, skipping
/// function-call callees (function names), `$variables`, and literals.
fn collect_field_refs<'a>(expr: &'a Expr, out: &mut Vec<(&'a str, Span)>) {
    match expr {
        Expr::Ident(name, span) => out.push((name.as_str(), *span)),
        Expr::Unary { operand, .. } => collect_field_refs(operand, out),
        Expr::Binary { left, right, .. } => {
            collect_field_refs(left, out);
            collect_field_refs(right, out);
        }
        Expr::Call { args, .. } => {
            for a in args {
                collect_field_refs(a, out);
            }
        }
        Expr::FieldAccess { object, .. } => collect_field_refs(object, out),
        _ => {}
    }
}

/// Reject a `WHERE` field the record type cannot answer (it would otherwise
/// silently match zero rows). #16
fn validate_where_fields(query: &Query) -> Result<(), CompileError> {
    let filter = match &query.filter {
        Some(f) => f,
        None => return Ok(()),
    };
    let (kind, valid) = match where_record_kind(query) {
        Some(k) => k,
        None => return Ok(()),
    };
    let mut refs = Vec::new();
    collect_field_refs(filter, &mut refs);
    for (name, span) in refs {
        if !valid.contains(&name) {
            return Err(CompileError::UnknownField {
                name: name.to_string(),
                kind,
                valid,
                span,
            });
        }
    }
    Ok(())
}

impl Compiler {
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            consts: Vec::new(),
            code: Vec::new(),
            debug: Vec::new(),
            source: source.into(),
        }
    }

    /// Compile a full query into a [`Program`].
    pub fn compile(mut self, query: &Query) -> Result<Program, CompileError> {
        // 0. Static check: a WHERE field that the record type can't answer would
        //    silently match nothing at runtime — reject it with a clear error
        //    listing the valid fields instead. #16
        validate_where_fields(query)?;

        // 1. FROM → OPEN_SOURCE + SCAN
        self.compile_from(&query.from);

        // 2. WHERE → FILTER (predicate appended after HALT, backpatched here)
        let mut filter_patch: Option<(usize, &Expr)> = None;
        if let Some(filter) = &query.filter {
            self.emit(OpCode::Filter, filter.span());
            let patch = self.code.len();
            self.emit_u16(0); // pred_off placeholder
            self.emit_u16(0); // pred_len placeholder
            filter_patch = Some((patch, filter));
        }

        // 3. WITH (SET_PARAM*) then CALL — params configure the upcoming call.
        match (&query.call, &query.with) {
            (Some(call), Some(with)) => {
                self.compile_with(with, call)?;
                self.compile_call(call);
            }
            (Some(call), None) => self.compile_call(call),
            (None, Some(with)) => {
                let span = with
                    .first()
                    .map(|(_, v)| v.span())
                    .unwrap_or(query.span);
                return Err(CompileError::ParamWithoutCall { span });
            }
            (None, None) => {}
        }

        // 4. SELECT → PROJECT (table appended after HALT)
        let mut project_patch: Option<(usize, Vec<SelectItem>)> = None;
        if let Some(items) = &query.select {
            // When the sink is VCF, make sure the fixed VCF identity columns
            // (CHROM/POS/REF/ALT) survive the projection even if the user did
            // not name them, so the writer doesn't emit a malformed `.` CHROM
            // (#15). Extra projected columns still flow to INFO.
            let items = augment_projection_for_vcf(items, query.into.as_ref());
            self.emit(OpCode::Project, span_of_items(&items, query.span));
            let patch = self.code.len();
            self.emit_u16(0);
            project_patch = Some((patch, items));
        }

        // 5. ORDER BY → ORDER_BY (table appended after HALT)
        let mut order_patch: Option<(usize, &[OrderItem])> = None;
        if let Some(items) = &query.order {
            self.emit(OpCode::OrderBy, span_of_order(items, query.span));
            let patch = self.code.len();
            self.emit_u16(0);
            order_patch = Some((patch, items));
        }

        // 6. LIMIT — count expression is evaluated inline, left on the stack.
        if let Some(limit) = &query.limit {
            self.compile_limit(limit);
        }

        // 7. INTO → WRITE_INTO
        if let Some(into) = &query.into {
            self.compile_into(into);
        }

        // 8. HALT terminates the main program.
        self.emit(OpCode::Halt, query.span);

        // 9. Append per-record sub-programs after HALT and backpatch refs.
        if let Some((patch, expr)) = filter_patch {
            let region = self.compile_region(&[expr])?.remove(0);
            let (off, len) = self.append_region(region);
            self.patch_u16(patch, off as u16);
            self.patch_u16(patch + 2, len as u16);
        }
        if let Some((patch, items)) = &project_patch {
            let off = self.append_projection(items)?;
            self.patch_u16(*patch, off as u16);
        }
        if let Some((patch, items)) = order_patch {
            let off = self.append_order(items, query.select.as_deref())?;
            self.patch_u16(patch, off as u16);
        }

        // Statically extract a seekable region from the WHERE clause, if any.
        let region = query.filter.as_ref().and_then(extract_region);

        Ok(Program {
            consts: self.consts,
            code: self.code,
            debug: self.debug,
            region,
        })
    }

    /// Compile a single expression into a standalone [`Program`] ending in
    /// `HALT`, with jumps relative to offset 0.  Used to exercise the VM's
    /// expression interpreter in isolation (see [`crate::compile_expr`]).
    pub fn compile_expr_program(mut self, expr: &Expr) -> Program {
        // Expression lowering is infallible (no WITH/param validation here).
        self.compile_expr(expr)
            .expect("expression lowering does not fail");
        self.emit(OpCode::Halt, expr.span());
        Program {
            consts: self.consts,
            code: self.code,
            debug: self.debug,
            region: None,
        }
    }

    // ── Clause lowering ──────────────────────────────────────────────────────

    fn compile_from(&mut self, from: &FromClause) {
        self.emit(OpCode::OpenSource, from.span);
        self.code.push(format_byte(&from.format));
        let path_idx = self.intern(Value::Str(Rc::from(from.path.as_str())));
        self.emit_u16(path_idx);
        self.emit(OpCode::Scan, from.span);
    }

    fn compile_into(&mut self, into: &IntoClause) {
        self.emit(OpCode::WriteInto, into.span);
        self.code.push(format_byte(&into.format));
        let path_idx = self.intern(Value::Str(Rc::from(into.path.as_str())));
        self.emit_u16(path_idx);
    }

    fn compile_call(&mut self, call: &CallClause) {
        let op = match call.operation.as_str() {
            "variants" => OpCode::CallVariants,
            "cnv" => OpCode::CallCnv,
            "coverage" => OpCode::CallCoverage,
            "reads" => OpCode::CallReads,
            "header" => OpCode::CallHeader,
            // The parser validates CALL operations, so this is unreachable for
            // parsed input; emit nothing rather than panic on hand-built ASTs.
            _ => return,
        };
        self.emit(op, call.span);
    }

    fn compile_with(
        &mut self,
        with: &[(String, Expr)],
        call: &CallClause,
    ) -> Result<(), CompileError> {
        let specs = params_for(&call.operation);
        for (key, value_expr) in with {
            let span = value_expr.span();

            // Key must be a known parameter for this CALL op (checked for both
            // constant and variable values).
            let _spec = specs.iter().find(|p| p.name == key).ok_or_else(|| {
                CompileError::UnknownParam {
                    key: key.clone(),
                    span,
                }
            })?;

            // A `$var` value defers to runtime: emit LOAD_VAR, then SET_PARAM,
            // which coerces against the param type when the call runs.
            if let Expr::Var(name, _) = value_expr {
                let nidx = self.intern(Value::Str(Rc::from(name.as_str())));
                self.emit(OpCode::LoadVar, span);
                self.emit_u16(nidx);
                let kidx = self.intern(Value::Str(Rc::from(key.as_str())));
                self.emit(OpCode::SetParam, span);
                self.emit_u16(kidx);
                continue;
            }

            // Otherwise the value must be a compile-time constant of the right
            // type (coercing Int -> Float where the param expects a float).
            let value =
                const_eval(value_expr).ok_or(CompileError::NonConstantParam { span })?;
            let coerced = _spec
                .ty
                .coerce(&value)
                .ok_or_else(|| CompileError::ParamTypeMismatch {
                    key: key.clone(),
                    expected: _spec.ty.expected_name(),
                    got: value.type_name(),
                    span,
                })?;

            let vidx = self.intern(coerced);
            self.emit(OpCode::LoadConst, span);
            self.emit_u16(vidx);
            let kidx = self.intern(Value::Str(Rc::from(key.as_str())));
            self.emit(OpCode::SetParam, span);
            self.emit_u16(kidx);
        }
        Ok(())
    }

    fn compile_limit(&mut self, limit: &Expr) {
        // Inline expression leaves the count on the stack; LIMIT consumes it.
        let _ = self.compile_expr(limit);
        self.emit(OpCode::Limit, limit.span());
    }

    // ── Sub-program (per-record) regions ─────────────────────────────────────

    /// Compile each expression into an independent region (offsets relative to
    /// its own start), each terminated by `RET_PRED`.
    fn compile_region(&mut self, exprs: &[&Expr]) -> Result<Vec<Region>, CompileError> {
        let mut regions = Vec::with_capacity(exprs.len());
        for expr in exprs {
            // Redirect emission into a fresh buffer.
            let saved_code = std::mem::take(&mut self.code);
            let saved_debug = std::mem::take(&mut self.debug);
            self.compile_expr(expr)?;
            self.emit(OpCode::RetPred, expr.span());
            let code = std::mem::replace(&mut self.code, saved_code);
            let debug = std::mem::replace(&mut self.debug, saved_debug);
            regions.push(Region { code, debug });
        }
        Ok(regions)
    }

    /// Append a region at the current end of code, fixing up its debug offsets.
    /// Returns `(absolute_offset, length)`.
    fn append_region(&mut self, region: Region) -> (usize, usize) {
        let base = self.code.len();
        let len = region.code.len();
        self.code.extend_from_slice(&region.code);
        for d in region.debug {
            self.debug.push(DebugInfo {
                code_offset: d.code_offset + base,
                span: d.span,
            });
        }
        (base, len)
    }

    /// Append a projection table: `[u16 count]` then, per item,
    /// `[u16 expr_off, u16 expr_len, u16 alias_idx]` (alias `0xFFFF` = none),
    /// followed by the item sub-programs.  Returns the table's offset.
    fn append_projection(&mut self, items: &[SelectItem]) -> Result<usize, CompileError> {
        let exprs: Vec<&Expr> = items.iter().map(|i| &i.expr).collect();
        let regions = self.compile_region(&exprs)?;
        // Each projected column gets a name: the explicit `AS` alias, or a name
        // derived from the expression (`chrom` for `SELECT chrom`, `*` for the
        // wildcard, `colN` for a computed expression). Phase 4 stored `0xFFFF`
        // for "no alias"; Phase 5 always records a usable column name so the
        // materializer can key the Row output.
        let alias_idxs: Vec<u16> = items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let name = match &item.alias {
                    Some(a) => a.clone(),
                    None => default_col_name(&item.expr, i),
                };
                self.intern(Value::Str(Rc::from(name.as_str())))
            })
            .collect();

        let table_off = self.code.len();
        let header_len = 2 + regions.len() * 6;
        let mut item_off = table_off + header_len;

        // Header.
        self.emit_u16(items.len() as u16);
        for (region, &alias_idx) in regions.iter().zip(&alias_idxs) {
            self.emit_u16(item_off as u16);
            self.emit_u16(region.code.len() as u16);
            self.emit_u16(alias_idx);
            item_off += region.code.len();
        }
        // Item sub-programs.
        for region in regions {
            // append_region recomputes base = current code len, which matches
            // the item_off values written above.
            self.append_region(region);
        }
        Ok(table_off)
    }

    /// Append an order-by table: `[u16 count]` then, per item,
    /// `[u16 expr_off, u16 expr_len, u8 direction]` (0 = ASC, 1 = DESC),
    /// followed by the key sub-programs.  Returns the table's offset.
    fn append_order(
        &mut self,
        items: &[OrderItem],
        select: Option<&[SelectItem]>,
    ) -> Result<usize, CompileError> {
        // ORDER BY runs on the original record *before* projection, so a key
        // that names a SELECT column (its `AS` alias or inferred name) must be
        // rewritten to the underlying expression — otherwise it loads a missing
        // field, sorts on all-NULL keys, and leaves the rows unordered. #14
        let resolved: Vec<Expr> = items
            .iter()
            .map(|i| resolve_order_key(&i.expr, select))
            .collect();
        let exprs: Vec<&Expr> = resolved.iter().collect();
        let regions = self.compile_region(&exprs)?;

        let table_off = self.code.len();
        let header_len = 2 + regions.len() * 5;
        let mut item_off = table_off + header_len;

        self.emit_u16(items.len() as u16);
        for (region, item) in regions.iter().zip(items) {
            self.emit_u16(item_off as u16);
            self.emit_u16(region.code.len() as u16);
            self.code
                .push(matches!(item.direction, Direction::Desc) as u8);
            item_off += region.code.len();
        }
        for region in regions {
            self.append_region(region);
        }
        Ok(table_off)
    }

    // ── Expression lowering (post-order tree walk) ───────────────────────────

    fn compile_expr(&mut self, expr: &Expr) -> Result<(), CompileError> {
        match expr {
            Expr::IntLit(n, s) => {
                let idx = self.intern(Value::Int(*n));
                self.emit(OpCode::LoadConst, *s);
                self.emit_u16(idx);
            }
            Expr::FloatLit(v, s) => {
                let idx = self.intern(Value::Float(*v));
                self.emit(OpCode::LoadConst, *s);
                self.emit_u16(idx);
            }
            Expr::StringLit(text, s) => {
                let idx = self.intern(Value::Str(Rc::from(text.as_str())));
                self.emit(OpCode::LoadConst, *s);
                self.emit_u16(idx);
            }
            Expr::BoolLit(b, s) => {
                self.emit(if *b { OpCode::LoadTrue } else { OpCode::LoadFalse }, *s);
            }
            Expr::Ident(name, s) => {
                let idx = self.intern(Value::Str(Rc::from(name.as_str())));
                self.emit(OpCode::LoadField, *s);
                self.emit_u16(idx);
            }
            Expr::Var(name, s) => {
                let idx = self.intern(Value::Str(Rc::from(name.as_str())));
                self.emit(OpCode::LoadVar, *s);
                self.emit_u16(idx);
            }
            Expr::Wildcard(s) => self.emit(OpCode::LoadWildcard, *s),
            Expr::Unary { op, operand, span } => {
                self.compile_expr(operand)?;
                self.emit(
                    match op {
                        UnaryOp::Neg => OpCode::Neg,
                        UnaryOp::Not => OpCode::Not,
                    },
                    *span,
                );
            }
            Expr::Binary { op, left, right, span } => {
                self.compile_binary(op, left, right, *span)?;
            }
            Expr::FieldAccess { object, field, span } => {
                self.compile_expr(object)?;
                let idx = self.intern(Value::Str(Rc::from(field.as_str())));
                self.emit(OpCode::GetField, *span);
                self.emit_u16(idx);
            }
            Expr::Call { callee, args, span } => {
                self.compile_call_expr(callee, args, *span)?;
            }
        }
        Ok(())
    }

    fn compile_binary(
        &mut self,
        op: &BinOp,
        left: &Expr,
        right: &Expr,
        span: Span,
    ) -> Result<(), CompileError> {
        // Short-circuit logical operators encode a jump over the RHS.
        if matches!(op, BinOp::And | BinOp::Or) {
            self.compile_expr(left)?;
            self.emit(if matches!(op, BinOp::And) { OpCode::And } else { OpCode::Or }, span);
            let jmp_at = self.code.len();
            self.emit_u16(0);
            self.compile_expr(right)?;
            self.patch_jump(jmp_at);
            return Ok(());
        }

        self.compile_expr(left)?;
        self.compile_expr(right)?;
        let opcode = match op {
            BinOp::Eq => OpCode::Eq,
            BinOp::NotEq => OpCode::Ne,
            BinOp::Lt => OpCode::Lt,
            BinOp::Gt => OpCode::Gt,
            BinOp::LtEq => OpCode::Le,
            BinOp::GtEq => OpCode::Ge,
            BinOp::Add => OpCode::Add,
            BinOp::Sub => OpCode::Sub,
            BinOp::Mul => OpCode::Mul,
            BinOp::Div => OpCode::Div,
            BinOp::And | BinOp::Or => unreachable!("handled above"),
        };
        self.emit(opcode, span);
        Ok(())
    }

    /// `Expr::Call` lowers either to `INDEX` (subscript desugaring: a one-arg
    /// call whose callee is a name and whose arg is a string key) or `CALL_FN`.
    fn compile_call_expr(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        span: Span,
    ) -> Result<(), CompileError> {
        // `name["key"]` is a subscript — UNLESS the name is a known builtin, in
        // which case `gc("ACGT")` is a real one-string-arg function call.
        let is_subscript = args.len() == 1
            && matches!(args[0], Expr::StringLit(..))
            && match callee {
                Expr::Ident(n, _) => !is_builtin(n),
                Expr::FieldAccess { .. } => true,
                _ => false,
            };
        if is_subscript {
            self.compile_expr(callee)?;
            self.compile_expr(&args[0])?;
            self.emit(OpCode::Index, span);
            return Ok(());
        }

        // Real function call: validate name/arity/arg-types before lowering.
        if let Expr::Ident(n, _) = callee {
            validate_builtin_call(n, args, span)?;
        }

        for arg in args {
            self.compile_expr(arg)?;
        }
        let name = match callee {
            Expr::Ident(n, _) => n.as_str(),
            // Non-identifier callee: keep the program total by naming it "<expr>".
            _ => "<expr>",
        };
        let name_idx = self.intern(Value::Str(Rc::from(name)));
        self.emit(OpCode::CallFn, span);
        self.emit_u16(name_idx);
        self.code.push(args.len().min(u8::MAX as usize) as u8);
        Ok(())
    }

    // ── Emission primitives ──────────────────────────────────────────────────

    /// Intern a constant, returning its pool index (deduplicated).
    fn intern(&mut self, v: Value) -> u16 {
        if let Some(i) = self.consts.iter().position(|c| *c == v) {
            return i as u16;
        }
        let idx = self.consts.len();
        self.consts.push(v);
        idx as u16
    }

    /// Emit an opcode byte and record its source span.
    fn emit(&mut self, op: OpCode, span: Span) {
        self.debug.push(DebugInfo {
            code_offset: self.code.len(),
            span,
        });
        self.code.push(op.byte());
    }

    /// Emit a little-endian `u16` operand (no debug entry).
    fn emit_u16(&mut self, v: u16) {
        self.code.extend_from_slice(&v.to_le_bytes());
    }

    /// Patch a forward jump operand at `offset` to point at the current end.
    fn patch_jump(&mut self, offset: usize) {
        let target = self.code.len() as u16;
        self.patch_u16(offset, target);
    }

    /// Overwrite the `u16` at `offset` (little-endian).
    fn patch_u16(&mut self, offset: usize, v: u16) {
        let bytes = v.to_le_bytes();
        self.code[offset] = bytes[0];
        self.code[offset + 1] = bytes[1];
    }
}

// ── Constant folding for WITH values ─────────────────────────────────────────

/// Evaluate a constant expression to a [`Value`], or `None` if it is not a
/// compile-time constant.  Handles literals and unary minus over numerics
/// (e.g. `del_threshold = -0.3`).
fn const_eval(expr: &Expr) -> Option<Value> {
    match expr {
        Expr::IntLit(n, _) => Some(Value::Int(*n)),
        Expr::FloatLit(v, _) => Some(Value::Float(*v)),
        Expr::StringLit(s, _) => Some(Value::Str(Rc::from(s.as_str()))),
        Expr::BoolLit(b, _) => Some(Value::Bool(*b)),
        Expr::Unary { op: UnaryOp::Neg, operand, .. } => match const_eval(operand)? {
            Value::Int(n) => Some(Value::Int(-n)),
            Value::Float(v) => Some(Value::Float(-v)),
            _ => None,
        },
        _ => None,
    }
}

// ── Static region extraction (for BAI seeking) ───────────────────────────────

/// Pull a seekable [`Region`](crate::runtime::Region) out of a `WHERE` clause.
///
/// Recognizes `chr = "chrN"` (or `chrom = ...`) optionally AND-ed with `pos`
/// bounds (`pos >= X`, `pos <= Y`, and the strict variants). Any other shape
/// yields `None`, in which case execution falls back to a full scan plus the
/// per-record predicate — so this is purely an optimization, never a
/// correctness requirement.
pub fn extract_region(filter: &Expr) -> Option<crate::runtime::Region> {
    let mut chrom: Option<String> = None;
    let mut start: Option<i64> = None;
    let mut end: Option<i64> = None;
    collect_region(filter, &mut chrom, &mut start, &mut end);
    chrom.map(|c| crate::runtime::Region {
        chrom: c,
        start,
        end,
    })
}

fn collect_region(
    e: &Expr,
    chrom: &mut Option<String>,
    start: &mut Option<i64>,
    end: &mut Option<i64>,
) {
    match e {
        Expr::Binary {
            op: BinOp::And,
            left,
            right,
            ..
        } => {
            collect_region(left, chrom, start, end);
            collect_region(right, chrom, start, end);
        }
        Expr::Binary {
            op, left, right, ..
        } => {
            if matches!(op, BinOp::Eq) {
                if let Some(name) = ident_string_eq(left, right) {
                    if chrom.is_none() {
                        *chrom = Some(name);
                    }
                    return;
                }
            }
            // Normalize `field OP int` (or `int OP field`, flipping the op).
            if let Some((field, lit, flipped)) = field_int_cmp(left, right) {
                if field == "pos" {
                    let op = if flipped { flip_op(op) } else { op.clone() };
                    match op {
                        BinOp::GtEq | BinOp::Gt => {
                            if start.is_none() {
                                *start = Some(lit);
                            }
                        }
                        BinOp::LtEq | BinOp::Lt => {
                            if end.is_none() {
                                *end = Some(lit);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        _ => {}
    }
}

/// If one operand is `chr`/`chrom` and the other a string literal, return the
/// chromosome name.
fn ident_string_eq(a: &Expr, b: &Expr) -> Option<String> {
    let is_chr = |e: &Expr| matches!(e, Expr::Ident(n, _) if n == "chr" || n == "chrom");
    match (a, b) {
        (Expr::StringLit(s, _), e) | (e, Expr::StringLit(s, _)) if is_chr(e) => Some(s.clone()),
        _ => None,
    }
}

/// If one operand is an identifier and the other an int literal, return
/// `(field_name, int_value, flipped)` where `flipped` is true when the int was
/// the left operand.
fn field_int_cmp(a: &Expr, b: &Expr) -> Option<(String, i64, bool)> {
    match (a, b) {
        (Expr::Ident(n, _), Expr::IntLit(v, _)) => Some((n.clone(), *v, false)),
        (Expr::IntLit(v, _), Expr::Ident(n, _)) => Some((n.clone(), *v, true)),
        _ => None,
    }
}

/// Mirror a comparison operator (used when the literal is the left operand).
fn flip_op(op: &BinOp) -> BinOp {
    match op {
        BinOp::Lt => BinOp::Gt,
        BinOp::Gt => BinOp::Lt,
        BinOp::LtEq => BinOp::GtEq,
        BinOp::GtEq => BinOp::LtEq,
        other => other.clone(),
    }
}

/// Resolve an `ORDER BY` key against the `SELECT` list: a bare identifier that
/// names a projected column (by `AS` alias or by its inferred default name) is
/// rewritten to that column's expression, so the sort key is computed from the
/// original record. Anything else is returned unchanged. #14
fn resolve_order_key(key: &Expr, select: Option<&[SelectItem]>) -> Expr {
    if let (Expr::Ident(name, _), Some(items)) = (key, select) {
        for (i, item) in items.iter().enumerate() {
            let col = item
                .alias
                .clone()
                .unwrap_or_else(|| default_col_name(&item.expr, i));
            if col == *name {
                return item.expr.clone();
            }
        }
    }
    key.clone()
}

/// True if a `SELECT` item already provides a column named `name` (by `AS`
/// alias or as a bare `Ident`).
fn item_provides(item: &SelectItem, name: &str) -> bool {
    if item.alias.as_deref() == Some(name) {
        return true;
    }
    matches!(&item.expr, Expr::Ident(n, _) if n == name)
}

/// For a VCF sink, prepend the canonical VCF identity columns
/// (`chrom`/`pos`/`ref`/`alt`) the projection does not already provide, so the
/// writer can fill the fixed `#CHROM/POS/REF/ALT` fields instead of `.` (#15).
/// A no-op for non-VCF sinks and for `SELECT *` (which already carries every
/// field). The `chr` / `ref_base` aliases the writer also honors count as
/// provided.
fn augment_projection_for_vcf(items: &[SelectItem], into: Option<&IntoClause>) -> Vec<SelectItem> {
    let span = match items.first() {
        Some(i) => i.span,
        None => return items.to_vec(),
    };
    let is_vcf = matches!(into, Some(c) if c.format == Format::Vcf);
    let has_wildcard = items.iter().any(|i| matches!(i.expr, Expr::Wildcard(_)));
    if !is_vcf || has_wildcard {
        return items.to_vec();
    }

    let mut prefix = Vec::new();
    for &(canon, alt) in &[
        ("chrom", Some("chr")),
        ("pos", None),
        ("ref", Some("ref_base")),
        ("alt", None),
    ] {
        let covered = items
            .iter()
            .any(|i| item_provides(i, canon) || alt.is_some_and(|a| item_provides(i, a)));
        if !covered {
            prefix.push(SelectItem {
                expr: Expr::Ident(canon.to_string(), span),
                alias: None,
                span,
            });
        }
    }
    if prefix.is_empty() {
        return items.to_vec();
    }
    prefix.extend_from_slice(items);
    prefix
}

/// Derive a column name for an un-aliased `SELECT` item. A bare field/var keeps
/// its name; a function call is named `fn_arg` (`round(qual)` → `round_qual`,
/// `gc(ref)` → `gc_ref`) so computed columns are self-documenting and stable
/// instead of positional `colN` (#18). `AS` always wins over this.
fn default_col_name(expr: &Expr, index: usize) -> String {
    fn name_of(expr: &Expr) -> Option<String> {
        match expr {
            Expr::Ident(name, _) => Some(name.clone()),
            Expr::Var(name, _) => Some(name.clone()),
            Expr::FieldAccess { field, .. } => Some(field.clone()),
            Expr::Wildcard(_) => Some("*".to_string()),
            Expr::Call { callee, args, .. } => {
                let fname = match callee.as_ref() {
                    Expr::Ident(n, _) => n.clone(),
                    _ => return None,
                };
                // `fn(field)` → `fn_field`; nullary or multi/complex arg → `fn`.
                match (args.len(), args.first().and_then(name_of)) {
                    (1, Some(arg)) if arg != "*" => Some(format!("{fname}_{arg}")),
                    _ => Some(fname),
                }
            }
            _ => None,
        }
    }
    name_of(expr).unwrap_or_else(|| format!("col{}", index + 1))
}

fn span_of_items(items: &[SelectItem], fallback: Span) -> Span {
    items.first().map(|i| i.span).unwrap_or(fallback)
}

fn span_of_order(items: &[OrderItem], fallback: Span) -> Span {
    items.first().map(|i| i.span).unwrap_or(fallback)
}

// ── Disassembler ─────────────────────────────────────────────────────────────

/// Produce a human-readable disassembly of `program`'s main code section
/// (offset 0 up to and including the first `HALT`), followed by the `WHERE`
/// predicate sub-program if present.
///
/// Constant, field, and path references are resolved inline.  Per-record
/// projection/order tables after `HALT` are summarised rather than decoded
/// (they are data, not instructions).
pub fn disassemble(program: &Program) -> String {
    let mut out = String::new();
    let code = &program.code;
    let mut pc = 0usize;
    let mut filter_region: Option<(usize, usize)> = None;

    while pc < code.len() {
        let op = match OpCode::from_byte(code[pc]) {
            Some(op) => op,
            None => {
                out.push_str(&format!("{pc:04X}  <unknown 0x{:02X}>\n", code[pc]));
                pc += 1;
                continue;
            }
        };
        let start = pc;
        let line = disasm_one(program, op, &mut pc, &mut filter_region);
        out.push_str(&format!("{start:04X}  {line}\n"));
        if op == OpCode::Halt {
            break;
        }
    }

    // Decode the WHERE predicate region (valid expression opcodes ending in
    // RET_PRED) for clarity.
    if let Some((off, len)) = filter_region {
        if off + len <= code.len() {
            out.push('\n');
            out.push_str(&format!("; predicate @ {off:04X} (len {len})\n"));
            let mut p = off;
            while p < off + len {
                let op = match OpCode::from_byte(code[p]) {
                    Some(op) => op,
                    None => break,
                };
                let start = p;
                let mut dummy = None;
                let line = disasm_one(program, op, &mut p, &mut dummy);
                out.push_str(&format!("{start:04X}  {line}\n"));
            }
        }
    }

    out
}

/// Decode a single instruction starting at `*pc`, advancing `*pc` past it.
fn disasm_one(
    program: &Program,
    op: OpCode,
    pc: &mut usize,
    filter_region: &mut Option<(usize, usize)>,
) -> String {
    let code = &program.code;
    *pc += 1; // opcode byte
    let read_u16 = |pc: &mut usize| -> u16 {
        let v = u16::from_le_bytes([code[*pc], code[*pc + 1]]);
        *pc += 2;
        v
    };
    let read_u8 = |pc: &mut usize| -> u8 {
        let v = code[*pc];
        *pc += 1;
        v
    };
    let konst = |i: u16| -> String {
        program
            .consts
            .get(i as usize)
            .map(|v| v.to_string())
            .unwrap_or_else(|| format!("?{i}"))
    };

    match op {
        OpCode::LoadConst => format!("{:<13}{}", op.name(), konst(read_u16(pc))),
        OpCode::LoadField | OpCode::GetField | OpCode::LoadVar => {
            format!("{:<13}{}", op.name(), konst(read_u16(pc)))
        }
        OpCode::And | OpCode::Or | OpCode::JumpIfFalse | OpCode::Jump => {
            format!("{:<13}-> {:04X}", op.name(), read_u16(pc))
        }
        OpCode::CallFn => {
            let name = konst(read_u16(pc));
            let argc = read_u8(pc);
            format!("{:<13}{} argc={}", op.name(), name, argc)
        }
        OpCode::OpenSource | OpCode::WriteInto => {
            let fmt = read_u8(pc);
            let path = konst(read_u16(pc));
            format!("{:<13}{} {}", op.name(), format_name(fmt), path)
        }
        OpCode::Filter => {
            let off = read_u16(pc);
            let len = read_u16(pc);
            *filter_region = Some((off as usize, len as usize));
            format!("{:<13}pred@{:04X} len={}", op.name(), off, len)
        }
        OpCode::Project | OpCode::OrderBy => {
            let off = read_u16(pc);
            format!("{:<13}table@{:04X}", op.name(), off)
        }
        OpCode::SetParam => format!("{:<13}{}", op.name(), konst(read_u16(pc))),
        // No-operand opcodes.
        _ => op.name().to_string(),
    }
}

#[cfg(test)]
mod builtin_validation_tests {
    use super::*;

    fn err(q: &str) -> CompileError {
        crate::compile(q).unwrap_err()
    }

    #[test]
    fn unknown_function_suggests() {
        let e = err(r#"FROM bam "x" WHERE abz(depth) > 0 CALL variants"#);
        assert!(matches!(e, CompileError::UnknownFunction { .. }), "{e:?}");
        // The CLI computes the hint via suggest_function.
        assert_eq!(suggest_function("abz").as_deref(), Some("abs"));
        assert_eq!(suggest_function("revcom").as_deref(), Some("revcomp"));
    }

    #[test]
    fn wrong_arity() {
        match err(r#"FROM bam "x" WHERE abs(depth, 2) > 0 CALL variants"#) {
            CompileError::FunctionArity {
                name,
                expected,
                got,
                ..
            } => {
                assert_eq!(name, "abs");
                assert_eq!(expected, "1");
                assert_eq!(got, 2);
            }
            other => panic!("expected FunctionArity, got {other:?}"),
        }
        // variadic: min(1, ..) needs at least 1.
        assert!(matches!(
            err(r#"FROM bam "x" WHERE min() > 0 CALL variants"#),
            CompileError::FunctionArity { .. }
        ));
    }

    #[test]
    fn genomic_arg_type_mismatch() {
        match err(r#"FROM bam "x" WHERE gc(5) > 0.5 CALL variants"#) {
            CompileError::FunctionArgType {
                name,
                arg,
                expected,
                got,
                ..
            } => {
                assert_eq!(name, "gc");
                assert_eq!(arg, 1);
                assert_eq!(expected, "string");
                assert_eq!(got, "number");
            }
            other => panic!("expected FunctionArgType, got {other:?}"),
        }
        // arithmetic expr is statically numeric → also caught.
        assert!(matches!(
            err(r#"FROM bam "x" WHERE revcomp(1 + 2) = "x" CALL variants"#),
            CompileError::FunctionArgType { .. }
        ));
    }

    #[test]
    fn valid_calls_compile() {
        // Field/var args have unknown static type → never flagged (no false
        // positive on the common gc(ref) case).
        assert!(crate::compile(r#"FROM bam "x" WHERE gc(ref) > 0.5 CALL variants"#).is_ok());
        // A string-literal arg to a builtin is a CALL, not an info["DP"] subscript.
        assert!(
            crate::compile(r#"FROM bam "x" WHERE translate("ATGAAATAG") = "MK*" CALL variants"#)
                .is_ok()
        );
        assert!(crate::compile(r#"FROM bam "x" WHERE abs(depth) > 0 CALL variants"#).is_ok());
        assert!(
            crate::compile(r#"FROM bam "x" WHERE concat(chr, ":", pos) = "7:1" CALL variants"#)
                .is_ok()
        );
    }

    #[test]
    fn subscript_still_works() {
        // `info` is not a builtin, so info["DP"] stays a subscript and compiles.
        assert!(crate::compile(r#"FROM vcf "x" WHERE info["DP"] > 10 CALL variants"#).is_ok());
    }
}
