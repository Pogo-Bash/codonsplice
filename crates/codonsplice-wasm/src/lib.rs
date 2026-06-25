//! WASM bindings for the CodonSplice engine — the `@codonsplice/wasm` package.
//!
//! The JS surface mirrors the native VM: compile/disassemble, type-check, and
//! execute a SpliceQL query against an in-memory file map (`{ "name.bam":
//! Uint8Array, "name.bam.bai": Uint8Array }`). Files are served to the VM via a
//! [`MapIo`] backend, so no filesystem access is needed in the browser.

use std::collections::HashMap;
use std::io;

use wasm_bindgen::prelude::*;

use codonsplice_core::vm::{records_to_json, Io};
use codonsplice_core::{compile, compile_and_disassemble, Program, RuntimeValue, VarMap, Vm, VmOutput};

/// In-memory I/O backend: reads come from the JS-provided file map; writes
/// (`INTO`) are captured so they can be returned to JS.
struct MapIo {
    files: HashMap<String, Vec<u8>>,
    writes: HashMap<String, Vec<u8>>,
}

impl MapIo {
    fn new(files: HashMap<String, Vec<u8>>) -> Self {
        MapIo {
            files,
            writes: HashMap::new(),
        }
    }
}

impl Io for MapIo {
    fn read_file(&self, path: &str) -> io::Result<Vec<u8>> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("no such file: {path}")))
    }
    fn read_sibling_index(&self, path: &str) -> Option<Vec<u8>> {
        self.files.get(&format!("{path}.bai")).cloned()
    }
    fn write_file(&mut self, path: &str, bytes: &[u8]) -> io::Result<()> {
        self.writes.insert(path.to_string(), bytes.to_vec());
        Ok(())
    }
}

/// Decode the JS file map `{ name: Uint8Array }` into a Rust `HashMap`.
fn parse_files(files: JsValue) -> Result<HashMap<String, Vec<u8>>, JsValue> {
    let mut out = HashMap::new();
    if files.is_undefined() || files.is_null() {
        return Ok(out);
    }
    let obj: js_sys::Object = files
        .dyn_into()
        .map_err(|_| JsValue::from_str("files must be an object of { name: Uint8Array }"))?;
    let entries = js_sys::Object::entries(&obj);
    for entry in entries.iter() {
        let pair: js_sys::Array = entry.into();
        let name = pair
            .get(0)
            .as_string()
            .ok_or_else(|| JsValue::from_str("file key must be a string"))?;
        let val = pair.get(1);
        let bytes = js_sys::Uint8Array::new(&val).to_vec();
        out.insert(name, bytes);
    }
    Ok(out)
}

/// Decode the JS vars object `{ name: value }` into a [`VarMap`]. JS strings →
/// Str, integral numbers → Int, other numbers → Float, booleans → Bool.
fn parse_vars(vars: JsValue) -> Result<VarMap, JsValue> {
    let mut out = VarMap::new();
    if vars.is_undefined() || vars.is_null() {
        return Ok(out);
    }
    let obj: js_sys::Object = vars
        .dyn_into()
        .map_err(|_| JsValue::from_str("vars must be an object of { name: value }"))?;
    for entry in js_sys::Object::entries(&obj).iter() {
        let pair: js_sys::Array = entry.into();
        let name = pair
            .get(0)
            .as_string()
            .ok_or_else(|| JsValue::from_str("var key must be a string"))?;
        let val = pair.get(1);
        let rv = if let Some(s) = val.as_string() {
            RuntimeValue::Str(std::sync::Arc::from(s.as_str()))
        } else if let Some(b) = val.as_bool() {
            RuntimeValue::Bool(b)
        } else if let Some(n) = val.as_f64() {
            if n.fract() == 0.0 {
                RuntimeValue::Int(n as i64)
            } else {
                RuntimeValue::Float(n)
            }
        } else {
            RuntimeValue::Null
        };
        out.insert(name, rv);
    }
    Ok(out)
}

