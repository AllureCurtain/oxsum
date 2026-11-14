/**
 * `globalThis.Buffer` for the Solana wallet chunk.
 *
 * `@solana/spl-token` (and parts of the `@solana/*` stack) reference the
 * Node `Buffer` global at module scope without importing it. The production
 * build tree-shakes the offending top-level statements away, but the Vite dev
 * server serves the pre-bundled dependency untouched, so opening a Solana
 * intent on the Topup page threw `ReferenceError: Buffer is not defined` and
 * white-screened the app in `npm run dev` (issue #1212).
 *
 * This module must be imported *before* any `@solana/*` import: ES modules
 * evaluate their imports in order, so the global exists by the time
 * `@solana/spl-token` evaluates. It lives in the lazy wallet chunk only — the
 * main bundle never pays for the polyfill.
 */
import { Buffer } from "buffer";

const scope = globalThis as typeof globalThis & { Buffer?: typeof Buffer };
if (typeof scope.Buffer === "undefined") {
  scope.Buffer = Buffer;
}

export {};
