// @codonsplice/vue — a useSpliceQL() composable over @codonsplice/wasm.
import { ref } from 'vue'
import { execute as csExecute } from '@codonsplice/wasm/helpers'

// Re-export the core tooling so apps can `import { useSpliceQL, compile, check }
// from '@codonsplice/vue'` without depending on @codonsplice/wasm directly.
export { execute, stream, compile, check, ast, initEngine } from '@codonsplice/wasm/helpers'

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
