//! `.spq` file directive parser.
//!
//! A `.spq` file is a SpliceQL query optionally preceded by a shebang, a vim
//! modeline, and `-- @key: value` metadata directives. [`parse_directives`]
//! strips the preamble and returns the structured metadata plus the remaining
//! source for the SpliceQL parser.

/// The runtime type of a declared variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarKind {
    Str,
    Float,
    Int,
    Bool,
}

impl VarKind {
    fn parse(s: &str) -> Option<VarKind> {
        match s {
            "str" | "string" => Some(VarKind::Str),
            "float" | "f64" => Some(VarKind::Float),
            "int" | "i64" => Some(VarKind::Int),
            "bool" => Some(VarKind::Bool),
            _ => None,
        }
    }
}

/// A declared `@input` variable.
#[derive(Debug, Clone)]
pub struct InputDecl {
    pub name: String,
    pub required: bool,
    pub kind: VarKind,
    pub default: Option<String>,
    pub desc: String,
}

/// A declared `@output` variable.
#[derive(Debug, Clone)]
pub struct OutputDecl {
    pub name: String,
    pub kind: VarKind,
    pub desc: String,
}

/// All parsed `.spq` directives.
#[derive(Debug, Clone, Default)]
pub struct Directives {
    pub name: Option<String>,
    pub version: Option<String>,
    pub description: Option<String>,
    pub author: Option<String>,
    pub inputs: Vec<InputDecl>,
    pub outputs: Vec<OutputDecl>,
    pub splice_version: Option<String>,
}

impl Directives {
    /// Find an input declaration by variable name.
    pub fn input(&self, name: &str) -> Option<&InputDecl> {
        self.inputs.iter().find(|i| i.name == name)
    }
}

/// Parse the `.spq` preamble, returning the directives and the remaining source
/// (everything from the first non-directive line onward) for the SpliceQL
/// parser.
///
/// - A line-1 shebang (`#!...`) is stripped.
/// - A vim modeline (`-- vim: ...`) anywhere in the preamble is stripped.
/// - `-- @key: value` lines are parsed until the first non-directive, non-blank
///   line. Unknown `@keys` are silently ignored (forward compatibility).
pub fn parse_directives(source: &str) -> (Directives, &str) {
    let mut dirs = Directives::default();
    let mut offset = 0usize;
    let mut first = true;

    for line in source.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        let t = trimmed.trim_start();

        // Line 1 shebang.
        if first && t.starts_with("#!") {
            offset += line.len();
            first = false;
            continue;
        }
        first = false;

        if t.is_empty() {
            offset += line.len();
            continue;
        }

        // Vim modeline.
        if is_modeline(t) {
            offset += line.len();
            continue;
        }

        // Directive line: `-- @key: value`.
        if let Some(rest) = t.strip_prefix("--") {
            let rest = rest.trim_start();
            if let Some(after_at) = rest.strip_prefix('@') {
                parse_directive_line(after_at, &mut dirs);
                offset += line.len();
                continue;
            }
            // A plain `-- comment` line in the preamble: skip it too.
            offset += line.len();
            continue;
        }

        // First line of actual query — stop.
        break;
    }

    (dirs, &source[offset..])
}

fn is_modeline(t: &str) -> bool {
    let l = t.to_ascii_lowercase();
    l.contains("vim:") || l.contains("vim>") || l.contains("ex:")
}

fn parse_directive_line(after_at: &str, dirs: &mut Directives) {
    // `key: value`
    let (key, value) = match after_at.split_once(':') {
        Some((k, v)) => (k.trim(), v.trim()),
        None => return,
    };
    match key {
        "name" => dirs.name = non_empty(value),
        "version" => dirs.version = non_empty(value),
        "description" => dirs.description = non_empty(value),
        "author" => dirs.author = non_empty(value),
        "splice-version" => dirs.splice_version = non_empty(value),
        "input" => {
            if let Some(i) = parse_input(value) {
                dirs.inputs.push(i);
            }
        }
        "output" => {
            if let Some(o) = parse_output(value) {
                dirs.outputs.push(o);
            }
        }
        _ => {} // unknown directive — ignore for forward compat
    }
}

fn non_empty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Split a directive value into its leading whitespace tokens and a trailing
/// quoted description.
fn split_desc(value: &str) -> (Vec<String>, String) {
    if let Some(q) = value.find('"') {
        let head = &value[..q];
        let rest = &value[q + 1..];
        let desc = rest.split('"').next().unwrap_or("").to_string();
        (head.split_whitespace().map(String::from).collect(), desc)
    } else {
        (value.split_whitespace().map(String::from).collect(), String::new())
    }
}

/// Tokenize a directive value on whitespace, treating a double-quoted run as a
/// single token (quotes stripped). Each token is tagged with whether it was
/// quoted, so a quoted string *default* (`"7"`) can be told apart from a bare
/// numeric/bool default and from the trailing quoted description.
fn tokenize(value: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    let mut quoted = false;
    let mut has_tok = false;
    for c in value.chars() {
        if in_quote {
            if c == '"' {
                in_quote = false;
            } else {
                cur.push(c);
            }
        } else if c == '"' {
            in_quote = true;
            quoted = true;
            has_tok = true;
        } else if c.is_whitespace() {
            if has_tok {
                out.push((std::mem::take(&mut cur), quoted));
                quoted = false;
                has_tok = false;
            }
        } else {
            cur.push(c);
            has_tok = true;
        }
    }
    if has_tok {
        out.push((cur, quoted));
    }
    out
}

