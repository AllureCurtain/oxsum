import assert from "node:assert/strict";
import test from "node:test";
import { surfaceForContext } from "../src/lib/session-surface.ts";
import type { SessionContextInfo } from "../src/api/hooks";

const user = { id: "u_1", email: "revoked@example.test", emailVerified: true } as const;
const staff = {
  businessId: "b_1",
  userId: "u_1",
  role: "finance",
  capabilities: [],
} as unknown as SessionContextInfo["business"];

const ctx = (
  context: "customer" | "business",
  organizationId: string | null,
  business: SessionContextInfo["business"],
): SessionContextInfo => ({ context, user, organizationId, business });

test("a live customer membership lands on the customer surface", () => {
  assert.equal(surfaceForContext(ctx("customer", "o_1", null)), "customer");
});

test("a session already in business context lands on the staff surface", () => {
  assert.equal(surfaceForContext(ctx("business", null, staff)), "business");
});

test("a staff seat with no remaining customer membership still lands on the staff surface", () => {
  // The schema answers `context: "customer"` here too — the staff seat is the
  // only authority left, so the shell must not bounce it to the customer side.
  assert.equal(surfaceForContext(ctx("customer", null, staff)), "business");
});

test("a session holding neither membership nor staff seat is revoked, not customer", () => {
  // Issue #1072: folding this arm into "customer" sent it to `GET /session`,
  // which 401s and bounces back to /login — the dead-end loop.
  assert.equal(surfaceForContext(ctx("customer", null, null)), "revoked");
  assert.equal(surfaceForContext(ctx("business", null, null)), "business");
});
