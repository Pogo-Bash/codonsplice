//! `splice` — the CodonSplice CLI and TUI.
//!
//! ```text
//! splice                       launch the interactive TUI
//! splice query   "FROM ..."    compile + run, print result/error
//! splice compile "FROM ..."    compile + disassemble, print bytecode
//! splice check   "FROM ..."    parse + type-check only, no execution
//! ```

mod build;
mod create;
mod directive;
mod installer;
mod spq;
mod tui;
mod update;

use clap::{Parser, Subcommand};
use codonsplice_core::{compile, disassemble, suggest_param, CompileError, Vm, VmOutput};

#[derive(Parser)]
#[command(
    name = "splice",
    about = "CodonSplice — SpliceQL query engine (compiler + VM)",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Skip the automatic update check for this run.
    #[arg(long, global = true)]
    no_update: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Compile and run a query (pipeline execution stubs until Phase 4).
    Query { source: String },
    /// Compile a query and print its disassembled bytecode.
    Compile { source: String },
    /// Parse and type-check a query without executing it.
    Check { source: String },
    /// Scaffold a new front-end project (react/vue/svelte/astro) wired to splice.
    Create {
        /// Framework: react | vue | svelte | astro.
        framework: String,
        /// Project directory name (default: splice-app).
        name: Option<String>,
    },
    /// Launch the guided TUI installer (detect environment + install).
    Install,
    /// Check for and install the latest release of splice.
    Update,
    /// Remove splice (binary + PATH entries; guides npm/cargo installs).
    Uninstall,
    /// Scaffold a new `<name>.spq` script.
    New { name: String },
    /// Run a `.spq` script, binding `$variables` from `--flag value` args.
    Run {
        file: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Compile a `.spq` script to a self-contained binary (or `.wasm`).
    Build {
        file: String,
        /// Output binary name (default: @name directive or file stem).
        #[arg(short, long)]
        output: Option<String>,
        /// Build in release mode.
        #[arg(long)]
        release: bool,
        /// Cross-compile target triple.
        #[arg(long)]
        target: Option<String>,
        /// Produce a `.wasm` instead of a native binary.
        #[arg(long)]
        wasm: bool,
        /// Also write `<name>.spq.bc` alongside the binary.
        #[arg(long = "emit-bc")]
        emit_bc: bool,
    },
}

fn main() -> std::process::ExitCode {
    // Shebang / direct execution: `splice query.spq [--args]` (and
    // `./query.spq [--args]` via `#!/usr/bin/env splice`) runs the script.
    let raw: Vec<String> = std::env::args().collect();
    if let Some(first) = raw.get(1) {
        if first.ends_with(".spq") && std::path::Path::new(first).is_file() {
            return spq::cmd_run(first, &raw[2..]);
        }
    }

    let cli = Cli::parse();

    // Auto-check for updates before normal commands (including the bare TUI):
    // check, prompt y/N if a newer release exists, then proceed to the command.
    // Skipped only for the update/uninstall/install flows, which manage versions
    // themselves.
    if !matches!(
        cli.command,
        Some(Command::Update) | Some(Command::Uninstall) | Some(Command::Install)
    ) {
        update::auto_check(cli.no_update);
    }

    match cli.command {
        None => match tui::run() {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("tui error: {e}");
                std::process::ExitCode::FAILURE
            }
        },
        Some(Command::Query { source }) => cmd_query(&source),
        Some(Command::Compile { source }) => cmd_compile(&source),
        Some(Command::Check { source }) => cmd_check(&source),
        Some(Command::Create { framework, name }) => create::cmd_create(&framework, name),
        Some(Command::Install) => match installer::run() {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("installer error: {e}");
                std::process::ExitCode::FAILURE
            }
        },
        Some(Command::Update) => update::cmd_update(),
        Some(Command::Uninstall) => update::cmd_uninstall(),
        Some(Command::New { name }) => spq::cmd_new(&name),
        Some(Command::Run { file, args }) => spq::cmd_run(&file, &args),
        Some(Command::Build {
            file,
            output,
            release,
            target,
            wasm,
            emit_bc,
        }) => build::cmd_build(
            &file,
            &build::BuildOpts {
                output,
                release,
                target,
                wasm,
                emit_bc,
            },
        ),
    }
}

