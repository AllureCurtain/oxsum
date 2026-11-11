import assert from "node:assert/strict";
import test from "node:test";
import { planStandingNotices } from "../src/lib/plan-standing.ts";
import type { components } from "../src/api/schema";

type View = components["schemas"]["OrgPlanView"];
const now = Date.parse("2026-11-11T12:00:00Z");
function view(overrides: Partial<View> = {}): View {
  return {
    plan: { state: "pastDue", cancelAtPeriodEnd: false },
    periods: [{ periodIndex: 0, periodEnd: "2026-11-10T00:00:00Z", state: "expired" }],
    credit: { outstandingMinor: 0, nextDueAt: null, creditLimitMinor: null },
    ...overrides,
  } as View;
}
test("an unpaid renewal with no debt never asks for statement repayment", () => {
  assert.deepEqual(planStandingNotices(view(), now), { renewal: true, overdueDebt: false });
});
test("current paid entitlement and overdue debt remain independent", () => {
  const v = view();
  v.periods.push({ ...v.periods[0], periodIndex: 1, periodEnd: "2026-12-10T00:00:00Z", state: "active" });
  v.credit = { outstandingMinor: 1, nextDueAt: "2026-11-11T11:59:59Z", creditLimitMinor: null };
  assert.deepEqual(planStandingNotices(v, now), { renewal: false, overdueDebt: true });
  v.periods.reverse();
  assert.deepEqual(planStandingNotices(v, now), { renewal: false, overdueDebt: true });
});
test("renewal and overdue debt may both require action; future debt is not overdue", () => {
  const v = view();
  v.credit = { outstandingMinor: 2_000_003, nextDueAt: "2026-11-11T12:00:00Z", creditLimitMinor: null };
  assert.deepEqual(planStandingNotices(v, now), { renewal: true, overdueDebt: true });
  v.credit.nextDueAt = "2026-11-12T00:00:00Z";
  assert.deepEqual(planStandingNotices(v, now), { renewal: true, overdueDebt: false });
});
test("cancelled, unbound and never-activated plans are not renewal instructions", () => {
  assert.deepEqual(planStandingNotices(undefined, now), { renewal: false, overdueDebt: false });
  assert.deepEqual(planStandingNotices(view({ periods: [] }), now), { renewal: false, overdueDebt: false });
  const v = view(); v.plan!.cancelAtPeriodEnd = true;
  assert.deepEqual(planStandingNotices(v, now), { renewal: false, overdueDebt: false });
});
