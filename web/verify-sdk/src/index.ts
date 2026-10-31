/**
 * The wasm-bindgen module `scripts/build-verify-wasm.sh` emits. The SDK ships it
 * inside `wasm/` (see `files` in package.json) so the whole thing — loader plus
 * the `.wasm` it drives — is one npm package, not a URL the host page must
 * also serve. The `verify_wasm.js` glue is a *module*: the default export is
 * the async init that instantiates the inner `.wasm`, and the named exports
 * are the `#[wasm_bindgen]` functions the crate surfaces.
 */

/** The `{"ok": bool, "detail": string}` shape every exported verdict shares. */
export interface Verdict {
  ok: boolean;
  detail: string;
}

/** `verifyBundle`'s answer — the bill verdict plus the charge/price prose. */
export interface BillVerdict {
  /** `verdict === "pass"`. */
  ok: boolean;
  /** `"pass"`, `"chargeMismatch"` or `"fail"` — the machine name `ok` mirrors. */
  verdict: "pass" | "chargeMismatch" | "fail" | string;
  charge?: string;
  price?: string;
}

/** `verifySignedHead` / `verifyConsistency` / `verifyWitnesses` answer this. */
export interface HeadVerdict extends Verdict {}

/** `verifyStatementAnchor` / `verifyUsageReport` / `verifyPriceHead` answer this. */
export interface CheckVerdict extends Verdict {}

interface VerifyWasmModule {
  verifyBundle(bundleJson: string, expectedHashHex: string): Promise<string>;
  verifySignedHead(json: string): Promise<string>;
  verifyConsistency(json: string, checkJson: string): Promise<string>;
  verifyWitnesses(json: string, pinsJson: string): Promise<string>;
  verifyStatementAnchor(json: string, checkJson: string): Promise<string>;
  verifyUsageReport(json: string): Promise<string>;
  verifyPriceHead(json: string): Promise<string>;
}

let modulePromise: Promise<VerifyWasmModule> | null = null;

/**
 * Lazily instantiates the embedded wasm module — the import of
 * `wasm/verify_wasm.js` triggers its default-export init, which fetches the
 * sibling `.wasm` and returns the bound exports. Called once per verifier; the
 * promise is memoized so the module loads a single time per page.
 */
async function load(): Promise<VerifyWasmModule> {
  if (!modulePromise) {
    modulePromise = (async () => {
      // wasm/verify_wasm.js is the wasm-bindgen `--target web` output — its
      // default export is the async initializer the glue installs. We import
      // it as a URL so the package ships the file it needs.
      const glue = (await import(
        /* @vite-ignore */ new URL("../wasm/verify_wasm.js", import.meta.url).href
      )) as { default: (input?: unknown) => Promise<unknown> };
      const init = glue.default;
      // The init resolves to the wasm-bindgen `__wbindgen` namespace when the
      // module is instantiated — the exports object carries the functions.
      const bound = (await init()) as VerifyWasmModule;
      return bound;
    })();
  }
  return modulePromise;
}

function parse<T extends object>(json: string): T {
  return JSON.parse(json) as T;
}

/**
 * Verifies a bill's proof bundle — recomputes the content hash from the entry
 * source, links it to the tree head, and (when the bundle carries them)
 * recomputes the charge and checks the signed price binding.
 *
 * @param bundleJson the `ProofBundle` JSON — or the `{"data": …}` envelope the
 *   `GET /api/v1/entries/{entryId}/proof` endpoint wraps it in.
 * @param expectedHashHex the 64-character content hash the user recorded with
 *   the bill.
 */
export async function verifyBill(
  bundleJson: string,
  expectedHashHex: string,
): Promise<BillVerdict> {
  const m = await load();
  return parse(await m.verifyBundle(bundleJson, expectedHashHex));
}

/**
 * Checks that a signed tree head is genuine — the operator's Ed25519
 * signed-note signature over `oxsum/ledgers/<tenant>`.
 */
export async function verifySignedHead(json: string): Promise<HeadVerdict> {
  const m = await load();
  return parse(await m.verifySignedHead(json));
}

/**
 * Checks that a new head extends a head the verifier already holds — the
 * consistency proof an archived head demands.
 */
export async function verifyConsistency(
  json: string,
  checkJson: string,
): Promise<HeadVerdict> {
  const m = await load();
  return parse(await m.verifyConsistency(json, checkJson));
}

/** Checks the witness cosignatures a checkpoint carries against a pinset. */
export async function verifyWitnesses(
  json: string,
  pinsJson: string,
): Promise<HeadVerdict> {
  const m = await load();
  return parse(await m.verifyWitnesses(json, pinsJson));
}

/** Verifies a statement's completeness anchor against its check payload. */
export async function verifyStatementAnchor(
  json: string,
  checkJson: string,
): Promise<CheckVerdict> {
  const m = await load();
  return parse(await m.verifyStatementAnchor(json, checkJson));
}

/** Verifies a usage-report rollup against the day/anchor leaves it commits. */
export async function verifyUsageReport(json: string): Promise<CheckVerdict> {
  const m = await load();
  return parse(await m.verifyUsageReport(json));
}

/** Verifies a signed price-head checkpoint against the row it attests. */
export async function verifyPriceHead(json: string): Promise<CheckVerdict> {
  const m = await load();
  return parse(await m.verifyPriceHead(json));
}
