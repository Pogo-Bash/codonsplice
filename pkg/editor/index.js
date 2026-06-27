// @codonsplice/editor — a self-contained CodeMirror 6 SpliceQL editor with
// IntelliSense (syntax highlighting + context-aware autocompletion).
//
// Framework-agnostic: `spliceqlExtensions()` returns an array of CM6 extensions
// you can drop into any EditorView, and `mountSpliceEditor()` constructs one for
// you. The framework wrappers (@codonsplice/react, /vue, /svelte) build on this.
//
// The vocabulary below mirrors the SpliceQL lexer/grammar (CodonSplice engine)
// and is kept identical to the `splice create` scaffold's editor. Keep in sync
// with crates/spliceql + the engine builtins when the language grows.
import { EditorView, minimalSetup } from 'codemirror'
import { lineNumbers, highlightActiveLine, keymap } from '@codemirror/view'
import { EditorState } from '@codemirror/state'
import { StreamLanguage, HighlightStyle, syntaxHighlighting, bracketMatching } from '@codemirror/language'
import { autocompletion, completionKeymap, closeBrackets, closeBracketsKeymap, snippetCompletion } from '@codemirror/autocomplete'
import { tags as t } from '@lezer/highlight'

export const KEYWORDS = ['FROM', 'SELECT', 'WHERE', 'AND', 'OR', 'NOT', 'CALL', 'WITH', 'ORDER', 'BY', 'ASC', 'DESC', 'LIMIT', 'INTO', 'AS']
export const BOOLEANS = ['true', 'false']
export const FORMATS = ['bam', 'vcf', 'bed', 'fasta', 'cram', 'json', 'tsv']
export const OPS = ['variants', 'cnv', 'coverage', 'reads', 'header']
export const PARAMS = ['min_af', 'min_allele_freq', 'min_depth', 'min_base_quality', 'min_mapping_quality', 'min_variant_reads', 'min_strand_bias', 'window_size', 'amp_threshold', 'del_threshold', 'min_windows', 'segmentation_method']
export const FIELDS = ['chr', 'chrom', 'pos', 'ref', 'alt', 'qual', 'depth', 'ref_count', 'alt_count', 'af', 'allele_freq', 'strand_bias', 'kind', 'filter', 'id', 'mapq', 'flag', 'strand', 'is_reverse', 'is_duplicate', 'is_secondary', 'start', 'end', 'coverage', 'normalized', 'masked']
export const FUNCTIONS = ['abs', 'floor', 'ceil', 'round', 'sqrt', 'pow', 'min', 'max', 'log', 'coalesce', 'len', 'upper', 'lower', 'concat', 'contains', 'starts_with', 'ends_with', 'substr', 'gc', 'revcomp', 'translate', 'codon_at']

const KW = new Set(KEYWORDS.map((s) => s.toLowerCase()))
const BOOL = new Set(BOOLEANS)
const TY = new Set([...FORMATS, ...OPS])
const PR = new Set(PARAMS)
const FD = new Set(FIELDS)
const FN = new Set(FUNCTIONS)

const tokenTable = {
  spliceKw: t.keyword, spliceStr: t.string, spliceNum: t.number, spliceCom: t.lineComment,
  spliceOp: t.operator, spliceTy: t.typeName, splicePr: t.propertyName, spliceFd: t.variableName,
  spliceVar: t.special(t.variableName), spliceFn: t.function(t.variableName), spliceBool: t.bool,
}

