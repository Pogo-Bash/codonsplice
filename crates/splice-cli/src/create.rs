//! `splice create <framework> [name]` — scaffold a front-end project wired to
//! `@codonsplice`.
//!
//! Wraps the official scaffolder (Vite for react/vue/svelte, `create-astro` for
//! astro), then injects the `@codonsplice/<framework>` dependency and a
//! ready-to-run SpliceQL demo, and finally runs `npm install`. The official
//! tooling does the heavy lifting so the generated project always matches the
//! framework's current conventions.

use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Framework {
    React,
    Vue,
    Svelte,
    Astro,
}

impl Framework {
    fn parse(s: &str) -> Option<Framework> {
        match s.to_ascii_lowercase().as_str() {
            "react" => Some(Framework::React),
            "vue" => Some(Framework::Vue),
            "svelte" => Some(Framework::Svelte),
            "astro" => Some(Framework::Astro),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Framework::React => "react",
            Framework::Vue => "vue",
            Framework::Svelte => "svelte",
            Framework::Astro => "astro",
        }
    }

    /// The `@codonsplice/*` wrapper package the demo's hook/composable comes from.
    fn pkg(self) -> &'static str {
        match self {
            Framework::React => "@codonsplice/react",
            Framework::Vue => "@codonsplice/vue",
            Framework::Svelte => "@codonsplice/svelte",
            Framework::Astro => "@codonsplice/astro",
        }
    }

    /// The `@codonsplice/*` packages the scaffolded demo depends on. Each wrapper
    /// re-exports the core tooling (`compile`/`check`/`execute`) from
    /// `@codonsplice/wasm`, so the app only needs the one wrapper package.
    fn deps(self) -> &'static [&'static str] {
        match self {
            Framework::React => &["@codonsplice/react"],
            Framework::Vue => &["@codonsplice/vue"],
            Framework::Svelte => &["@codonsplice/svelte"],
            Framework::Astro => &["@codonsplice/astro"],
        }
    }
}

/// `npm` (or `npm.cmd` on Windows).
fn npm() -> &'static str {
    if cfg!(windows) {
        "npm.cmd"
    } else {
        "npm"
    }
}

