import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useBusiness, useCapability } from "../../api/hooks";
import {
  useCreateCustomerInvitation, useCustomerInvitations, useRegistrationPolicy,
  useRevokeCustomerInvitation, type CustomerInvitationRequest,
  type CustomerInvitationSummary, type InvitationDeliveryState,
} from "../../api/hooks/customer-invitations";
import {
  AlertDialog, AlertDialogContent, AlertDialogDescription, AlertDialogFooter,
  AlertDialogHeader, AlertDialogTitle, Badge, Banner, Button, Card, EmptyRows,
  Field, NativeSelect, TableShell, Td, TextInput,
} from "../../components/ui";
import { AuditAction } from "./biz";

function mailLabelKey(state: InvitationDeliveryState): string {
  switch (state) {
    case "notRequested": return "biz.invitations.mail.notRequested";
    case "notAttempted": return "biz.invitations.mail.notAttempted";
    case "submitted": return "biz.invitations.mail.submitted";
    case "unavailable": return "biz.invitations.mail.unavailable";
    case "unknown": return "biz.invitations.mail.unknown";
  }
}

function Registration({ mode, mailConfigured }: { mode: "invite" | "open"; mailConfigured: boolean }) {
  const { t } = useTranslation();
  const canChange = useCapability(useBusiness().data, "registrationWrite");
  const change = useRegistrationPolicy();
  const target = mode === "open" ? "invite" : "open";
  return <Card title={t("biz.invitations.registrationTitle")}>
    <p className="text-sm">{t("biz.invitations.savedPolicy")} <strong>{mode === "invite" ? t("biz.invitations.modeInvite") : t("biz.invitations.modeOpen")}</strong>. {t("biz.invitations.policyNote")}</p>
    <p className="mt-2 text-sm text-muted">{mailConfigured ? t("biz.invitations.smtpConfigured") : t("biz.invitations.smtpMissing")}</p>
    {canChange ? <div className="mt-3"><AuditAction
      trigger={<Button tone="ghost">{target === "open" ? t("biz.invitations.enableOpen") : t("biz.invitations.requireInvite")}</Button>}
      title={target === "open" ? t("biz.invitations.openTitle") : t("biz.invitations.inviteTitle")}
      description={target === "open" ? t("biz.invitations.openExplain") : t("biz.invitations.inviteExplain")}
      confirmLabel={t("biz.invitations.savePolicy")} reasonLabel={t("biz.reason")} error={change.error} isPending={change.isPending}
      onConfirm={(reason, idempotencyKey) => change.mutateAsync({ mode: target, reason, idempotencyKey })}
    /></div> : <p className="mt-3 text-sm text-muted">{t("biz.invitations.ownerOnly")}</p>}
  </Card>;
}