const language = StreamLanguage.define({
  token(stream) {
    if (stream.match(/--.*/)) return 'spliceCom'
    if (stream.match(/"(?:[^"\\]|\\.)*"/)) return 'spliceStr'
    if (stream.match(/\$[A-Za-z_]\w*/)) return 'spliceVar'
    if (stream.match(/\d+(?:\.\d+)?/)) return 'spliceNum'
    if (stream.match(/>=|<=|!=|=|>|<|\+|-|\*|\//)) return 'spliceOp'
    const m = stream.match(/[A-Za-z_]\w*/)
    if (m) {
      const w = m[0].toLowerCase()
      if (KW.has(w)) return 'spliceKw'
      if (BOOL.has(w)) return 'spliceBool'
      if (TY.has(w)) return 'spliceTy'
      if (PR.has(w)) return 'splicePr'
      if (FN.has(w)) return 'spliceFn'
      if (FD.has(w)) return 'spliceFd'
      return null
    }
    stream.next()
    return null
  },
  tokenTable,
})

const highlight = HighlightStyle.define([
  { tag: t.keyword, color: '#cba6f7', fontWeight: '600' },
  { tag: t.string, color: '#a6e3a1' },
  { tag: t.number, color: '#fab387' },
  { tag: t.bool, color: '#fab387', fontWeight: '600' },
  { tag: t.lineComment, color: '#6c7086', fontStyle: 'italic' },
  { tag: t.operator, color: '#89dceb' },
  { tag: t.typeName, color: '#f9e2af' },
  { tag: t.propertyName, color: '#74c7ec' },
  { tag: t.variableName, color: '#cdd6f4' },
  { tag: t.special(t.variableName), color: '#f5c2e7' },
  { tag: t.function(t.variableName), color: '#89b4fa' },
])

// Pre-built completion groups. Functions expand to `name()` with the cursor
// placed between the parens via a CodeMirror snippet.
const KEYWORD_COMPLETIONS = KEYWORDS.map((l) => ({ label: l, type: 'keyword', detail: 'clause' }))
const FORMAT_COMPLETIONS = FORMATS.map((l) => ({ label: l, type: 'type', detail: 'format' }))
const OP_COMPLETIONS = OPS.map((l) => ({ label: l, type: 'function', detail: 'operation' }))
const PARAM_COMPLETIONS = PARAMS.map((l) => ({ label: l, type: 'property', detail: 'param' }))
const FIELD_COMPLETIONS = FIELDS.map((l) => ({ label: l, type: 'variable', detail: 'field' }))
const BOOL_COMPLETIONS = BOOLEANS.map((l) => ({ label: l, type: 'constant', detail: 'literal' }))
const FUNCTION_COMPLETIONS = FUNCTIONS.map((l) =>
  snippetCompletion(l + '(${})', { label: l, type: 'function', detail: 'function' })
)
const ALL_COMPLETIONS = [
  ...KEYWORD_COMPLETIONS, ...FORMAT_COMPLETIONS, ...OP_COMPLETIONS,
  ...FUNCTION_COMPLETIONS, ...PARAM_COMPLETIONS, ...FIELD_COMPLETIONS, ...BOOL_COMPLETIONS,
]

// The word immediately before the token being typed, lower-cased — used to pick
// a context-appropriate completion set (e.g. formats right after FROM/INTO).
function prevWord(ctx) {
  const before = ctx.state.sliceDoc(Math.max(0, ctx.pos - 240), ctx.pos)
  const m = /([A-Za-z_]\w*)\s+[\w$]*$/.exec(before)
  return m ? m[1].toLowerCase() : null
}

function complete(ctx) {
  const word = ctx.matchBefore(/[\w$]+/)
  if (!word && !ctx.explicit) return null
  const prev = prevWord(ctx)
  let options
  if (prev === 'from' || prev === 'into') options = FORMAT_COMPLETIONS
  else if (prev === 'call') options = OP_COMPLETIONS
  else if (prev === 'with') options = PARAM_COMPLETIONS
  else if (prev === 'order' || prev === 'by') options = [...FIELD_COMPLETIONS, ...FUNCTION_COMPLETIONS]
  else options = ALL_COMPLETIONS
  return { from: word ? word.from : ctx.pos, options, validFor: /[\w$]*/ }
}

const theme = EditorView.theme(
  {
    '&': { height: '100%', backgroundColor: '#11111b', color: '#cdd6f4', fontSize: '13px', textAlign: 'left' },
    '.cm-scroller': { fontFamily: "'JetBrains Mono', ui-monospace, monospace", lineHeight: '1.6', overflow: 'auto' },
    '.cm-content': { caretColor: '#cba6f7', padding: '8px 0', textAlign: 'left' },
    '.cm-gutters': { backgroundColor: '#11111b', color: '#45475a', border: 'none' },
    '.cm-activeLine': { backgroundColor: 'rgba(49,50,68,0.25)' },
    '.cm-activeLineGutter': { backgroundColor: 'transparent', color: '#7f849c' },
    '.cm-cursor': { borderLeftColor: '#cba6f7' },
    '.cm-selectionBackground, &.cm-focused .cm-selectionBackground': { backgroundColor: 'rgba(203,166,247,0.18)' },
    '&.cm-focused': { outline: 'none' },
    '.cm-tooltip.cm-tooltip-autocomplete': { border: '1px solid #313244', backgroundColor: '#181825', borderRadius: '6px' },
    '.cm-tooltip.cm-tooltip-autocomplete > ul': { fontFamily: "'JetBrains Mono', ui-monospace, monospace", maxHeight: '14em' },
    '.cm-tooltip-autocomplete ul li': { color: '#bac2de', padding: '3px 10px' },
    '.cm-tooltip-autocomplete ul li[aria-selected]': { backgroundColor: '#cba6f7', color: '#11111b' },
    '.cm-completionLabel': { fontWeight: '500' },
    '.cm-completionDetail': { color: '#7f849c', fontStyle: 'normal', marginLeft: '1.5em' },
    '.cm-matchingBracket, &.cm-focused .cm-matchingBracket': { backgroundColor: 'rgba(137,180,250,0.22)', color: '#89dceb' },
    '.cm-nonmatchingBracket': { color: '#f38ba8' },
  },
  { dark: true }
)

// The full set of CM6 extensions that make up the SpliceQL editor: the
// StreamLanguage token mode, Catppuccin syntax highlighting, context-aware
// autocompletion, bracket matching/closing, and the theme. Drop these into any
// EditorView, optionally alongside your own extensions.
export function spliceqlExtensions() {
  return [
    keymap.of([...closeBracketsKeymap, ...completionKeymap]),
    minimalSetup,
    lineNumbers(),
    highlightActiveLine(),
    bracketMatching(),
    closeBrackets(),
    language,
    syntaxHighlighting(highlight),
    autocompletion({ override: [complete], activateOnTyping: true }),
    theme,
  ]
}

// Construct an EditorView mounted into `parent` with the SpliceQL extensions and
// an update listener that calls `onChange(docString)` on every document change.
// Returns the EditorView so callers can `.destroy()` it on teardown.
export function mountSpliceEditor(parent, { doc = '', onChange = () => {} } = {}) {
  return new EditorView({
    parent,
    state: EditorState.create({
      doc,
      extensions: [
        ...spliceqlExtensions(),
        EditorView.updateListener.of((u) => {
          if (u.docChanged) onChange(u.state.doc.toString())
        }),
      ],
    }),
  })
}
