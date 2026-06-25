// @codonsplice/svelte — a createSpliceQL() store factory over @codonsplice/wasm.
import { writable } from 'svelte/store'
import { execute as csExecute } from '../index.js'

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
