import assert from "node:assert/strict";
import test from "node:test";
import {
  solanaCountdown,
  solanaExpired,
  solanaStatus,
  solanaUrgent,
  SOLANA_URGENT_SECS,
} from "../src/lib/solanaStatus.ts";

// Issue #1148: every confirm answer reads as one status line — the 404 the
// poll-race or an in-flight settle produces is informational, never an error.

test("403 reads as still waiting for the on-chain payment", () => {
  const line = solanaStatus({ status: 403, code: "FORBIDDEN" });
  assert.equal(line.phase, "waiting");
  assert.equal(line.tone, "info");
});

test("404 reads as settling — informational, not an error", () => {
  const line = solanaStatus({ status: 404, code: "NOT_FOUND" });
  assert.equal(line.phase, "settling");
  assert.equal(line.tone, "info");
});

test("a receipt (200) reads as credited", () => {
  const line = solanaStatus({ status: 200 });
  assert.equal(line.phase, "credited");
  assert.equal(line.tone, "success");
});

test("409 names the exception its code or message carries", () => {
  // #1142's route words: "underpaid the intent" / "landed after the intent
  // expired" — the mapping matches the word, not the sentence.
  assert.equal(
    solanaStatus({
      status: 409,
      code: "CONFLICT",
      message: "the paying transfer underpaid the intent; the deposit is queued for review",
    }).phase,
    "underpaid",
  );
  assert.equal(
    solanaStatus({
      status: 409,
      code: "CONFLICT",
      message: "the paying transaction landed after the intent expired; the deposit is queued for review",
    }).phase,
    "late",
  );
  // A code that names it directly maps without reading the message.
  assert.equal(solanaStatus({ status: 409, code: "SOLANA_UNDERPAID" }).phase, "underpaid");
  // #1184's flagged confirm names the hold in its message — the parked
  // deposit reads held-for-review, never the "settling" a 404 would pretend.
  const flagged = solanaStatus({
    status: 409,
    code: "CONFLICT",
    message: "the paying transfer was flagged by screening; the deposit is held for review",
  });
  assert.equal(flagged.phase, "flagged");
  assert.equal(flagged.tone, "warn");
  assert.equal(flagged.key, "topup.solanaStatusFlagged");
  // An unnamed 409 still surfaces as an exception, never as waiting.
  const unknown = solanaStatus({ status: 409, code: "CONFLICT", message: "no" });
  assert.equal(unknown.phase, "exception");
  assert.equal(unknown.tone, "error");
});

test("a queue row's literal status maps the same as the wire answer", () => {
  // The seam to #1142's deposits list: the row's own `status` word drops in
  // as `code` and lands the same line the 409 would.
  assert.equal(solanaStatus({ code: "underpaid" }).phase, "underpaid");
  assert.equal(solanaStatus({ code: "late" }).phase, "late");
  assert.equal(solanaStatus({ code: "overpaid" }).phase, "overpaid");
  assert.equal(solanaStatus({ code: "flagged" }).phase, "flagged");
});

test("an expired intent reads as resettable, outranking a stale error", () => {
  const line = solanaStatus({ status: 403, expired: true });
  assert.equal(line.phase, "expired");
  assert.equal(line.tone, "warn");
});

test("an unexpected failure surfaces as an error, not as waiting", () => {
  const line = solanaStatus({ status: 500 });
  assert.equal(line.phase, "exception");
  assert.equal(line.tone, "error");
});

test("the countdown writes m:ss clamped at zero", () => {
  const expiry = new Date(1_000_000 + 65_000).toISOString();
  assert.equal(solanaCountdown(expiry, 1_000_000), "1:05");
  assert.equal(solanaCountdown(expiry, 1_000_000 + 5_000), "1:00");
  assert.equal(solanaCountdown(expiry, 1_000_000 + 65_000), "0:00");
  // Past the deadline the clock shows 0:00 — never a negative.
  assert.equal(solanaCountdown(expiry, 1_000_000 + 90_000), "0:00");
  // A timestamp that cannot parse gives no countdown at all.
  assert.equal(solanaCountdown("not a date", 1_000_000), null);
});

test("expiry and urgency judge the same instant the countdown shows", () => {
  const expiry = new Date(1_000_000 + SOLANA_URGENT_SECS * 1000).toISOString();
  assert.equal(solanaExpired(expiry, 1_000_000), false);
  assert.equal(solanaUrgent(expiry, 1_000_000), true);
  assert.equal(solanaUrgent(expiry, 1_000_000 - 1_000), false);
  assert.equal(solanaExpired(expiry, 1_000_000 + SOLANA_URGENT_SECS * 1000), true);
  // Once expired the countdown is not "urgent" — it is done.
  assert.equal(solanaUrgent(expiry, 1_000_000 + SOLANA_URGENT_SECS * 1000 + 1), false);
});
