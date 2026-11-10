import type { components } from "../api/schema";

type PlanView = components["schemas"]["OrgPlanView"];

/** A plan's lifecycle and its debt are independent reasons to take action. */
export function planStandingNotices(view: PlanView | undefined, now = Date.now()) {
  const plan = view?.plan;
  if (!plan || plan.state === "cancelled" || plan.state === "expired") {
    return { renewal: false, overdueDebt: false };
  }
  const latest = view.periods.reduce<(typeof view.periods)[number] | undefined>(
    (last, period) => !last || period.periodIndex > last.periodIndex ? period : last,
    undefined,
  );
  const credit = view.credit;
  return {
    renewal: !plan.cancelAtPeriodEnd && latest != null && Date.parse(latest.periodEnd) <= now,
    overdueDebt: credit != null && credit.outstandingMinor > 0 &&
      credit.nextDueAt != null && Date.parse(credit.nextDueAt) <= now,
  };
}
