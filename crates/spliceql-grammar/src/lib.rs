//! spliceql-grammar — data-only crate.
//!
//! The payload is the editor/grammar assets under `grammars/`, `linguist/`, and
//! `vscode/`; this file exists only so the package has a build target. See
//! `grammars/spliceql.tmLanguage.json` for the TextMate grammar and
//! `linguist/LINGUIST_PR.md` for the GitHub Linguist submission steps.

/// The TextMate scope name SpliceQL grammars register under.
pub const SCOPE: &str = "source.spq";
