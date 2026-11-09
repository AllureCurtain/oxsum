import { useRef, useState } from "react";
import { Link } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { AlertTriangle, Landmark, Wallet } from "lucide-react";
import { decimalMinor } from "../../lib/money";
import { ApiError } from "../../api/client";
import {
  useBusiness,
  useBusinessOrders,
  useCapability,
  useDisplay,
  useManualOrder,
  useCollectionTerms,
  usePaymentReconciliation,
  useStatementPaymentEvidence,
} from "../../api/hooks";
import {
  Banner,
  Button,
  Card,
  Cr,
  EmptyRows,
  Field,
  Kpi,
  NativeSelect,
  Skeleton,
  TableShell,
  Td,
  TextInput,
} from "../../components/ui";
import { DetailGrid, Field2, stateBadge } from "./biz";

// ---------------------------------------------------------------------------
// /app/business#business/payments — cross-org orders (T16), the
// paid/fulfilled/needs_attention reconciliation strip (T15) and the
// manual-receipt form a staff member records a verified bank payment under.
// ---------------------------------------------------------------------------

function ReconStrip() {
  const { t } = useTranslation();
  const recon = usePaymentReconciliation();
  const d = recon.data;
  if (recon.isLoading) {
    return <Skeleton className="h-20 w-full" />;
  }
  if (recon.isError || !d) {
    return <Banner tone="error">{t("biz.loadError")}</Banner>;
  }
  return (
    <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
      <Kpi label={t("biz.payments.paid")} icon={<Wallet />} value={d.paid} />
      <Kpi label={t("biz.payments.fulfilled")} icon={<Landmark />} value={d.fulfilled} />
      <Kpi
        label={t("biz.payments.needsAttention")}
        icon={<AlertTriangle />}
        value={d.needsAttention}
        hint={d.paidUnfulfilled.length > 0 ? t("biz.payments.unfulfilled", { count: d.paidUnfulfilled.length }) : undefined}
      />
      <Kpi label={t("biz.payments.pending")} icon={<Wallet />} value={d.providerPending + d.created} />
    </div>
  );
}

const PURPOSES = ["walletTopup", "bundle", "planPeriod", "invoiceRepayment", "statementRepayment"] as const;
const STATUSES = ["created", "provider_pending", "paid", "fulfilled", "expired", "failed", "needs_attention"] as const;

