import { useState } from "react";
import { useTranslation } from "react-i18next";
import { ArrowLeftRight, ShieldCheck, Ticket } from "lucide-react";
import {
  useBusiness,
  useBusinessCreditNotes,
  useBusinessDisputes,
  useBusinessRefunds,
  useCapability,
  useDisplay,
  useResolveDispute,
  useResolveRefund,
} from "../../api/hooks";
import {
  Banner,
  Button,
  Card,
  Cr,
  EmptyRows,
  TableShell,
  Td,
} from "../../components/ui";
import { AuditAction, DetailGrid, Field2, stateBadge } from "./biz";

// ---------------------------------------------------------------------------
// /app/business#business/refunds + #business/disputes — the finance-review
// surfaces (T17). A `requested` refund resolves approve/decline with a reason;
// an open dispute resolves evidenceDue/won/lost/closed. Every decision lands
// in finance_audit under an idempotency key. Rows drill to order + customer.
// ---------------------------------------------------------------------------

function RefundRows() {
  const { t } = useTranslation();
  const display = useDisplay();
  const [after, setAfter] = useState<string | undefined>();
  const refunds = useBusinessRefunds(after);
  const resolve = useResolveRefund();
  const canWrite = useCapability(useBusiness().data, "refundsWrite");
  const page = refunds.data;

  return (
    <Card title={t("biz.refunds.listTitle")}>
      {refunds.isError ? (
        <Banner tone="error">{t("biz.loadError")}</Banner>
      ) : (
        <TableShell
          head={[
            t("biz.refunds.colId"),
            t("biz.refunds.colOrder"),
            t("biz.refunds.colOrg"),
            { label: t("biz.refunds.colAmount"), align: "right" },
            t("common.status"),
            t("biz.refunds.colAsked"),
            "",
          ]}
          loading={refunds.isLoading ? 5 : 0}
        >
          {!refunds.isLoading &&
            (page?.refunds ?? []).map((r) => (
              <tr key={r.refundId}>
                <Td mono truncate>{r.refundId.slice(0, 8)}</Td>
                <Td mono truncate>{r.orderId.slice(0, 8)}</Td>
                <Td mono truncate>{r.organizationId.slice(0, 8)}</Td>
                <Td mono align="right"><Cr minor={r.amountMinor} currency={display} /></Td>
                <td>
                  {stateBadge(r.state)}
                  {r.cashEffect ? <div className="mt-1 text-xs">
                    {t("refundCash.returned")} <Cr minor={r.cashEffect.cashMinor} />
                    {r.cashEffect.requiresReview ? <p>{t("refundCash.review")}</p> : null}
                    <p>{t("refundCash.walletTaken")} <Cr minor={r.cashEffect.walletMinor} /></p>
                    {r.cashEffect.suspenseMinor > 0 ? <p>{t("refundCash.suspense")} <Cr minor={r.cashEffect.suspenseMinor} /></p> : null}
                    {r.cashEffect.walletRestoredMinor > 0 ? <p>{t("refundCash.walletRestored")} <Cr minor={r.cashEffect.walletRestoredMinor} /></p> : null}
                    <p className="text-muted">{t("refundCash.stateRetained")}</p>
                  </div> : null}
                </td>
                <Td mono>{r.createdAt.slice(0, 10)}</Td>
                <td className="text-right">
                  {canWrite && r.state === "requested" && !r.cashEffect ? (
                    <span className="inline-flex gap-1">
                      <AuditAction
                        trigger={<Button tone="ghost">{t("biz.refunds.approve")}</Button>}
                        title={t("biz.refunds.approveTitle")}
                        description={t("biz.refunds.approveExplain")}
                        confirmLabel={t("biz.refunds.approve")}
                        reasonLabel={t("biz.reason")}
                        isPending={resolve.isPending}
                        error={resolve.error}
                        onConfirm={(reason, key) =>
                          resolve.mutate({ refundId: r.refundId, idempotencyKey: key, decision: "approve", reason })
                        }
                      />
                      <AuditAction
                        trigger={<Button tone="danger">{t("biz.refunds.decline")}</Button>}
                        title={t("biz.refunds.declineTitle")}
                        description={t("biz.refunds.declineExplain")}
                        confirmLabel={t("biz.refunds.decline")}
                        tone="danger"
                        reasonLabel={t("biz.reason")}
                        isPending={resolve.isPending}
                        error={resolve.error}
                        onConfirm={(reason, key) =>
                          resolve.mutate({ refundId: r.refundId, idempotencyKey: key, decision: "decline", reason })
                        }
                      />
                    </span>
                  ) : null}
                </td>
              </tr>
            ))}
          {!refunds.isLoading && (page?.refunds.length ?? 0) === 0 ? (
            <EmptyRows colSpan={7} icon={<ArrowLeftRight />}>
              {t("biz.refunds.empty")}
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

function CreditNotes() {
  const { t } = useTranslation();
  const display = useDisplay();
  const notes = useBusinessCreditNotes();
  return (
    <Card title={t("biz.refunds.notesTitle")}>
      {notes.isError ? (
        <Banner tone="error">{t("biz.loadError")}</Banner>
      ) : (
        <TableShell
          density="compact"
          head={[
            t("biz.refunds.colNote"),
            t("biz.refunds.colOrder"),
            { label: t("biz.refunds.colAmount"), align: "right" },
            t("common.status"),
            t("biz.refunds.colWhen"),
          ]}
          loading={notes.isLoading ? 3 : 0}
          sticky={false}
        >
          {!notes.isLoading &&
            (notes.data?.notes ?? []).map((n) => (
              <tr key={n.creditNoteId}>
                <Td mono truncate>{n.creditNoteId.slice(0, 8)}</Td>
                <Td mono truncate>{n.orderId.slice(0, 8)}</Td>
                <Td mono align="right"><Cr minor={n.amountMinor} currency={display} /></Td>
                <td>{stateBadge(n.state)}{n.cashEffect ? <p className="mt-1 text-xs">{t("refundCash.returned")} <Cr minor={n.cashEffect.cashMinor} />{n.cashEffect.requiresReview ? ` · ${t("refundCash.review")}` : ""}</p> : null}</td>
                <Td mono>{n.createdAt.slice(0, 10)}</Td>
              </tr>
            ))}
          {!notes.isLoading && (notes.data?.notes.length ?? 0) === 0 ? (
            <EmptyRows colSpan={5} icon={<Ticket />}>
              {t("biz.refunds.noNotes")}
            </EmptyRows>
          ) : null}
        </TableShell>
      )}
    </Card>
  );
}

export function BusinessRefunds() {
  const { t } = useTranslation();
  return (
    <div className="flex flex-col gap-6">
      <h2 className="text-xl font-bold">{t("biz.refunds.title")}</h2>
      <RefundRows />
      <CreditNotes />
    </div>
  );
}

export function BusinessDisputes() {
  const { t } = useTranslation();
  const disputes = useBusinessDisputes();
  const resolve = useResolveDispute();
  const canWrite = useCapability(useBusiness().data, "refundsWrite");
  const [expanded, setExpanded] = useState<string | null>(null);

  return (
    <div className="flex flex-col gap-6">
      <h2 className="text-xl font-bold">{t("biz.disputes.title")}</h2>
      <Card title={t("biz.disputes.listTitle")}>
        {disputes.isError ? (
          <Banner tone="error">{t("biz.loadError")}</Banner>
        ) : (
          <TableShell
            head={[
              t("biz.disputes.colId"),
              t("biz.disputes.colOrder"),
              t("biz.disputes.colCase"),
              { label: t("biz.disputes.colAmount"), align: "right" },
              t("common.status"),
              t("biz.disputes.colOpened"),
              "",
            ]}
            loading={disputes.isLoading ? 5 : 0}
          >
            {!disputes.isLoading &&
              (disputes.data?.disputes ?? []).map((d) => (
                <>
                  <tr key={d.disputeId} className="cursor-pointer" onClick={() => setExpanded(expanded === d.disputeId ? null : d.disputeId)}>
                    <Td mono truncate>{d.disputeId.slice(0, 8)}</Td>
                    <Td mono truncate>{d.orderId.slice(0, 8)}</Td>
                    <Td mono truncate>{d.providerDisputeId}</Td>
                    <Td mono align="right"><Cr minor={d.amountMinor} /></Td>
                    <td>{stateBadge(d.state)}</td>
                    <Td mono>{d.createdAt.slice(0, 10)}</Td>
                    <td className="text-right">
                      {canWrite && (d.state === "opened" || d.state === "evidence_due") ? (
                        <AuditAction
                          trigger={<Button tone="ghost">{t("biz.disputes.resolve")}</Button>}
                          title={t("biz.disputes.resolveTitle")}
                          description={t("biz.disputes.resolveExplain")}
                          confirmLabel={t("biz.disputes.resolve")}
                          reasonLabel={t("biz.reason")}
                          isPending={resolve.isPending}
                          error={resolve.error}
                          onConfirm={(reason, key) =>
                            resolve.mutate({ disputeId: d.disputeId, idempotencyKey: key, resolution: "closed", reason })
                          }
                        />
                      ) : null}
                    </td>
                  </tr>
                  {expanded === d.disputeId ? (
                    <tr key={`${d.disputeId}-d`}>
                      <td colSpan={7} className="bg-canvas-inset">
                        <DetailGrid>
                          <Field2 label={t("biz.disputes.colOrg")}>
                            <span className="mono text-xs">{d.organizationId}</span>
                          </Field2>
                          <Field2 label={t("biz.disputes.collectionCurrency")}>{d.currency}</Field2>
                          <Field2 label={t("biz.disputes.recharge")}>
                            <span className="mono text-xs">{d.rechargeEntryId ?? "—"}</span>
                          </Field2>
                          <Field2 label={t("biz.disputes.updated")}>{d.updatedAt.slice(0, 10)}</Field2>
                          {d.cashCorrection ? <>
                            <Field2 label={t("chargebackCorrection.actualLoss")}><Cr minor={d.cashCorrection.actualLossMinor} /></Field2>
                            <Field2 label={t("chargebackCorrection.falseReceipt")}><Cr minor={d.cashCorrection.falseReceiptMinor} /></Field2>
                            <Field2 label={t("chargebackCorrection.totalCorrection")}><Cr minor={d.cashCorrection.cashCorrectionMinor} /></Field2>
                            <Field2 label={t("refundCash.walletTaken")}><Cr minor={d.cashCorrection.walletMinor} /></Field2>
                            <Field2 label={t("refundCash.suspense")}><Cr minor={d.cashCorrection.suspenseMinor} /></Field2>
                            <Field2 label={t("chargebackCorrection.entry")}><span className="mono text-xs">{d.cashCorrection.correctionEntryId}</span></Field2>
                            <Field2 label={t("refundCash.review")}>{t("chargebackCorrection.retained")}</Field2>
                          </> : null}
                        </DetailGrid>
                      </td>
                    </tr>
                  ) : null}
                </>
              ))}
            {!disputes.isLoading && (disputes.data?.disputes.length ?? 0) === 0 ? (
              <EmptyRows colSpan={7} icon={<ShieldCheck />}>
                {t("biz.disputes.empty")}
              </EmptyRows>
            ) : null}
          </TableShell>
        )}
      </Card>
    </div>
  );
}
