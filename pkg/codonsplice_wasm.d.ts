/* tslint:disable */
/* eslint-disable */

/**
 * The CodonSplice engine handle exposed to JavaScript.
 */
export class CodonSplice {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Parse a query and return its AST as a readable tree (for the demo's AST
     * view). Errors as the parse error string.
     */
    ast(source: string): string;
    /**
     * Parse + type-check only. Returns `null` on success, the error string on
     * failure.
     */
    check(source: string): string | undefined;
    /**
     * Compile a SpliceQL query and return its disassembled bytecode.
     */
    compile(source: string): string;
    /**
     * Execute a query against the JS file map, binding `$variables` from the
     * `vars` object (`{ name: value }`). Returns the result as a JSON value
     * (an array of records/rows, or `{ "text": ... }` for header/`INTO`).
     */
    execute(source: string, files: any, vars: any): any;
    /**
     * Execute pre-compiled `.spq.bc` bytecode (a `Uint8Array`) against the file
     * map + variables. Mirrors `execute` but skips parsing/compilation.
     */
    execute_bytecode(bc_bytes: Uint8Array, files: any, vars: any): any;
    /**
     * Initialize the engine. Installs the panic hook so Rust panics surface in
     * the JS console. Call once before anything else.
     */
    constructor();
    /**
     * The SpliceQL language crate version.
     */
    static spliceql_version(): string;
    /**
     * Execute and stream: `on_record` is called per record, `on_done` when the
     * stream completes, `on_error` on failure.
     */
    stream(source: string, files: any, vars: any, on_record: Function, on_done: Function, on_error: Function): void;
    /**
     * The codonsplice-core engine version.
     */
    static version(): string;
}

/**
 * Coverage + CNV analysis. `opts_json` is the snake_case `CoverageOptions`
 * JSON; returns the result object as a JSON string.
 */
export function analyze_coverage(bam: Uint8Array, bai: Uint8Array, opts_json: string): string;

/**
 * SNV variant calling. `opts_json` is the snake_case `VariantOptions` JSON.
 */
export function call_variants(bam: Uint8Array, bai: Uint8Array, opts_json: string): string;

/**
 * One-time panic-hook install so Rust panics surface in the JS console.
 */
export function init(): void;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_codonsplice_free: (a: number, b: number) => void;
    readonly codonsplice_ast: (a: number, b: number, c: number) => [number, number, number, number];
    readonly codonsplice_check: (a: number, b: number, c: number) => [number, number];
    readonly codonsplice_compile: (a: number, b: number, c: number) => [number, number, number, number];
    readonly codonsplice_execute: (a: number, b: number, c: number, d: any, e: any) => [number, number, number];
    readonly codonsplice_execute_bytecode: (a: number, b: number, c: number, d: any, e: any) => [number, number, number];
    readonly codonsplice_new: () => number;
    readonly codonsplice_spliceql_version: () => [number, number];
    readonly codonsplice_stream: (a: number, b: number, c: number, d: any, e: any, f: any, g: any, h: any) => [number, number];
    readonly analyze_coverage: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number];
    readonly call_variants: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number];
    readonly init: () => void;
    readonly codonsplice_version: () => [number, number];
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_exn_store: (a: number) => void;
    readonly __externref_table_alloc: () => number;
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __wbindgen_free: (a: number, b: number, c: number) => void;
    readonly __externref_table_dealloc: (a: number) => void;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