/// The official scaffolder invocation (program + args) for a fresh project.
/// Vite and create-astro are both told NOT to install, so we can inject the
/// `@codonsplice` dependency before a single `npm install`.
fn scaffold_command(fw: Framework, name: &str) -> (String, Vec<String>) {
    match fw {
        Framework::Astro => (
            npm().to_string(),
            [
                "create",
                "astro@latest",
                name,
                "--",
                "--template",
                "minimal",
                "--no-install",
                "--no-git",
                "--skip-houston",
                "--yes",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        ),
        _ => {
            let template = match fw {
                Framework::React => "react",
                Framework::Vue => "vue",
                Framework::Svelte => "svelte",
                Framework::Astro => unreachable!(),
            };
            (
                npm().to_string(),
                ["create", "vite@latest", name, "--", "--template", template]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            )
        }
    }
}

/// Add the framework's `@codonsplice/*` packages to a `package.json`'s
/// `dependencies` (pinned to `latest`), returning the re-serialized JSON.
fn inject_dependencies(pkg_json: &str, fw: Framework) -> serde_json::Result<String> {
    let mut v: serde_json::Value = serde_json::from_str(pkg_json)?;
    let obj = v.as_object_mut().ok_or_else(|| {
        serde_json::from_str::<serde_json::Value>("\"package.json is not an object\"").unwrap_err()
    })?;
    let deps = obj
        .entry("dependencies")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if let Some(map) = deps.as_object_mut() {
        for pkg in fw.deps() {
            map.insert(
                pkg.to_string(),
                serde_json::Value::String("latest".to_string()),
            );
        }
    }
    let mut out = serde_json::to_string_pretty(&v)?;
    out.push('\n');
    Ok(out)
}

/// The demo file(s) to write: `(path relative to project root, contents)`.
/// React ships a separate stylesheet; vue/svelte/astro use scoped styles.
fn example_files(fw: Framework) -> Vec<(&'static str, &'static str)> {
    match fw {
        Framework::React => vec![("src/App.jsx", REACT_DEMO), ("src/splice-demo.css", DEMO_CSS)],
        Framework::Vue => vec![("src/App.vue", VUE_DEMO)],
        Framework::Svelte => vec![("src/App.svelte", SVELTE_DEMO)],
        Framework::Astro => vec![("src/pages/index.astro", ASTRO_DEMO)],
    }
}

/// Run a command inheriting stdio (so interactive prompts + output flow through).
fn run_inherit(prog: &str, args: &[String], cwd: Option<&Path>) -> bool {
    let mut cmd = Command::new(prog);
    cmd.args(args).stdin(Stdio::inherit());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    matches!(cmd.status(), Ok(s) if s.success())
}

pub fn cmd_create(framework: &str, name: Option<String>) -> ExitCode {
    let fw = match Framework::parse(framework) {
        Some(f) => f,
        None => {
            eprintln!("✗ unknown framework {framework:?} — choose: react, vue, svelte, astro");
            return ExitCode::FAILURE;
        }
    };
    let name = name.unwrap_or_else(|| "splice-app".to_string());
    let root = Path::new(&name);
    if root.exists() {
        eprintln!("✗ `{name}` already exists — choose another name or remove it.");
        return ExitCode::FAILURE;
    }

    // 1. Scaffold via the official tooling.
    let (prog, args) = scaffold_command(fw, &name);
    println!("→ scaffolding {} project `{name}`…", fw.label());
    println!("  $ {prog} {}", args.join(" "));
    if !run_inherit(&prog, &args, None) {
        eprintln!("✗ scaffold failed (is Node.js / npm installed?).");
        return ExitCode::FAILURE;
    }

    // 2. Inject the @codonsplice dependency.
    let pkg_path = root.join("package.json");
    match std::fs::read_to_string(&pkg_path) {
        Ok(s) => match inject_dependencies(&s, fw) {
            Ok(updated) => {
                if std::fs::write(&pkg_path, updated).is_ok() {
                    println!("✓ added {} to dependencies", fw.deps().join(", "));
                }
            }
            Err(e) => eprintln!("⚠ could not edit package.json ({e}); add {} manually", fw.pkg()),
        },
        Err(e) => eprintln!("⚠ could not read {} ({e}); add {} manually", pkg_path.display(), fw.pkg()),
    }

    // 3. Write the SpliceQL demo (replacing the framework's default starter).
    for (rel, contents) in example_files(fw) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::write(&path, contents).is_ok() {
            println!("✓ wrote {}", path.display());
        }
    }

    // 4. Install dependencies (including the @codonsplice wrapper just added).
    println!("→ installing dependencies…");
    if !run_inherit(npm(), &["install".to_string()], Some(root)) {
        eprintln!("⚠ `npm install` failed — run it yourself in {name}/");
    }

    println!("\n✓ created {name}");
    println!("  cd {name}");
    println!("  npm run dev");
    ExitCode::SUCCESS
}

// ── Demo templates ───────────────────────────────────────────────────────────

const REACT_DEMO: &str = r#"import { useState, useEffect } from 'react'
import { useSpliceQL, compile, check } from '@codonsplice/react'
import './splice-demo.css'

const SAMPLE = `FROM bam "sample.bam"
WHERE chr = "7" AND depth > 30
CALL variants
WITH min_af = 0.05
LIMIT 20`

