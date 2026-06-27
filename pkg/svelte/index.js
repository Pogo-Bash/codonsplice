// @codonsplice/svelte — a createSpliceQL() store factory + spliceEditor action
// over @codonsplice/wasm.
import { writable } from 'svelte/store'
import { execute as csExecute } from '@codonsplice/wasm/helpers'
import { mountSpliceEditor } from '@codonsplice/editor'

// Re-export the core tooling so apps can `import { createSpliceQL, compile,
// check } from '@codonsplice/svelte'` without depending on @codonsplice/wasm.
export { execute, stream, compile, check, ast, initEngine } from '@codonsplice/wasm/helpers'
// Re-export the editor primitives for apps that want to mount their own view.
export { spliceqlExtensions, mountSpliceEditor } from '@codonsplice/editor'

// A Svelte action that mounts a CodeMirror 6 SpliceQL editor into the node:
//
//   <script>
//     let query = 'FROM bam "x.bam" CALL variants'
//   </script>
//   <div use:spliceEditor={{ value: query, onChange: (v) => (query = v) }} />
//
// Params:
//   value    — the editor's initial/controlled document (external changes reconciled)
//   onChange — (docString) => void, fired on every edit
export function spliceEditor(node, { value = '', onChange } = {}) {
  const view = mountSpliceEditor(node, {
    doc: value,
    onChange: (v) => { onChange && onChange(v) },
  })
  return {
    // Reconcile an external `value` change without clobbering live typing.
    update({ value: next = '', onChange: nextOnChange } = {}) {
      onChange = nextOnChange
      const current = view.state.doc.toString()
      if (next !== current) {
        view.dispatch({ changes: { from: 0, to: current.length, insert: next ?? '' } })
      }
    },
    destroy() { view.destroy() },
  }
}

export function createSpliceQL() {
  const result = writable(null)
  const error = writable(null)
  const loading = writable(false)

  async function execute({ query, files, vars }) {
    loading.set(true)
    error.set(null)
    try {
      const r = await csExecute({ query, files, vars })
      result.set(r)
      return r
    } catch (e) {
      error.set(e)
      throw e
    } finally {
      loading.set(false)
    }
  }

  return { execute, result, error, loading }
}
