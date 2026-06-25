// @codonsplice/react — a useSpliceQL() hook over @codonsplice/wasm.
import { useState, useCallback } from 'react'
import { execute as csExecute } from '../index.js'

export function useSpliceQL() {
  const [result, setResult] = useState(null)
  const [error, setError] = useState(null)
  const [loading, setLoading] = useState(false)

  const execute = useCallback(async ({ query, files }) => {
    setLoading(true)
    setError(null)
    try {
      const r = await csExecute({ query, files })
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
