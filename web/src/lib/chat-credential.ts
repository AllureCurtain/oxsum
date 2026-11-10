/** Browser chat credentials belong to one customer and one human principal. */
export interface ChatIdentity { organizationId: string; userId: string }
export interface ChatKey {
  id: string;
  name: string | null;
  prefix: string;
  createdBy: string | null;
  revokedAt: string | null;
  expiresAt: string | null;
}
export interface ChatCredential extends ChatIdentity { keyId: string; secret: string }
interface Dependencies {
  storage: Pick<Storage, "getItem" | "setItem">;
  session: () => Promise<ChatIdentity>;
  keys: () => Promise<ChatKey[]>;
  mint: () => Promise<ChatKey & { secret: string }>;
  keyOrganization: (secret: string) => Promise<string>;
  now?: () => number;
}
export function chatIdentityKey(identity: ChatIdentity): string {
  return `${identity.organizationId}:${identity.userId}`;
}
function matches(left: ChatIdentity, right: ChatIdentity): boolean {
  return left.organizationId === right.organizationId && left.userId === right.userId;
}
export class ChatIdentityChangedError extends Error {}

/** Reject a session transition before a credential can be used or retained. */
export async function requireChatIdentity(identity: ChatIdentity, session: Dependencies["session"]): Promise<void> {
  if (!matches(identity, await session())) throw new ChatIdentityChangedError("The active account changed. Reload Chat before sending.");
}
function live(key: ChatKey, identity: ChatIdentity, now: number): boolean {
  return key.name === "chat" && key.createdBy === identity.userId && !key.revokedAt &&
    (key.expiresAt === null || Date.parse(key.expiresAt) > now);
}
/** Deduplicate concurrent initialization, including React's effect replay. */
export function createChatCredentialStore(deps: Dependencies) {
  const pending = new Map<string, Promise<ChatCredential>>();
  const load = async (identity: ChatIdentity): Promise<ChatCredential> => {
    await requireChatIdentity(identity, deps.session);
    const storageKey = `oxsum.chat-key.v2:${chatIdentityKey(identity)}`;
    let stored: ChatCredential | null = null;
    try { stored = JSON.parse(deps.storage.getItem(storageKey) ?? "null") as ChatCredential | null; }
    catch { /* Invalid or unavailable storage cannot authorize a credential. */ }
    const keys = await deps.keys();
    const now = deps.now?.() ?? Date.now();
    const existing = stored && matches(stored, identity) && typeof stored.secret === "string" &&
      keys.find((key) => key.id === stored?.keyId && live(key, identity, now) && stored.secret.startsWith(key.prefix));
    if (existing && stored && await deps.keyOrganization(stored.secret) === identity.organizationId) {
      await requireChatIdentity(identity, deps.session);
      return stored;
    }
    await requireChatIdentity(identity, deps.session);
    const key = await deps.mint();
    if (!live(key, identity, now) || await deps.keyOrganization(key.secret) !== identity.organizationId) {
      throw new Error("The created chat key does not belong to this account. Reload Chat before sending.");
    }
    await requireChatIdentity(identity, deps.session);
    const credential = { ...identity, keyId: key.id, secret: key.secret };
    try { deps.storage.setItem(storageKey, JSON.stringify(credential)); }
    catch { /* The current page can still use its verified in-memory credential. */ }
    return credential;
  };
  return {
    ensure(identity: ChatIdentity): Promise<ChatCredential> {
      const id = chatIdentityKey(identity);
      const prior = pending.get(id);
      if (prior) return prior;
      const next = load(identity).finally(() => { if (pending.get(id) === next) pending.delete(id); });
      pending.set(id, next);
      return next;
    },
  };
}
