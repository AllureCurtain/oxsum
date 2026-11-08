import { useRef, useState } from "react";
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

function mailLabel(state: InvitationDeliveryState): string {
  switch (state) {
    case "notRequested": return "Copy link selected; no mail requested";
    case "notAttempted": return "Mail was requested but not attempted; replace a lost link";
    case "submitted": return "Submitted to SMTP; recipient delivery is unverified";
    case "unavailable": return "Not sent: SMTP is not configured";
    case "unknown": return "Send did not confirm; delivery may have occurred. No automatic resend";
  }
}

function Registration({ mode, mailConfigured }: { mode: "invite" | "open"; mailConfigured: boolean }) {
  const canChange = useCapability(useBusiness().data, "registrationWrite");
  const change = useRegistrationPolicy();
  const target = mode === "open" ? "invite" : "open";
  return <Card title="Customer registration">
    <p className="text-sm">Saved policy: <strong>{mode === "invite" ? "Invitation only" : "Open registration"}</strong>. This controls new personal accounts; accepting an existing invitation remains available.</p>
    <p className="mt-2 text-sm text-muted">{mailConfigured ? "SMTP is configured. Individual mail submissions can still fail." : "SMTP is not configured. Copy-link invitations are available; email verification and password reset need a working mailer."}</p>
    {canChange ? <div className="mt-3"><AuditAction
      trigger={<Button tone="ghost">{target === "open" ? "Enable open registration" : "Require invitations"}</Button>}
      title={target === "open" ? "Allow anyone to create a customer account?" : "Require an invitation for new accounts?"}
      description={target === "open" ? "This changes the saved password and identity-provider registration policy. It grants no staff role or purchased funds. Confirm mail readiness before relying on email verification." : "Existing accounts and pending invitations remain usable. New public signups will be refused."}
      confirmLabel="Save registration policy" reasonLabel="Reason" error={change.error} isPending={change.isPending}
      onConfirm={(reason, idempotencyKey) => change.mutate({ mode: target, reason, idempotencyKey })}
    /></div> : <p className="mt-3 text-sm text-muted">Only the business owner can change registration policy.</p>}
  </Card>;
}

