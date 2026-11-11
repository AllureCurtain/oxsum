import { useState } from "react";
import { Link } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { ApiError } from "../../api/client";
import {
  useBusiness, useBusinessJournal, useCorrectInvoice, useInvoiceCorrectionPreview,
  type InvoiceCorrectionPreview, type InvoiceCorrectionRequest,
} from "../../api/hooks";
import {
  Banner, Button, Cr, Dialog, DialogContent, DialogDescription, DialogHeader,
  DialogTitle, Field, TextInput,
} from "../../components/ui";
import { decimalMinor } from "../../lib/money";

interface Retained { request: InvoiceCorrectionRequest; preview: InvoiceCorrectionPreview }

/** Persist the exact accepted intent before sending; scope it to the staff actor. */
export function InvoiceCorrection({ journalId }: { journalId: string }) {
  const business = useBusiness().data;
  if (!business?.capabilities.includes("fundsWrite")) return null;
  const scope = `invoice-correction:${business.businessId}:${business.userId}:${journalId}`;
  return <CorrectionForm key={scope} journalId={journalId} storageKey={scope} />;
}

function CorrectionForm({ journalId, storageKey }: { journalId: string; storageKey: string }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [reason, setReason] = useState("");
  const [values, setValues] = useState<Record<string, string>>({});
  const [localError, setLocalError] = useState<string>();
  const readIntent = (): Retained | null => {
    try {
      const saved = JSON.parse(sessionStorage.getItem(storageKey) ?? "null") as Retained | null;
      return saved?.preview.journalId === journalId && typeof saved.request.idempotencyKey === "string" ? saved : null;
    } catch { return null; }
  };
  const [intent, setIntent] = useState<Retained | null>(readIntent);
  const detail = useBusinessJournal(open ? journalId : undefined);
  const preview = useInvoiceCorrectionPreview();
  const apply = useCorrectInvoice();
  const source = detail.data;
  const reviewed = intent?.preview ?? preview.data;
  const originalIds = new Set(source?.lines.map((line) => line.lineId));
  const charges = source?.lines.filter((line) => line.kind === "baseFee" && line.amountMinor >= 0 && !originalIds.has(line.sourceRef ?? "")) ?? [];
  const format = (value: number) => Number.isSafeInteger(value)
    ? `${BigInt(value) / 1_000_000n}.${(BigInt(value) % 1_000_000n).toString().padStart(6, "0")}` : "";
  const selections = charges.map((line) => ({
    sourceLineId: line.lineId, amountMinor: decimalMinor(values[line.lineId] ?? format(line.amountMinor), 6, true),
  }));
  const valid = charges.every((line) => Number.isSafeInteger(line.amountMinor)) && selections.length > 0 && selections.every((line) => line.amountMinor !== null);
  const busy = preview.isPending || apply.isPending;
  const submit = () => {
    if (!reviewed || (!intent && !reason.trim())) return;
    const retained = intent ?? {
      preview: reviewed,
      request: {
        lines: reviewed.lines.filter((line) => line.kind === "baseFee").map((line) => ({ sourceLineId: line.sourceLineId, amountMinor: line.correctedMinor })),
        sourceRevision: reviewed.sourceRevision, reason, idempotencyKey: crypto.randomUUID(),
      },
    };
    try { sessionStorage.setItem(storageKey, JSON.stringify(retained)); }
    catch { setLocalError(t("invoiceCorrection.storageError")); return; }
    setIntent(retained);
    setLocalError(undefined);
    apply.mutate({ journalId, request: retained.request }, {
      onSuccess: () => { sessionStorage.removeItem(storageKey); setIntent(null); },
      onError: (error) => {
        // A definite refusal has no committed correction. Unknown transport and
        // server outcomes retain the original bytes/key for explicit recovery.
        if (error instanceof ApiError && [400, 403, 404, 409].includes(error.status)) {
          sessionStorage.removeItem(storageKey); setIntent(null); preview.reset();
        }
      },
    });
  };
  return <>
    <Button tone="ghost" onClick={() => { setIntent(readIntent()); setOpen(true); }}>{intent ? t("invoiceCorrection.recover") : t("invoiceCorrection.open")}</Button>
    <Dialog open={open} onOpenChange={(next) => { if (!busy) setOpen(next); }}>
      <DialogContent className="max-h-[85vh] overflow-y-auto">
        <DialogHeader><DialogTitle>{t("invoiceCorrection.title")}</DialogTitle><DialogDescription>{t("invoiceCorrection.help")}</DialogDescription></DialogHeader>
        {apply.data ? <div className="flex flex-col gap-3">
          <Banner tone="info">{t("invoiceCorrection.success")}</Banner>
          <Link className="text-primary" to={`/business?journal=${apply.data.journalId}#business/statements`} onClick={() => setOpen(false)}>{t("invoiceCorrection.newInvoice")}</Link>
          <Link className="text-primary" to={`/business?statement=${apply.data.statementId}#business/statements`} onClick={() => setOpen(false)}>{t("invoiceCorrection.newStatement")}</Link>
        </div> : <>
          {detail.isLoading ? <p>{t("common.loading")}</p> : null}
          {detail.error ? <Banner tone="error">{detail.error.message}</Banner> : null}
          {source?.successorJournalId && !intent ? <Link className="text-primary" to={`/business?journal=${source.successorJournalId}#business/statements`} onClick={() => setOpen(false)}>{t("invoiceCorrection.successor")}</Link> : null}
          {!intent && !source?.successorJournalId ? charges.map((line) => <Field key={line.lineId} label={line.description} hint={t("invoiceCorrection.exactInput")}>
            <TextInput aria-label={t("invoiceCorrection.amount")} inputMode="decimal" value={values[line.lineId] ?? format(line.amountMinor)} disabled={busy}
              onChange={(e) => { setValues({ ...values, [line.lineId]: e.target.value }); preview.reset(); apply.reset(); }} />
          </Field>) : null}
          {!intent && !valid && source ? <Banner tone="warn">{t("invoiceCorrection.invalid")}</Banner> : null}
          {!intent && !source?.successorJournalId ? <Button disabled={!valid || busy} onClick={() => preview.mutate({ journalId, lines: selections.map((line) => ({ sourceLineId: line.sourceLineId, amountMinor: line.amountMinor! })) })}>{t("invoiceCorrection.preview")}</Button> : null}
          {reviewed ? <div className="flex flex-col gap-3">
            <p>{t("invoiceCorrection.period")}: {reviewed.serviceStart} → {reviewed.serviceEnd}</p>
            <p>{t("invoiceCorrection.oldPayable")}: <Cr minor={reviewed.originalPayableMinor} /></p>
            <p>{t("invoiceCorrection.newDebt")}: <Cr minor={reviewed.successorReceivableMinor} /></p>
            <p>{t("invoiceCorrection.earned")}: <Cr minor={reviewed.successorEarnedMinor} /> · {t("invoiceCorrection.deferred")}: <Cr minor={reviewed.successorDeferredMinor} /></p>
            <Banner tone="info">{t("invoiceCorrection.cashless")}</Banner>
            {reviewed.lines.map((line) => <p key={line.sourceLineId} className="text-sm">{line.description}: <Cr minor={line.originalMinor} /> → <Cr minor={line.correctedMinor} /></p>)}
            <Field label={t("biz.reason")}><TextInput value={intent?.request.reason ?? reason} disabled={busy || !!intent} maxLength={500} onChange={(e) => setReason(e.target.value)} /></Field>
            {intent ? <Banner tone="warn">{t("invoiceCorrection.retained")}</Banner> : null}
            <Button isLoading={apply.isPending} disabled={busy || (!intent && !reason.trim())} onClick={submit}>{intent ? t("invoiceCorrection.recover") : t("invoiceCorrection.confirm")}</Button>
          </div> : null}
          {preview.error ? <Banner tone="error">{preview.error.message}</Banner> : null}
          {apply.error ? <Banner tone="error">{apply.error.message}</Banner> : null}
          {localError ? <Banner tone="error">{localError}</Banner> : null}
        </>}
        <Button tone="ghost" disabled={busy} onClick={() => setOpen(false)}>{t("invoiceCorrection.close")}</Button>
      </DialogContent>
    </Dialog>
  </>;
}
