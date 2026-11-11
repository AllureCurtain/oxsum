import { PlanPastDueLabel, PlanStandingNotices } from "../components/plan-standing-notices";
import { useState } from "react";
import { Link, useParams } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { AlertCircle, CreditCard, FileText, Receipt } from "lucide-react";
import { StatementPayment } from "../components/statement-payment";
import {
  useDisplay,
  useOrgPlan,
  usePlanStatement,
  usePlanStatements,
  type PlanStatement,
} from "../api/hooks";
import {
  Badge,
  Banner,
  Button,
  Card,
  Cr,
  EmptyRows,
  Kpi,
  PageState,
  StatusBadge,
  TableShell,
  Td,
} from "../components/ui";

// ---------------------------------------------------------------------------
// /app/billing — the customer's billing surface (T23): the live plan and its
// credit standing up top, the plan statements a period close issues below,
// and exact statement repayment that routes the
// browser into the checkout the orders pages own (T16). The dunning state
// reads honest — `pastDue` is shown, never smoothed.
// ---------------------------------------------------------------------------

function statementBadge(state: string) {
  switch (state) {
    case "paid":
      return <Badge tone="success">paid</Badge>;
    case "pastDue":
      return <Badge tone="danger">past due</Badge>;
    case "issued":
      return <Badge tone="secondary">issued</Badge>;
    case "void":
      return <Badge tone="secondary">void</Badge>;
    default:
      return <Badge tone="secondary">{state}</Badge>;
  }
}

/** The plan + credit-standing header card: what the customer owes, against
 * what limit, and the earliest open statement's due date. */
function PlanStanding() {
  const { t } = useTranslation();
  const orgPlan = useOrgPlan();
  const display = useDisplay();

  if (orgPlan.isLoading) {
    return <PageState>{t("common.loading")}</PageState>;
  }
  if (orgPlan.isError) {
    return <Banner tone="error">{t("billing.loadError")}</Banner>;
  }
  const plan = orgPlan.data?.plan;
  const credit = orgPlan.data?.credit;
  if (!plan) {
    return (
      <Card title={t("billing.plan")}>
        <p className="text-sm text-muted">{t("billing.noPlan")}</p>
        <p className="mt-2">
          <Link to="/plans" className="text-sm font-medium text-primary">
            {t("billing.browsePlans")} →
          </Link>
        </p>
      </Card>
    );
  }

  return (
    <Card title={t("billing.plan")}>
      <div className="flex flex-col gap-4">
        {credit && credit.outstandingMinor > 0 ? <p className="text-sm text-ink-secondary">{t("statementPayment.selectStatement")}</p> : null}
        <PlanStandingNotices view={orgPlan.data} page="billing" />
        {plan.cancelAtPeriodEnd ? (
          <Banner tone="info">{t("billing.cancelAtPeriodEnd")}</Banner>
        ) : null}
        <div className="grid grid-cols-2 gap-4 sm:grid-cols-4">
          <Kpi
            label={t("billing.creditLimit")}
            icon={<CreditCard />}
            value={
              credit?.creditLimitMinor != null ? (
                <Cr minor={credit.creditLimitMinor} currency={display} />
              ) : (
                t("billing.unlimited")
              )
            }
          />
          <Kpi
            label={t("billing.outstanding")}
            icon={<AlertCircle />}
            value={<Cr minor={credit?.outstandingMinor ?? 0} currency={display} />}
          />
          <Kpi
            label={t("billing.nextDue")}
            icon={<FileText />}
            value={credit?.nextDueAt?.slice(0, 10) ?? "—"}
          />
          <Kpi
            label={t("billing.state")}
            icon={<Receipt />}
            value={plan.state === "pastDue" ? <PlanPastDueLabel view={orgPlan.data} /> : plan.state}
          />
        </div>

      </div>
    </Card>
  );
}

