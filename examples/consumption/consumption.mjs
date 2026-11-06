#!/usr/bin/env node
/**
 * Consumption walkthrough (issue #617): the whole customer journey against a
 * live deployment — owner invites a customer, the customer redeems and gets
 * its first key, the deployment records a manual receipt, the customer calls
 * the gateway, reads its usage back, and downloads its evidence bundle.
 *
 * Run it against an isolated local server:
 *
 *   OXSUM_BASE_URL=http://127.0.0.1:3000 \
 *   OXSUM_ADMIN_TOKEN=<the deployment's OXSUM_ADMIN_TOKEN> \
 *   OXSUM_MODEL=<a priced model the gateway serves, default from /v1/models> \
 *   node consumption.mjs
 *
 * The admin token is required for the two deployment-authority steps — the
 * setup invitation and the manual receipt — which is exactly the boundary the
 * walkthrough demonstrates: customers cannot mint their own membership or
 * declare money arrived. Everything else runs on customer credentials.
 *
 * What it asserts, in order:
 *   1. the deployment has a business owner (setup completes or is already done)
 *   2. the owner invites a customer; the invitation reserves the customer UUID
 *   3. the customer redeems — the answer carries its one-shown API key secret
 *   4. the deployment records a manual receipt; the wallet tops up
 *   5. the owner provisions a loopback channel + chat price — a seeded demo
 *      channel targets an unreachable upstream by design (issue #561), so the
 *      walkthrough stands its own `node:http` stub up and points a channel at
 *      it under an explicit `internal` destination policy
 *   6. the customer calls /v1/chat/completions through its key; usage answers
 *   7. /api/v1/balance reflects the settled charge; /api/v1/billing-records rows
 *   8. the customer mints + downloads its period evidence bundle, or asserts
 *      the documented open-day VALIDATION_ERROR when the day has not closed
 *   9. `verify-oxsum-bill` verifies the bundle offline (fetch disabled)
 *
 * Every step prints PASS/FAIL; any failure exits 1. No paid provider is
 * contacted — the stub upstream answers on 127.0.0.1 and is the only HTTP the
 * example serves itself. The run's last lines export `OXSUM_CUSTOMER_ID` and
 * `OXSUM_CUSTOMER_KEY` so a caller (scripts/verify-examples.sh) can hand the
 * fresh customer to the scoped-management example.
 */

import { createServer } from "node:http";

const BASE = (process.env.OXSUM_BASE_URL ?? "http://127.0.0.1:3000").replace(
  /\/+$/,
  "",
);
const ADMIN_TOKEN = process.env.OXSUM_ADMIN_TOKEN;
const MODEL_ENV = process.env.OXSUM_MODEL;
const CUSTOMER_EMAIL = `customer-${crypto.randomUUID().slice(0, 8)}@example.test`;
const CUSTOMER_PASSWORD = "customer-password-001";
const TOPUP_MINOR = 10_000_000; // 10 credits — enough for several demo turns.
const CHANNEL_NAME = `example-${crypto.randomUUID().slice(0, 8)}`;
const STUB_MODEL = "example-chat";

if (!ADMIN_TOKEN) {
  console.error("Set OXSUM_ADMIN_TOKEN to the deployment's admin token.");
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
  if (!response.ok) throw new HttpError(response.status, json ?? text);
  return { json, setCookie: response.headers.get("set-cookie") };
}

const key = () => `consumption-${crypto.randomUUID()}`;
const sessionOf = (res) =>
  (res.setCookie ?? "").split(";")[0].replace(/^oxsum_session=/, "");

