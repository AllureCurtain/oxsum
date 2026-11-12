import { PlanPastDueLabel, PlanStandingNotices } from "../components/plan-standing-notices";
import { CheckoutChoice, useCheckoutChoice } from "../components/checkout-choice";
import { useRef, useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import { usePlanPaymentQuote } from "../api/hooks/plans";
import { useCreateOrder } from "../api/hooks/orders";
import { useTranslation } from "react-i18next";
import { CalendarClock, CircleCheck, Layers, Tag } from "lucide-react";
import { ApiError } from "../api/client";
import {
  useCancelPlan,
  useChangePlan,
  useDisplay,
  useOrgPlan,
  usePlans,
  type SoldPlan,
} from "../api/hooks";
import {
  AlertDialog,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
  Badge,
  Banner,
  Button,
  Card,
  Cr,
  EmptyRows,
  Skeleton,
  TableShell,
  Td,
} from "../components/ui";

// ---------------------------------------------------------------------------
// /app/plans — the sold-plan catalogue plus the caller's own binding (T23):
// browse what is on sale, see the live plan and its credit standing, book a
// scheduled change at the next boundary, or cancel at the period end. The
// plan states read honest — `pendingPayment`/`pastDue`/`renewalPending` are
// shown, never smoothed over.
// ---------------------------------------------------------------------------

function planStateBadge(state: string | undefined) {
  switch (state) {
    case "active":
      return <Badge tone="success" live>active</Badge>;
    case "pastDue":
      return <Badge tone="danger">past due</Badge>;
    case "pendingPayment":
      return <Badge tone="secondary">pending payment</Badge>;
    case "renewalPending":
      return <Badge tone="secondary">renewal pending</Badge>;
    case "cancelled":
      return <Badge tone="secondary">cancelled</Badge>;
    case "expired":
      return <Badge tone="secondary">expired</Badge>;
    default:
      return <Badge tone="secondary">{state ?? "—"}</Badge>;
  }
}

function PlanPayment({ enabled }: { enabled: boolean }) {
  const { t } = useTranslation();
  const checkout = useCheckoutChoice(enabled);
  const quote = usePlanPaymentQuote(enabled);
  const create = useCreateOrder();
  const navigate = useNavigate();
  const intent = useRef<{ payload: string; key: string } | null>(null);
  const [review, setReview] = useState(false);
  if (!enabled) return null;
  const payment = quote.data;
  const terms = payment?.terms;
  const amount = terms ? BigInt(terms.displayAmountMinor) : 0n;
  const price = `${amount / 100n}.${(amount % 100n).toString().padStart(2, "0")} ${terms?.displayCurrency ?? ""}`;
  const pay = () => {
    if (intent.current) {
      create.mutate({ ...JSON.parse(intent.current.payload), idempotencyKey: intent.current.key }, { onSuccess: (result) => navigate(`/orders/${result.order.orderId}`) });
      return;
    }
    if (!payment || !terms || !checkout.choice) return;
    if (!intent.current) intent.current = { key: crypto.randomUUID(), payload: JSON.stringify({
      purpose: "planPeriod", ...checkout.choice,
      purposeRef: terms.customerPlanId, expectedPlanQuote: payment.fingerprint,
      expectedCollectionRevision: terms.collectionRevision,
      displayAmountMinor: terms.displayAmountMinor, displayCurrency: terms.displayCurrency,
    }) };
    create.mutate({ ...JSON.parse(intent.current.payload), idempotencyKey: intent.current.key    }, { onSuccess: (result) => navigate(`/orders/${result.order.orderId}`) });
  };
  return <div className="flex flex-col gap-3">
    <p className="text-sm text-muted">{t("plans.paymentIntro")}</p>
    {quote.isLoading ? <Skeleton className="h-8 w-full" /> : null}
    {quote.error ? <Banner tone="warn">{quote.error instanceof Error ? quote.error.message : t("plans.quoteError")}</Banner> : null}
    {payment?.existingOrderId ? <Link className="text-primary" to={`/orders/${payment.existingOrderId}`}>{t("plans.resumePayment")}</Link> : terms ? <div className="flex flex-col gap-2">
      <p>{t("plans.paymentLine", { price })} <Cr minor={terms.feeMinor} /> {t("plans.paymentPeriod", { index: terms.periodIndex + 1 })} {terms.periodStart ? t("plans.paymentRange", { start: terms.periodStart, end: terms.periodEnd }) : t("plans.paymentFirst")}</p>
      <Button onClick={() => setReview(true)}>{t("plans.reviewPayment")}</Button>
    </div> : null}
    <Button tone="ghost" disabled={create.isPending} onClick={() => void quote.refetch()}>{t("plans.refreshAvailability")}</Button>
    <AlertDialog open={review} onOpenChange={(open) => { if (!create.isPending) setReview(open); }}>
      <AlertDialogContent><AlertDialogHeader><AlertDialogTitle>{t("plans.payTitle", { price })}</AlertDialogTitle><AlertDialogDescription>{t("plans.payExplain")}</AlertDialogDescription></AlertDialogHeader>
        {intent.current ? <Banner tone="info">{t("plans.retryNote", { amount: JSON.parse(intent.current.payload).displayAmountMinor, currency: JSON.parse(intent.current.payload).displayCurrency, rail: JSON.parse(intent.current.payload).rail })}</Banner> : null}
        <div onChange={() => { if (!create.isPending) { intent.current = null; create.reset(); } }}><CheckoutChoice state={checkout} disabled={create.isPending} /></div>
        {create.error ? <Banner tone="error">{create.error.message}. {t("plans.retryError")}</Banner> : null}
        <AlertDialogFooter><Button tone="ghost" disabled={create.isPending} onClick={() => setReview(false)}>{t("plans.keepReviewing")}</Button><Button disabled={!intent.current && !checkout.choice} isLoading={create.isPending} onClick={pay}>{t("plans.continueToPayment")}</Button></AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  </div>;
}

/** The live plan card: name, price, period kind, the credit standing beside
 * it (issue #612's `credit` block) and the cancel/change affordances. */
function CurrentPlan() {
  const { t } = useTranslation();
  const orgPlan = useOrgPlan();
  const plans = usePlans();
  const display = useDisplay();
  const cancel = useCancelPlan();
  const [cancelOpen, setCancelOpen] = useState(false);

  if (orgPlan.isLoading) {
    return (
      <Card title={t("plans.current")}>
        <div className="flex flex-col gap-3">
          <Skeleton className="h-6 w-48" />
          <Skeleton className="h-4 w-72" />
        </div>
      </Card>
    );
  }
  if (orgPlan.isError) {
    return (
      <Card title={t("plans.current")}>
        <Banner tone="error">{t("plans.loadError")}</Banner>
      </Card>
    );
  }
  const view = orgPlan.data;
  const plan = view?.plan;
  if (!plan) {
    return (
      <Card title={t("plans.current")}>
        <p className="text-sm text-muted">{t("plans.none")}</p>
      </Card>
    );
  }

  const sold = (plans.data ?? []).find((p) => p.planVersionId === plan.planVersionId);
  const scheduled = plan.scheduledChange
    ? (plans.data ?? []).find((p) => p.planVersionId === plan.scheduledChange)
    : undefined;
  const credit = view?.credit;
  const activePeriod = view?.periods.find((p) => p.state === "active");

  return (
    <Card title={t("plans.current")}>
      <div className="flex flex-col gap-4">
        <PlanPayment enabled={sold ? sold.priceMinor > 0 : true} />
        {sold ? <p className="text-sm text-muted">{t("plans.recognitionPolicy")}: {t(sold.accountingPolicyVersion === 2 ? "plans.policyV2" : "plans.policyV1")}</p> : null}
        <div className="flex flex-wrap items-center gap-2">
          <p className="text-lg font-semibold text-ink">{sold?.name ?? t("plans.unnamed")}</p>
          {plan.state === "pastDue" ? <Badge tone="danger"><PlanPastDueLabel view={view} /></Badge> : planStateBadge(plan.state)}
          {plan.cancelAtPeriodEnd ? (
            <Badge tone="secondary">{t("plans.cancelAtPeriodEnd")}</Badge>
          ) : null}
        </div>
        <PlanStandingNotices view={view} page="plans" />
        <dl className="grid grid-cols-2 gap-3 text-sm sm:grid-cols-4">
          <div>
            <dt className="text-xs font-medium text-muted">{t("plans.price")}</dt>
            <dd className="mt-0.5 font-semibold tabular-nums">
              {sold ? <Cr minor={sold.priceMinor} currency={display} /> : "—"}
              <span className="ml-1 text-xs font-normal text-muted">
                / {sold ? t(`plans.period.${sold.periodKind}`) : "—"}
              </span>
            </dd>
          </div>
          <div>
            <dt className="text-xs font-medium text-muted">{t("plans.quota")}</dt>
            <dd className="mt-0.5 font-semibold tabular-nums">
              {sold?.quotaMinor != null ? (
                <Cr minor={sold.quotaMinor} currency={display} />
              ) : (
                t("plans.unmetered")
              )}
            </dd>
          </div>
          <div>
            <dt className="text-xs font-medium text-muted">{t("plans.creditLimit")}</dt>
            <dd className="mt-0.5 font-semibold tabular-nums">
              {credit?.creditLimitMinor != null ? (
                <Cr minor={credit.creditLimitMinor} currency={display} />
              ) : (
                t("plans.unlimited")
              )}
            </dd>
          </div>
          <div>
            <dt className="text-xs font-medium text-muted">{t("plans.outstanding")}</dt>
            <dd className="mt-0.5 font-semibold tabular-nums">
              {credit ? <Cr minor={credit.outstandingMinor} currency={display} /> : "—"}
            </dd>
          </div>
        </dl>
        <dl className="grid grid-cols-2 gap-3 text-sm sm:grid-cols-4">
          <div>
            <dt className="text-xs font-medium text-muted">{t("plans.currentPeriod")}</dt>
            <dd className="mt-0.5">
              {activePeriod
                ? `${activePeriod.periodStart.slice(0, 10)} → ${activePeriod.periodEnd.slice(0, 10)}`
                : "—"}
            </dd>
          </div>
          <div>
            <dt className="text-xs font-medium text-muted">{t("plans.consumed")}</dt>
            <dd className="mt-0.5 tabular-nums">
              {activePeriod ? <Cr minor={activePeriod.consumedMinor} currency={display} /> : "—"}
            </dd>
          </div>
          <div>
            <dt className="text-xs font-medium text-muted">{t("plans.nextDue")}</dt>
            <dd className="mt-0.5">{credit?.nextDueAt?.slice(0, 10) ?? "—"}</dd>
          </div>
          <div>
            <dt className="text-xs font-medium text-muted">{t("plans.scheduledChange")}</dt>
            <dd className="mt-0.5">
              {scheduled ? (
                <span className="inline-flex items-center gap-1">
                  <CalendarClock className="size-3.5 text-muted" />
                  {scheduled.name}
                </span>
              ) : (
                "—"
              )}
            </dd>
          </div>
        </dl>
        {!plan.cancelAtPeriodEnd && plan.state !== "cancelled" && plan.state !== "expired" ? (
          <div className="flex flex-wrap gap-2 border-t border-line pt-3">
            <AlertDialog open={cancelOpen} onOpenChange={setCancelOpen}>
              <AlertDialogTrigger asChild>
                <Button tone="ghost" className="text-danger">
                  {t("plans.cancelPlan")}
                </Button>
              </AlertDialogTrigger>
              <AlertDialogContent>
                <AlertDialogHeader>
                  <AlertDialogTitle>{t("plans.cancelTitle")}</AlertDialogTitle>
                  <AlertDialogDescription>
                    {t("plans.cancelExplain", {
                      end: activePeriod?.periodEnd.slice(0, 10) ?? "",
                    })}
                  </AlertDialogDescription>
                </AlertDialogHeader>
                <AlertDialogFooter>
                  <AlertDialogCancel>{t("common.cancel")}</AlertDialogCancel>
                  <Button
                    tone="danger"
                    isLoading={cancel.isPending}
                    loadingText={t("plans.cancelling")}
                    onClick={() =>
                      cancel.mutate(undefined, { onSuccess: () => setCancelOpen(false) })
                    }
                  >
                    {t("plans.cancelConfirm")}
                  </Button>
                </AlertDialogFooter>
              </AlertDialogContent>
            </AlertDialog>
            {cancel.error instanceof ApiError ? (
              <Banner tone="error">{cancel.error.message}</Banner>
            ) : null}
          </div>
        ) : null}
      </div>
    </Card>
  );
}

/** The change-plan dialog behind one catalogue row: books the swap at the
 * next boundary (never a mid-period pro-rating). */
function ChangePlanDialog({ target }: { target: SoldPlan }) {
  const { t } = useTranslation();
  const change = useChangePlan();
  const [open, setOpen] = useState(false);
  return (
    <AlertDialog open={open} onOpenChange={setOpen}>
      <AlertDialogTrigger asChild>
        <Button tone="ghost">{t("plans.changeTo")}</Button>
      </AlertDialogTrigger>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{t("plans.changeTitle", { name: target.name })}</AlertDialogTitle>
          <AlertDialogDescription>{t("plans.changeExplain")}</AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>{t("common.cancel")}</AlertDialogCancel>
          <Button
            isLoading={change.isPending}
            loadingText={t("plans.scheduling")}
            onClick={() =>
              change.mutate(target.planVersionId, { onSuccess: () => setOpen(false) })
            }
          >
            {t("plans.changeConfirm")}
          </Button>
        </AlertDialogFooter>
        {change.error instanceof ApiError ? (
          <Banner tone="error">{change.error.message}</Banner>
        ) : null}
      </AlertDialogContent>
    </AlertDialog>
  );
}

/** The catalogue table: every published version still on sale, with the
 * caller's live version and scheduled change marked. */
function Catalogue() {
  const { t } = useTranslation();
  const plans = usePlans();
  const orgPlan = useOrgPlan();
  const display = useDisplay();
  const currentId = orgPlan.data?.plan?.planVersionId;
  const scheduledId = orgPlan.data?.plan?.scheduledChange;

  return (
    <Card title={t("plans.catalogue")}>
      {plans.isError ? (
        <Banner tone="error">{t("plans.catalogueError")}</Banner>
      ) : (
        <TableShell
          head={[
            t("plans.colName"),
            { label: t("plans.price"), align: "right" },
            { label: t("plans.quota"), align: "right" },
            t("plans.colPeriod"),
            t("plans.colOverage"),
            "",
          ]}
          loading={plans.isLoading ? 4 : 0}
        >
          {!plans.isLoading &&
            (plans.data ?? []).map((p) => (
              <tr key={p.planVersionId}>
                <td className="font-medium">
                  {p.name}
                  {p.planVersionId === currentId ? (
                    <Badge tone="success" className="ml-2">
                      {t("plans.currentBadge")}
                    </Badge>
                  ) : null}
                  {p.planVersionId === scheduledId ? (
                    <Badge tone="secondary" className="ml-2">
                      {t("plans.scheduledBadge")}
                    </Badge>
                  ) : null}
                </td>
                <Td mono align="right">
                  <Cr minor={p.priceMinor} currency={display} />
                </Td>
                <Td mono align="right">
                  {p.quotaMinor != null ? (
                    <Cr minor={p.quotaMinor} currency={display} />
                  ) : (
                    t("plans.unmetered")
                  )}
                </Td>
                <td>{t(`plans.period.${p.periodKind}`)}</td>
                <td>{p.overageEnabled ? t("plans.overageOn") : t("plans.overageOff")}</td>
                <td className="text-right">
                  {currentId && p.planVersionId !== currentId && p.planVersionId !== scheduledId ? (
                    <ChangePlanDialog target={p} />
                  ) : null}
                </td>
              </tr>
            ))}
          {!plans.isLoading && (plans.data?.length ?? 0) === 0 ? (
            <EmptyRows colSpan={6} icon={<Tag />}>
              {t("plans.catalogueEmpty")}
            </EmptyRows>
          ) : null}
        </TableShell>
      )}
    </Card>
  );
}

export function Plans() {
  const { t } = useTranslation();
  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold">{t("plans.title")}</h1>
        <Layers className="size-5 text-muted" aria-hidden />
      </div>
      <CurrentPlan />
      <Catalogue />
      <Card title={t("plans.howItWorks")}>
        <ul className="flex list-disc flex-col gap-1.5 pl-5 text-sm text-muted">
          <li className="flex items-start gap-2">
            <CircleCheck className="mt-0.5 size-4 shrink-0 text-success" aria-hidden />
            {t("plans.note1")}
          </li>
          <li className="flex items-start gap-2">
            <CircleCheck className="mt-0.5 size-4 shrink-0 text-success" aria-hidden />
            {t("plans.note2")}
          </li>
          <li className="flex items-start gap-2">
            <CircleCheck className="mt-0.5 size-4 shrink-0 text-success" aria-hidden />
            {t("plans.note3")}
          </li>
        </ul>
      </Card>
    </div>
  );
}
