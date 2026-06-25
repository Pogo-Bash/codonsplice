# CodonSplice

CodonSplice is the **engine** half of a two-crate genomic query system:

```text
spliceql (language)        →  codonsplice-core (engine)
Lexer → Parser → AST       →  Compiler → Bytecode → VM
```

You write [SpliceQL](https://github.com/Pogo-Bash/spliceql) — a small, SQL-like
query language for genomic files — and CodonSplice compiles it to a compact
stack-machine bytecode and runs it. Phase 3 (this milestone) implements the
**bytecode compiler**, a **VM** that fully evaluates expressions, the
**disassembler**, and the `splice` **CLI + TUI**. Pipeline opcodes that touch
real genomic data (`OPEN_SOURCE … WRITE_INTO`, `CALL_*`) compile and disassemble
today and stub out at runtime with `VmError::NotYetImplemented` — the
[cnvlens-core bridge that lights them up is Phase 4](docs/PHASE4_DESIGN.md).

## Workspace layout

```text
codonsplice/
├── Cargo.toml                 workspace root
├── crates/
│   ├── spliceql/              → symlink to ../../spliceql (path dep, read-only)
│   ├── codonsplice-core/      bytecode compiler + VM + (Phase 4) cnvlens bridge
│   └── splice-cli/            the `splice` binary: CLI + ratatui TUI
├── docs/
│   ├── PHASE4_DESIGN.md       cnvlens-core integration design
│   └── NPM_PACKAGE.md         WASM/npm package + framework integration design
└── README.md
```

`spliceql` is referenced through the `crates/spliceql` symlink and is
**excluded** as a workspace member (it has its own fuzz sub-crate and lockfile);
it is consumed purely as a path dependency. No spliceql source is modified by
this crate — all engine work is additive.

## Build & test

```sh
cargo test --workspace      # 27 compiler/VM/disassembler tests
cargo clippy --workspace    # clean
cargo run -p splice-cli     # launch the TUI
```

## The `splice` CLI

```text
splice                        launch the interactive TUI
splice query   "FROM bam ..."  compile + run (pipeline ops stub until Phase 4)
splice compile "FROM bam ..."  compile + print disassembled bytecode
splice check   "FROM bam ..."  parse + type-check only, no execution
```

Example:

```sh
$ splice compile 'FROM bam "x.bam" WHERE depth > 30 CALL variants WITH min_allele_freq = 0.05'
0000  OPEN_SOURCE  bam "x.bam"
0004  SCAN
0005  FILTER       pred@0012 len=8
000A  LOAD_CONST   0.05
000D  SET_PARAM    "min_allele_freq"
0010  CALL_VARIANTS
0011  HALT

; predicate @ 0012 (len 8)
0012  LOAD_FIELD   "depth"
0015  LOAD_CONST   30
0018  GT
0019  RET_PRED
```

A compile error renders with `query:line:col`, a caret, and a "did you mean"
hint (Levenshtein + shared-token ranking over the known parameter names):

```sh
$ splice check 'FROM bam "x.bam" CALL variants WITH min_freq = 0.05'
error[E001]: unknown parameter "min_freq"
  --> query:1:48
   |
 1 | FROM bam "x.bam" CALL variants WITH min_freq = 0.05
   |                                                ^^^^ did you mean "min_allele_freq"?
```

> The caret points at the parameter **value** rather than the key: the spliceql
> AST stores `WITH` keys as bare `String`s with no `Span`, and spliceql is
> read-only, so the value's span is the closest anchor available.

## The TUI

Launching `splice` with no subcommand opens a three-pane educational TUI:

```text
┌─────────────────────────────────────────────────┐
│  CodonSplice  │  SpliceQL query engine           │
├──────────────────────┬──────────────────────────┤
│  EDITOR              │  OUTPUT                  │
│  FROM bam "x.bam"    │  bytecode / results /    │
│  WHERE depth > 30    │  errors render here      │
│  CALL variants       │                          │
├──────────────────────┴──────────────────────────┤
│  ARCHITECTURE  (always visible)                  │
│  spliceql (language)  →  codonsplice (engine)    │
│  Lexer→Parser→AST     →  Compiler→Bytecode→VM    │
└─────────────────────────────────────────────────┘
```

| Key | Action |
| --- | --- |
| `Ctrl+Enter` / `F5` | compile + run the current query |
| `Ctrl+D` | disassemble bytecode (opcodes cyan, operands yellow, addresses dim, comments gray) |
| `Ctrl+A` | pretty-print the parsed AST |
| `Tab` | switch focus between editor and output |
| `F1` | toggle the keybindings help overlay |
| `Ctrl+Q` | quit |

The **ARCHITECTURE** panel is always on screen so the two-crate boundary —
`spliceql` is the language, `codonsplice-core` is the engine — is explicit to
anyone using the tool.

---

## Deliverable A — `codonsplice-core` public API

### Top-level functions (`lib.rs`)

```rust
/// Parse + compile `source` into a Program.
pub fn compile(source: &str) -> Result<Program, CompileError>;

/// compile() then disassemble() — the TUI's bytecode view.
pub fn compile_and_disassemble(source: &str) -> Result<String, CompileError>;

/// compile() + Vm::new + Vm::run. Pipeline opcodes stub out as
/// VmError::NotYetImplemented until Phase 4.
pub fn execute(source: &str) -> Result<VmOutput, VmError>;

/// Compile a standalone expression (wrapped as a WHERE predicate) into an
/// expression-only Program ending in HALT, relocated to offset 0.
pub fn compile_expr(expr_source: &str) -> Result<Program, CompileError>;

/// Evaluate a constant expression to a RuntimeValue (compile_expr + eval).
pub fn eval_expr(expr_source: &str) -> Result<RuntimeValue, EvalError>;

pub enum EvalError { Compile(CompileError), Vm(VmError) }
```

### Compiler module (`pub use compiler::*`)

```rust
pub struct Program { pub consts: Vec<Value>, pub code: Vec<u8>, pub debug: Vec<DebugInfo> }
impl Program { pub fn span_at(&self, offset: usize) -> Option<Span>; }

pub struct DebugInfo { pub code_offset: usize, pub span: Span }

pub enum Value { Int(i64), Float(f64), Str(Rc<str>), Bool(bool), Null }
impl Value { pub fn type_name(&self) -> &'static str; }

pub struct Compiler { /* consts, code, debug, source */ }
impl Compiler {
    pub fn new(source: impl Into<String>) -> Self;
    pub fn compile(self, query: &Query) -> Result<Program, CompileError>;
    pub fn compile_expr_program(self, expr: &Expr) -> Program;
}

pub enum OpCode { /* full instruction set */ }
impl OpCode {
    pub fn byte(self) -> u8;
    pub fn from_byte(b: u8) -> Option<OpCode>;
    pub fn operand_len(self) -> usize;
    pub fn name(self) -> &'static str;
}

pub enum CompileError {
    UnknownParam      { key: String, span: Span },
    ParamTypeMismatch { key: String, expected: &'static str, got: &'static str, span: Span },
    NonConstantParam  { span: Span },
    ParamWithoutCall  { span: Span },
    MultipleFrom      { span: Span },
    ParseError(ParseError),
}
impl CompileError {
    pub fn code(&self) -> &'static str;          // E000..E005
    pub fn span(&self) -> Span;
    pub fn message(&self) -> String;
    pub fn render(&self, source: &str, suggestion: Option<&str>) -> String;
}
impl From<ParseError> for CompileError;
impl Display for CompileError;                    // one-line form

pub fn disassemble(program: &Program) -> String;

// "did you mean" helpers (snake_case-aware ranking)
pub fn did_you_mean(unknown: &str, candidates: &[&str]) -> Option<String>;
pub fn suggest_param(unknown: &str, operation: &str) -> Option<String>;
pub fn param_names_for(operation: &str) -> Vec<&'static str>;
pub fn levenshtein(a: &str, b: &str) -> usize;
```

### VM module (`pub use vm::*`)

```rust
pub struct Vm { /* program, stack, pc */ }
impl Vm {
    pub fn new(program: Program) -> Self;
    pub fn run(&mut self) -> Result<VmOutput, VmError>;       // main program
    pub fn eval_expr(&mut self) -> Result<RuntimeValue, VmError>; // expr-only program
}

pub enum RuntimeValue {
    Int(i64), Float(f64), Str(Rc<str>), Bool(bool), Null,
    Pending,   // placeholder for Phase 4 Dataset/Cursor/Record handles
}
impl RuntimeValue { pub fn type_name(&self) -> &'static str; }

pub enum VmOutput { Ready(Program), Text(String) }

pub enum VmError {
    UnknownOpcode(u8, usize),
    StackUnderflow(usize),
    TypeMismatch { expected: &'static str, got: &'static str, pc: usize },
    NotYetImplemented(String),   // pipeline / CALL ops, until Phase 4
}
```

### Bytecode instruction set

| Range | Opcodes |
| --- | --- |
| `0x01–0x07` | `LOAD_CONST(u16)` `LOAD_TRUE` `LOAD_FALSE` `LOAD_FIELD(u16)` `GET_FIELD(u16)` `INDEX` `LOAD_WILDCARD` |
| `0x10–0x21` | `NEG` `NOT` `ADD` `SUB` `MUL` `DIV` `EQ` `NE` `LT` `GT` `LE` `GE` `AND(u16 jmp)` `OR(u16 jmp)` `CALL_FN(u16,u8)` `RET_PRED` `JUMP_IF_FALSE(u16)` `JUMP(u16)` |
| `0x40–0x4F` | `OPEN_SOURCE(u8,u16)` `SCAN` `FILTER(u16,u16)` `PROJECT(u16)` `SET_PARAM(u16)` `ORDER_BY(u16)` `LIMIT` `WRITE_INTO(u8,u16)` `HALT` |
| `0x50–0x54` | `CALL_VARIANTS` `CALL_CNV` `CALL_COVERAGE` `CALL_READS` `CALL_HEADER` |

Per-record sub-programs (the `WHERE` predicate, `SELECT` items, `ORDER BY` keys)
are appended **after** `HALT`; the referencing opcode carries the absolute byte
offset (and, for `FILTER`, the length). Jumps inside a sub-program are encoded
relative to that sub-program's start.

---

## Roadmap

- **Phase 1** — spliceql lexer ✅
- **Phase 2** — spliceql parser + AST ✅
- **Phase 3** — codonsplice-core compiler + VM + disassembler + CLI/TUI ✅ *(this)*
- **Phase 4** — [cnvlens-core execution bridge](docs/PHASE4_DESIGN.md) + [npm/WASM package](docs/NPM_PACKAGE.md)

## License

MIT.
