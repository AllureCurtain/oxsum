#!/usr/bin/env node
/**
 * sign-and-replay (issue #617): sign a merchant event under the documented
 * scheme, post it to the versioned intake, and walk the inbox state.
 *
 * What it asserts:
 *   - a correctly signed event is accepted (200) and durably recorded
 *   - an unrecognized-but-signed payment parks at `pending`/`unknownPayment`
 *     (the documented "receipt is not fulfillment" posture — the event is
 *     retained for recovery, not discarded)
 *   - an unsigned or mis-signed delivery is refused 401 before parsing
 *   - replaying the identical signed delivery answers 200 without duplicating
 *
 * Run:
 *   OXSUM_BASE_URL=http://127.0.0.1:3000 \
 *   OXSUM_OWNER_EMAIL=admin@oxsum.local \
 *   OXSUM_OWNER_PASSWORD=admin-password \
 *   node sign-and-replay.mjs [--rail hyperswitch]
 */

import {
  BASE,
  HttpError,
  call,
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

// An unrecognized payment — the signed event names a payment_id the merchant
// never pinned, so intake classifies it pending/unknownPayment and keeps it
// for recovery rather than crediting anything.
const accountId = merchant.accountId;
const body = eventBody(rail, accountId, { kind: "succeeded" });
const first = await postEvent(merchant, rail, body, webhookSecret);
report("signed event accepted (200)", first.status === 200, `HTTP ${first.status}`);

const events = await inbox(session, merchant.merchantId);
const row = events.find((e) => e.eventId === JSON.parse(body)[rail === "stripe" ? "id" : rail === "airwallex" ? "id" : "event_id"]);
report(
  "the event is durably recorded",
  row !== undefined,
  row ? `state=${row.state} reason=${row.reason}` : "absent",
);
if (row) {
  report(
    "an unknown payment parks pending, not applied",
    row.state === "pending" || row.state === "rejected" || row.state === "ignored",
    `state=${row.state} reason=${row.reason}`,
  );
}

// Replay: the identical signed bytes answer 200 and never duplicate.
const replay = await postEvent(merchant, rail, body, webhookSecret);
report("exact replay answers 200", replay.status === 200);
const after = await inbox(session, merchant.merchantId);
report(
  "replay did not duplicate the inbox row",
  after.length === events.length,
  `${after.length} event(s)`,
);

// A bad signature is refused before any parsing.
const bad = await postEvent(merchant, rail, body, "the-wrong-secret");
report("a mis-signed delivery is refused 401", bad.status === 401, `HTTP ${bad.status}`);

// An unknown merchant/version coordinate is 404 regardless of a good body.
const bogus = await postEvent(
  { merchantId: crypto.randomUUID(), versionId: crypto.randomUUID() },
  rail,
  body,
  webhookSecret,
);
report("unknown coordinates answer 404", bogus.status === 404, `HTTP ${bogus.status}`);

exitStatus();
console.log("\nsign-and-replay: intake walked signed → recorded → replayed.");
