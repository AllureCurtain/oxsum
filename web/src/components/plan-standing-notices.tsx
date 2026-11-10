import { Link } from "react-router-dom";
import { useTranslation } from "react-i18next";
import type { OrgPlanView } from "../api/hooks/plans";
import { planStandingNotices } from "../lib/plan-standing";
import { Banner } from "./ui";

/** Keep manual renewal and repayment guidance tied to their own facts. */
export function PlanStandingNotices({ view, page }: { view: OrgPlanView | undefined; page: "plans" | "billing" }) {
  const { t } = useTranslation();
  const { renewal, overdueDebt } = planStandingNotices(view);
  return <>
    {renewal ? <Banner tone="warn">
      {t("planStanding.renewal")}
      {page === "billing" ? <> <Link className="underline" to="/plans">{t("planStanding.reviewRenewal")}</Link></> : null}
    </Banner> : null}
    {overdueDebt ? <Banner tone="warn">
      {t("planStanding.overdueDebt")}
      {page === "plans" ? <> <Link className="underline" to="/billing">{t("planStanding.reviewDebt")}</Link></> : null}
    </Banner> : null}
  </>;
}

/** Explain the ambiguous pastDue lifecycle label without claiming absent debt. */
export function PlanPastDueLabel({ view }: { view: OrgPlanView | undefined }) {
  const { t } = useTranslation();
  const { renewal, overdueDebt } = planStandingNotices(view);
  return <>{t(renewal ? overdueDebt ? "planStanding.renewalAndDebt" : "planStanding.renewalDue" : overdueDebt ? "planStanding.debtDue" : "billing.pastDue")}</>;
}