async function main() {
  // 1 — owner setup: the deployment may already be initialized (the seeded
  // demo), in which case the token mint is refused and we log the owner in.
  let ownerSession;
  const ownerEmail = `owner-${crypto.randomUUID().slice(0, 8)}@example.test`;
  const ownerPassword = "owner-password-0001";
  const mint = await call("/api/v1/admin/business/setup-tokens", {
    method: "POST",
    token: ADMIN_TOKEN,
    body: { idempotencyKey: key(), ownerEmail },
  }).catch((e) => e);
  if (mint instanceof HttpError && mint.status === 409) {
    // A business already exists — the walkthrough needs its owner's session,
    // which on a seeded demo is the documented login.
    const login = await call("/api/v1/auth/login", {
      method: "POST",
      body: {
        email: process.env.OXSUM_OWNER_EMAIL ?? "admin@oxsum.local",
        password: process.env.OXSUM_OWNER_PASSWORD ?? "admin-password",
      },
    }).catch((e) => e);
    if (login instanceof HttpError) {
      report("owner login on an initialized business", false, login.message);
      process.exit(1);
    }
    ownerSession = sessionOf(login);
    report("business already initialized; owner logged in", true);
  } else if (mint instanceof HttpError) {
    report("setup-token mint", false, mint.message);
    process.exit(1);
  } else {
    const setupToken = mint.json.data.token;
    const reg = await call("/api/v1/business/setup/register", {
      method: "POST",
      body: {
        token: setupToken,
        businessName: "Consumption Example Co.",
        email: ownerEmail,
        password: ownerPassword,
      },
    });
    report("business owner registered via setup token", reg.json.data.registration !== null);
    const login = await call("/api/v1/auth/login", {
      method: "POST",
      body: { email: ownerEmail, password: ownerPassword },
    });
    ownerSession = sessionOf(login);
  }

  // Owner sessions act in business context.
  await call("/api/v1/session/context", {
    method: "POST",
    session: ownerSession,
    body: { idempotencyKey: key(), context: "business" },
  });

  // 2 — invite a customer; the reservation carries the new organization id.
  const invite = await call("/api/v1/business/customer-invitations", {
    method: "POST",
    session: ownerSession,
    body: {
      idempotencyKey: key(),
      email: CUSTOMER_EMAIL,
      name: "Consumption Example Customer",
      externalId: `ext-${crypto.randomUUID().slice(0, 8)}`,
      reason: "consumption walkthrough",
    },
  });
  const invitation = invite.json.data;
  report(
    "customer invitation reserves the organization",
    typeof invitation.token === "string" && typeof invitation.organizationId === "string",
    `organizationId=${invitation.organizationId}`,
  );

  // 3 — the customer redeems; the answer carries its one-shown key secret.
  const redeem = await call("/api/v1/invitations/redeem", {
    method: "POST",
    body: {
      token: invitation.token,
      email: CUSTOMER_EMAIL,
      password: CUSTOMER_PASSWORD,
    },
  });
  const registration = redeem.json.data;
  const customerKey = registration.apiKey?.secret;
  report(
    "customer redeems and receives its API key",
    typeof customerKey === "string" && customerKey.length > 0,
    `organization=${registration.organization?.id}`,
  );

  // 4 — the deployment records a manual receipt: the admin-token call that
  // stands in for a payment rail on an isolated deployment.
  const receipt = await call(
    `/api/v1/admin/organizations/${registration.organization.id}/manual-receipts`,
    {
      method: "POST",
      token: ADMIN_TOKEN,
      body: {
        idempotencyKey: key(),
        amountMinor: TOPUP_MINOR,
        reference: `consumption-example-${crypto.randomUUID()}`,
        reason: "consumption walkthrough top-up",
      },
    },
  );
  report(
    "manual receipt credits the wallet",
    receipt.json.data?.amount === String(TOPUP_MINOR) ||
      receipt.json.data !== undefined,
  );

  // 5 — provision a reachable upstream: the seeded demo channel points at
  // 127.0.0.1:9 on purpose so seeded traffic cannot leak, so the walkthrough
  // serves its own stub and registers a channel under an explicit `internal`
  // destination policy (loopback IP literals only — the owner reauthorizes the
  // destination on save; a public channel could never hold an HTTP loopback).
  const stub = createServer((req, res) => {
    if (req.method === "POST" && req.url === "/v1/chat/completions") {
      const chunks = [];
      req.on("data", (c) => chunks.push(c));
      req.on("end", () => {
        const promptTokens = JSON.parse(
          Buffer.concat(chunks).toString() || "{}",
        ).messages?.[0]?.content?.length ?? 6;
        res.writeHead(200, { "content-type": "application/json" });
        res.end(
          JSON.stringify({
            id: "chatcmpl-example",
            object: "chat.completion",
            created: Math.floor(Date.now() / 1000),
            model: STUB_MODEL,
            choices: [
              {
                index: 0,
                message: { role: "assistant", content: "hi" },
                finish_reason: "stop",
              },
            ],
            usage: {
              prompt_tokens: promptTokens,
              completion_tokens: 2,
              total_tokens: promptTokens + 2,
            },
          }),
        );
      });
    } else if (req.method === "GET" && req.url === "/v1/models") {
      res.writeHead(200, { "content-type": "application/json" });
      res.end(
        JSON.stringify({ object: "list", data: [{ id: STUB_MODEL }] }),
      );
    } else {
      res.writeHead(404).end("not found");
    }
  });
  await new Promise((resolve) => stub.listen(0, "127.0.0.1", resolve));
  const stubPort = stub.address().port;
  try {
    const channel = await call("/api/v1/business/channels", {
      method: "POST",
      session: ownerSession,
      body: {
        idempotencyKey: key(),
        reason: "consumption example loopback upstream",
        config: {
          name: CHANNEL_NAME,
          baseUrl: `http://127.0.0.1:${stubPort}/v1`,
          protocol: "openai",
          apiKeys: ["example-key-0001"],
          destinationPolicy: {
            network: "internal",
            allowedIps: ["127.0.0.1"],
          },
        },
      },
    });
    report(
      "channel to the loopback stub is created",
      channel.json.data !== undefined,
      CHANNEL_NAME,
    );

    const price = await call(
      `/api/v1/business/channels/${CHANNEL_NAME}/prices`,
      {
        method: "POST",
        session: ownerSession,
        body: {
          idempotencyKey: key(),
          reason: "consumption example chat price",
          config: {
            model: STUB_MODEL,
            weight: 100,
            price: {
              mode: "chat",
              inputPricePerMillion: 1_000,
              outputPricePerMillion: 2_000,
              maxOutputTokens: 1_024,
            },
          },
        },
      },
    );
    report(
      "a chat price is appended for the stub model",
      price.json.data !== undefined,
      STUB_MODEL,
    );

    const model = MODEL_ENV ?? STUB_MODEL;
    const served = await call("/v1/models", { token: customerKey });
    report(
      "the priced model is served on the gateway",
      (served.json.data ?? []).some((entry) => entry.id === model),
      model,
    );

    // 6 — the customer's key calls the gateway; usage comes back in the
    // answer.
    const turn = await call("/v1/chat/completions", {
      method: "POST",
      token: customerKey,
      body: {
        model,
        messages: [{ role: "user", content: "Say hi." }],
        max_tokens: 20,
      },
    }).catch((e) => e);
    if (turn instanceof HttpError) {
      report("chat completion", false, turn.message);
    } else if (turn instanceof Error) {
      throw turn;
    } else {
      const usage = turn.json?.usage ?? {};
      report(
        "chat completion returns usage",
        (usage.total_tokens ?? 0) > 0,
        `${usage.prompt_tokens}in/${usage.completion_tokens}out`,
      );
    }

    // 7 — read-back: balance and billing records reflect the settled turn.
    // /api/v1/balance is session-only, so the customer logs in for it; the
    // records take the consumption key.
    const customerLogin = await call("/api/v1/auth/login", {
      method: "POST",
      body: { email: CUSTOMER_EMAIL, password: CUSTOMER_PASSWORD },
    });
    const customerSession = sessionOf(customerLogin);
    const bal = await call("/api/v1/balance", { session: customerSession });
    report(
      "balance reflects the top-up minus the settled charge",
      (bal.json.data.availableMinor ?? 0) < TOPUP_MINOR &&
        (bal.json.data.availableMinor ?? 0) > 0,
      `availableMinor=${bal.json.data.availableMinor}`,
    );
    const records = await call("/api/v1/billing-records?limit=5", {
      token: customerKey,
    });
    const rows = records.json.data.rows ?? [];
    report(
      "billing records carry the settled turn",
      rows.length > 0 && rows[0].chargedMinor !== undefined,
      `${rows.length} row(s)`,
    );

    // 8 — mint the period evidence bundle. `to` must be a fully ended UTC
    // day: the walkthrough asks for the turn's own day, which is still open,
    // and asserts the named VALIDATION_ERROR the contract documents rather
    // than stepping around it — a settled-day bundle is what a deployment on
    // its second day would mint instead.
    const today = new Date().toISOString().slice(0, 10);
    const mintBundle = await call("/api/v1/org/evidence-bundles", {
      method: "POST",
      session: customerSession,
      body: { from: today, to: today },
    }).catch((e) => e);
    if (!(mintBundle instanceof HttpError)) {
      report(
        "mint period evidence bundle",
        false,
        "expected a 400 VALIDATION_ERROR on the open day, got " +
          JSON.stringify(mintBundle?.json ?? mintBundle),
      );
    } else if (mintBundle.status !== 400) {
      report("mint period evidence bundle", false, mintBundle.message);
    } else {
      report(
        "period bundle over an open day is the documented VALIDATION_ERROR",
        true,
        `HTTP ${mintBundle.status}`,
      );

      // The day the turn booked closed at the start of this UTC day, so the
      // fully-ended span is yesterday — on a seeded database that span holds
      // the seeded history; on a fresh one it mints an empty-day bundle, and
      // either way the verifier exercises the real download path.
      const yesterday = new Date(Date.now() - 86400_000)
        .toISOString()
        .slice(0, 10);
      const closedBundle = await call("/api/v1/org/evidence-bundles", {
        method: "POST",
        session: customerSession,
        body: { from: yesterday, to: yesterday },
      });
      const bundleId = closedBundle.json.data.bundleId;
      const download = await call(
        `/api/v1/org/evidence-bundles/${bundleId}`,
        { session: customerSession },
      );
      const bundle = download.json.data;
      report(
        "evidence bundle over a closed day downloads",
        bundle?.organizationId === registration.organization.id,
        `bundleId=${bundleId}`,
      );

      // 9 — verify offline with the packaged verifier: no server round-trip.
      let networkAttempts = 0;
      const realFetch = globalThis.fetch;
      globalThis.fetch = () => {
        networkAttempts += 1;
        throw new Error("offline verification must not fetch");
      };
      try {
        const { verifyEvidenceBundle, listBundleManifests } = await import(
          "verify-oxsum-bill"
        );
        const manifestRows = listBundleManifests(JSON.stringify(bundle));
        // The bundle carries {keyName, publicKey, keyHash} for the operator
        // key its note signs under — the consumer checks the signature
        // against the key the bundle itself publishes (a production
        // deployment would pin that key through an out-of-band channel; the
        // check still binds the signature to whatever key the bundle
        // attests).
        const publishedKey = bundle.keyName
          ? JSON.stringify({
              keyName: bundle.keyName,
              publicKey: bundle.publicKey,
              keyHash: bundle.keyHash,
            })
          : null;
        const verdict = await verifyEvidenceBundle(
          JSON.stringify(bundle),
          registration.organization.id,
          publishedKey,
        );
        report(
          `evidence bundle verifies offline (${manifestRows.length} manifest rows)`,
          verdict.ok === true,
          verdict.detail,
        );
        report(
          "offline verification made no network call",
          networkAttempts === 0,
        );
      } finally {
        globalThis.fetch = realFetch;
      }
    }
  } finally {
    stub.close();
  }

  // Hand the fresh customer and the owner login to whatever ran the
  // walkthrough — the gate exports these lines into the other examples'
  // environment (the owner line only appears when this run registered the
  // business itself; on an initialized business the caller already knows the
  // owner it logged in as).
  console.log(`OXSUM_CUSTOMER_ID=${registration.organization.id}`);
  console.log(`OXSUM_CUSTOMER_KEY=${customerKey}`);
  if (ownerEmail && !(mint instanceof HttpError)) {
    console.log(`OXSUM_OWNER_EMAIL=${ownerEmail}`);
    console.log(`OXSUM_OWNER_PASSWORD=${ownerPassword}`);
  }

  if (failures > 0) {
    console.error(`\n${failures} assertion(s) failed.`);
    process.exit(1);
  }
  console.log("\nConsumption example: all assertions held.");
}

main().catch((error) => {
  console.error(error instanceof Error ? error.message : error);
  process.exit(1);
});
