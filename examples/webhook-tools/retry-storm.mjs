#!/usr/bin/env node
/**
 * retry-storm (issue #617): fire N concurrent duplicate signed callbacks and
 * assert the intake single-credits — one durable row, every answer 200,
 * no double-apply.
 *
 * What it asserts:
 *   - N concurrent deliveries of the identical signed event all answer 200
 *   - the merchant's inbox holds exactly one row for that event id
 *   - a second storm under a *new* event id is processed independently
 *
 * Run:
 *   OXSUM_BASE_URL=http://127.0.0.1:3000 \
 *   OXSUM_OWNER_EMAIL=admin@oxsum.local \
 *   OXSUM_OWNER_PASSWORD=admin-password \
 *   node retry-storm.mjs [--rail hyperswitch] [--n 16]
 */

import {
  createMerchant,
  eventBody,
  exitStatus,
  inbox,
  ownerSession,
  postEvent,
  report,
  requireBaseUrl,
} from "./lib.mjs";

const rail = process.argv.find((a) => a.startsWith("--rail="))?.split("=")[1]
  ?? process.env.OXSUM_RAIL ?? "hyperswitch";
const N = Number(
  process.argv.find((a) => a.startsWith("--n="))?.split("=")[1]
    ?? process.env.OXSUM_STORM_N ?? "16",
);

requireBaseUrl();
const session = await ownerSession();
const webhookSecret = `whsec-${crypto.randomUUID()}`;
const merchant = await createMerchant(session, rail, webhookSecret);
report(`merchant created and enabled (${rail})`, true, merchant.merchantId);

const accountId = merchant.accountId;

async function storm(eventId, label) {
  const body = eventBody(rail, accountId, { eventId });
  const results = await Promise.all(
    Array.from({ length: N }, () => postEvent(merchant, rail, body, webhookSecret)),
  );
  const ok = results.filter((r) => r.status === 200).length;
  report(
    `${label}: ${N} concurrent signed deliveries all answer 200`,
    ok === N,
    `${ok}/${N} ok`,
  );
  const events = await inbox(session, merchant.merchantId);
  const rows = events.filter((e) => e.eventId === eventId);
  report(
    `${label}: the inbox holds exactly one row`,
    rows.length === 1,
    `${rows.length} row(s)`,
  );
  return rows[0];
}

const a = await storm(`evt_${crypto.randomUUID().replaceAll("-", "")}`, "storm A");
await storm(`evt_${crypto.randomUUID().replaceAll("-", "")}`, "storm B");

// The rows the merchant list returns are redacted: no payload, no secret —
// assert the normalized fields are the only things served.
const events = await inbox(session, merchant.merchantId);
const serialized = JSON.stringify(events);
report(
  "inbox rows are redacted (no webhook secret or payload bytes)",
  !serialized.includes(webhookSecret) && !serialized.includes("metadata"),
  `${events.length} row(s) inspected`,
);

exitStatus();
console.log(`\nretry-storm: ${N} concurrent duplicates → one durable row, once applied.`);
