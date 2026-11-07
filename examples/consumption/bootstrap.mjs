/** Shared owner-consent/customer setup for the consumption example and local demo. */
export class HttpError extends Error {
  constructor(status, body) {
    super(`HTTP ${status}: ${body?.error?.code ?? "unexpected response"}`);
    this.status = status;
    this.body = body;
  }
}

export function createClient(base) {
  const origin = new URL(base);
  if (origin.username || origin.password || origin.search || origin.hash ||
      (origin.protocol !== "https:" && !(origin.protocol === "http:" &&
        ["localhost", "127.0.0.1", "[::1]"].includes(origin.hostname)))) {
    throw new Error("Use HTTPS or an isolated loopback HTTP origin without credentials/query/fragment");
  }
  return async (path, { method = "GET", token, session, body } = {}) => {
    const headers = {};
    if (body !== undefined) headers["content-type"] = "application/json";
    if (token) headers.authorization = `Bearer ${token}`;
    if (session) headers.cookie = `oxsum_session=${session}`;
    const response = await fetch(base.replace(/\/+$/, "") + path, {
      method, headers, redirect: "manual", signal: AbortSignal.timeout(30_000),
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    const text = await response.text();
    const json = text ? JSON.parse(text) : null;
    if (!response.ok) throw new HttpError(response.status, json);
    return { json, setCookie: response.headers.get("set-cookie") };
  };
}

export const key = () => `example-${crypto.randomUUID()}`;
export function sessionOf(response) {
  const token = (response.setCookie ?? "").split(";")[0].replace(/^oxsum_session=/, "");
  if (!token) throw new Error("Login did not establish a session; complete any second factor separately");
  return token;
}

/** Test-only manual receipt stands for declared local funding, never a real PSP capture. */
export async function provisionCustomer(call, { adminToken, amountMinor, ownerEmail, ownerPassword }) {
  if (!adminToken) throw new Error("An explicit deployment token is required for local setup/funding");
  let email = `owner-${crypto.randomUUID()}@example.test`;
  let password = crypto.randomUUID();
  let createdOwner = true;
  let minted;
  try {
    minted = await call("/api/v1/admin/business/setup-tokens", {
      method: "POST", token: adminToken, body: { idempotencyKey: key(), ownerEmail: email },
    });
  } catch (error) {
    if (!(error instanceof HttpError) || error.status !== 409) throw error;
    if (!ownerEmail || !ownerPassword) throw new Error("Existing business: supply its owner credentials explicitly; no demo-password fallback");
    email = ownerEmail; password = ownerPassword; createdOwner = false;
  }
  if (minted) {
    await call("/api/v1/business/setup/register", {
      method: "POST", body: { token: minted.json.data.token, businessName: "Local Example Business", email, password },
    });
  }
  const ownerSession = sessionOf(await call("/api/v1/auth/login", {
    method: "POST", body: { email, password },
  }));
  await call("/api/v1/session/context", {
    method: "POST", session: ownerSession, body: { idempotencyKey: key(), context: "business" },
  });
  const customerEmail = `customer-${crypto.randomUUID()}@example.test`;
  const customerPassword = crypto.randomUUID();
  const invite = await call("/api/v1/business/customer-invitations", {
    method: "POST", session: ownerSession,
    body: { idempotencyKey: key(), email: customerEmail, name: "Local Example Customer",
      externalId: key(), reason: "Isolated example customer; no real person" },
  });
  const registration = (await call("/api/v1/invitations/redeem", {
    method: "POST", body: { token: invite.json.data.token, email: customerEmail, password: customerPassword },
  })).json.data;
  if (registration.organization.id !== invite.json.data.organizationId || !registration.apiKey?.secret) {
    throw new Error("Invitation did not yield the reserved customer and first key");
  }
  const customerSession = sessionOf(await call("/api/v1/auth/login", {
    method: "POST", body: { email: customerEmail, password: customerPassword },
  }));
  const receiptBody = { idempotencyKey: key(), amountMinor, reference: key(),
    reason: "Synthetic local fixture funding; no payment or PSP certification" };
  const receiptPath = `/api/v1/admin/organizations/${registration.organization.id}/manual-receipts`;
  const receipt = (await call(receiptPath, { method: "POST", token: adminToken, body: receiptBody })).json.data;
  const replay = (await call(receiptPath, { method: "POST", token: adminToken, body: receiptBody })).json.data;
  const balance = (await call("/api/v1/balance", { session: customerSession })).json.data;
  if (receipt.entryId !== replay.entryId || replay.isNew !== false || balance.availableMinor !== amountMinor) {
    throw new Error("Local receipt replay or exact funded balance failed");
  }
  return { ownerSession, ownerEmail: email, ownerPassword: password, createdOwner,
    customerSession, customerEmail, customerPassword, registration, receipt };
}

/** Provision a loopback route and explicit customer sale terms, without calling a model. */
export async function configureFixtureRoute(call, { ownerSession, organizationId, channelName, model, upstream, price }) {
  const url = new URL(upstream);
  if (url.hostname !== "127.0.0.1" || url.protocol !== "http:") throw new Error("Fixture upstream must be literal IPv4 loopback HTTP");
  const mutate = (path, config) => call(path, { method: "POST", session: ownerSession,
    body: { idempotencyKey: key(), reason: "Isolated deterministic local fixture", config } });
  await mutate("/api/v1/business/channels", { name: channelName, baseUrl: upstream, protocol: "openai",
    apiKeys: ["local-stub-no-provider-credential"], destinationPolicy: { network: "internal", allowedIps: ["127.0.0.1"] } });
  await mutate(`/api/v1/business/channels/${channelName}/prices`, { model, weight: 100, price });
  const change = async (change) => (await call("/api/v1/business/sale-books/changes", {
    method: "POST", session: ownerSession, body: { idempotencyKey: key(), reason: "Explicit fixture customer sale terms", change },
  })).json.data;
  const book = await change({ operation: "createBook", name: `Fixture ${crypto.randomUUID()}` });
  const draft = await change({ operation: "saveDraft", bookId: book.resourceId, expectedRevision: 0,
    models: { [model]: { price, discountPercent: 0 } } });
  const published = await change({ operation: "publishDraft", bookId: book.resourceId, draftRevision: draft.revision });
  await change({ operation: "assignBook", organizationId, bookId: book.resourceId, version: published.revision });
  return { bookId: book.resourceId, version: published.revision };
}
