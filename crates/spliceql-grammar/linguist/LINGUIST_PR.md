# Submitting SpliceQL to GitHub Linguist

GitHub colours and classifies `.spq` files once Linguist knows about the
language. This is a checklist for the upstream PR.

## 1. Fork & branch

Fork <https://github.com/github-linguist/linguist> and branch from `main`:

```sh
git clone git@github.com:<you>/linguist.git
cd linguist
git checkout -b add-spliceql
```

## 2. Files to add / modify

1. **`lib/linguist/languages.yml`** — insert the entry from
   [`languages.yml.fragment`](./languages.yml.fragment), in alphabetical order
   (between "Spline Font Database" and "Squirrel"). Leave `language_id: null` —
   the maintainers assign the real ID.

2. **`vendor/grammars/`** — add the grammar as a submodule. Push
   [`spliceql.tmLanguage.json`](../grammars/spliceql.tmLanguage.json) to a public
   repo (e.g. `Pogo-Bash/spliceql-tmbundle`) and:

   ```sh
   git submodule add https://github.com/Pogo-Bash/spliceql-tmbundle \
     vendor/grammars/spliceql-tmbundle
   ```

   Then register it in **`grammars.yml`**:

   ```yaml
   vendor/grammars/spliceql-tmbundle:
   - source.spq
   ```

3. **`samples/SpliceQL/sample.spq`** — copy [`sample.spq`](./sample.spq). The
   sample must lex/parse cleanly (it uses the GRCh37 contig name `"7"`, matching
   the bundled NA12878 sample BAM).

## 3. Validate locally

```sh
bundle install
bundle exec rake samples       # regenerate the classifier from samples
bundle exec rake test          # must pass
script/licensed                # grammar license check
```

## 4. PR title & description

**Title:** `Add SpliceQL language support`

**Description (template):**

```
Adds the SpliceQL language — a SQL-like query language for genomic files
(BAM/VCF) used by CodonSplice (https://github.com/Pogo-Bash/codonsplice).

- Extension: `.spq`
- Grammar: source.spq (vendored from spliceql-tmbundle)
- Sample: samples/SpliceQL/sample.spq
- Color: #a6e3a1

Checklist (per CONTRIBUTING.md):
- [x] Language is used in >2,000 public .spq repos / files OR has an
      established ecosystem (link usage evidence here)
- [x] Grammar is MIT-licensed and self-contained
- [x] Sample parses with the SpliceQL parser
- [x] languages.yml entry is alphabetical with language_id left null
```

See the Linguist submission checklist:
<https://github.com/github-linguist/linguist/blob/main/CONTRIBUTING.md#adding-a-language>

> Note: Linguist requires a language to be in meaningful public use before it is
> accepted. Until the `.spq` corpus is large enough, the TextMate grammar and the
> VS Code extension (see `../vscode/`) give full local highlighting; the vim
> modeline (`-- vim: set ft=sql:`) covers vim/neovim/helix with zero config.
