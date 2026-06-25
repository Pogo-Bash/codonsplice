# npm package & WASM target design

CodonSplice ships to JavaScript as a WebAssembly module so a SpliceQL query can
run entirely in the browser — no server, no uploads — reusing the **same** Rust
engine that powers the native `splice` binary and CNVLens. This document
describes the WASM build target, the JS/TS API surface, the framework
integrations, and how the WASM execution layer differs from the native VM.

## 1. WASM build target

A new thin crate `crates/codonsplice-wasm` wraps `codonsplice-core` with
`wasm-bindgen`, mirroring how `cnvlens-core` already gates its shim to
`cfg(target_arch = "wasm32")`:

```toml
# crates/codonsplice-wasm/Cargo.toml
[lib]
crate-type = ["cdylib", "rlib"]

[dependencies]
codonsplice-core = { path = "../codonsplice-core" }
serde = { version = "1", features = ["derive"] }
serde-wasm-bindgen = "0.6"

[target.'cfg(target_arch = "wasm32")'.dependencies]
wasm-bindgen = "0.2"
wasm-bindgen-futures = "0.4"
console_error_panic_hook = "0.1"
```

Built with `wasm-pack build --release --target web` (same toolchain CNVLens
uses: `npm run build:wasm`). Because cnvlens-core is already WASM-ready, the
whole pipeline — compiler, VM, and genomic backend — compiles to one `.wasm`.

```rust
#[wasm_bindgen]
pub struct CodonSplice { /* holds a configured VM + Io backed by the files map */ }

#[wasm_bindgen]
impl CodonSplice {
    #[wasm_bindgen(js_name = init)]
    pub async fn init() -> CodonSplice { console_error_panic_hook::set_once(); /* … */ }

    /// Disassemble without running — pure, no files needed.
    pub fn compile(&self, query: &str) -> Result<String, JsValue>;

    /// Compile + run. `request` is { query, files }; returns the result object.
    pub async fn execute(&self, request: JsValue) -> Result<JsValue, JsValue>;

    /// Streaming variant: invokes onRecord per row, onDone with stats.
    pub async fn stream(&self, request: JsValue) -> Result<(), JsValue>;
}
```

`files` arrive as `{ "sample.bam": ArrayBuffer | Uint8Array }` and back the VM's
`Io` trait (the same trait the native CLI implements over `std::fs`) — so
`OPEN_SOURCE "sample.bam"` reads from the JS map instead of disk. Nothing leaves
the browser tab.

## 2. JS/TS API surface

### `@codonsplice/wasm` (core)

```ts
import { CodonSplice } from '@codonsplice/wasm'

const engine = await CodonSplice.init()

// one-shot
const result = await engine.execute({
  query: `
    FROM bam "sample.bam"
    WHERE depth > 30
    CALL variants
    WITH min_af = 0.05
  `,
  files: { "sample.bam": bamArrayBuffer },
})
console.log(result.variants)

// streaming
await engine.stream({
  query: `FROM bam "sample.bam" SELECT reads WHERE depth > 10`,
  files: { "sample.bam": bamFile },
  onRecord: (record) => console.log(record),
  onDone:   (stats)  => console.log(`Done: ${stats.total} records`),
})

// compile-only (bytecode disassembly), no files
const asm = engine.compile(`FROM bam "x.bam" CALL variants`)
```

TypeScript types are generated from the Rust result structs via
`serde-wasm-bindgen` + `wasm-pack`'s `.d.ts` emission, then re-exported:

```ts
export interface ExecuteRequest { query: string; files: Record<string, ArrayBuffer | Uint8Array | File> }
export interface Variant { chrom: string; pos: number; ref: string; alt: string; qual: number; depth: number; allele_freq: number; /* … */ }
export interface ExecuteResult { variants?: Variant[]; windows?: CoverageWindow[]; cnvs?: Cnv[]; text?: string }
```

### `@codonsplice/cli` (npm-distributed binary)

A Node wrapper that, on `postinstall`, detects OS/arch and downloads the matching
prebuilt `splice` binary from GitHub Releases (no Rust toolchain required) —
exactly as the site's install page documents. It is a packaging convenience over
the native crate, not a second implementation.

## 3. Framework integrations

All framework packages are thin wrappers over `@codonsplice/wasm` that lazily
`init()` the engine once and expose idiomatic reactive state
(`{ execute, result, error, loading }`).

### React — `@codonsplice/react`

```tsx
import { useSpliceQL } from '@codonsplice/react'

function VariantCaller({ bamFile }) {
  const { execute, result, error, loading } = useSpliceQL()
  const run = () => execute({
    query: `FROM bam "sample.bam" CALL variants WITH min_af = 0.05`,
    files: { "sample.bam": bamFile },
  })
  if (loading) return <div>Running query...</div>
  if (error)   return <div>Error: {error.message}</div>
  return <><button onClick={run}>Call Variants</button>{result && <pre>{JSON.stringify(result, null, 2)}</pre>}</>
}
```