export default function App() {
  const { execute, result, error, loading } = useSpliceQL()
  const [query, setQuery] = useState(SAMPLE)
  const [bytecode, setBytecode] = useState('')
  const [typeError, setTypeError] = useState(null)
  const [files, setFiles] = useState({})
  const [fileName, setFileName] = useState(null)

  // Live tooling: type-check + compile to bytecode as you type — no file needed.
  useEffect(() => {
    let cancelled = false
    ;(async () => {
      try {
        const err = await check(query)
        if (cancelled) return
        setTypeError(err)
        setBytecode(err ? '' : await compile(query))
      } catch (e) {
        if (!cancelled) { setTypeError(String(e)); setBytecode('') }
      }
    })()
    return () => { cancelled = true }
  }, [query])

  async function onFile(e) {
    const f = e.target.files[0]
    if (!f) return
    setFiles({ [f.name]: new Uint8Array(await f.arrayBuffer()) })
    setFileName(f.name)
  }

  return (
    <main className="splice">
      <header>
        <span className="logo">🧬 SpliceQL</span>
        <span className="badge">× React · powered by @codonsplice/wasm</span>
      </header>
      <p className="tagline">
        A genomic query engine compiled to WebAssembly. Edit the query — it
        type-checks and compiles to bytecode live, right here in the browser.
      </p>

      <textarea
        className="editor"
        value={query}
        spellCheck={false}
        onChange={(e) => setQuery(e.target.value)}
        rows={6}
      />

      <div className="grid">
        <section className="card">
          <h3>Type check</h3>
          {typeError ? <pre className="err">{typeError}</pre> : <p className="ok">✓ valid SpliceQL</p>}
        </section>
        <section className="card">
          <h3>Compiled bytecode</h3>
          <pre className="code">{bytecode || '—'}</pre>
        </section>
      </div>

      <div className="run">
        <label className="file">
          {fileName ? `📄 ${fileName}` : 'Choose a BAM…'}
          <input type="file" accept=".bam" onChange={onFile} hidden />
        </label>
        <button onClick={() => execute({ query, files })} disabled={loading || !!typeError}>
          {loading ? 'Running…' : 'Run query ▶'}
        </button>
      </div>

      {error && <pre className="err">{String(error)}</pre>}
      {result && <pre className="code result">{JSON.stringify(result, null, 2)}</pre>}
    </main>
  )
}
"#;

