// @codonsplice/wasm — ergonomic entry point over the wasm-bindgen bindings.
//
// `codonsplice_wasm.js` (the wasm-pack output) exports the raw `CodonSplice`
// class and a default `init()` that loads the .wasm. This module wraps them in
// a singleton engine plus `execute`/`stream` helpers that accept File /
// ArrayBuffer / Uint8Array values and normalize them to the Uint8Array map the
// engine expects.

import init, { CodonSplice } from './codonsplice_wasm.js'

let _engine = null

/** Initialize (once) and return the shared engine. */
export async function initEngine() {
  if (_engine) return _engine
  await init()
  _engine = new CodonSplice()
  return _engine
}

export { CodonSplice }

async function normalizeFiles(files) {
  const fileMap = {}
  for (const [name, fileOrBuffer] of Object.entries(files || {})) {
    if (typeof File !== 'undefined' && fileOrBuffer instanceof File) {
      fileMap[name] = new Uint8Array(await fileOrBuffer.arrayBuffer())
    } else if (fileOrBuffer instanceof ArrayBuffer) {
      fileMap[name] = new Uint8Array(fileOrBuffer)
    } else {
      fileMap[name] = fileOrBuffer // assume Uint8Array
    }
  }
  return fileMap
}

/** Execute a query and return the result (array of records, or { text }). */
export async function execute({ query, files }) {
  const engine = await initEngine()
  return engine.execute(query, await normalizeFiles(files))
}

/** Stream a query's records: onRecord per row, onDone at end, onError on fail. */
export async function stream({ query, files, onRecord, onDone, onError }) {
  const engine = await initEngine()
  return engine.stream(
    query,
    await normalizeFiles(files),
    onRecord || (() => {}),
    onDone || (() => {}),
    onError || ((e) => console.error(e)),
  )
}

/** Compile a query to disassembled bytecode (throws on error). */
export async function compile(query) {
  const engine = await initEngine()
  return engine.compile(query)
}

/** Type-check a query: returns null on success, an error string on failure. */
export async function check(query) {
  const engine = await initEngine()
  return engine.check(query)
}
