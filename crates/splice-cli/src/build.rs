//! `splice build <file.spq>` — compile a `.spq` query to a self-contained
//! native (or WASM) binary that embeds the bytecode + runtime.

use std::path::{Path, PathBuf};
use std::process::Command;

use codonsplice_core::compile;

use crate::directive::{parse_directives, scan_vars, Directives, InputDecl, VarKind};

/// Path to the codonsplice-core crate, embedded at compile time so generated
/// binaries can depend on it by path during local builds.
const CORE_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../codonsplice-core");
const TEMPLATE_CARGO: &str = include_str!("../../../templates/binary/Cargo.toml.tmpl");
const TEMPLATE_MAIN: &str = include_str!("../../../templates/binary/src/main.rs.tmpl");

#[derive(Default)]
pub struct BuildOpts {
    pub output: Option<String>,
    pub release: bool,
    pub target: Option<String>,
    pub wasm: bool,
    pub emit_bc: bool,
}

pub fn cmd_build(file: &str, opts: &BuildOpts) -> std::process::ExitCode {
    let source = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {file}: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let (dirs, query) = parse_directives(&source);

    let program = match compile(query) {
        Ok(p) => p,
        Err(e) => {
            eprint!("{}", e.render(query, None));
            return std::process::ExitCode::FAILURE;
        }
    };
    let bc = program.to_bytes();

    let stem = Path::new(file)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("query")
        .to_string();
    let name = opts
        .output
        .clone()
        .or_else(|| dirs.name.clone())
        .unwrap_or(stem.clone());
    let version = dirs.version.clone().unwrap_or_else(|| "0.1.0".to_string());

    if opts.emit_bc {
        let bc_path = format!("{stem}.spq.bc");
        if let Err(e) = std::fs::write(&bc_path, &bc) {
            eprintln!("error: writing {bc_path}: {e}");
        } else {
            println!("✓ wrote {bc_path} ({} bytes)", bc.len());
        }
    }

    // Materialize a throwaway crate in a temp dir.
    let dir = match scaffold(&name, &version, &bc, &dirs, query) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let ok = if opts.wasm {
        run_streamed(
            "wasm-pack",
            &["build", "--target", "web", "--release"],
            &dir,
        )
    } else {
        let mut args = vec!["build".to_string()];
        if opts.release {
            args.push("--release".to_string());
        }
        if let Some(t) = &opts.target {
            args.push("--target".to_string());
            args.push(t.clone());
        }
        let argrefs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        run_streamed("cargo", &argrefs, &dir)
    };

    if !ok {
        eprintln!("✗ build failed");
        let _ = std::fs::remove_dir_all(&dir);
        return std::process::ExitCode::FAILURE;
    }

    match collect_artifact(&dir, &name, opts) {
        Ok((dest, size)) => {
            let mode = if opts.release { "release" } else { "debug" };
            let tgt = opts.target.clone().unwrap_or_else(host_triple);
            println!(
                "✓ Built ./{} ({:.1} MB, {}, {})",
                dest.display(),
                size as f64 / 1_048_576.0,
                mode,
                tgt
            );
            println!("  Run: {}", run_hint(&name, &dirs.inputs));
        }
        Err(e) => {
            eprintln!("error: {e}");
            let _ = std::fs::remove_dir_all(&dir);
            return std::process::ExitCode::FAILURE;
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
    std::process::ExitCode::SUCCESS
}

/// Write the generated Cargo.toml + main.rs into a fresh temp dir.
fn scaffold(
    name: &str,
    version: &str,
    bc: &[u8],
    dirs: &Directives,
    query: &str,
) -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join(format!("splice-build-{}-{}", name, std::process::id()));
    std::fs::create_dir_all(dir.join("src")).map_err(|e| e.to_string())?;

    let cargo = TEMPLATE_CARGO
        .replace("{{name}}", name)
        .replace("{{version}}", version)
        .replace("{{core_path}}", CORE_PATH);
    std::fs::write(dir.join("Cargo.toml"), cargo).map_err(|e| e.to_string())?;

    let bc_bytes = bc
        .iter()
        .map(|b| b.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let main = TEMPLATE_MAIN
        .replace("{{bc_bytes}}", &bc_bytes)
        .replace("{{clap_args}}", CLAP_ARGS)
        .replace("{{var_bindings}}", &gen_var_bindings(dirs, query));
    std::fs::write(dir.join("src").join("main.rs"), main).map_err(|e| e.to_string())?;

    Ok(dir)
}

/// The argument-parsing preamble injected as `{{clap_args}}`.
const CLAP_ARGS: &str = r#"let __args: Vec<String> = std::env::args().skip(1).collect();
    let mut __argmap = std::collections::HashMap::<String, String>::new();
    let mut __i = 0;
    while __i < __args.len() {
        if let Some(rest) = __args[__i].strip_prefix("--") {
            if let Some((k, v)) = rest.split_once('=') {
                __argmap.insert(k.replace('-', "_"), v.to_string());
            } else if __i + 1 < __args.len() {
                __argmap.insert(rest.replace('-', "_"), __args[__i + 1].clone());
                __i += 1;
            }
        }
        __i += 1;
    }"#;

/// Generate `vars.insert(...)` lines for every variable used by the query.
fn gen_var_bindings(dirs: &Directives, query: &str) -> String {
    let mut names = scan_vars(query);
    for inp in &dirs.inputs {
        if !names.contains(&inp.name) {
            names.push(inp.name.clone());
        }
    }

    let mut out = String::new();
    for name in names {
        let decl = dirs.input(&name);
        let kind = decl.map(|d| d.kind).unwrap_or(VarKind::Str);
        let flag = name.replace('_', "-");
        let fetch = match decl.and_then(|d| d.default.clone()) {
            Some(d) => format!(
                "__argmap.get({name:?}).cloned().unwrap_or_else(|| {d:?}.to_string())"
            ),
            None => format!(
                "__argmap.get({name:?}).cloned().unwrap_or_else(|| {{ eprintln!(\"error: --{flag} is required\"); std::process::exit(2); }})"
            ),
        };
        let value = match kind {
            VarKind::Str => "RuntimeValue::Str(std::sync::Arc::from(__raw.as_str()))",
            VarKind::Float => "RuntimeValue::Float(__raw.parse().unwrap_or(0.0))",
            VarKind::Int => "RuntimeValue::Int(__raw.parse().unwrap_or(0))",
            VarKind::Bool => "RuntimeValue::Bool(__raw == \"true\" || __raw == \"1\")",
        };
        out.push_str(&format!(
            "    {{ let __raw = {fetch}; vars.insert({name:?}.to_string(), {value}); }}\n"
        ));
    }
    out
}

/// Spawn a command in `dir`, inheriting stdio so output streams live.
fn run_streamed(cmd: &str, args: &[&str], dir: &Path) -> bool {
    println!("> {} {}", cmd, args.join(" "));
    Command::new(cmd)
        .args(args)
        .current_dir(dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Copy the built artifact next to the cwd and return (path, size).
fn collect_artifact(dir: &Path, name: &str, opts: &BuildOpts) -> Result<(PathBuf, u64), String> {
    if opts.wasm {
        // wasm-pack emits pkg/<name>_bg.wasm.
        let wasm = dir.join("pkg").join(format!("{name}_bg.wasm"));
        let dest = PathBuf::from(format!("{name}.wasm"));
        std::fs::copy(&wasm, &dest).map_err(|e| format!("copying wasm: {e}"))?;
        let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
        return Ok((dest, size));
    }
    let profile = if opts.release { "release" } else { "debug" };
    let mut bin = dir.join("target");
    if let Some(t) = &opts.target {
        bin = bin.join(t);
    }
    bin = bin.join(profile).join(name);
    let mut dest = PathBuf::from(name);
    if cfg!(windows) {
        bin.set_extension("exe");
        dest.set_extension("exe");
    }
    std::fs::copy(&bin, &dest).map_err(|e| format!("copying binary from {}: {e}", bin.display()))?;
    let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
    Ok((dest, size))
}

fn run_hint(name: &str, inputs: &[InputDecl]) -> String {
    let mut s = format!("./{name}");
    for inp in inputs {
        let flag = inp.name.replace('_', "-");
        s.push_str(&format!(" --{flag} <{}>", inp.name));
    }
    s
}

fn host_triple() -> String {
    format!(
        "{}-{}",
        std::env::consts::ARCH,
        if cfg!(target_os = "linux") {
            "linux"
        } else if cfg!(target_os = "macos") {
            "macos"
        } else {
            std::env::consts::OS
        }
    )
}
