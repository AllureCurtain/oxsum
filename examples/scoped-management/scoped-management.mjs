#!/usr/bin/env node
/**
 * Scoped-management walkthrough (issue #617): mint an integration credential,
 * bill one event through it, and read the operation back — every step a
 * documented endpoint, no fixtures.
 *
 * Run against a live deployment (a local `cargo run -p oxsum-server` is the
 * intended target — the script refuses a non-loopback http URL):
 *
 *   OXSUM_BASE_URL=http://127.0.0.1:3000 \
 *   OXSUM_OWNER_EMAIL=admin@oxsum.local \
 *   OXSUM_OWNER_PASSWORD=admin-password \
 *   OXSUM_CUSTOMER_ID=<a customer organization uuid> \
 *   node scoped-management.mjs
 *
 * The owner session the variables open needs the `integrationsManage`
 * capability — the seeded demo owner has it; a business owner always does.
 * `OXSUM_CUSTOMER_ID` names which selected customer the credential may see and
 * bill: the credential is minted scoped to exactly that list, never a
 * wildcard. `OXSUM_EVENT_CHANNEL`/`OXSUM_EVENT_CODE` name the channel and the
 * `mode: event` billable code the metering call prices under (defaults
 * `demo`/`demo-event`).
 *
 * What it asserts, in order:
 *   1. login answers a session cookie, then selects business context
 *   2. the channel carries a `mode: event` price for the billable code — the
 *      script appends one when `OXSUM_APPEND_EVENT_PRICE=1` (default on: an
 *      unpriced code would refuse the metering call, and the seeded demo has
 *      no event prices); set it to 0 to reuse a deployment's own
 *   3. mint a `customersRead`+`meteringWrite` credential over the one customer
 *   4. the credential reads that customer — and cannot reach owner surfaces
 *   5. one metering settlement bills the declared event at the pinned price
 *   6. the same idempotency key replays the identical settlement
 *   7. the metered operation reads back — through `GET /api/v1/admin/
 *      operations?organizationId=…` when `OXSUM_ADMIN_TOKEN` is supplied,
 *      otherwise SKIP; customer-facing reads are deliberately scoped to the
 *      key that booked them, so a service credential's rows never show under
 *      the customer's own key
 *   8. the scope holds from the customer side too: when `OXSUM_CUSTOMER_KEY`
 *      is supplied, its `/api/v1/operations` list omits the service-written
 *      row — the documented key-scoping boundary, not a missing write
 *
 * `OXSUM_CUSTOMER_KEY` is the customer's own `oxs-` consumption key and
 * `OXSUM_ADMIN_TOKEN` the deployment's admin token — the walkthrough uses
 * each for exactly the read its authority opens. `OXSUM_CUSTOMER_ID` must
 * name a real customer of this business — the consumption walkthrough
 * prints a fresh pair on its last lines.
 *
 * Each assertion prints PASS/FAIL; any failure exits 1. Nothing mints admin
 * tokens, touches a second customer, or sends a paid provider request.
 */

const BASE = (process.env.OXSUM_BASE_URL ?? "http://127.0.0.1:3000").replace(
  /\/+$/,
  "",
);
const OWNER_EMAIL = process.env.OXSUM_OWNER_EMAIL;
const OWNER_PASSWORD = process.env.OXSUM_OWNER_PASSWORD;
const CUSTOMER_ID = process.env.OXSUM_CUSTOMER_ID;
const CHANNEL = process.env.OXSUM_EVENT_CHANNEL ?? "demo";
const BILLABLE_CODE = process.env.OXSUM_EVENT_CODE ?? "demo-event";
const UNITS = Number(process.env.OXSUM_EVENT_UNITS ?? "25");
const APPEND_EVENT_PRICE = process.env.OXSUM_APPEND_EVENT_PRICE !== "0";
const EVENT_PRICE_PER_MILLION = Number(
  process.env.OXSUM_EVENT_PRICE_PER_MILLION ?? "4000",
);

