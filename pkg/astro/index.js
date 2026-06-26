// @codonsplice/astro — framework-agnostic helpers for Astro islands.
//
// Astro components are framework-agnostic, so this simply re-exports the core
// helpers; use them inside a client:* island (or any of the framework wrappers
// for reactive state).
export { execute, stream, compile, check, ast, initEngine, CodonSplice } from '@codonsplice/wasm/helpers'
