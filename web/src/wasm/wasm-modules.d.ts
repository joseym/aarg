/** Ambient fallback for the generated wasm glue. `src/wasm/pkg` is built by
 *  `wasm-pack` and git-ignored, so in a fresh clone the real
 *  `aarg_wasm.d.ts` isn't present. This wildcard keeps the app type-checking
 *  either way; `WasmService` casts the dynamic import to its own local
 *  `WasmExports` interface regardless. */
declare module '*aarg_wasm.js' {
  const mod: unknown;
  export default mod;
}