function OrderTable() {
  const { t } = useTranslation();
  const display = useDisplay();
  const [purpose, setPurpose] = useState("");
  const [status, setStatus] = useState("");
  const [orgId, setOrgId] = useState("");
  const [after, setAfter] = useState<string | undefined>();
  const [expanded, setExpanded] = useState<string | null>(null);
  const orders = useBusinessOrders({
    purpose: purpose || undefined,
    status: status || undefined,
    organizationId: orgId || undefined,
    after,
  });
  const page = orders.data;

  return (
    <Card title={t("biz.payments.orders")}>
      <div className="mb-3 grid grid-cols-2 gap-2 sm:grid-cols-4">
        <Field label={t("biz.payments.fPurpose")}>
          <NativeSelect value={purpose} onChange={(e) => { setPurpose(e.target.value); setAfter(undefined); }}>
            <option value="">{t("common.all")}</option>
            {PURPOSES.map((p) => <option key={p} value={p}>{t(`biz.payments.purpose.${p}`)}</option>)}
          </NativeSelect>
        </Field>
        <Field label={t("common.status")}>
          <NativeSelect value={status} onChange={(e) => { setStatus(e.target.value); setAfter(undefined); }}>
            <option value="">{t("common.all")}</option>
            {STATUSES.map((s) => <option key={s} value={s}>{s}</option>)}
          </NativeSelect>
        </Field>
        <Field label={t("biz.payments.fOrg")}>
          <TextInput value={orgId} onChange={(e) => { setOrgId(e.target.value); setAfter(undefined); }} placeholder="org uuid" />
        </Field>
      </div>
      {orders.isError ? (
        <Banner tone="error">{t("biz.loadError")}</Banner>
      ) : (
        <TableShell
          head={[
            t("biz.payments.colId"),
            t("biz.payments.colOrg"),
            t("biz.payments.colPurpose"),
            t("common.status"),
            { label: t("common.amount"), align: "right" },
            t("biz.payments.colSource"),
            t("biz.payments.colWhen"),
          ]}
          loading={orders.isLoading ? 6 : 0}
        >
          {!orders.isLoading &&
            (page?.orders ?? []).map((o) => (
              <>
                <tr key={o.orderId} className="cursor-pointer" onClick={() => setExpanded(expanded === o.orderId ? null : o.orderId)}>
                  <Td mono truncate>{o.orderId.slice(0, 8)}</Td>
                  <Td mono truncate>{o.organizationId.slice(0, 8)}</Td>
                  <td>{t(`biz.payments.purpose.${o.purpose}`)}</td>
                  <td>{stateBadge(o.status)}</td>
                  <Td mono align="right"><Cr minor={o.amountMinor} currency={display} /></Td>
                  <td>{o.source}</td>
                  <Td mono>{o.createdAt.slice(0, 10)}</Td>
                </tr>
                {expanded === o.orderId ? (
                  <tr key={`${o.orderId}-detail`}>
                    <td colSpan={7} className="bg-canvas-inset">
                      <DetailGrid>
                        <Field2 label={t("biz.payments.fiat")}>
                          {o.displayAmountMinor} {o.displayCurrency}
                        </Field2>
                        <Field2 label={t("biz.payments.providerRef")}>
                          <span className="mono text-xs">{o.providerPaymentId ?? "—"}</span>
                        </Field2>
                        <Field2 label={t("biz.payments.paidAt")}>{o.paidAt?.slice(0, 10) ?? "—"}</Field2>
                        <Field2 label={t("biz.payments.merchant")}>
                          <span className="mono text-xs">{o.merchantId?.slice(0, 8) ?? "manual"}</span>
                        </Field2>
                        {o.statementPayment ? <Field2 label={t("statementPayment.allocations")}>
                          <Link className="text-primary" to={`/business?statement=${o.statementPayment.statementId}#business/statements`}>{t("statementPayment.statementLink")}</Link>
                          {o.statementPayment.allocations.map((slice) => <p key={slice.journalId}>
                            <Link className="text-primary" to={`/business?journal=${slice.journalId}#business/statements`}>{t("statementPayment.journalLink")} · {slice.journalId.slice(0, 8)}</Link> · <Cr minor={slice.amountMinor} />
                          </p>)}
                          <StatementCashEvidence orderId={o.orderId} />
                        </Field2> : null}
                        {o.purpose === "bundle" ? (
                          <Field2 label={t("biz.payments.promo")}>
                            <Cr minor={o.promoMinor} currency={display} />
                          </Field2>
                        ) : null}
                        {o.source === "manual" && Object.keys(o.evidence).length > 0 ? (
                          <Field2 label={t("biz.payments.reference")}>
                            <span className="mono text-xs">{String((o.evidence as Record<string, unknown>).reference ?? "—")}</span>
                          </Field2>
                        ) : null}
                      </DetailGrid>
                    </td>
                  </tr>
                ) : null}
              </>
            ))}
          {!orders.isLoading && (page?.orders.length ?? 0) === 0 ? (
            <EmptyRows colSpan={7} icon={<Wallet />}>
              {t("biz.payments.empty")}
            </EmptyRows>
          ) : null}
        </TableShell>
      )}
      {page?.nextCursor ? (
        <Button tone="ghost" className="mt-3" onClick={() => setAfter(page.nextCursor ?? undefined)}>
          {t("common.loadMore")}
        </Button>
      ) : null}
    </Card>
  );
}

