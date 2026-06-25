//! Phase 5 — spliceql-grammar assets (TextMate grammar, Linguist sample/yaml).

use std::path::PathBuf;

fn grammar_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../spliceql-grammar")
}

#[test]
fn tmlanguage_is_valid_json() {
    let path = grammar_dir().join("grammars/spliceql.tmLanguage.json");
    let text = std::fs::read_to_string(&path).unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
    assert_eq!(json["scopeName"], "source.spq");
}

#[test]
fn all_thirteen_scopes_present() {
    let path = grammar_dir().join("grammars/spliceql.tmLanguage.json");
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let repo = json["repository"].as_object().expect("repository object");
    for key in [
        "shebang",
        "directives",
        "comments",
        "keywords",
        "genomic",
        "formats",
        "variables",
        "strings",
        "numbers",
        "booleans",
        "comparison",
        "arithmetic",
        "identifiers",
    ] {
        assert!(repo.contains_key(key), "missing scope rule: {key}");
    }
    assert_eq!(repo.len(), 13, "expected exactly 13 scope rules");
}

#[test]
fn variable_scope_matches_dollar_names() {
    let path = grammar_dir().join("grammars/spliceql.tmLanguage.json");
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let v = &json["repository"]["variables"];
    assert_eq!(v["name"], "variable.other.spliceql");
    assert!(v["match"].as_str().unwrap().contains("\\$"));
}

#[test]
fn sample_spq_parses() {
    let path = grammar_dir().join("linguist/sample.spq");
    let text = std::fs::read_to_string(&path).unwrap();
    // Strip the shebang + directive/comment preamble (lines starting with `#!`
    // or `--`) before handing the query body to the SpliceQL parser.
    let body: String = text
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            !(t.starts_with("#!") || t.starts_with("--"))
        })
        .collect::<Vec<_>>()
        .join("\n");
    spliceql::parse(&body).expect("sample.spq query body parses");
}

#[test]
fn languages_yml_fragment_well_formed() {
    let path = grammar_dir().join("linguist/languages.yml.fragment");
    let text = std::fs::read_to_string(&path).unwrap();
    // Lightweight structural checks (no YAML dependency).
    assert!(text.starts_with("SpliceQL:"));
    assert!(text.contains("tm_scope: source.spq"));
    assert!(text.contains("\".spq\""));
    assert!(text.contains("type: programming"));
}
