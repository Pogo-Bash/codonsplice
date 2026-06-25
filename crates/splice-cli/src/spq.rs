//! `.spq` file execution: `splice run`, `splice new`, and variable binding from
//! CLI arguments.

use std::collections::HashMap;

use codonsplice_core::{compile, RuntimeValue, VarMap, Vm, VmOutput};

use crate::directive::{parse_directives, scan_vars, Directives, VarKind};

/// The template emitted by `splice new <name>`.
pub fn new_template(name: &str) -> String {
    format!(
        r#"#!/usr/bin/env splice
-- vim: set ft=sql:
-- @name: {name}
-- @version: 0.1.0
-- @description:
-- @input: bam required "Input BAM file"
-- @input: min_af optional float 0.05 "Minimum allele frequency"
-- @output: vcf "Output VCF file"

-- Write your SpliceQL query below
FROM bam $bam
WHERE depth > 10
CALL variants
WITH min_af = $min_af
INTO vcf $output
"#
    )
}

/// `splice new <name>` — scaffold `<name>.spq`.
pub fn cmd_new(name: &str) -> std::process::ExitCode {
    let filename = if name.ends_with(".spq") {
        name.to_string()
    } else {
        format!("{name}.spq")
    };
    let stem = filename.trim_end_matches(".spq");
    if std::path::Path::new(&filename).exists() {
        eprintln!("error: {filename} already exists");
        return std::process::ExitCode::FAILURE;
    }
    if let Err(e) = std::fs::write(&filename, new_template(stem)) {
        eprintln!("error: could not write {filename}: {e}");
        return std::process::ExitCode::FAILURE;
    }
    println!("✓ created {filename}");
    println!("  edit it, then run:  splice run {filename} --bam sample.bam --output out.vcf");
    println!("  or open the editor: splice {filename}");
    std::process::ExitCode::SUCCESS
}

/// `splice run <file.spq> [--name value ...]` — parse directives, bind variables
/// from args, compile, and execute.
pub fn cmd_run(file: &str, args: &[String]) -> std::process::ExitCode {
    let source = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {file}: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let (dirs, query) = parse_directives(&source);

    let vars = match vars_from_args(&dirs, query, args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return std::process::ExitCode::from(2);
        }
    };

    let program = match compile(query) {
        Ok(p) => p,
        Err(e) => {
            eprint!("{}", e.render(query, None));
            return std::process::ExitCode::FAILURE;
        }
    };

    match Vm::new(program).with_vars(vars).run() {
        Ok(VmOutput::Records(recs)) | Ok(VmOutput::Rows(recs)) => {
            for r in &recs {
                println!("{}", codonsplice_core::vm::record_to_json(r));
            }
            std::process::ExitCode::SUCCESS
        }
        Ok(VmOutput::Text(t)) => {
            println!("{t}");
            std::process::ExitCode::SUCCESS
        }
        Ok(VmOutput::Ready(_)) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Parse `--flag value` / `--flag=value` pairs (hyphens → underscores in keys).
pub fn parse_flag_args(args: &[String]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut i = 0;
    while i < args.len() {
        if let Some(rest) = args[i].strip_prefix("--") {
            if let Some((k, v)) = rest.split_once('=') {
                map.insert(k.replace('-', "_"), v.to_string());
            } else if i + 1 < args.len() {
                map.insert(rest.replace('-', "_"), args[i + 1].clone());
                i += 1;
            } else {
                map.insert(rest.replace('-', "_"), String::new());
            }
        }
        i += 1;
    }
    map
}

/// Build a [`VarMap`] from CLI args, honoring `@input` types/defaults/required.
/// Every `$var` used in the query must resolve (via an arg or a declared
/// default), else a precise error is returned.
pub fn vars_from_args(
    dirs: &Directives,
    query: &str,
    args: &[String],
) -> Result<VarMap, String> {
    let argmap = parse_flag_args(args);
    let mut vars = VarMap::new();

    // The set of names to bind: every $var referenced, plus declared inputs.
    let mut names = scan_vars(query);
    for inp in &dirs.inputs {
        if !names.contains(&inp.name) {
            names.push(inp.name.clone());
        }
    }

    for name in names {
        let decl = dirs.input(&name);
        let kind = decl.map(|d| d.kind).unwrap_or(VarKind::Str);
        let default = decl.and_then(|d| d.default.clone());

        let raw = argmap.get(&name).cloned().or(default);
        let raw = match raw {
            Some(v) => v,
            None => {
                let flag = name.replace('_', "-");
                return Err(format!(
                    "${name} has no value — pass --{flag} <value> (or add an @input default)",
                ));
            }
        };
        vars.insert(name.clone(), coerce(&name, &raw, kind)?);
    }
    Ok(vars)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directive::parse_directives;

    #[test]
    fn vars_from_args_builds_typed_varmap() {
        let src = "-- @input: bam required \"\"\n\
            -- @input: min_af optional float 0.05 \"\"\n\
            FROM bam $bam CALL variants WITH min_af = $min_af INTO vcf $output\n";
        let (dirs, query) = parse_directives(src);
        let args: Vec<String> = ["--bam", "s.bam", "--min-af", "0.1", "--output", "o.vcf"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let vm = vars_from_args(&dirs, query, &args).unwrap();
        assert!(matches!(vm.get("bam"), Some(RuntimeValue::Str(s)) if &**s == "s.bam"));
        assert!(matches!(vm.get("min_af"), Some(RuntimeValue::Float(x)) if (*x - 0.1).abs() < 1e-9));
        assert!(matches!(vm.get("output"), Some(RuntimeValue::Str(s)) if &**s == "o.vcf"));
    }

    #[test]
    fn missing_required_var_errors() {
        let src = "-- @input: bam required \"\"\nFROM bam $bam CALL variants\n";
        let (dirs, query) = parse_directives(src);
        let err = vars_from_args(&dirs, query, &[]).unwrap_err();
        assert!(err.contains("bam"), "err: {err}");
    }

    #[test]
    fn optional_default_applies() {
        let src = "-- @input: min_af optional float 0.05 \"\"\nFROM bam \"x\" CALL variants WITH min_af = $min_af\n";
        let (dirs, query) = parse_directives(src);
        let vm = vars_from_args(&dirs, query, &[]).unwrap();
        assert!(matches!(vm.get("min_af"), Some(RuntimeValue::Float(x)) if (*x - 0.05).abs() < 1e-9));
    }

    #[test]
    fn wrong_type_errors() {
        let src = "-- @input: n optional int 0 \"\"\nFROM bam \"x\" WHERE depth > $n CALL variants\n";
        let (dirs, query) = parse_directives(src);
        let args: Vec<String> = ["--n", "notanint"].iter().map(|s| s.to_string()).collect();
        assert!(vars_from_args(&dirs, query, &args).is_err());
    }
}

fn coerce(name: &str, raw: &str, kind: VarKind) -> Result<RuntimeValue, String> {
    match kind {
        VarKind::Str => Ok(RuntimeValue::Str(std::sync::Arc::from(raw))),
        VarKind::Float => raw
            .parse::<f64>()
            .map(RuntimeValue::Float)
            .map_err(|_| format!("--{} expects a float, got {raw:?}", name.replace('_', "-"))),
        VarKind::Int => raw
            .parse::<i64>()
            .map(RuntimeValue::Int)
            .map_err(|_| format!("--{} expects an int, got {raw:?}", name.replace('_', "-"))),
        VarKind::Bool => Ok(RuntimeValue::Bool(raw == "true" || raw == "1")),
    }
}
