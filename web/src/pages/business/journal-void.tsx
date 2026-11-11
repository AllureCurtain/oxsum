import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useVoidJournal } from "../../api/hooks";
import type { components } from "../../api/schema";
import {
  AlertDialog, AlertDialogContent, AlertDialogDescription, AlertDialogFooter,
  AlertDialogHeader, AlertDialogTitle, Banner, Button, Field, Textarea,
} from "../../components/ui";

type Decision = { journalId: string; idempotencyKey: string; reason: string };

/** Retain a source-specific financial correction until its response is confirmed. */
export function JournalVoid({ journal }: { journal: components["schemas"]["InvoiceJournal"] }) {
  const { t } = useTranslation();
  const mutation = useVoidJournal();
  const busy = useRef(false);
  const intent = useRef<Decision | null>(null);
  const [open, setOpen] = useState(false);
  const [reason, setReason] = useState("");
  const [submitted, setSubmitted] = useState<Decision | null>(null);
  const send = async () => {
    if (busy.current || !reason.trim()) return;
    const body = intent.current ?? { journalId: journal.journalId, idempotencyKey: crypto.randomUUID(), reason: reason.trim() };
    intent.current = body;
    setSubmitted(body);
    busy.current = true;
    try {
      await mutation.mutateAsync(body);
      intent.current = null;
      setSubmitted(null);
      setReason("");
      setOpen(false);
    } catch { /* Retain the visible error and exact request for retry. */ }
    finally { busy.current = false; }
  };
  if (journal.state !== "posted" && journal.state !== "paid" && !submitted) return null;
  return <>
    <Button tone="danger" onClick={() => setOpen(true)}>{t(submitted ? "biz.journals.retryVoid" : "biz.journals.void")}</Button>
    <AlertDialog open={open} onOpenChange={(next) => { if (!busy.current) setOpen(next); }}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{t("biz.journals.voidTitle")}</AlertDialogTitle>
          <AlertDialogDescription>{journal.journalId}: {t("biz.journals.voidExplain")}</AlertDialogDescription>
        </AlertDialogHeader>
        <Field label={t("biz.reason")}><Textarea value={reason} disabled={submitted !== null || mutation.isPending} onChange={(event) => setReason(event.target.value)} required maxLength={500} /></Field>
        {submitted ? <p className="text-sm text-muted">{t("biz.journals.frozenVoid")}</p> : null}
        {mutation.error ? <Banner tone="error">{mutation.error.message}</Banner> : null}
        <AlertDialogFooter>
          <Button tone="ghost" disabled={mutation.isPending} onClick={() => setOpen(false)}>{t("common.cancel")}</Button>
          <Button tone="danger" disabled={!reason.trim()} isLoading={mutation.isPending} onClick={() => void send()}>{t(submitted ? "biz.journals.retryVoid" : "biz.journals.void")}</Button>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  </>;
}
