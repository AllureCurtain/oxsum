/**
 * Shared helpers for the webhook-tools examples (issue #617): an HTTP client
 * that never forwards credentials off the base URL, the owner-session
 * bootstrap the merchant create needs, the per-rail HMAC signer, and the
 * event payloads each tool posts.
 *
 * The tools drive the *versioned* merchant intake —
 * `POST /api/v1/webhooks/merchants/{merchantId}/versions/{versionId}` —
 * which is the durable, scoped surface; the rail's signature is verified
 * against the version's stored `webhookSecret` before any parsing runs.
 */

export const BASE = (
  process.env.OXSUM_BASE_URL ?? "http://127.0.0.1:3000"
).replace(/\/+$/, "");

export function requireBaseUrl() {
  const parsed = new URL(BASE);
  const loopback = ["localhost", "127.0.0.1", "::1"].includes(parsed.hostname);
  if (parsed.username || parsed.password || parsed.search || parsed.hash) {
    console.error("OXSUM_BASE_URL must not carry credentials, query or fragment");
    process.exit(2);
  }
  if (parsed.protocol !== "https:" && !(parsed.protocol === "http:" && loopback)) {
    console.error("Use HTTPS, or HTTP on loopback for an isolated test.");
    process.exit(2);
  }
}

let failures = 0;
export function report(label, ok, detail) {
  console.log(`[${ok ? "PASS" : "FAIL"}] ${label}${detail ? ` — ${detail}` : ""}`);
  if (!ok) failures += 1;
  return ok;
}
export function exitStatus() {
  if (failures > 0) {
    console.error(`\n${failures} assertion(s) failed.`);
    process.exit(1);
  }
}

export class HttpError extends Error {
  constructor(status, body) {
    super(`HTTP ${status}: ${JSON.stringify(body)}`);
    this.status = status;
    this.body = body;
  }
}

export async function call(path, { method = "GET", token, session, body, headers = {} } = {}) {
  const requestHeaders = { ...headers };
  if (body !== undefined && !requestHeaders["content-type"]) {
    requestHeaders["content-type"] = "application/json";
  }
  if (token) requestHeaders.authorization = `Bearer ${token}`;
  if (session) requestHeaders.cookie = `oxsum_session=${session}`;
  const response = await fetch(BASE + path, {
    method,
    headers: requestHeaders,
    body: body === undefined ? undefined : typeof body === "string" ? body : JSON.stringify(body),
    redirect: "manual",
  });
  const text = await response.text();
  let json = null;
  try {
    json = text ? JSON.parse(text) : null;
  } catch {
    json = text;
  }
  if (!response.ok) throw new HttpError(response.status, json ?? text);
  return { json, status: response.status, setCookie: response.headers.get("set-cookie") };
}

export const key = () => `webhook-tools-${crypto.randomUUID()}`;
export const sessionOf = (res) =>
  (res.setCookie ?? "").split(";")[0].replace(/^oxsum_session=/, "");

/** Owner session in business context — the merchant create/read surface needs it. */
export async function ownerSession() {
  const email = process.env.OXSUM_OWNER_EMAIL;
  const password = process.env.OXSUM_OWNER_PASSWORD;
  if (!email || !password) {
    console.error("Set OXSUM_OWNER_EMAIL and OXSUM_OWNER_PASSWORD.");
    process.exit(2);
  }
  const login = await call("/api/v1/auth/login", {
    method: "POST",
    body: { email, password },
  });
  const session = sessionOf(login);
  await call("/api/v1/session/context", {
    method: "POST",
    session,
    body: { idempotencyKey: key(), context: "business" },
  });
  return session;
}

/**
 * Creates and enables one merchant for `rail` ("hyperswitch"|"stripe"|"airwallex")
 * under the owner's business, answering `{merchantId, versionId, webhookSecret}`.
 * `webhookSecret` is the version's stored secret — the tools sign with it,
 * matching what the provider would hold.
 */
export async function createMerchant(session, rail, webhookSecret) {
  const config = {
    apiUrl: "https://api.example.test",
    apiKey: "example-api-key-000000",
    webhookSecret,
  };
  if (rail === "hyperswitch") {
    config.publishableKey = "pk_example";
    config.sdkUrl = "https://sdk.example.test";
  }
  if (rail === "airwallex") {
    config.clientId = "example-client-id";
    config.checkoutUrl = "https://checkout.example.test";
  }
  const accountId = `acct_${crypto.randomUUID().slice(0, 12)}`;
  const created = await call("/api/v1/business/merchants", {
    method: "POST",
    session,
    body: {
      idempotencyKey: key(),
      reason: "webhook-tools example merchant",
      name: `webhook-tools-${crypto.randomUUID().slice(0, 8)}`,
      rail,
      mode: "test",
      accountId,
      config,
    },
  });
  const { merchantId, versionId } = created.json.data;
  await call(`/api/v1/business/merchants/${merchantId}/status`, {
    method: "POST",
    session,
    body: { idempotencyKey: key(), reason: "enable for intake", enabled: true },
  });
  // The caller needs `accountId` for its event bodies: the intake rejects an
  // event whose merchant_id is not this merchant's account (merchantMismatch).
  return { merchantId, versionId, webhookSecret, accountId };
}