/// Stylesheet for the React demo (vue/svelte/astro use component-scoped styles).
const DEMO_CSS: &str = r#":root { color-scheme: dark; }
body { margin: 0; background: #11111b; color: #cdd6f4; font-family: system-ui, sans-serif; }
.splice { max-width: 860px; margin: 0 auto; padding: 2.5rem 1.5rem; }
.splice header { display: flex; align-items: baseline; gap: .6rem; }
.splice .logo { font-size: 1.6rem; font-weight: 700; color: #cba6f7; }
.splice .badge { font-size: .85rem; color: #7f849c; }
.splice .tagline { color: #a6adc8; line-height: 1.5; max-width: 60ch; }
.splice .editor {
  width: 100%; box-sizing: border-box; margin-top: .5rem; padding: 1rem;
  font: 14px/1.5 ui-monospace, monospace; color: #cdd6f4; background: #181825;
  border: 1px solid #313244; border-radius: 10px; resize: vertical;
}
.splice .grid { display: grid; grid-template-columns: 1fr 1fr; gap: 1rem; margin-top: 1rem; }
.splice .card { background: #181825; border: 1px solid #313244; border-radius: 10px; padding: 1rem; }
.splice .card h3 { margin: 0 0 .5rem; font-size: .8rem; text-transform: uppercase; letter-spacing: .08em; color: #7f849c; }
.splice .ok { color: #a6e3a1; margin: 0; }
.splice .err { color: #f38ba8; white-space: pre-wrap; margin: 0; }
.splice .code { color: #89dceb; font: 13px/1.5 ui-monospace, monospace; white-space: pre-wrap; margin: 0; max-height: 260px; overflow: auto; }
.splice .result { margin-top: 1rem; background: #181825; border: 1px solid #313244; border-radius: 10px; padding: 1rem; }
.splice .run { display: flex; align-items: center; gap: 1rem; margin-top: 1rem; }
.splice .file { cursor: pointer; padding: .5rem .9rem; border: 1px dashed #45475a; border-radius: 8px; color: #a6adc8; }
.splice button {
  padding: .55rem 1.1rem; font-weight: 600; color: #11111b; background: #cba6f7;
  border: 0; border-radius: 8px; cursor: pointer;
}
.splice button:disabled { opacity: .5; cursor: not-allowed; }
"#;

const VUE_DEMO: &str = r#"<script setup>
import { ref, watch } from 'vue'
import { useSpliceQL, compile, check } from '@codonsplice/vue'

const { execute, result, error, loading } = useSpliceQL()
const query = ref(`FROM bam "sample.bam"
WHERE chr = "7" AND depth > 30
CALL variants
WITH min_af = 0.05
LIMIT 20`)
const bytecode = ref('')
const typeError = ref(null)
const files = ref({})
const fileName = ref(null)

// Live tooling: type-check + compile to bytecode as you type — no file needed.
watch(query, async (q) => {
  try {
    const err = await check(q)
    typeError.value = err
    bytecode.value = err ? '' : await compile(q)
  } catch (e) {
    typeError.value = String(e)
    bytecode.value = ''
  }
}, { immediate: true })

async function onFile(e) {
  const f = e.target.files[0]
  if (!f) return
  files.value = { [f.name]: new Uint8Array(await f.arrayBuffer()) }
  fileName.value = f.name
}
</script>

<template>
  <main class="splice">
    <header>
      <span class="logo">🧬 SpliceQL</span>
      <span class="badge">× Vue · powered by @codonsplice/wasm</span>
    </header>
    <p class="tagline">
      A genomic query engine compiled to WebAssembly. Edit the query — it
      type-checks and compiles to bytecode live, right here in the browser.
    </p>

    <textarea class="editor" v-model="query" rows="6" spellcheck="false"></textarea>

    <div class="grid">
      <section class="card">
        <h3>Type check</h3>
        <pre v-if="typeError" class="err">{{ typeError }}</pre>
        <p v-else class="ok">✓ valid SpliceQL</p>
      </section>
      <section class="card">
        <h3>Compiled bytecode</h3>
        <pre class="code">{{ bytecode || '—' }}</pre>
      </section>
    </div>

    <div class="run">
      <label class="file">
        {{ fileName ? `📄 ${fileName}` : 'Choose a BAM…' }}
        <input type="file" accept=".bam" @change="onFile" hidden />
      </label>
      <button @click="execute({ query, files })" :disabled="loading || !!typeError">
        {{ loading ? 'Running…' : 'Run query ▶' }}
      </button>
    </div>

    <pre v-if="error" class="err">{{ String(error) }}</pre>
    <pre v-if="result" class="code result">{{ JSON.stringify(result, null, 2) }}</pre>
  </main>
</template>

<style>
:root { color-scheme: dark; }
body { margin: 0; background: #11111b; color: #cdd6f4; font-family: system-ui, sans-serif; }
.splice { max-width: 860px; margin: 0 auto; padding: 2.5rem 1.5rem; }
.splice header { display: flex; align-items: baseline; gap: .6rem; }
.splice .logo { font-size: 1.6rem; font-weight: 700; color: #cba6f7; }
.splice .badge { font-size: .85rem; color: #7f849c; }
.splice .tagline { color: #a6adc8; line-height: 1.5; max-width: 60ch; }
.splice .editor {
  width: 100%; box-sizing: border-box; margin-top: .5rem; padding: 1rem;
  font: 14px/1.5 ui-monospace, monospace; color: #cdd6f4; background: #181825;
  border: 1px solid #313244; border-radius: 10px; resize: vertical;
}
.splice .grid { display: grid; grid-template-columns: 1fr 1fr; gap: 1rem; margin-top: 1rem; }
.splice .card { background: #181825; border: 1px solid #313244; border-radius: 10px; padding: 1rem; }
.splice .card h3 { margin: 0 0 .5rem; font-size: .8rem; text-transform: uppercase; letter-spacing: .08em; color: #7f849c; }
.splice .ok { color: #a6e3a1; margin: 0; }
.splice .err { color: #f38ba8; white-space: pre-wrap; margin: 0; }
.splice .code { color: #89dceb; font: 13px/1.5 ui-monospace, monospace; white-space: pre-wrap; margin: 0; max-height: 260px; overflow: auto; }
.splice .result { margin-top: 1rem; background: #181825; border: 1px solid #313244; border-radius: 10px; padding: 1rem; }
.splice .run { display: flex; align-items: center; gap: 1rem; margin-top: 1rem; }
.splice .file { cursor: pointer; padding: .5rem .9rem; border: 1px dashed #45475a; border-radius: 8px; color: #a6adc8; }
.splice button { padding: .55rem 1.1rem; font-weight: 600; color: #11111b; background: #cba6f7; border: 0; border-radius: 8px; cursor: pointer; }
.splice button:disabled { opacity: .5; cursor: not-allowed; }
</style>
"#;

const SVELTE_DEMO: &str = r#"<script>
  import { createSpliceQL, compile, check } from '@codonsplice/svelte'

  const { execute, result, error, loading } = createSpliceQL()
  let query = `FROM bam "sample.bam"
WHERE chr = "7" AND depth > 30
CALL variants
WITH min_af = 0.05
LIMIT 20`
  let bytecode = ''
  let typeError = null
  let files = {}
  let fileName = null

  // Live tooling: type-check + compile to bytecode as you type — no file needed.
  $: liveCompile(query)
  async function liveCompile(q) {
    try {
      const err = await check(q)
      typeError = err
      bytecode = err ? '' : await compile(q)
    } catch (e) {
      typeError = String(e)
      bytecode = ''
    }
  }

  async function onFile(e) {
    const f = e.target.files[0]
    if (!f) return
    files = { [f.name]: new Uint8Array(await f.arrayBuffer()) }
    fileName = f.name
  }
</script>

<main class="splice">
  <header>
    <span class="logo">🧬 SpliceQL</span>
    <span class="badge">× Svelte · powered by @codonsplice/wasm</span>
  </header>
  <p class="tagline">
    A genomic query engine compiled to WebAssembly. Edit the query — it
    type-checks and compiles to bytecode live, right here in the browser.
  </p>

  <textarea class="editor" bind:value={query} rows="6" spellcheck="false"></textarea>

  <div class="grid">
    <section class="card">
      <h3>Type check</h3>
      {#if typeError}<pre class="err">{typeError}</pre>{:else}<p class="ok">✓ valid SpliceQL</p>{/if}
    </section>
    <section class="card">
      <h3>Compiled bytecode</h3>
      <pre class="code">{bytecode || '—'}</pre>
    </section>
  </div>

  <div class="run">
    <label class="file">
      {fileName ? `📄 ${fileName}` : 'Choose a BAM…'}
      <input type="file" accept=".bam" on:change={onFile} hidden />
    </label>
    <button on:click={() => execute({ query, files })} disabled={$loading || !!typeError}>
      {$loading ? 'Running…' : 'Run query ▶'}
    </button>
  </div>

  {#if $error}<pre class="err">{String($error)}</pre>{/if}
  {#if $result}<pre class="code result">{JSON.stringify($result, null, 2)}</pre>{/if}
</main>

<style>
  :global(body) { margin: 0; background: #11111b; color: #cdd6f4; font-family: system-ui, sans-serif; }
  .splice { max-width: 860px; margin: 0 auto; padding: 2.5rem 1.5rem; }
  header { display: flex; align-items: baseline; gap: .6rem; }
  .logo { font-size: 1.6rem; font-weight: 700; color: #cba6f7; }
  .badge { font-size: .85rem; color: #7f849c; }
  .tagline { color: #a6adc8; line-height: 1.5; max-width: 60ch; }
  .editor {
    width: 100%; box-sizing: border-box; margin-top: .5rem; padding: 1rem;
    font: 14px/1.5 ui-monospace, monospace; color: #cdd6f4; background: #181825;
    border: 1px solid #313244; border-radius: 10px; resize: vertical;
  }
  .grid { display: grid; grid-template-columns: 1fr 1fr; gap: 1rem; margin-top: 1rem; }
  .card { background: #181825; border: 1px solid #313244; border-radius: 10px; padding: 1rem; }
  .card h3 { margin: 0 0 .5rem; font-size: .8rem; text-transform: uppercase; letter-spacing: .08em; color: #7f849c; }
  .ok { color: #a6e3a1; margin: 0; }
  .err { color: #f38ba8; white-space: pre-wrap; margin: 0; }
  .code { color: #89dceb; font: 13px/1.5 ui-monospace, monospace; white-space: pre-wrap; margin: 0; max-height: 260px; overflow: auto; }
  .result { margin-top: 1rem; background: #181825; border: 1px solid #313244; border-radius: 10px; padding: 1rem; }
  .run { display: flex; align-items: center; gap: 1rem; margin-top: 1rem; }
  .file { cursor: pointer; padding: .5rem .9rem; border: 1px dashed #45475a; border-radius: 8px; color: #a6adc8; }
  button { padding: .55rem 1.1rem; font-weight: 600; color: #11111b; background: #cba6f7; border: 0; border-radius: 8px; cursor: pointer; }
  button:disabled { opacity: .5; cursor: not-allowed; }
</style>
"#;

const ASTRO_DEMO: &str = r#"---
// SpliceQL runs client-side; the module script below is bundled by Astro.
const sample = `FROM bam "sample.bam"
WHERE chr = "7" AND depth > 30
CALL variants
WITH min_af = 0.05
LIMIT 20`
---

<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>SpliceQL × Astro</title>
  </head>
  <body>
    <main class="splice">
      <header>
        <span class="logo">🧬 SpliceQL</span>
        <span class="badge">× Astro · powered by @codonsplice/wasm</span>
      </header>
      <p class="tagline">
        A genomic query engine compiled to WebAssembly. Edit the query — it
        type-checks and compiles to bytecode live, right here in the browser.
      </p>

      <textarea id="q" class="editor" rows="6" spellcheck="false">{sample}</textarea>

      <div class="grid">
        <section class="card">
          <h3>Type check</h3>
          <pre id="typecheck" class="ok">✓ valid SpliceQL</pre>
        </section>
        <section class="card">
          <h3>Compiled bytecode</h3>
          <pre id="bytecode" class="code">—</pre>
        </section>
      </div>

      <div class="run">
        <label class="file" id="fileLabel">
          Choose a BAM…
          <input id="bam" type="file" accept=".bam" hidden />
        </label>
        <button id="run">Run query ▶</button>
      </div>

      <pre id="out" class="code result"></pre>
    </main>

    <script>
      import { execute, compile, check } from '@codonsplice/astro'
      const $ = (id) => document.getElementById(id)
      const q = $('q'), typecheck = $('typecheck'), bytecode = $('bytecode')
      const out = $('out'), runBtn = $('run'), fileLabel = $('fileLabel')
      let files = {}

      async function live() {
        try {
          const err = await check(q.value)
          typecheck.textContent = err || '✓ valid SpliceQL'
          typecheck.className = err ? 'err' : 'ok'
          bytecode.textContent = err ? '—' : await compile(q.value)
          runBtn.disabled = !!err
        } catch (e) {
          typecheck.textContent = String(e); typecheck.className = 'err'; bytecode.textContent = '—'
        }
      }
      q.addEventListener('input', live); live()

      $('bam').addEventListener('change', async (e) => {
        const f = e.target.files[0]; if (!f) return
        files = { [f.name]: new Uint8Array(await f.arrayBuffer()) }
        fileLabel.childNodes[0].nodeValue = `📄 ${f.name} `
      })
      runBtn.addEventListener('click', async () => {
        runBtn.disabled = true
        try { out.textContent = JSON.stringify(await execute({ query: q.value, files }), null, 2) }
        catch (err) { out.textContent = String(err) }
        finally { runBtn.disabled = false }
      })
    </script>

    <style is:global>
      :root { color-scheme: dark; }
      body { margin: 0; background: #11111b; color: #cdd6f4; font-family: system-ui, sans-serif; }
      .splice { max-width: 860px; margin: 0 auto; padding: 2.5rem 1.5rem; }
      .splice header { display: flex; align-items: baseline; gap: .6rem; }
      .splice .logo { font-size: 1.6rem; font-weight: 700; color: #cba6f7; }
      .splice .badge { font-size: .85rem; color: #7f849c; }
      .splice .tagline { color: #a6adc8; line-height: 1.5; max-width: 60ch; }
      .splice .editor {
        width: 100%; box-sizing: border-box; margin-top: .5rem; padding: 1rem;
        font: 14px/1.5 ui-monospace, monospace; color: #cdd6f4; background: #181825;
        border: 1px solid #313244; border-radius: 10px; resize: vertical;
      }
      .splice .grid { display: grid; grid-template-columns: 1fr 1fr; gap: 1rem; margin-top: 1rem; }
      .splice .card { background: #181825; border: 1px solid #313244; border-radius: 10px; padding: 1rem; }
      .splice .card h3 { margin: 0 0 .5rem; font-size: .8rem; text-transform: uppercase; letter-spacing: .08em; color: #7f849c; }
      .splice .ok { color: #a6e3a1; margin: 0; }
      .splice .err { color: #f38ba8; white-space: pre-wrap; margin: 0; }
      .splice .code { color: #89dceb; font: 13px/1.5 ui-monospace, monospace; white-space: pre-wrap; margin: 0; max-height: 260px; overflow: auto; }
      .splice .result { margin-top: 1rem; }
      .splice .run { display: flex; align-items: center; gap: 1rem; margin-top: 1rem; }
      .splice .file { cursor: pointer; padding: .5rem .9rem; border: 1px dashed #45475a; border-radius: 8px; color: #a6adc8; }
      .splice button { padding: .55rem 1.1rem; font-weight: 600; color: #11111b; background: #cba6f7; border: 0; border-radius: 8px; cursor: pointer; }
      .splice button:disabled { opacity: .5; cursor: not-allowed; }
    </style>
  </body>
</html>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frameworks_case_insensitively() {
        assert_eq!(Framework::parse("React"), Some(Framework::React));
        assert_eq!(Framework::parse("vue"), Some(Framework::Vue));
        assert_eq!(Framework::parse("SVELTE"), Some(Framework::Svelte));
        assert_eq!(Framework::parse("astro"), Some(Framework::Astro));
        assert_eq!(Framework::parse("angular"), None);
    }

    #[test]
    fn vite_scaffold_is_noninstall_with_template() {
        let (prog, args) = scaffold_command(Framework::React, "demo");
        assert!(prog.starts_with("npm"));
        assert_eq!(
            args,
            vec!["create", "vite@latest", "demo", "--", "--template", "react"]
        );
    }

    #[test]
    fn astro_scaffold_skips_install_and_git() {
        let (_, args) = scaffold_command(Framework::Astro, "demo");
        assert!(args.contains(&"create".to_string()));
        assert!(args.contains(&"astro@latest".to_string()));
        assert!(args.contains(&"--no-install".to_string()));
        assert!(args.contains(&"--no-git".to_string()));
    }

    #[test]
    fn injects_dependencies_into_existing_deps() {
        let pkg = r#"{"name":"demo","dependencies":{"react":"^18.0.0"}}"#;
        let out = inject_dependencies(pkg, Framework::React).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["dependencies"]["react"], "^18.0.0");
        assert_eq!(v["dependencies"]["@codonsplice/react"], "latest");
        // The wrapper re-exports the core tooling, so wasm is transitive only.
        assert!(v["dependencies"]["@codonsplice/wasm"].is_null());
    }

    #[test]
    fn injects_dependencies_when_deps_absent() {
        let pkg = r#"{"name":"demo"}"#;
        let out = inject_dependencies(pkg, Framework::Astro).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["dependencies"]["@codonsplice/astro"], "latest");
        // Astro re-exports the core API, so it doesn't add @codonsplice/wasm.
        assert!(v["dependencies"]["@codonsplice/wasm"].is_null());
    }

    #[test]
    fn example_paths_match_framework() {
        assert_eq!(example_files(Framework::React)[0].0, "src/App.jsx");
        assert_eq!(example_files(Framework::Vue)[0].0, "src/App.vue");
        assert_eq!(example_files(Framework::Svelte)[0].0, "src/App.svelte");
        assert_eq!(example_files(Framework::Astro)[0].0, "src/pages/index.astro");
        // React ships a separate stylesheet too.
        assert!(example_files(Framework::React)
            .iter()
            .any(|(p, _)| *p == "src/splice-demo.css"));
        // Each demo wires up the SpliceQL tooling.
        assert!(example_files(Framework::React)[0].1.contains("@codonsplice/react"));
        assert!(example_files(Framework::React)[0].1.contains("compile"));
        assert!(example_files(Framework::Astro)[0].1.contains("@codonsplice/astro"));
    }
}
