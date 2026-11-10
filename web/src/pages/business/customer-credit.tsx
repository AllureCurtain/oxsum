import { useState } from "react";
import { useTranslation } from "react-i18next";
import { ApiError } from "../../api/client";
import {
  idem, useBusiness, useCapability, useCustomerCreditTerms, useSetCustomerCreditTerms,
  type BusinessCustomer, type BusinessCustomerCreditTerms, type BusinessCustomerCreditTermsRequest,
} from "../../api/hooks";
import {
  AlertDialog, AlertDialogContent, AlertDialogDescription, AlertDialogFooter,
  AlertDialogHeader, AlertDialogTitle, Banner, Button, Card, Cr, Field, Skeleton, Textarea, TextInput,
} from "../../components/ui";
import { creditLimitText, creditTermsInput } from "../../lib/customer-credit-input";
import { DetailGrid, Field2 } from "./biz";

function CreditEditor({ customer, initial }: { customer: BusinessCustomer; initial: BusinessCustomerCreditTerms }) {
  const { t } = useTranslation();
  const mutation = useSetCustomerCreditTerms(customer.organizationId);
  const [limit, setLimit] = useState(() => creditLimitText(initial.creditLimitMinor));
  const [terms, setTerms] = useState(String(initial.paymentTermsDays));
  const [grace, setGrace] = useState(initial.dunningGraceDays === null ? "" : String(initial.dunningGraceDays));
  const [reason, setReason] = useState("");
  const [intent, setIntent] = useState<BusinessCustomerCreditTermsRequest | null>(null);
  const [attempted, setAttempted] = useState(false);
  const [open, setOpen] = useState(false);
  const [saved, setSaved] = useState(false);
  const parsed = creditTermsInput(limit, terms, grace);
  const frozen = attempted && intent !== null;
  const submit = async () => {
    if (!intent) return;
    setAttempted(true);
    try {
      await mutation.mutateAsync(intent);
      setIntent(null); setAttempted(false); setOpen(false); setSaved(true);
    } catch (error) {
      // This endpoint's validation refusal rolls back the complete transaction.
      // Keep identity and fields for ambiguous transport/server outcomes.
      if (error instanceof ApiError && error.status === 400 && error.code === "VALIDATION_ERROR") setAttempted(false);
    }
  };
  return <>
    {saved ? <Banner tone="success">{t("creditTerms.saved")}</Banner> : null}
    <form className="mt-4 flex flex-col gap-3 border-t border-line pt-4" onSubmit={(event) => {
      event.preventDefault();
      if (!parsed || !reason.trim()) return;
      if (!intent) setIntent({ ...parsed, reason: reason.trim(), idempotencyKey: idem() });
      mutation.reset(); setSaved(false); setOpen(true);
    }}>
      <Field label={t("creditTerms.limit")}><TextInput inputMode="decimal" value={limit} disabled={frozen} onChange={(event) => { setLimit(event.target.value); setIntent(null); }} required /></Field>
      <Field label={t("creditTerms.terms")}><TextInput inputMode="numeric" value={terms} disabled={frozen} onChange={(event) => { setTerms(event.target.value); setIntent(null); }} required /></Field>
      <Field label={t("creditTerms.grace")} hint={t("creditTerms.graceHint")}><TextInput inputMode="numeric" value={grace} disabled={frozen} onChange={(event) => { setGrace(event.target.value); setIntent(null); }} /></Field>
      <Field label={t("biz.reason")}><Textarea value={reason} disabled={frozen} maxLength={500} onChange={(event) => { setReason(event.target.value); setIntent(null); }} required /></Field>
      {!parsed ? <Banner tone="error">{t("creditTerms.invalid")}</Banner> : null}
      {frozen ? <Banner tone="warn">{t("creditTerms.retained")}</Banner> : null}
      <Button type="submit" disabled={!parsed || !reason.trim() || mutation.isPending}>{frozen ? t("creditTerms.retry") : t("creditTerms.review")}</Button>
    </form>
    <AlertDialog open={open} onOpenChange={(next) => { if (!mutation.isPending) setOpen(next); }}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{t("creditTerms.confirm", { name: customer.name })}</AlertDialogTitle>
          <AlertDialogDescription>{t("creditTerms.explain")}</AlertDialogDescription>
        </AlertDialogHeader>
        {intent ? <DetailGrid>
          <Field2 label={t("creditTerms.limit")}><Cr minor={intent.creditLimitMinor} /></Field2>
          <Field2 label={t("creditTerms.terms")}>{intent.paymentTermsDays}</Field2>
          <Field2 label={t("creditTerms.grace")}>{intent.dunningGraceDays ?? t("creditTerms.off")}</Field2>
        </DetailGrid> : null}
        <p className="whitespace-pre-wrap break-words text-sm text-ink-secondary">{intent?.reason}</p>
        {mutation.error ? <Banner tone="error">{mutation.error instanceof ApiError ? mutation.error.message : t("creditTerms.uncertain")}</Banner> : null}
        <AlertDialogFooter>
          <Button tone="ghost" disabled={mutation.isPending} onClick={() => setOpen(false)}>{t("common.cancel")}</Button>
          <Button isLoading={mutation.isPending} onClick={() => void submit()}>{frozen ? t("creditTerms.retry") : t("common.confirm")}</Button>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  </>;
}

export function CustomerCreditCard({ customer }: { customer: BusinessCustomer }) {
  const { t } = useTranslation();
  const principal = useBusiness().data;
  const read = useCapability(principal, "reportsRead");
  const write = useCapability(principal, "commercialWrite");
  const query = useCustomerCreditTerms(customer.organizationId, read);
  return <Card title={t("creditTerms.title")}>
    <p className="mb-4 text-sm text-muted">{t("creditTerms.description")}</p>
    {query.isError ? <Banner tone="error">{t("biz.loadError")}</Banner> : null}
    {query.data ? <>
      <DetailGrid>
        <Field2 label={t("creditTerms.limit")}><Cr minor={query.data.creditLimitMinor} /></Field2>
        <Field2 label={t("creditTerms.used")}><Cr minor={query.data.creditUsedMinor} /></Field2>
        <Field2 label={t("creditTerms.terms")}>{query.data.paymentTermsDays}</Field2>
        <Field2 label={t("creditTerms.grace")}>{query.data.dunningGraceDays ?? t("creditTerms.off")}</Field2>
      </DetailGrid>
      {write && customer.state === "active" ? <CreditEditor customer={customer} initial={query.data} /> : null}
    </> : query.isLoading ? <Skeleton className="h-20 w-full" /> : null}
  </Card>;
}
