# Phase 5 — public API additions

Phase 5 adds VCF input, `$variable` templating, real `SELECT` projection, true
streaming, the `.spq` file format, compiled binaries, and the grammar crate.
This lists every new public symbol, grouped by crate, with deviations noted.

## spliceql (additive — Phase 1/2 otherwise frozen)

```rust
// token.rs
TokenKind::Var(String)          // $name (name excludes the leading $)
// ast.rs
Expr::Var(String, Span)         // $name in expression / path position
```

Behavior: the lexer lexes `$[A-Za-z_][A-Za-z0-9_]*` → `Var`; the parser produces
`Expr::Var` in prefix position and accepts `$var` in `FROM`/`INTO` path position
(stored as the string `"$name"`). `FILTER` is now also accepted as a contextual
identifier in expression position (so VCF's `filter` column is usable in `WHERE`).

## cnvlens-core

```rust
// error.rs (Phase 4) — unchanged
// model.rs
struct Variant { … , filter: Option<String>, id: Option<String> } // NEW fields (skipped in JSON when None)

// vcf.rs (new module)
pub fn stream_vcf(bytes: &[u8], region: Option<&Region>)
    -> impl Iterator<Item = Result<Variant, CoreError>>;

// variants.rs
pub fn stream_variants(
    bam: &[u8], bai: Option<&[u8]>, opts: &VariantOptions,
    region: Option<&Region>, limit: Option<usize>,
) -> Box<dyn Iterator<Item = Result<Variant, CoreError>>>;
pub fn collect_variants(...) -> Result<Vec<Variant>, CoreError>;   // unchanged (now wraps gather)
pub fn call_variants_region(...) -> Result<Vec<Variant>, CoreError>; // unchanged
```

`stream_variants`' `limit` short-circuits the per-window pileup
(`call_from_pileup` returns once `limit` variants are produced).

## codonsplice-core

```rust
// compiler.rs
OpCode::LoadVar                  // 0x08, u16 name_idx
pub fn extract_region(...)       // (Phase 4) — unchanged
// `min_af` added as an alias of `min_allele_freq` in the variant WITH params.

// runtime.rs
Record::Row(Vec<(String, RuntimeValue)>)   // NEW: projected output row
Record::into_row(self) -> Record
pub struct ProjItem { pub prog: Program, pub name: String, pub wildcard: bool }
pub struct VarMap(pub HashMap<String, RuntimeValue>);
impl VarMap { fn new(); fn insert(name, value); fn get(name) }
// Cursor gains: projection: Option<Vec<ProjItem>>, producer: Option<RecordProducer>, vars: VarMap
pub type RecordProducer = Box<dyn FnOnce(Option<usize>) -> Result<Vec<Record>, CoreError>>;

// vm.rs
VmOutput::Rows(Vec<Record>)      // NEW: projected SELECT output
VmError::UnboundVariable { name: String, pc: usize }
impl Vm { pub fn with_vars(self, vars: VarMap) -> Self }

// bytecode.rs (new module)
impl Program { pub fn to_bytes(&self) -> Vec<u8>; pub fn from_bytes(&[u8]) -> Result<Program, BytecodeError>; }
pub enum BytecodeError { InvalidMagic, UnsupportedVersion(u8), Truncated, InvalidUtf8(Utf8Error) }
```

Re-exported from the crate root: `VarMap`, `ProjItem`, `BytecodeError`, plus the
existing Phase 4 set.

## splice-cli

```text
splice new <name>                       scaffold <name>.spq
splice run <file.spq> [--var value ...] run, binding $vars from args
splice build <file.spq> [-o name] [--release] [--target T] [--wasm] [--emit-bc]
splice <file.spq> [--var value ...]     shebang/direct execution → run
```

New modules: `directive` (`.spq` preamble parser: `Directives`, `InputDecl`,
`OutputDecl`, `VarKind`, `parse_directives`, `scan_vars`), `spq` (`cmd_new`,
`cmd_run`, `vars_from_args`), `build` (`cmd_build`, `BuildOpts`).

## codonsplice-wasm

```rust
CodonSplice::execute(source, files, vars) -> JsValue          // vars param NEW
CodonSplice::execute_bytecode(bc_bytes, files, vars) -> JsValue  // NEW
CodonSplice::stream(source, files, vars, onRecord, onDone, onError)  // vars param NEW
```

`pkg/index.js` `execute`/`stream` gain a `vars` option + a new `executeBytecode`
helper; the react/vue/svelte/astro wrappers thread `vars` through.

## spliceql-grammar (new data-only crate)

TextMate grammar (`grammars/spliceql.tmLanguage.json`, scope `source.spq`, 13
rules), Linguist assets (`linguist/{languages.yml.fragment, sample.spq,
LINGUIST_PR.md}`), and a VS Code manifest (`vscode/package.json`).

## Deviations from the Phase 5 spec

1. **`Value::SubProgramTable` not added.** Projection reuses Phase 4's existing
   code-section projection table (`append_projection` already serialized
   `(expr_off, expr_len, name_idx)` per column); `op_project` parses it into
   `Vec<ProjItem>`. Functionally identical to the spec's const-pool table.
2. **`Cursor.projection` is `Option<Vec<ProjItem>>`**, not `Option<Program>` —
   a SELECT has multiple columns. `Cursor` also gains `producer`/`vars` beyond
   the spec's fields.
3. **VCF reader is hand-rolled**, not noodles-vcf (avoids enabling the heavy
   `vcf` feature; BGZF-compressed VCF is still inflated via noodles bgzf).
4. **`CALL_VARIANTS` is deferred** (a `RecordProducer` run at materialization)
   so the resolved `LIMIT` reaches the pileup — `LIMIT` is emitted after `CALL`
   in bytecode order, so eager calling couldn't see it. The limit hint is only
   passed when there is no per-record predicate (a predicate filters afterward).
5. **`.spq.bc` does not carry `region`** (the format is consts/code/debug only,
   per spec). Compiled binaries therefore fall back to full-scan + predicate
   filtering — correct, just without the BAI-seek optimization.
6. **`splice run`/`build` parse args manually** (`--flag value` / `--flag=value`)
   rather than building a dynamic clap `Command` — simpler and dependency-free.
7. **`splice new` prints next steps** instead of auto-launching the TUI editor
   (the TUI requires an interactive terminal; auto-launch would hang in CI).
