// @codonsplice/vue — a useSpliceQL() composable over @codonsplice/wasm.
import { ref } from 'vue'
import { execute as csExecute } from '../index.js'

export function useSpliceQL() {
  const result = ref(null)
  const error = ref(null)
  const loading = ref(false)

  async function execute({ query, files }) {
    loading.value = true
    error.value = null
    try {
      result.value = await csExecute({ query, files })
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
