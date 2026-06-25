#!/usr/bin/env node
// Assemble the publishable `pkg/` from the wasm-pack output in `pkg/wasm`.
//
// wasm-pack emits codonsplice_wasm.js / .d.ts / _bg.wasm into pkg/wasm; this
// script copies the hand-authored convenience entry point (index.js) and the
// framework wrappers (react/vue/svelte/astro) alongside them so the published
// package exposes both the raw bindings and the ergonomic helpers.
const fs = require('fs')
const path = require('path')

const ROOT = path.resolve(__dirname, '..')
const PKG = path.join(ROOT, 'pkg')
const WASM = path.join(PKG, 'wasm')

function exists(p) {
  try { fs.accessSync(p); return true } catch { return false }
}

if (!exists(WASM)) {
  console.error('✗ pkg/wasm not found — run wasm-pack first (scripts/build-wasm.sh)')
  process.exit(1)
}

// wasm-pack derives the npm name from the crate name (codonsplice-wasm); rewrite
// the generated pkg/wasm/package.json to the scoped name we publish under.
const WASM_PKG_JSON = path.join(WASM, 'package.json')
if (exists(WASM_PKG_JSON)) {
  const manifest = JSON.parse(fs.readFileSync(WASM_PKG_JSON, 'utf8'))
  if (manifest.name !== '@codonsplice/wasm') {
    manifest.name = '@codonsplice/wasm'
    manifest.repository = {
      type: 'git',
      url: 'git+https://github.com/Pogo-Bash/codonsplice.git'
    }
    fs.writeFileSync(WASM_PKG_JSON, JSON.stringify(manifest, null, 2) + '\n')
    console.log('✓ pkg/wasm/package.json name set to @codonsplice/wasm')
  }
}

// Flatten the wasm-pack artifacts up into pkg/ so `main`/`types` resolve.
for (const f of fs.readdirSync(WASM)) {
  if (f === 'package.json' || f === '.gitignore' || f === 'README.md') continue
  fs.copyFileSync(path.join(WASM, f), path.join(PKG, f))
}

console.log('✓ wasm artifacts flattened into pkg/')
console.log('✓ framework wrappers present:', ['react', 'vue', 'svelte', 'astro']
  .filter((d) => exists(path.join(PKG, d))).join(', '))