/// `@input: name [required|optional] [type] [default] ["desc"]`
///
/// The description, when present, is the trailing **quoted** token; any token
/// between the type and the description is the default. This is what lets a
/// quoted string default (`"7"`) survive instead of being mistaken for the
/// description (previously the *first* quote was always taken as the desc, so
/// string defaults were silently dropped).
fn parse_input(value: &str) -> Option<InputDecl> {
    let toks = tokenize(value);
    let name = toks.first()?.0.clone();
    let mut i = 1;

    let required = match toks.get(i).map(|(t, _)| t.as_str()) {
        Some("required") => {
            i += 1;
            true
        }
        Some("optional") => {
            i += 1;
            false
        }
        _ => false,
    };

    // An unquoted recognized type keyword (str/float/int/bool) is the kind.
    let mut kind = VarKind::Str;
    if let Some((t, quoted)) = toks.get(i) {
        if !quoted {
            if let Some(k) = VarKind::parse(t) {
                kind = k;
                i += 1;
            }
        }
    }

    // The trailing quoted token (beyond the leading fixed tokens) is the desc.
    let mut end = toks.len();
    let mut desc = String::new();
    if end > i {
        let (last, last_quoted) = &toks[end - 1];
        if *last_quoted {
            desc = last.clone();
            end -= 1;
        }
    }

    // Whatever remains between the type and the description is the default.
    let default = if end > i { Some(toks[i].0.clone()) } else { None };

    Some(InputDecl {
        name,
        required,
        kind,
        default,
        desc,
    })
}

/// `@output: name [type] "desc"`
fn parse_output(value: &str) -> Option<OutputDecl> {
    let (tokens, desc) = split_desc(value);
    let name = tokens.first()?.clone();
    let kind = tokens
        .get(1)
        .and_then(|t| VarKind::parse(t))
        .unwrap_or(VarKind::Str);
    Some(OutputDecl { name, kind, desc })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "#!/usr/bin/env splice\n\
        -- vim: set ft=sql:\n\
        -- @name: caller\n\
        -- @version: 1.2.3\n\
        -- @description: call variants\n\
        -- @author: me\n\
        -- @input: bam required \"Input BAM\"\n\
        -- @input: min_af optional float 0.05 \"Min AF\"\n\
        -- @output: vcf \"Output\"\n\
        -- @splice-version: 0.1.0\n\
        -- @unknown_key: ignored\n\
        FROM bam $bam\nCALL variants\n";

    #[test]
    fn parses_all_directive_keys() {
        let (d, _) = parse_directives(SRC);
        assert_eq!(d.name.as_deref(), Some("caller"));
        assert_eq!(d.version.as_deref(), Some("1.2.3"));
        assert_eq!(d.description.as_deref(), Some("call variants"));
        assert_eq!(d.author.as_deref(), Some("me"));
        assert_eq!(d.splice_version.as_deref(), Some("0.1.0"));
        assert_eq!(d.inputs.len(), 2);
        assert_eq!(d.outputs.len(), 1);
    }

    #[test]
    fn strips_shebang_and_modeline_and_unknown() {
        let (_, rest) = parse_directives(SRC);
        assert!(rest.starts_with("FROM bam $bam"), "rest: {rest:?}");
        assert!(!rest.contains("#!"));
        assert!(!rest.contains("vim:"));
    }

    #[test]
    fn input_required_optional_type_default() {
        let (d, _) = parse_directives(SRC);
        let bam = d.input("bam").unwrap();
        assert!(bam.required);
        assert_eq!(bam.kind, VarKind::Str);
        assert_eq!(bam.default, None);

        let af = d.input("min_af").unwrap();
        assert!(!af.required);
        assert_eq!(af.kind, VarKind::Float);
        assert_eq!(af.default.as_deref(), Some("0.05"));
        assert_eq!(af.desc, "Min AF");
    }

    #[test]
    fn quoted_string_default_is_not_swallowed_by_desc() {
        // Regression for #17: a quoted string default used to be parsed as the
        // description, leaving the variable with no value.
        let src = "-- @input: chr optional string \"7\" \"Chromosome to scan\"\n\
                   FROM bam $bam\n";
        let (d, _) = parse_directives(src);
        let chr = d.input("chr").expect("chr input parsed");
        assert!(!chr.required);
        assert_eq!(chr.kind, VarKind::Str);
        assert_eq!(chr.default.as_deref(), Some("7"));
        assert_eq!(chr.desc, "Chromosome to scan");
    }

    #[test]
    fn scan_vars_dedups_in_order() {
        let vars = scan_vars("FROM bam $bam WHERE $x > $y CALL variants WITH a = $x");
        assert_eq!(vars, vec!["bam", "x", "y"]);
    }

    #[test]
    fn no_preamble_is_all_query() {
        let (d, rest) = parse_directives("FROM bam \"s.bam\" CALL variants");
        assert!(d.name.is_none());
        assert_eq!(rest, "FROM bam \"s.bam\" CALL variants");
    }
}

/// Scan a query body for `$name` template-variable references (deduplicated,
/// in first-seen order).
pub fn scan_vars(query: &str) -> Vec<String> {
    let bytes = query.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            let start = i + 1;
            let mut j = start;
            if j < bytes.len() && (bytes[j].is_ascii_alphabetic() || bytes[j] == b'_') {
                j += 1;
                while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                    j += 1;
                }
                let name = query[start..j].to_string();
                if !out.contains(&name) {
                    out.push(name);
                }
                i = j;
                continue;
            }
        }
        i += 1;
    }
    out
}
