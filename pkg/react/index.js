// @codonsplice/react — a useSpliceQL() hook + <SpliceEditor> over @codonsplice/wasm.
import { useState, useCallback, useRef, useEffect, createElement } from 'react'
import { execute as csExecute } from '@codonsplice/wasm/helpers'
import { mountSpliceEditor } from '@codonsplice/editor'

// Re-export the core tooling so apps can `import { useSpliceQL, compile, check }
// from '@codonsplice/react'` without depending on @codonsplice/wasm directly.
export { execute, stream, compile, check, ast, initEngine } from '@codonsplice/wasm/helpers'
// Re-export the editor primitives for apps that want to mount their own view.
export { spliceqlExtensions, mountSpliceEditor } from '@codonsplice/editor'

// A controlled CodeMirror 6 SpliceQL editor.
//
//   import { SpliceEditor } from '@codonsplice/react'
//   <SpliceEditor value={query} onChange={setQuery} className="editor" style={{ height: 240 }} />
//
// Props:
//   value     — the editor's document (controlled; external changes are reconciled)
//   onChange  — (docString) => void, fired on every edit
//   className — passed to the host <div>
//   style     — passed to the host <div>
export function SpliceEditor({ value = '', onChange, className, style }) {
  const host = useRef(null)
  const viewRef = useRef(null)
  const onChangeRef = useRef(onChange)
  onChangeRef.current = onChange

  useEffect(() => {
    const view = mountSpliceEditor(host.current, {
      doc: value,
      onChange: (v) => { onChangeRef.current && onChangeRef.current(v) },
    })
    viewRef.current = view
    return () => { view.destroy(); viewRef.current = null }
    // Mount once; external value changes are reconciled by the effect below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  // Reconcile a controlled `value` that diverges from the editor's contents
  // (e.g. set programmatically), without clobbering in-progress typing.
  useEffect(() => {
    const view = viewRef.current
    if (!view) return
    const current = view.state.doc.toString()
    if (value !== current) {
      view.dispatch({ changes: { from: 0, to: current.length, insert: value ?? '' } })
    }
  }, [value])

  return createElement('div', { ref: host, className, style })
}

export function useSpliceQL() {
  const [result, setResult] = useState(null)
  const [error, setError] = useState(null)
  const [loading, setLoading] = useState(false)

  const execute = useCallback(async ({ query, files, vars }) => {
    setLoading(true)
    setError(null)
    try {
      const r = await csExecute({ query, files, vars })
      setResult(r)
      return r
    } catch (e) {
      setError(e)
      throw e
    } finally {
      setLoading(false)
    }
  }, [])

  return { execute, result, error, loading }
}
