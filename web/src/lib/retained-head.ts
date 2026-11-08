/** Customer-scoped trust inputs. A server-fetched key is a recorded first observation,
 * not an independently authenticated identity. Never migrate the old unscoped archive. */
import type { components, paths } from "../api/schema";

export type SignedHead = components["schemas"]["SignedTreeHead"];
export type TreeHead = components["schemas"]["TreeHead"];
export type PublishedKey = components["schemas"]["VerifyingKey"];
export type Consistency = paths["/api/v1/log/consistency"]["get"]["responses"][200]["content"]["application/json"]["data"];
export interface LedgerIdentity { site: string; organizationId: string; tenantId: string }
export interface RetainedHead {
  version: 1;
  identity: LedgerIdentity;
  origin: string;
  key: PublishedKey;
  head: TreeHead;
  note: string;
  retainedAt: string;
  trust: "server-first-observation";
}
export interface HeadVerifier {
  verifySignedHead(note: string, head: string, origin: string, key: string): string | Promise<string>;
  verifyConsistency(response: string, held: string, origin: string, key: string): string | Promise<string>;
}
export interface HeadCheck {
  kind: "unchanged" | "extended" | "emptyAnchor";
  head: SignedHead;
  consistency?: Consistency;
}

export function archiveKey(identity: LedgerIdentity): string {
  return `oxsum.retained-head.v1:${encodeURIComponent(identity.site)}:${encodeURIComponent(identity.organizationId)}:${encodeURIComponent(identity.tenantId)}`;
}

function originFor(identity: LedgerIdentity): string {
  return `oxsum/ledgers/${identity.tenantId}`;
}

function tree(value: TreeHead): TreeHead {
  if (!value || !Number.isSafeInteger(value.size) || value.size < 0 || !/^[0-9a-f]{64}$/.test(value.root)) throw new Error("Invalid ledger head.");
  return { size: value.size, root: value.root };
}

function keyOf(value: PublishedKey): PublishedKey {
  if (!value || typeof value.keyName !== "string" || !value.keyName || !/^[0-9a-f]{8}$/.test(value.keyHash) || typeof value.publicKey !== "string") throw new Error("Invalid operator key.");
  let publicKey = value.publicKey;
  if (!/^[0-9a-f]{64}$/i.test(publicKey)) {
    const bytes = atob(publicKey);
    if (bytes.length !== 32) throw new Error("Invalid operator key.");
    publicKey = Array.from(bytes, (byte) => byte.charCodeAt(0).toString(16).padStart(2, "0")).join("");
  }
  return { keyName: value.keyName, publicKey: publicKey.toLowerCase(), keyHash: value.keyHash };
}

function passed(json: string): void {
  const verdict = JSON.parse(json) as { ok?: unknown };
  if (verdict?.ok !== true) throw new Error("The local verifier did not confirm the proof.");
}

/** Invalid or foreign records fail visibly rather than becoming an empty archive. */
export function readArchive(raw: string | null, identity: LedgerIdentity): RetainedHead | null {
  if (raw === null) return null;
  const value = JSON.parse(raw) as RetainedHead;
  if (value?.version !== 1 || value.identity?.site !== identity.site || value.identity?.organizationId !== identity.organizationId || value.identity?.tenantId !== identity.tenantId || value.origin !== originFor(identity) || value.trust !== "server-first-observation" || typeof value.note !== "string" || typeof value.retainedAt !== "string" || !Number.isFinite(Date.parse(value.retainedAt))) throw new Error("The retained trust record is invalid or belongs to another customer.");
  return { ...value, key: keyOf(value.key), head: tree(value.head) };
}

export async function retainHead(identity: LedgerIdentity, served: SignedHead, verifier: HeadVerifier): Promise<RetainedHead> {
  if (served.origin !== originFor(identity)) throw new Error("The served ledger belongs to another customer.");
  const head = tree(served);
  const key = keyOf(served);
  passed(await verifier.verifySignedHead(served.note, JSON.stringify(head), originFor(identity), JSON.stringify(key)));
  return { version: 1, identity: { ...identity }, origin: originFor(identity), key, head, note: served.note, retainedAt: new Date().toISOString(), trust: "server-first-observation" };
}

/** Always authenticate with the original retained key and origin. A fresh response
 * must never supply replacement trust. The original archive remains unchanged. */
export async function checkHead(held: RetainedHead, served: SignedHead, verifier: HeadVerifier, fetchConsistency: (size: number) => Promise<Consistency>): Promise<HeadCheck> {
  const stored = readArchive(JSON.stringify(held), held.identity)!;
  const head = tree(served);
  const key = JSON.stringify(stored.key);
  if (served.origin !== stored.origin || JSON.stringify(keyOf(served)) !== key) throw new Error("The ledger origin or operator key changed. Retained trust was preserved; compare keys through an independent channel.");
  passed(await verifier.verifySignedHead(stored.note, JSON.stringify(stored.head), stored.origin, key));
  passed(await verifier.verifySignedHead(served.note, JSON.stringify(head), stored.origin, key));
  if (head.size < stored.head.size) throw new Error("The served ledger is smaller than the retained head.");
  if (head.size === stored.head.size && head.root !== stored.head.root) throw new Error("The ledger has a different root at the retained size.");
  // The API intentionally refuses from=0: every history extends an empty tree.
  if (stored.head.size === 0) return { kind: "emptyAnchor", head: served };
  if (head.size === stored.head.size) return { kind: "unchanged", head: served };
  const response = await fetchConsistency(stored.head.size);
  if (JSON.stringify(keyOf(response)) !== key) throw new Error("The consistency response changed the operator key.");
  const latest = tree(response.head);
  if (latest.size !== head.size || latest.root !== head.root) throw new Error("The consistency response differs from the observed head. Retry the check; retained trust was preserved.");
  passed(await verifier.verifyConsistency(JSON.stringify(response), JSON.stringify(stored.head), stored.origin, key));
  return { kind: "extended", head: { ...served, ...latest, note: response.note }, consistency: response };
}

let loading: Promise<HeadVerifier> | null = null;
/** Missing/placeholder WASM remains a verification error; there is no HTTP verdict fallback. */
export function loadHeadVerifier(): Promise<HeadVerifier> {
  if (!loading) {
    loading = (async () => {
      const url = new URL("/verify-wasm/verify_wasm.js", window.location.origin).href;
      const module = await import(/* @vite-ignore */ url) as HeadVerifier & { default: () => Promise<unknown> };
      await module.default();
      return module;
    })().catch((error: unknown) => { loading = null; throw error; });
  }
  return loading;
}