/**
 * Signs `body` (exact bytes) under `secret` for `rail`, answering the headers
 * the versioned intake verifies — `crates/integrations/src/merchant_events.rs`:
 *   hyperswitch: X-Webhook-Signature-512, hex HMAC-SHA512 over the raw body
 *   stripe:      Stripe-Signature "t=<unix>,v1=<hex HMAC-SHA256 of t.body>"
 *   airwallex:   x-timestamp (ms) + x-signature (hex HMAC-SHA256 of ts+body)
 */
export async function sign(rail, secret, body, now = Date.now()) {
  const enc = new TextEncoder();
  const mac = async (alg, data) => {
    const key = await crypto.subtle.importKey(
      "raw",
      enc.encode(secret),
      { name: "HMAC", hash: `SHA-${alg}` },
      false,
      ["sign"],
    );
    const tag = await crypto.subtle.sign("HMAC", key, data);
    return [...new Uint8Array(tag)].map((b) => b.toString(16).padStart(2, "0")).join("");
  };
  if (rail === "hyperswitch") {
    return { "x-webhook-signature-512": await mac(512, enc.encode(body)) };
  }
  if (rail === "stripe") {
    const t = Math.floor(now / 1000);
    return {
      "stripe-signature": `t=${t},v1=${await mac(256, enc.encode(`${t}.${body}`))}`,
    };
  }
  if (rail === "airwallex") {
    const ts = String(now);
    return {
      "x-timestamp": ts,
      "x-signature": await mac(256, enc.encode(`${ts}${body}`)),
    };
  }
  throw new Error(`unknown rail ${rail}`);
}

/**
 * A well-formed provider event body for `rail`. `payment` names the local
 * expected payment UUID (or a random one to exercise `unknownPayment`);
 * `opts` overrides `eventId`/`amount`/`currency`/`org`/`kind`.
 */
export function eventBody(rail, accountId, opts = {}) {
  const eventId = opts.eventId ?? `evt_${crypto.randomUUID().replaceAll("-", "")}`;
  const paymentId = opts.paymentId ?? crypto.randomUUID();
  const org = opts.org ?? crypto.randomUUID();
  const amount = opts.amount ?? 2500;
  const currency = opts.currency ?? "USD";
  const kind = opts.kind ?? "succeeded";
  if (rail === "hyperswitch") {
    return JSON.stringify({
      merchant_id: accountId,
      event_id: eventId,
      event_type:
        kind === "succeeded"
          ? "payment_succeeded"
          : kind === "failed"
            ? "payment_failed"
            : kind === "expired"
              ? "payment_expired"
              : "unsupported_kind",
      livemode: false,
      content: {
        type: "payment_details",
        object: {
          payment_id: `pay_${crypto.randomUUID().slice(0, 12)}`,
          amount,
          currency,
          metadata: { payment_id: paymentId, organization_id: org },
        },
      },
    });
  }
  if (rail === "stripe") {
    return JSON.stringify({
      id: eventId,
      type: kind === "succeeded" ? "checkout.session.completed" : "unsupported_kind",
      livemode: false,
      data: {
        object: {
          id: `cs_test_${crypto.randomUUID().slice(0, 12)}`,
          payment_status: "paid",
          mode: "payment",
          amount_total: amount,
          currency: currency.toLowerCase(),
          client_reference_id: paymentId,
          metadata: { payment_id: paymentId, organization_id: org },
        },
      },
    });
  }
  if (rail === "airwallex") {
    return JSON.stringify({
      id: eventId,
      name:
        kind === "succeeded"
          ? "payment_intent.succeeded"
          : kind === "failed"
            ? "payment_intent.failed"
            : "unsupported_kind",
      account_id: accountId,
      livemode: false,
      data: {
        object: {
          id: `int_${crypto.randomUUID().slice(0, 12)}`,
          amount: (amount / 100).toFixed(2),
          currency,
          merchant_order_id: paymentId,
          metadata: { payment_id: paymentId, organization_id: org },
        },
      },
    });
  }
  throw new Error(`unknown rail ${rail}`);
}

/** Posts a signed event to the versioned intake; answers {status, json}. */
export async function postEvent({ merchantId, versionId }, rail, body, secret, now) {
  const headers = await sign(rail, secret, body, now);
  const res = await fetch(
    `${BASE}/api/v1/webhooks/merchants/${merchantId}/versions/${versionId}`,
    {
      method: "POST",
      headers: { "content-type": "application/json", ...headers },
      body,
      redirect: "manual",
    },
  );
  const text = await res.text();
  let json = null;
  try {
    json = text ? JSON.parse(text) : null;
  } catch {
    json = text;
  }
  return { status: res.status, json };
}

/** The business event-inbox read for one merchant (redacted). */
export async function inbox(session, merchantId) {
  const page = await call(`/api/v1/business/merchants/${merchantId}/events`, {
    session,
  });
  return page.json.data.events ?? [];
}
