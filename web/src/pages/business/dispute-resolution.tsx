import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useResolveDispute } from "../../api/hooks/business";
import type { components } from "../../api/schema";
import {
  AlertDialog, AlertDialogContent, AlertDialogDescription, AlertDialogFooter,
  AlertDialogHeader, AlertDialogTitle, Banner, Button, Field, Select, Textarea,
} from "../../components/ui";

type Outcome = "evidenceDue" | "won" | "lost" | "closed";
type Decision = { disputeId: string; idempotencyKey: string; resolution: Outcome; reason: string };
const outcomes: Outcome[] = ["evidenceDue", "won", "lost", "closed"];

/** An explicit financial decision retains its identity until the server confirms it. */
export function DisputeResolution({ dispute }: { dispute: components["schemas"]["OrderDispute"] }) {
  const { t } = useTranslation();
  const resolve = useResolveDispute();
  const busy = useRef(false);
  const intent = useRef<Decision | null>(null);
  const [open, setOpen] = useState(false);
  const [outcome, setOutcome] = useState<Outcome | "">("");
  const [reason, setReason] = useState("");
  const [submitted, setSubmitted] = useState<Decision | null>(null);
  const actionable = dispute.state === "opened" || dispute.state === "evidence_due";
  const send = async () => {
    if (busy.current || !outcome || !reason.trim()) return;
    const body = intent.current ?? { disputeId: dispute.disputeId, idempotencyKey: crypto.randomUUID(), resolution: outcome, reason: reason.trim() };
    intent.current = body;
    setSubmitted(body);
    busy.current = true;
    try {
      await resolve.mutateAsync(body);
      intent.current = null;
      setSubmitted(null);
      setOutcome("");
      setReason("");
      setOpen(false);
    } catch { /* The visible error and immutable intent survive dialog reopen. */ }
    finally { busy.current = false; }
  };
  if (!actionable && !submitted) return null;
  return <>
    <Button tone="ghost" onClick={(event) => { event.stopPropagation(); setOpen(true); }}>{t(submitted ? "biz.disputes.retryResolution" : "biz.disputes.resolve")}</Button>
    <AlertDialog open={open} onOpenChange={(next) => { if (!busy.current) setOpen(next); }}>
      <AlertDialogContent onClick={(event) => event.stopPropagation()}>
        <AlertDialogHeader>
          <AlertDialogTitle>{t("biz.disputes.resolveTitle")}</AlertDialogTitle>
          <AlertDialogDescription>{dispute.providerDisputeId}: {t("biz.disputes.resolveExplain")}</AlertDialogDescription>
        </AlertDialogHeader>
        <Field label={t("biz.disputes.outcome")}><Select value={outcome} disabled={submitted !== null || resolve.isPending} onChange={(event) => setOutcome(event.target.value as Outcome | "")} required>
          <option value="">{t("biz.disputes.chooseOutcome")}</option>
          {outcomes.map((value) => <option key={value} value={value}>{t(`biz.disputes.outcomes.${value}`)}</option>)}
        </Select></Field>
        {outcome ? <Banner tone={outcome === "lost" ? "warn" : "info"}>{t(`biz.disputes.effects.${outcome}`)}</Banner> : null}
        <Field label={t("biz.reason")}><Textarea value={reason} disabled={submitted !== null || resolve.isPending} onChange={(event) => setReason(event.target.value)} required maxLength={500} /></Field>
        {submitted ? <p className="text-sm text-muted">{t("biz.disputes.frozenResolution")}</p> : null}
        {resolve.error ? <Banner tone="error">{resolve.error.message}</Banner> : null}
        <AlertDialogFooter>
          <Button tone="ghost" disabled={resolve.isPending} onClick={() => setOpen(false)}>{t("common.cancel")}</Button>
          <Button tone={outcome === "lost" ? "danger" : "primary"} disabled={!outcome || !reason.trim()} isLoading={resolve.isPending} onClick={() => void send()}>{t(submitted ? "biz.disputes.retryResolution" : "biz.disputes.resolve")}</Button>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  </>;
}