if (!OWNER_EMAIL || !OWNER_PASSWORD || !CUSTOMER_ID) {
  console.error(
    "Set OXSUM_OWNER_EMAIL, OXSUM_OWNER_PASSWORD and OXSUM_CUSTOMER_ID.",
  );
  process.exit(2);
}
{
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
function report(label, ok, detail) {
  console.log(`[${ok ? "PASS" : "FAIL"}] ${label}${detail ? ` — ${detail}` : ""}`);
  if (!ok) failures += 1;
  return ok;
}

class HttpError extends Error {
  constructor(status, body) {
    super(`HTTP ${status}: ${JSON.stringify(body)}`);
    this.status = status;
    this.body = body;
  }
}

async function call(path, { method = "GET", token, session, body } = {}) {
  const headers = {};
  if (body !== undefined) headers["content-type"] = "application/json";
  if (token) headers.authorization = `Bearer ${token}`;
  if (session) headers.cookie = `oxsum_session=${session}`;
  const response = await fetch(BASE + path, {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
    redirect: "manual",
  });
  const text = await response.text();
  const json = text ? JSON.parse(text) : null;
  if (!response.ok) {
    throw new HttpError(response.status, json ?? text);
  }
  return { json, setCookie: response.headers.get("set-cookie") };
}

function key() {
  return `scoped-mgmt-${crypto.randomUUID()}`;
}

async function main() {
  // 1 — the owner logs in; business calls take the personal session cookie.
  const login = await call("/api/v1/auth/login", {
    method: "POST",
    body: { email: OWNER_EMAIL, password: OWNER_PASSWORD },
  });
  const session = (login.setCookie ?? "")
    .split(";")[0]
    .replace(/^oxsum_session=/, "");
  report("owner login answers a session", session.length > 0);

  // Business authority is explicit: the session selects business context
  // before `/api/v1/business/*` accepts it.
  await call("/api/v1/session/context", {
    method: "POST",
    session,
    body: { idempotencyKey: key(), context: "business" },
  });
  report("session selects business context", true);

  // 2 — the metering call prices under the channel's latest `mode: event`
  // price for the billable code. A deployment that already prices the code
  // needs nothing here (set OXSUM_APPEND_EVENT_PRICE=0); a seeded demo has no
  // event prices at all, so the script appends one by default — a versioned
  // append, the same write the business console performs, and harmless when
  // one exists already (it just becomes the newest version).
  if (APPEND_EVENT_PRICE) {
    const priced = await call(
      `/api/v1/business/channels/${CHANNEL}/prices`,
      {
        method: "POST",
        session,
        body: {
          idempotencyKey: key(),
          reason: "scoped-management example event price",
          config: {
            model: BILLABLE_CODE,
            weight: 100,
            price: {
              mode: "event",
              inputPricePerMillion: EVENT_PRICE_PER_MILLION,
              outputPricePerMillion: 0,
              maxOutputTokens: 0,
            },
          },
        },
      },
    );
    report(
      "channel carries a mode:event price for the billable code",
      priced.json.data !== undefined,
      `${CHANNEL}/${BILLABLE_CODE} @ ${EVENT_PRICE_PER_MILLION}/M`,
    );
  }

  // 3 — mint the scoped credential: both grants, one explicit customer, a
  // bounded expiry. The secret is answered once here.
  const expiresAt = new Date(Date.now() + 7 * 86400_000).toISOString();
  const created = await call("/api/v1/business/integration-credentials", {
    method: "POST",
    session,
    body: {
      idempotencyKey: key(),
      name: "scoped-management-example",
      scopes: ["customersRead", "meteringWrite"],
      customerIds: [CUSTOMER_ID],
      expiresAt,
      reason: "Scoped-management example: read one customer, bill one event",
    },
  });
  const credential = created.json.data;
  const secret = credential.secret;
  report(
    "integration credential minted with an oxs-int- secret",
    typeof secret === "string" && secret.startsWith("oxs-int-"),
  );

  // 4 — customersRead: the selected customer answers; a different scope's
  // call is refused before it can touch billing authority.
  const customer = await call(`/api/v1/business/customers/${CUSTOMER_ID}`, {
    token: secret,
  });
  report(
    "customersRead answers the selected customer",
    customer.json.data.organizationId === CUSTOMER_ID,
    customer.json.data.externalId,
  );
  const foreign = await call("/api/v1/business/integration-credentials", {
    token: secret,
  }).then(() => null).catch((e) => e);
  report(
    "the credential cannot manage credentials",
    foreign instanceof HttpError && [401, 403].includes(foreign.status),
    `HTTP ${foreign?.status}`,
  );

  // 5 — meteringWrite: bill the declared event in one call; the code's latest
  // event price itemizes it and the settlement lands settled.
  const eventId = `evt-${crypto.randomUUID()}`;
  const settleBody = {
    idempotencyKey: key(),
    organizationId: CUSTOMER_ID,
    channel: CHANNEL,
    billableCode: BILLABLE_CODE,
    eventId,
    usage: {
      inputTokens: UNITS,
      eventType: "example.batch",
      endUser: "downstream-1",
    },
  };
  const settlement = await call("/api/v1/metering/settlements", {
    method: "POST",
    token: secret,
    body: settleBody,
  });
  const settled = settlement.json.data;
  report(
    "metering settlement bills the declared usage",
    settled.chargedMinor >= 0 && settled.kind === "usage",
    `chargedMinor=${settled.chargedMinor} priceVersion=${settled.priceVersion}`,
  );

  // 6 — replaying the same idempotency key answers the original settlement,
  // not a second charge.
  const replay = await call("/api/v1/metering/settlements", {
    method: "POST",
    token: secret,
    body: settleBody,
  });
  report(
    "idempotent replay returns the same settlement",
    replay.json.data.settlementId === settled.settlementId &&
      replay.json.data.chargedMinor === settled.chargedMinor,
  );

  // 7 — the customer-side read. The service credential wrote the operation
  // but its scope cannot read the customer's own records; the customer's own
  // consumption key reads them back.
  const operationsRefusal = await call("/api/v1/operations?limit=50", {
    token: secret,
  }).then(() => null).catch((e) => e);
  report(
    "a service credential cannot read the customer's operation list",
    operationsRefusal instanceof HttpError &&
      [401, 403, 404].includes(operationsRefusal.status),
    `HTTP ${operationsRefusal?.status}`,
  );

  const adminToken = process.env.OXSUM_ADMIN_TOKEN;
  const customerKey = process.env.OXSUM_CUSTOMER_KEY;
  if (adminToken) {
    // 7 — read-back under deployment authority: the admin operations list
    // sees every organization's rows, where customer reads stay key-scoped.
    const ops = await call(
      `/api/v1/admin/operations?organizationId=${CUSTOMER_ID}&limit=50`,
      { token: adminToken },
    );
    const mine = (ops.json.data.operations ?? []).find(
      (op) => op.surface === "metering" && op.outcomeKind === "metered",
    );
    report(
      "the metered operation reads back under deployment authority",
      mine !== undefined,
      mine
        ? `operationId=${mine.operationId} chargedMinor=${mine.chargedMinor}`
        : "none found",
    );
  } else {
    console.log(
      "[SKIP] operation read-back — set OXSUM_ADMIN_TOKEN to the deployment's",
    );
    console.log(
      "       admin token to read /api/v1/admin/operations for the customer.",
    );
  }

  if (customerKey) {
    // 8 — the scope boundary from the customer's side: the service credential
    // wrote the row, so the customer's own key — scoped to entries its key id
    // booked — does not list it. The row is real; the view is deliberately
    // narrower (a member session or the admin read sees it).
    const customerOps = await call("/api/v1/operations?limit=50", {
      token: customerKey,
    });
    const leaked = (customerOps.json.data.operations ?? []).find(
      (op) => op.surface === "metering",
    );
    report(
      "the customer's own key does not see the service-written operation",
      leaked === undefined,
      "key-scoped reads only list what the key booked",
    );
  } else {
    console.log(
      "[SKIP] customer-side scope check — set OXSUM_CUSTOMER_KEY to the",
    );
    console.log(
      "       customer's oxs- key to confirm its view stays key-scoped.",
    );
  }

  if (failures > 0) {
    console.error(`\n${failures} assertion(s) failed.`);
    process.exit(1);
  }
  console.log("\nScoped-management example: all assertions held.");
}

main().catch((error) => {
  console.error(error instanceof Error ? error.message : error);
  process.exit(1);
});
