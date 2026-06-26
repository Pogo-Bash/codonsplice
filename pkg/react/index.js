// @codonsplice/react — a useSpliceQL() hook over @codonsplice/wasm.
import { useState, useCallback } from 'react'
import { execute as csExecute } from '@codonsplice/wasm/helpers'

// Re-export the core tooling so apps can `import { useSpliceQL, compile, check }
// from '@codonsplice/react'` without depending on @codonsplice/wasm directly.
export { execute, stream, compile, check, initEngine } from '@codonsplice/wasm/helpers'

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
