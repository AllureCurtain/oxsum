import { useState } from "react";
import { useTranslation } from "react-i18next";
import { ApiError } from "../../api/client";
import { idem, useMonthlyPayment, type MonthlyPaymentRequest, type MonthlyPaymentResult, type BusinessMonthlyStatementDetail } from "../../api/hooks";
import { decimalMinor } from "../../lib/money";
import { AlertDialog, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle, Banner, Button, Cr, Field, Textarea, TextInput } from "../../components/ui";
import { DetailGrid, Field2 } from "./biz";

export function MonthlyPayment({ customer, name, detail, active }: { customer: string; name: string; detail: BusinessMonthlyStatementDetail; active: boolean }) {
  const { t } = useTranslation();
  const mutation = useMonthlyPayment(customer, detail.statement.id);
  const [amount, setAmount] = useState("");
  const [reference, setReference] = useState("");
  const [reason, setReason] = useState("");
  const [intent, setIntent] = useState<MonthlyPaymentRequest | null>(null);
  const [attempted, setAttempted] = useState(false);
  const [open, setOpen] = useState(false);
  const [saved, setSaved] = useState<MonthlyPaymentResult>();
  const minor = decimalMinor(amount, 6);
  const frozen = attempted && intent !== null;
  const editable = active && detail.statement.status === "finalized" && detail.statement.paymentStatus !== "paid";
  const valid = minor !== null && reference.trim().length > 0 && reference === reference.trim() && !/[\x00-\x1f\x7f]/.test(reference) && reason.trim().length > 0;
  const submit = async () => {
    if (!intent) return;
    setAttempted(true);
    try {
      const receipt = await mutation.mutateAsync(intent);
      setSaved(receipt); setOpen(false); setIntent(null); setAttempted(false); setAmount(""); setReference(""); setReason("");
    } catch (error) {
      if (error instanceof ApiError && ((error.status === 400 && error.code === "VALIDATION_ERROR") || (error.status === 409 && error.message === "payment reference is already recorded"))) {
        setAttempted(false); setIntent(null); setOpen(false);
      }
    }
  };
  if (!editable && !frozen && !saved) return null;
  return <section className="flex flex-col gap-3 border-t border-line pt-4" aria-label={t("monthlyPayment.title")}>
    <h3 className="font-medium">{t("monthlyPayment.title")}</h3>
    <p className="text-sm text-muted">{t("monthlyPayment.hint")}</p>
    {saved ? <div className="flex flex-col gap-3">
      <Banner tone="success">{t("monthlyPayment.success")}</Banner>
      <DetailGrid>
        <Field2 label={t("common.amount")}><Cr minor={saved.amountMinor} /></Field2>
        <Field2 label={t("monthlyPayment.reference")}><span className="break-all">{saved.reference}</span></Field2>
        <Field2 label={t("monthlyPayment.entry")}><span className="break-all font-mono text-xs">{saved.entryId}</span></Field2>
        <Field2 label={t("monthlyPayment.hash")}><span className="break-all font-mono text-xs">{saved.contentHash}</span></Field2>
      </DetailGrid>
    </div> : null}
    {editable || frozen ? <form className="flex flex-col gap-3" onSubmit={event => {
      event.preventDefault(); if (!valid && !frozen) return;
      if (!intent) setIntent({ amountMinor: minor!, reference, reason: reason.trim(), idempotencyKey: idem() });
      setOpen(true); mutation.reset();
    }}>
      <Field label={t("monthlyPayment.amount")} hint={t("monthlyPayment.amountHint")}><TextInput inputMode="decimal" value={amount} maxLength={40} required disabled={frozen || mutation.isPending} onChange={e => {setAmount(e.target.value); setIntent(null);}} /></Field>
      {amount && minor === null ? <Banner tone="warn">{t("monthlyPayment.invalidAmount")}</Banner> : null}
      <Field label={t("monthlyPayment.reference")} hint={t("monthlyPayment.referenceHint")}><TextInput value={reference} maxLength={200} required disabled={frozen || mutation.isPending} onChange={e => {setReference(e.target.value); setIntent(null);}} /></Field>
      <Field label={t("biz.reason")}><Textarea value={reason} maxLength={500} required disabled={frozen || mutation.isPending} onChange={e => {setReason(e.target.value); setIntent(null);}} /></Field>
      {frozen ? <Banner tone="warn">{t("monthlyPayment.retained")}</Banner> : null}
      {!open && mutation.error ? <Banner tone="error">{mutation.error instanceof ApiError ? mutation.error.message : t("monthlyPayment.uncertain")}</Banner> : null}
      <Button type="submit" disabled={(!valid && !frozen) || mutation.isPending}>{t(frozen ? "monthlyStatements.retry" : "monthlyPayment.review")}</Button>
    </form> : null}
    <AlertDialog open={open} onOpenChange={next => { if (!mutation.isPending) setOpen(next); }}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{t("monthlyPayment.confirm")}</AlertDialogTitle>
          <AlertDialogDescription>{t("monthlyPayment.confirmHint")}</AlertDialogDescription>
        </AlertDialogHeader>
        <DetailGrid>
          <Field2 label={t("monthlyPayment.customer")}><span className="break-words">{name}</span><span className="mt-1 block break-all font-mono text-xs">{customer}</span></Field2>
          <Field2 label={t("monthlyStatements.period")}>{detail.statement.period}</Field2>
          <Field2 label={t("common.amount")}><Cr minor={intent?.amountMinor ?? 0} /></Field2>
          <Field2 label={t("monthlyPayment.reference")}><span className="break-all">{intent?.reference}</span></Field2>
        </DetailGrid>
        <p className="break-words whitespace-pre-wrap text-sm">{intent?.reason}</p>
        {mutation.error ? <Banner tone="error">{mutation.error instanceof ApiError ? mutation.error.message : t("monthlyPayment.uncertain")}</Banner> : null}
        <AlertDialogFooter>
          <Button tone="ghost" disabled={mutation.isPending} onClick={() => setOpen(false)}>{t("common.cancel")}</Button>
          <Button isLoading={mutation.isPending} onClick={() => void submit()}>{t(frozen ? "monthlyStatements.retry" : "common.confirm")}</Button>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  </section>;
}
