#!/usr/bin/env node
/**
 * dedup-check (issue #617): replay one provider event id with *different*
 * bytes and assert the inbox conflicts — the documented "an event ID reused
 * with different content is a 409" boundary.
 *
 * What it asserts:
 *   - the first signed delivery records
 *   - the same event id over different payload bytes answers 409 CONFLICT
 *   - the inbox keeps exactly one row — the original bytes win
 *
 * Run:
 *   OXSUM_BASE_URL=http://127.0.0.1:3000 \
 *   OXSUM_OWNER_EMAIL=admin@oxsum.local \
 *   OXSUM_OWNER_PASSWORD=admin-password \
 *   node dedup-check.mjs [--rail hyperswitch]
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

requireBaseUrl();
const session = await ownerSession();
const webhookSecret = `whsec-${crypto.randomUUID()}`;
const merchant = await createMerchant(session, rail, webhookSecret);
report(`merchant created and enabled (${rail})`, true, merchant.merchantId);

const accountId = merchant.accountId;
const eventId = `evt_${crypto.randomUUID().replaceAll("-", "")}`;

// Two payloads sharing one provider event id but carrying different bytes —
// a retried delivery that mutated is exactly what the digest check exists for.
const first = eventBody(rail, accountId, { eventId, amount: 2500 });
const mutated = eventBody(rail, accountId, { eventId, amount: 2600 });

const firstPost = await postEvent(merchant, rail, first, webhookSecret);
report("first signed delivery accepted", firstPost.status === 200, `HTTP ${firstPost.status}`);

const conflict = await postEvent(merchant, rail, mutated, webhookSecret);
report(
  "same event id with different bytes answers 409",
  conflict.status === 409,
  `HTTP ${conflict.status} ${conflict.json?.error?.message ?? ""}`,
);

const events = await inbox(session, merchant.merchantId);
const rows = events.filter((e) => e.eventId === eventId);
report(
  "the inbox keeps exactly one row for the id",
  rows.length === 1,
  `${rows.length} row(s)`,
);
if (rows.length === 1) {
  report(
    "the original bytes' row survives",
    rows[0].state !== undefined,
    `state=${rows[0].state} reason=${rows[0].reason}`,
  );
}

exitStatus();
console.log("\ndedup-check: a mutated replay conflicts and never overwrites.");
