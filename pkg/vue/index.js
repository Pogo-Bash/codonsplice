// @codonsplice/vue — a useSpliceQL() composable + <SpliceEditor> over @codonsplice/wasm.
import { ref, defineComponent, h, onMounted, onBeforeUnmount, watch } from 'vue'
import { execute as csExecute } from '@codonsplice/wasm/helpers'
import { mountSpliceEditor } from '@codonsplice/editor'

// Re-export the core tooling so apps can `import { useSpliceQL, compile, check }
// from '@codonsplice/vue'` without depending on @codonsplice/wasm directly.
export { execute, stream, compile, check, ast, initEngine } from '@codonsplice/wasm/helpers'
// Re-export the editor primitives for apps that want to mount their own view.
export { spliceqlExtensions, mountSpliceEditor } from '@codonsplice/editor'

// A controlled CodeMirror 6 SpliceQL editor. Uses v-model:
//
//   import { SpliceEditor } from '@codonsplice/vue'
//   <SpliceEditor v-model="query" />
//
// Props/events:
//   modelValue           — the editor's document (v-model; external changes reconciled)
//   update:modelValue    — emitted (docString) on every edit (v-model write-back)
export const SpliceEditor = defineComponent({
  name: 'SpliceEditor',
  props: { modelValue: { type: String, default: '' } },
  emits: ['update:modelValue'],
  setup(props, { emit }) {
    const host = ref(null)
    let view = null

    onMounted(() => {
      view = mountSpliceEditor(host.value, {
        doc: props.modelValue,
        onChange: (v) => emit('update:modelValue', v),
      })
    })

    onBeforeUnmount(() => {
      if (view) { view.destroy(); view = null }
    })

    // Reconcile an external modelValue change without clobbering live typing.
    watch(() => props.modelValue, (value) => {
      if (!view) return
      const current = view.state.doc.toString()
      if (value !== current) {
        view.dispatch({ changes: { from: 0, to: current.length, insert: value ?? '' } })
      }
    })

    return () => h('div', { ref: host })
  },
})

export function useSpliceQL() {
  const result = ref(null)
  const error = ref(null)
  const loading = ref(false)

  async function execute({ query, files, vars }) {
    loading.value = true
    error.value = null
    try {
      result.value = await csExecute({ query, files, vars })
      return result.value
    } catch (e) {
      error.value = e
      throw e
    } finally {
      loading.value = false
    }
  }

  return { execute, result, error, loading }
}