/** The statements list — newest first, each row linking its detail. */
function StatementRows() {
  const { t } = useTranslation();
  const display = useDisplay();
  const [cursorStack, setCursorStack] = useState<string[]>([]);
  const page = usePlanStatements(cursorStack[cursorStack.length - 1]);
  const rows = page.data?.statements ?? [];
  const nextCursor = page.data?.nextCursor ?? null;

  if (page.isError) {
    return <Banner tone="error">{t("billing.listError")}</Banner>;
  }
  return (
    <>
      <TableShell
        className="[&_th]:whitespace-nowrap [&_td]:whitespace-nowrap"
        head={[
          t("billing.colStatement"),
          t("common.status"),
          { label: t("common.total"), align: "right" },
          { label: t("billing.colPaid"), align: "right" },
          { label: t("billing.colOwed"), align: "right" },
          t("billing.colDue"),
          "",
        ]}
        loading={page.isLoading ? 4 : 0}
      >
        {!page.isLoading &&
          rows.map((s: PlanStatement) => (
            <tr key={s.statementId}>
              <td className="mono">
                {t("billing.periodIndex", { index: s.periodIndex ?? 0 })}
              </td>
              <td>{statementBadge(s.state)}</td>
              <Td mono align="right">
                <Cr minor={s.totalMinor} currency={display} />
              </Td>
              <Td mono align="right">
                <Cr minor={s.paidMinor} currency={display} />
              </Td>
              <Td mono align="right">
                <Cr minor={s.totalMinor - s.paidMinor} currency={display} />
              </Td>
              <td>{s.dueAt?.slice(0, 10) ?? "—"}</td>
              <td>
                <Link to={`/billing/${s.statementId}`} className="text-primary">
                  {t("billing.detailLink")}
                </Link>
              </td>
            </tr>
          ))}
        {!page.isLoading && rows.length === 0 ? (
          <EmptyRows colSpan={7} icon={<FileText />}>
            {t("billing.empty")}
          </EmptyRows>
        ) : null}
      </TableShell>
      <div className="flex items-center gap-2 px-1 pt-2">
        <Button tone="ghost" disabled={cursorStack.length === 0} onClick={() => setCursorStack((s) => s.slice(0, -1))}>
          {t("orders.prev")}
        </Button>
        <Button
          tone="ghost"
          disabled={!nextCursor}
          onClick={() => setCursorStack((s) => [...s, rows[rows.length - 1].statementId])}
        >
          {t("orders.next")}
        </Button>
      </div>
    </>
  );
}

export function Billing() {
  const { t } = useTranslation();
  return (
    <div className="flex flex-col gap-6">
      <h1 className="text-2xl font-bold">{t("billing.title")}</h1>
      <PlanStanding />
      <Card title={t("billing.statements")}>
        <StatementRows />
      </Card>
    </div>
  );
}

// ---------------------------------------------------------------------------
// /app/billing/{id} — one plan statement's detail: header KPIs plus the
// itemized lines in customer vocabulary (planFee, usage, discount,
// refundCredit, disputeChargeback, paymentApplied).
// ---------------------------------------------------------------------------

export function BillingStatement() {
  const { t } = useTranslation();
  const { id } = useParams<{ id: string }>();
  const display = useDisplay();
  const detail = usePlanStatement(id);
  const s = detail.data;

  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold">{t("billing.detailTitle")}</h1>
        {s ? statementBadge(s.state) : null}
      </div>
      {detail.isError ? (
        <PageState>{t("billing.detailError")}</PageState>
      ) : detail.isLoading ? (
        <PageState>{t("common.loading")}</PageState>
      ) : s ? (
        <>
          <div className="grid grid-cols-2 gap-4 sm:grid-cols-4">
            <Kpi label={t("billing.total")} icon={<FileText />} value={<Cr minor={s.totalMinor} currency={display} />} />
            <Kpi label={t("billing.paid")} icon={<CreditCard />} value={<Cr minor={s.paidMinor} currency={display} />} />
            <Kpi label={t("billing.owed")} icon={<AlertCircle />} value={<Cr minor={s.totalMinor - s.paidMinor} currency={display} />} />
            <Kpi label={t("billing.due")} icon={<Receipt />} value={s.dueAt?.slice(0, 10) ?? "—"} />
          </div>
          {s.state === "pastDue" ? <Banner tone="warn">{t("billing.pastDueNote")}</Banner> : null}
          <Link to={`/evidence?statement=${s.statementId}`} className="text-sm text-primary">{t("evidence.statementLink")}</Link>
          {s.replacesStatementId || s.successorStatementId ? <div className="flex flex-wrap gap-3">
            {s.replacesStatementId ? <Link className="text-primary" to={`/billing/${s.replacesStatementId}`}>{t("invoiceCorrection.predecessor")}</Link> : null}
            {s.successorStatementId ? <Link className="text-primary" to={`/billing/${s.successorStatementId}`}>{t("invoiceCorrection.successor")}</Link> : null}
          </div> : null}
          <StatementPayment key={s.statementId} statementId={s.statementId} />
          <Card title={t("billing.lines")}>
            <TableShell
              head={[
                t("billing.colKind"),
                { label: t("billing.colDescription"), truncate: true },
                { label: t("billing.colAmount"), align: "right" },
              ]}
            >
              {(s.lines ?? []).map((line) => (
                <tr key={line.lineId}>
                  <td>
                    <StatusBadge status={line.kind} />
                  </td>
                  <Td truncate className="text-muted">
                    {line.description}
                  </Td>
                  <Td mono align="right">
                    <Cr minor={line.amountMinor} currency={display} />
                  </Td>
                </tr>
              ))}
              {(s.lines?.length ?? 0) === 0 ? (
                <EmptyRows colSpan={3} icon={<FileText />}>
                  {t("billing.noLines")}
                </EmptyRows>
              ) : null}
            </TableShell>
          </Card>
        </>
      ) : null}
      <p>
        <Link to="/billing" className="text-sm text-primary">
          ← {t("billing.back")}
        </Link>
      </p>
    </div>
  );
}