function InviteForm({ mailConfigured }: { mailConfigured: boolean }) {
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
  return <Card title="Invite a new customer">
    <p className="mb-4 text-sm text-muted">Reserve one external customer ID and invite its first owner. The recipient must use the bound email and accept within seven days. To add a teammate to an existing customer, use that customer's team invitation.</p>
    <form className="grid grid-cols-1 gap-3 sm:grid-cols-2" onSubmit={(event) => {
      event.preventDefault();
      const fields = { email, name, externalId, reason, sendEmail };
      const prior = standing.current;
      const same = prior && prior.email === email && prior.name === name && prior.externalId === externalId && prior.reason === reason && Boolean(prior.sendEmail) === sendEmail;
      const intent = { ...fields, idempotencyKey: same ? prior.idempotencyKey : crypto.randomUUID() };
      standing.current = intent;
      setReview(intent);
    }}>
      <Field label="Recipient email"><TextInput type="email" autoComplete="email" required maxLength={320} value={email} onChange={(e) => setEmail(e.target.value)} /></Field>
      <Field label="Customer name"><TextInput required maxLength={80} value={name} onChange={(e) => setName(e.target.value)} /></Field>
      <Field label="External customer ID" hint="A stable identifier for this new customer account."><TextInput required maxLength={200} value={externalId} onChange={(e) => setExternalId(e.target.value)} /></Field>
      <Field label="Reason"><TextInput required maxLength={500} value={reason} onChange={(e) => setReason(e.target.value)} /></Field>
      <Field label="Delivery"><NativeSelect value={sendEmail ? "email" : "copy"} onChange={(e) => setSendEmail(e.target.value === "email")}>
        <option value="copy">Copy a link</option><option value="email" disabled={!mailConfigured}>Submit one email to the recipient</option>
      </NativeSelect></Field>
      <div className="flex items-end"><Button type="submit" disabled={create.isPending}>Review invitation</Button></div>
    </form>
    {create.data ? <div className="mt-4 flex flex-col gap-3">
      <Banner tone={create.data.mailState === "submitted" || create.data.mailState === "notRequested" ? "success" : "warn"}>{mailLabel(create.data.mailState)}. Invitation expires {create.data.expiresAt}.</Banner>
      {url ? <>
        <Field label="Invitation link" hint="Shown once. Share only with the intended email holder; it is not recoverable from the history."><TextInput readOnly value={url} onFocus={(e) => e.target.select()} /></Field>
        <Button tone="ghost" onClick={() => { void navigator.clipboard.writeText(url).then(() => setCopy("Link copied"), () => setCopy("Copy failed. Select the link and copy it manually.")); }}>Copy invitation link</Button>
        <p className="text-sm" role="status">{copy}</p>
      </> : <Banner tone="warn">The original request already created this invitation. Its secret is not shown on replay. Use the original link or submitted mail; otherwise revoke it in the history and create a new invitation.</Banner>}
    </div> : null}
    <AlertDialog open={review !== null} onOpenChange={(open) => { if (!open && !create.isPending) setReview(null); }}>
      <AlertDialogContent><AlertDialogHeader><AlertDialogTitle>Invite {review?.email}?</AlertDialogTitle><AlertDialogDescription>Reserve {review?.externalId} for {review?.name}. {review?.sendEmail ? "Submit one email to this recipient." : "Create a copy link without sending email."} The recipient chooses to join. No staff role or purchased funds are granted.</AlertDialogDescription></AlertDialogHeader>
        {create.error ? <Banner tone="error">{create.error.message}. Retry this unchanged request to recover its result.</Banner> : null}
        <AlertDialogFooter><Button tone="ghost" disabled={create.isPending} onClick={() => setReview(null)}>Keep editing</Button><Button isLoading={create.isPending} onClick={submit}>Create invitation</Button></AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  </Card>;
}
function Revoke({ item }: { item: CustomerInvitationSummary }) {
  const revoke = useRevokeCustomerInvitation();
  return <AuditAction trigger={<Button tone="danger">Revoke</Button>}
    title={`Revoke the invitation for ${item.email}?`} description={`The unused link for ${item.externalId} will stop working and release this pending customer ID. Accepted customer accounts are managed separately.`}
    confirmLabel="Revoke invitation" tone="danger" reasonLabel="Reason" error={revoke.error} isPending={revoke.isPending}
    onConfirm={(reason, idempotencyKey) => revoke.mutate({ id: item.id, reason, idempotencyKey })}
  />;
}
export function CustomerInvitations() {
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
    <Card title="Customer invitations" actions={<Button tone="ghost" onClick={() => void history.refetch()} isLoading={history.isFetching}>Refresh</Button>}>
      {history.error ? <Banner tone="error">{history.error.message}</Banner> : <TableShell
        head={["Customer / external ID", "Recipient", "Status", "Expires (UTC)", "Mail submission", ""]} loading={history.isLoading ? 3 : 0}
      >
        {(page?.invitations ?? []).map((item) => <tr key={item.id}>
          <td><p className="font-medium">{item.name}</p><p className="mono text-xs">{item.externalId}</p></td><Td>{item.email}</Td>
          <td><Badge tone={item.state === "pending" ? "default" : "secondary"}>{item.state}</Badge></td><Td mono>{item.expiresAt}</Td>
          <Td>{mailLabel(item.mailState)}</Td><td>{canWrite && item.state === "pending" ? <Revoke item={item} /> : null}</td>
        </tr>)}
        {!history.isLoading && !page?.invitations.length ? <EmptyRows colSpan={6}>No customer invitations on this page.</EmptyRows> : null}
      </TableShell>}
      <div className="mt-3 flex gap-3">{after ? <Button tone="ghost" onClick={() => setAfter(undefined)}>Newest invitations</Button> : null}{page?.nextCursor ? <Button tone="ghost" onClick={() => setAfter(page.nextCursor ?? undefined)}>Older invitations</Button> : null}</div>
    </Card>
  </div>;
}