function ManualReceipt() {
  const { t } = useTranslation();
  const manual = useManualOrder();
  const [orgId, setOrgId] = useState("");
  const [amount, setAmount] = useState("");
  const terms = useCollectionTerms(true);
  const currency = terms.data?.currency?.code ?? "";
  const intent = useRef<{payload:string; key:string}|null>(null);
  const [reference, setReference] = useState("");
  const [reason, setReason] = useState("");
  const canWrite = useCapability(useBusiness().data, "fundsWrite");
  if (!canWrite) return null;

  return (
    <Card title={t("biz.payments.manualTitle")}>
      <form
        className="flex flex-col gap-3"
        onSubmit={(e) => {
          e.preventDefault();
          const minor = decimalMinor(amount, 0);
          if (minor === null || !terms.data?.ready) return;
          const payload = JSON.stringify({ orgId, minor, currency, reference, reason });
          if (intent.current?.payload !== payload) intent.current = { payload, key: crypto.randomUUID() };
          manual.mutate(
            {
              organizationId: orgId,
              idempotencyKey: intent.current.key,
              expectedCollectionRevision: terms.data.revision,
              displayAmountMinor: minor,
              displayCurrency: currency,
              reference,
              reason,
            },
            { onSuccess: () => { setReference(""); setReason(""); setAmount(""); } },
          );
        }}
      >
        {terms.data?.ready ? <p className="font-mono text-sm">1 credit = {terms.data.currency?.ratePerCredit} {currency}</p> : <Banner tone="warn">Collection terms are unavailable. Configure them before recording a receipt.</Banner>}
        <Button type="button" tone="ghost" onClick={() => terms.refetch()}>Refresh collection terms</Button>
        <Field label={t("biz.payments.fOrg")} hint={t("biz.payments.manualHint")}>
          <TextInput value={orgId} onChange={(e) => setOrgId(e.target.value)} required placeholder="org uuid" />
        </Field>
        <div className="grid grid-cols-2 gap-3">
          <Field label={t("biz.payments.fiatAmount")}>
            <TextInput inputMode="numeric" value={amount} onChange={(e) => setAmount(e.target.value)} required placeholder="minor units" />
          </Field>
          <Field label={t("biz.plans.colCurrency")}>
            <TextInput value={currency} readOnly />
          </Field>
        </div>
        <Field label={t("biz.payments.reference")}>
          <TextInput value={reference} onChange={(e) => setReference(e.target.value)} required />
        </Field>
        <Field label={t("biz.reason")}>
          <TextInput value={reason} onChange={(e) => setReason(e.target.value)} required />
        </Field>
        {manual.error instanceof ApiError ? <Banner tone="error">{manual.error.message}</Banner> : null}
        {manual.data ? (
          <Banner tone="success">
            {t("biz.payments.recorded", { id: manual.data.order.orderId.slice(0, 8) })}
          </Banner>
        ) : null}
        <Button type="submit" disabled={!terms.data?.ready || decimalMinor(amount, 0) === null} isLoading={manual.isPending}>{t("biz.payments.record")}</Button>
      </form>
    </Card>
  );
}

export function BusinessPayments() {
  const { t } = useTranslation();
  return (
    <div className="flex flex-col gap-6">
      <h2 className="text-xl font-bold">{t("biz.payments.title")}</h2>
      <ReconStrip />
      <ManualReceipt />
      <OrderTable />
    </div>
  );
}

function StatementCashEvidence({ orderId }: { orderId: string }) {
  const { t } = useTranslation();
  const effects = useStatementPaymentEvidence(orderId);
  if (effects.isLoading) return <p>{t("common.loading")}</p>;
  if (effects.error) return <Banner tone="error">{effects.error.message}</Banner>;
  return <TableShell head={[t("billing.colKind"), t("statementPayment.source"), t("statementPayment.cash"), t("statementPayment.receivable"), t("statementPayment.payableLabel"), t("statementPayment.suspense"), t("statementPayment.entry")]}>
    {(effects.data ?? []).map((effect) => <tr key={effect.id}>
      <td>{effect.kind}</td><Td mono>{effect.source}</Td>
      <Td mono align="right"><Cr minor={effect.cash} /></Td>
      <Td mono align="right"><Cr minor={effect.receivable} /></Td>
      <Td mono align="right"><Cr minor={effect.payable} /></Td>
      <Td mono align="right"><Cr minor={effect.suspense} /></Td>
      <Td mono><p>{effect.entry}</p><p>{effect.hash}</p>{effect.original ? <p>{t("statementPayment.original")}: {effect.original}</p> : null}</Td>
    </tr>)}
    {effects.data?.length === 0 ? <EmptyRows colSpan={7}>{t("statementPayment.awaiting")}</EmptyRows> : null}
  </TableShell>;
}