fn js_err<E: std::fmt::Display>(e: E) -> JsValue {
    JsValue::from_str(&e.to_string())
}

/// The CodonSplice engine handle exposed to JavaScript.
#[wasm_bindgen]
pub struct CodonSplice {}

#[wasm_bindgen]
impl CodonSplice {
    /// Initialize the engine. Installs the panic hook so Rust panics surface in
    /// the JS console. Call once before anything else.
    #[wasm_bindgen(constructor)]
    pub fn new() -> CodonSplice {
        console_error_panic_hook::set_once();
        CodonSplice {}
    }

    /// Compile a SpliceQL query and return its disassembled bytecode.
    pub fn compile(&self, source: &str) -> Result<String, JsValue> {
        compile_and_disassemble(source).map_err(js_err)
    }

    /// Parse + type-check only. Returns `null` on success, the error string on
    /// failure.
    pub fn check(&self, source: &str) -> Option<String> {
        match compile(source) {
            Ok(_) => None,
            Err(e) => Some(e.to_string()),
        }
    }

    /// Execute a query against the JS file map, binding `$variables` from the
    /// `vars` object (`{ name: value }`). Returns the result as a JSON value
    /// (an array of records/rows, or `{ "text": ... }` for header/`INTO`).
    pub fn execute(&self, source: &str, files: JsValue, vars: JsValue) -> Result<JsValue, JsValue> {
        let program = compile(source).map_err(js_err)?;
        self.run_program(program, files, vars)
    }

    /// Execute pre-compiled `.spq.bc` bytecode (a `Uint8Array`) against the file
    /// map + variables. Mirrors `execute` but skips parsing/compilation.
    pub fn execute_bytecode(
        &self,
        bc_bytes: &[u8],
        files: JsValue,
        vars: JsValue,
    ) -> Result<JsValue, JsValue> {
        let program = Program::from_bytes(bc_bytes).map_err(js_err)?;
        self.run_program(program, files, vars)
    }

    /// Execute and stream: `on_record` is called per record, `on_done` when the
    /// stream completes, `on_error` on failure.
    pub fn stream(
        &self,
        source: &str,
        files: JsValue,
        vars: JsValue,
        on_record: &js_sys::Function,
        on_done: &js_sys::Function,
        on_error: &js_sys::Function,
    ) -> Result<(), JsValue> {
        match self.execute(source, files, vars) {
            Ok(value) => {
                if js_sys::Array::is_array(&value) {
                    let arr = js_sys::Array::from(&value);
                    for v in arr.iter() {
                        let _ = on_record.call1(&JsValue::NULL, &v);
                    }
                } else {
                    let _ = on_record.call1(&JsValue::NULL, &value);
                }
                let _ = on_done.call0(&JsValue::NULL);
                Ok(())
            }
            Err(e) => {
                let _ = on_error.call1(&JsValue::NULL, &e);
                Ok(())
            }
        }
    }

    /// The SpliceQL language crate version.
    pub fn spliceql_version() -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    /// The codonsplice-core engine version.
    pub fn version() -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }
}

impl CodonSplice {
    /// Shared execution path for `execute` / `execute_bytecode`.
    fn run_program(
        &self,
        program: Program,
        files: JsValue,
        vars: JsValue,
    ) -> Result<JsValue, JsValue> {
        let map = parse_files(files)?;
        let varmap = parse_vars(vars)?;
        let mut vm = Vm::with_io(program, Box::new(MapIo::new(map))).with_vars(varmap);
        let out = vm.run().map_err(js_err)?;
        let json = match out {
            VmOutput::Records(recs) | VmOutput::Rows(recs) => records_to_json(&recs),
            VmOutput::Text(t) => serde_json::json!({ "text": t }).to_string(),
            VmOutput::Ready(_) => "[]".to_string(),
        };
        js_sys::JSON::parse(&json)
    }
}

impl Default for CodonSplice {
    fn default() -> Self {
        Self::new()
    }
}