function InviteForm({ mailConfigured }: { mailConfigured: boolean }) {
  const { t } = useTranslation();
  const create = useCreateCustomerInvitation();
  const [email, setEmail] = useState("");
  const [name, setName] = useState("");
  const [externalId, setExternalId] = useState("");
  const [reason, setReason] = useState("");
  const [sendEmail, setSendEmail] = useState(false);
  const [review, setReview] = useState<CustomerInvitationRequest | null>(null);
  const standing = useRef<CustomerInvitationRequest | null>(null);
  const [copy, setCopy] = useState("");
  const token = create.data?.token;
  const url = token ? `${window.location.origin}/app/invite?token=${encodeURIComponent(token)}` : "";
  const submit = () => {
    if (!review) return;
    create.mutate(review, { onSuccess: () => { setReview(null); standing.current = null; setCopy(""); } });
  };
  return <Card title={t("biz.invitations.inviteTitle2")}>
    <p className="mb-4 text-sm text-muted">{t("biz.invitations.inviteIntro")}</p>
    <form className="grid grid-cols-1 gap-3 sm:grid-cols-2" onSubmit={(event) => {
      event.preventDefault();
      const fields = { email, name, externalId, reason, sendEmail };
      const prior = standing.current;
      const same = prior && prior.email === email && prior.name === name && prior.externalId === externalId && prior.reason === reason && Boolean(prior.sendEmail) === sendEmail;
      const intent = { ...fields, idempotencyKey: same ? prior.idempotencyKey : crypto.randomUUID() };
      standing.current = intent;
      setReview(intent);
    }}>
      <Field label={t("biz.invitations.email")}><TextInput type="email" autoComplete="email" required maxLength={320} value={email} onChange={(e) => setEmail(e.target.value)} /></Field>
      <Field label={t("biz.invitations.customerName")}><TextInput required maxLength={80} value={name} onChange={(e) => setName(e.target.value)} /></Field>
      <Field label={t("biz.invitations.externalId")} hint={t("biz.invitations.externalIdHint")}><TextInput required maxLength={200} value={externalId} onChange={(e) => setExternalId(e.target.value)} /></Field>
      <Field label={t("biz.reason")}><TextInput required maxLength={500} value={reason} onChange={(e) => setReason(e.target.value)} /></Field>
      <Field label={t("biz.invitations.delivery")}><NativeSelect value={sendEmail ? "email" : "copy"} onChange={(e) => setSendEmail(e.target.value === "email")}>
        <option value="copy">{t("biz.invitations.copyLink")}</option><option value="email" disabled={!mailConfigured}>{t("biz.invitations.sendMail")}</option>
      </NativeSelect></Field>
      <div className="flex items-end"><Button type="submit" disabled={create.isPending}>{t("biz.invitations.review")}</Button></div>
    </form>
    {create.data ? <div className="mt-4 flex flex-col gap-3">
      <Banner tone={create.data.mailState === "submitted" || create.data.mailState === "notRequested" ? "success" : "warn"}>{t(mailLabelKey(create.data.mailState))}. {t("biz.invitations.expiresAt", { at: create.data.expiresAt })}</Banner>
      {url ? <>
        <Field label={t("biz.invitations.link")} hint={t("biz.invitations.linkHint")}><TextInput readOnly value={url} onFocus={(e) => e.target.select()} /></Field>
        <Button tone="ghost" onClick={() => { void navigator.clipboard.writeText(url).then(() => setCopy(t("biz.invitations.linkCopied")), () => setCopy(t("biz.invitations.copyFailed"))); }}>{t("biz.invitations.copyLinkButton")}</Button>
        <p className="text-sm" role="status">{copy}</p>
      </> : <Banner tone="warn">{t("biz.invitations.replayWarn")}</Banner>}
    </div> : null}
    <AlertDialog open={review !== null} onOpenChange={(open) => { if (!open && !create.isPending) setReview(null); }}>
      <AlertDialogContent><AlertDialogHeader><AlertDialogTitle>{t("biz.invitations.confirmTitle", { email: review?.email })}</AlertDialogTitle><AlertDialogDescription>{review?.sendEmail ? t("biz.invitations.confirmExplainMail", { externalId: review?.externalId, name: review?.name }) : t("biz.invitations.confirmExplainCopy", { externalId: review?.externalId, name: review?.name })}</AlertDialogDescription></AlertDialogHeader>
        {create.error ? <Banner tone="error">{create.error.message}. {t("biz.invitations.retryNote")}</Banner> : null}
        <AlertDialogFooter><Button tone="ghost" disabled={create.isPending} onClick={() => setReview(null)}>{t("biz.invitations.keepEditing")}</Button><Button isLoading={create.isPending} onClick={submit}>{t("biz.invitations.create")}</Button></AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  </Card>;
}
function Revoke({ item }: { item: CustomerInvitationSummary }) {
  const { t } = useTranslation();
  const revoke = useRevokeCustomerInvitation();
  return <AuditAction trigger={<Button tone="danger">{t("biz.invitations.revoke")}</Button>}
    title={t("biz.invitations.revokeTitle", { email: item.email })} description={t("biz.invitations.revokeExplain", { externalId: item.externalId })}
    confirmLabel={t("biz.invitations.revokeConfirm")} tone="danger" reasonLabel={t("biz.reason")} error={revoke.error} isPending={revoke.isPending}
    onConfirm={(reason, idempotencyKey) => revoke.mutateAsync({ id: item.id, reason, idempotencyKey })}
  />;
}
export function CustomerInvitations() {
  const { t } = useTranslation();
  const biz = useBusiness().data;
  const canRead = useCapability(biz, "customersRead");
  const canWrite = useCapability(biz, "customersWrite");
  const [after, setAfter] = useState<string | undefined>();
  const history = useCustomerInvitations(after, canRead);
  const page = history.data;
  if (!canRead) return null;
  return <div className="flex flex-col gap-6">
    {page ? <Registration mode={page.registrationMode} mailConfigured={page.mailConfigured} /> : null}
    {canWrite && page ? <InviteForm mailConfigured={page.mailConfigured} /> : null}
    <Card title={t("biz.invitations.listTitle")} actions={<Button tone="ghost" onClick={() => void history.refetch()} isLoading={history.isFetching}>{t("common.refresh")}</Button>}>
      {history.error ? <Banner tone="error">{history.error.message}</Banner> : <TableShell
        head={[t("biz.invitations.colCustomer"), t("biz.invitations.colRecipient"), t("common.status"), t("biz.invitations.colExpires"), t("biz.invitations.colMail"), ""]} loading={history.isLoading ? 3 : 0}
      >
        {(page?.invitations ?? []).map((item) => <tr key={item.id}>
          <td><p className="font-medium">{item.name}</p><p className="mono text-xs">{item.externalId}</p></td><Td>{item.email}</Td>
          <td><Badge tone={item.state === "pending" ? "default" : "secondary"}>{item.state}</Badge></td><Td mono>{item.expiresAt}</Td>
          <Td>{t(mailLabelKey(item.mailState))}</Td><td>{canWrite && item.state === "pending" ? <Revoke item={item} /> : null}</td>
        </tr>)}
        {!history.isLoading && !page?.invitations.length ? <EmptyRows colSpan={6}>{t("biz.invitations.empty")}</EmptyRows> : null}
      </TableShell>}
      <div className="mt-3 flex gap-3">{after ? <Button tone="ghost" onClick={() => setAfter(undefined)}>{t("biz.invitations.newest")}</Button> : null}{page?.nextCursor ? <Button tone="ghost" onClick={() => setAfter(page.nextCursor ?? undefined)}>{t("biz.invitations.older")}</Button> : null}</div>
    </Card>
  </div>;
}