fn cmd_query(source: &str) -> std::process::ExitCode {
    let program = match compile(source) {
        Ok(p) => p,
        Err(e) => return fail_with(source, &e),
    };
    let bytes = program.code.len();
    match Vm::new(program).run() {
        Ok(VmOutput::Ready(_)) => {
            println!("✓ compiled and reached HALT ({bytes} bytes of bytecode).");
            std::process::ExitCode::SUCCESS
        }
        Ok(VmOutput::Text(t)) => {
            println!("{t}");
            std::process::ExitCode::SUCCESS
        }
        Ok(VmOutput::Records(records)) | Ok(VmOutput::Rows(records)) => {
            print!("{}", render_records(&records));
            println!("({} record(s))", records.len());
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("runtime error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Render a materialized record stream as newline-delimited JSON (capped, with
/// an elision note for large results).
fn render_records(records: &[codonsplice_core::Record]) -> String {
    const CAP: usize = 50;
    let mut out = String::new();
    for r in records.iter().take(CAP) {
        out.push_str(&codonsplice_core::vm::record_to_json(r).to_string());
        out.push('\n');
    }
    if records.len() > CAP {
        out.push_str(&format!("… {} more\n", records.len() - CAP));
    }
    out
}

fn cmd_compile(source: &str) -> std::process::ExitCode {
    match compile(source) {
        Ok(p) => {
            print!("{}", disassemble(&p));
            std::process::ExitCode::SUCCESS
        }
        Err(e) => fail_with(source, &e),
    }
}

fn cmd_check(source: &str) -> std::process::ExitCode {
    match compile(source) {
        Ok(_) => {
            println!("✓ query type-checks.");
            std::process::ExitCode::SUCCESS
        }
        Err(e) => fail_with(source, &e),
    }
}

fn fail_with(source: &str, err: &CompileError) -> std::process::ExitCode {
    eprint!("{}", err.render(source, suggestion_for(source, err).as_deref()));
    std::process::ExitCode::FAILURE
}

/// For an unknown-parameter error, look up the CALL operation (by re-parsing)
/// and compute a "did you mean" suggestion against that op's parameter set.
pub fn suggestion_for(source: &str, err: &CompileError) -> Option<String> {
    if let CompileError::UnknownParam { key, .. } = err {
        let op = spliceql::parse(source).ok()?.call?.operation;
        suggest_param(key, &op)
    } else {
        None
    }
}

// ── AST pretty-printer (shared with the TUI) ─────────────────────────────────

use spliceql::ast::*;

/// Render a parsed [`Query`] as an indented tree, for the TUI's Ctrl+A view.
pub fn pretty_ast(q: &Query) -> String {
    let mut s = String::new();
    s.push_str("Query\n");
    s.push_str(&format!(
        "├─ from: {:?} {:?}{}\n",
        q.from.format,
        q.from.path,
        q.from
            .alias
            .as_ref()
            .map(|a| format!(" AS {a}"))
            .unwrap_or_default()
    ));
    if let Some(sel) = &q.select {
        s.push_str("├─ select:\n");
        for item in sel {
            s.push_str(&format!("│   • {}\n", pretty_expr(&item.expr)));
            if let Some(a) = &item.alias {
                s.push_str(&format!("│       AS {a}\n"));
            }
        }
    }
    if let Some(f) = &q.filter {
        s.push_str(&format!("├─ where: {}\n", pretty_expr(f)));
    }
    if let Some(c) = &q.call {
        s.push_str(&format!("├─ call: {}\n", c.operation));
    }
    if let Some(w) = &q.with {
        s.push_str("├─ with:\n");
        for (k, v) in w {
            s.push_str(&format!("│   {k} = {}\n", pretty_expr(v)));
        }
    }
    if let Some(o) = &q.order {
        s.push_str("├─ order:\n");
        for item in o {
            s.push_str(&format!(
                "│   • {} {:?}\n",
                pretty_expr(&item.expr),
                item.direction
            ));
        }
    }
    if let Some(l) = &q.limit {
        s.push_str(&format!("├─ limit: {}\n", pretty_expr(l)));
    }
    if let Some(i) = &q.into {
        s.push_str(&format!("└─ into: {:?} {:?}\n", i.format, i.path));
    }
    s
}

/// Render an [`Expr`] in a compact, fully-parenthesised infix form.
pub fn pretty_expr(e: &Expr) -> String {
    match e {
        Expr::IntLit(n, _) => n.to_string(),
        Expr::FloatLit(v, _) => v.to_string(),
        Expr::StringLit(s, _) => format!("{s:?}"),
        Expr::BoolLit(b, _) => b.to_string(),
        Expr::Ident(name, _) => name.clone(),
        Expr::Var(name, _) => format!("${name}"),
        Expr::Wildcard(_) => "*".to_string(),
        Expr::Unary { op, operand, .. } => {
            let o = match op {
                UnaryOp::Neg => "-",
                UnaryOp::Not => "NOT ",
            };
            format!("({o}{})", pretty_expr(operand))
        }
        Expr::Binary { op, left, right, .. } => {
            format!("({} {} {})", pretty_expr(left), bin_sym(op), pretty_expr(right))
        }
        Expr::FieldAccess { object, field, .. } => {
            format!("{}.{field}", pretty_expr(object))
        }
        Expr::Call { callee, args, .. } => {
            let a: Vec<String> = args.iter().map(pretty_expr).collect();
            format!("{}({})", pretty_expr(callee), a.join(", "))
        }
    }
}

fn bin_sym(op: &BinOp) -> &'static str {
    match op {
        BinOp::And => "AND",
        BinOp::Or => "OR",
        BinOp::Eq => "=",
        BinOp::NotEq => "!=",
        BinOp::Lt => "<",
        BinOp::Gt => ">",
        BinOp::LtEq => "<=",
        BinOp::GtEq => ">=",
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
    }
}