`useSpliceQL` memoizes a module-level engine promise so every hook instance
shares one WASM instance; `execute` runs it inside a Web Worker (below) and sets
`loading`/`error`/`result`.

### Vue — `@codonsplice/vue`

```vue
<script setup>
import { useSpliceQL } from '@codonsplice/vue'
const props = defineProps(['bamFile'])
const { execute, result, error, loading } = useSpliceQL()
const run = () => execute({
  query: `FROM bam "sample.bam" CALL variants WITH min_af = 0.05`,
  files: { "sample.bam": props.bamFile },
})
</script>
```

Returns `ref`s (`result`, `error`, `loading`) so templates bind directly.

### Svelte — `@codonsplice/svelte`

```svelte
<script>
import { createSpliceQL } from '@codonsplice/svelte'
export let bamFile
const { execute, result, error, loading } = createSpliceQL()  // Svelte stores
const run = () => execute({ query: `FROM bam "sample.bam" CALL variants`, files: { "sample.bam": bamFile } })
</script>
```

`result`/`error`/`loading` are Svelte stores (`$result`, `$loading`).

### Astro — `@codonsplice/wasm` directly

Astro has no reactive runtime, so islands use the core package in a client
`<script>`:

```astro
<script>
import { CodonSplice } from '@codonsplice/wasm'
const engine = await CodonSplice.init()
document.getElementById('run').addEventListener('click', async () => {
  const file = document.getElementById('bam').files[0]
  const result = await engine.execute({ query: `FROM bam "sample.bam" CALL variants`, files: { "sample.bam": file } })
  document.getElementById('output').textContent = JSON.stringify(result, null, 2)
})
</script>
```

Each framework package is ~50 lines: lazy engine init + a worker bridge + the
framework's reactive primitive. The genomic logic lives only in the shared
`.wasm`.

## 4. Web Worker execution & COOP/COEP

Pileup over a BAM blocks for seconds, so the engine runs in a dedicated module
Web Worker (CNVLens already configures `worker.format = 'es'` in `vite.config`).
The worker holds the WASM instance; the main-thread API posts `{ query, files }`
(files transferred as `ArrayBuffer`, zero-copy) and receives results — or, for
`stream()`, a stream of `onRecord` messages. The host must serve with
`Cross-Origin-Opener-Policy: same-origin` and `Cross-Origin-Embedder-Policy`
(CNVLens already sets these) so `SharedArrayBuffer`/threaded WASM is available.

## 5. How the WASM execution layer differs from the native VM

The compiler and the expression interpreter are **identical** bytecode on both
targets — a query lowers to the same bytes natively and in the browser. The
difference is entirely in the pipeline-opcode backend, isolated behind the `Io`
trait and the source readers:

| Concern | Native VM (`splice`) | WASM VM (`@codonsplice/wasm`) |
| --- | --- | --- |
| Bytecode + expr opcodes | shared `codonsplice-core` | identical |
| `OPEN_SOURCE` file access | `std::fs` read from path | `files` map (ArrayBuffer) via `Io` |
| Index files (`.bai`/`.csi`) | sibling file on disk | provided in `files` map |
| Genomic backend | `cnvlens-core` native | `cnvlens-core` compiled to wasm32 |
| Output | stdout table / `INTO` file | JS object / `onRecord` callbacks |
| Threading | OS threads | Web Worker + threaded WASM (COOP/COEP) |
| Memory | OS-paged, large files OK | WASM linear memory (~practical ~2 GB cap) |

There is **no second bytecode layer** — "two layers" refers to this clean split:
layer 1 is the portable compiler + bytecode + expression VM (same everywhere);
layer 2 is the swappable execution backend (filesystem-native vs. browser-WASM)
selected by the `Io` implementation and the `cnvlens-core` build target. Adding a
new host (Node native addon, Deno, edge runtime) means implementing `Io` and
choosing a `cnvlens-core` target — the language, compiler, and VM are reused
unchanged.

## 6. Package matrix

| Package | Contents | Rust toolchain needed? |
| --- | --- | --- |
| `@codonsplice/wasm` | core `.wasm` + JS/TS glue + worker | no |
| `@codonsplice/cli` | prebuilt `splice` binary downloader | no |
| `@codonsplice/react` | `useSpliceQL` hook | no |
| `@codonsplice/vue` | `useSpliceQL` composable | no |
| `@codonsplice/svelte` | `createSpliceQL` stores | no |
| `codonsplice` (crates.io) | native engine crate | yes |
| `spliceql` (crates.io) | language crate (lexer/parser/AST) | yes |
