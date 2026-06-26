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

    /// The `@codonsplice/*` wrapper package the demo imports.
    fn pkg(self) -> &'static str {
        match self {
            Framework::React => "@codonsplice/react",
            Framework::Vue => "@codonsplice/vue",
            Framework::Svelte => "@codonsplice/svelte",
            Framework::Astro => "@codonsplice/astro",
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

/// Add `@codonsplice/<framework>` to a `package.json`'s `dependencies`,
/// returning the re-serialized JSON. `version` is pinned to `latest`.
fn inject_dependency(pkg_json: &str, fw: Framework) -> serde_json::Result<String> {
    let mut v: serde_json::Value = serde_json::from_str(pkg_json)?;
    let obj = v.as_object_mut().ok_or_else(|| {
        serde_json::from_str::<serde_json::Value>("\"package.json is not an object\"").unwrap_err()
    })?;
    let deps = obj
        .entry("dependencies")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if let Some(map) = deps.as_object_mut() {
        map.insert(
            fw.pkg().to_string(),
            serde_json::Value::String("latest".to_string()),
        );
    }
    let mut out = serde_json::to_string_pretty(&v)?;
    out.push('\n');
    Ok(out)
}

/// The demo file to write: a path relative to the project root and its contents.
fn example_file(fw: Framework) -> (&'static str, &'static str) {
    match fw {
        Framework::React => ("src/App.jsx", REACT_DEMO),
        Framework::Vue => ("src/App.vue", VUE_DEMO),
        Framework::Svelte => ("src/App.svelte", SVELTE_DEMO),
        Framework::Astro => ("src/pages/index.astro", ASTRO_DEMO),
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
        Ok(s) => match inject_dependency(&s, fw) {
            Ok(updated) => {
                if std::fs::write(&pkg_path, updated).is_ok() {
                    println!("✓ added {} to dependencies", fw.pkg());
                }
            }
            Err(e) => eprintln!("⚠ could not edit package.json ({e}); add {} manually", fw.pkg()),
        },
        Err(e) => eprintln!("⚠ could not read {} ({e}); add {} manually", pkg_path.display(), fw.pkg()),
    }

    // 3. Write the SpliceQL demo.
    let (rel, contents) = example_file(fw);
    let demo = root.join(rel);
    if let Some(parent) = demo.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::write(&demo, contents).is_ok() {
        println!("✓ wrote SpliceQL demo to {}", demo.display());
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

const REACT_DEMO: &str = r#"import { useState } from 'react'
import { useSpliceQL } from '@codonsplice/react'

export default function App() {
  const { execute, result, error, loading } = useSpliceQL()
  const [query, setQuery] = useState(
    'FROM bam "sample.bam" WHERE chr = "7" CALL variants LIMIT 20'
  )
  const [files, setFiles] = useState({})

  async function onFile(e) {
    const f = e.target.files[0]
    if (!f) return
    setFiles({ [f.name]: new Uint8Array(await f.arrayBuffer()) })
  }

  return (
    <main style={{ fontFamily: 'system-ui', maxWidth: 720, margin: '2rem auto' }}>
      <h1>SpliceQL × React</h1>
      <p>Upload a BAM and run a SpliceQL query entirely in your browser.</p>
      <input type="file" accept=".bam" onChange={onFile} />
      <textarea
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        rows={5}
        style={{ width: '100%', marginTop: 8 }}
      />
      <button onClick={() => execute({ query, files })} disabled={loading} style={{ marginTop: 8 }}>
        {loading ? 'Running…' : 'Run query'}
      </button>
      {error && <pre style={{ color: 'crimson' }}>{String(error)}</pre>}
      {result && <pre>{JSON.stringify(result, null, 2)}</pre>}
    </main>
  )
}
"#;

const VUE_DEMO: &str = r#"<script setup>
import { ref } from 'vue'
import { useSpliceQL } from '@codonsplice/vue'

const { execute, result, error, loading } = useSpliceQL()
const query = ref('FROM bam "sample.bam" WHERE chr = "7" CALL variants LIMIT 20')
const files = ref({})

async function onFile(e) {
  const f = e.target.files[0]
  if (!f) return
  files.value = { [f.name]: new Uint8Array(await f.arrayBuffer()) }
}
</script>

<template>
  <main style="font-family: system-ui; max-width: 720px; margin: 2rem auto">
    <h1>SpliceQL × Vue</h1>
    <p>Upload a BAM and run a SpliceQL query entirely in your browser.</p>
    <input type="file" accept=".bam" @change="onFile" />
    <textarea v-model="query" rows="5" style="width: 100%; margin-top: 8px"></textarea>
    <button @click="execute({ query, files })" :disabled="loading" style="margin-top: 8px">
      {{ loading ? 'Running…' : 'Run query' }}
    </button>
    <pre v-if="error" style="color: crimson">{{ String(error) }}</pre>
    <pre v-if="result">{{ JSON.stringify(result, null, 2) }}</pre>
  </main>
</template>
"#;

const SVELTE_DEMO: &str = r#"<script>
  import { createSpliceQL } from '@codonsplice/svelte'

  const { execute, result, error, loading } = createSpliceQL()
  let query = 'FROM bam "sample.bam" WHERE chr = "7" CALL variants LIMIT 20'
  let files = {}

  async function onFile(e) {
    const f = e.target.files[0]
    if (!f) return
    files = { [f.name]: new Uint8Array(await f.arrayBuffer()) }
  }
</script>

<main style="font-family: system-ui; max-width: 720px; margin: 2rem auto">
  <h1>SpliceQL × Svelte</h1>
  <p>Upload a BAM and run a SpliceQL query entirely in your browser.</p>
  <input type="file" accept=".bam" on:change={onFile} />
  <textarea bind:value={query} rows="5" style="width: 100%; margin-top: 8px"></textarea>
  <button on:click={() => execute({ query, files })} disabled={$loading} style="margin-top: 8px">
    {$loading ? 'Running…' : 'Run query'}
  </button>
  {#if $error}<pre style="color: crimson">{String($error)}</pre>{/if}
  {#if $result}<pre>{JSON.stringify($result, null, 2)}</pre>{/if}
</main>
"#;

const ASTRO_DEMO: &str = r#"---
// SpliceQL runs client-side; see the inline module script below.
---

<html lang="en">
  <head>
    <meta charset="utf-8" />
    <title>SpliceQL × Astro</title>
  </head>
  <body>
    <main style="font-family: system-ui; max-width: 720px; margin: 2rem auto">
      <h1>SpliceQL × Astro</h1>
      <p>Upload a BAM and run a SpliceQL query entirely in your browser.</p>
      <input id="bam" type="file" accept=".bam" />
      <textarea id="q" rows="5" style="width: 100%; margin-top: 8px">FROM bam "sample.bam" WHERE chr = "7" CALL variants LIMIT 20</textarea>
      <button id="run" style="margin-top: 8px">Run query</button>
      <pre id="out"></pre>
    </main>

    <script>
      import { execute } from '@codonsplice/astro'
      const out = document.getElementById('out')
      let files = {}
      document.getElementById('bam').addEventListener('change', async (e) => {
        const f = e.target.files[0]
        if (!f) return
        files = { [f.name]: new Uint8Array(await f.arrayBuffer()) }
      })
      document.getElementById('run').addEventListener('click', async () => {
        try {
          out.textContent = JSON.stringify(
            await execute({ query: document.getElementById('q').value, files }),
            null,
            2
          )
        } catch (err) {
          out.textContent = String(err)
        }
      })
    </script>
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
    fn injects_dependency_into_existing_deps() {
        let pkg = r#"{"name":"demo","dependencies":{"react":"^18.0.0"}}"#;
        let out = inject_dependency(pkg, Framework::React).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["dependencies"]["react"], "^18.0.0");
        assert_eq!(v["dependencies"]["@codonsplice/react"], "latest");
    }

    #[test]
    fn injects_dependency_when_deps_absent() {
        let pkg = r#"{"name":"demo"}"#;
        let out = inject_dependency(pkg, Framework::Vue).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["dependencies"]["@codonsplice/vue"], "latest");
    }

    #[test]
    fn example_paths_match_framework() {
        assert_eq!(example_file(Framework::React).0, "src/App.jsx");
        assert_eq!(example_file(Framework::Vue).0, "src/App.vue");
        assert_eq!(example_file(Framework::Svelte).0, "src/App.svelte");
        assert_eq!(example_file(Framework::Astro).0, "src/pages/index.astro");
        // Each demo imports its wrapper package.
        assert!(example_file(Framework::React).1.contains("@codonsplice/react"));
        assert!(example_file(Framework::Astro).1.contains("@codonsplice/astro"));
    }
}
