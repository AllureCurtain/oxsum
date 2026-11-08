import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useApplications, useCreateApplication, type Application, type ApplicationRequest } from "../../api/hooks/applications";
import { useBusinessCustomers, useCreateIntegrationCredential, useIntegrationCredentials, useRevokeIntegrationCredential, type IntegrationCredential } from "../../api/hooks/business";
import { credentialRequest, grantScopes, type CredentialRequest, type GrantDraft, type GrantMode } from "../../lib/integration-grant";
import { AlertDialog, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle, Badge, Banner, Button, Card, EmptyRows, Field, Select, TableShell, Td, Textarea, TextInput } from "../../components/ui";

function PageButtons({ next, onPage }: { next?: string | null; onPage: (cursor?: string) => void }) {
  const { t } = useTranslation();
  return <div className="mt-3 flex gap-2"><Button type="button" tone="ghost" onClick={() => onPage()}>{t("biz.applicationAccess.firstPage")}</Button><Button type="button" tone="ghost" disabled={!next} onClick={() => onPage(next ?? undefined)}>{t("biz.applicationAccess.nextPage")}</Button></div>;
}

function Applications({ selected, onSelect }: { selected: Application | null; onSelect: (app: Application) => void }) {
  const { t } = useTranslation();
  const [after, setAfter] = useState<string>();
  const apps = useApplications(after);
  const create = useCreateApplication();
  const busy = useRef(false);
  const [key, setKey] = useState("");
  const [name, setName] = useState("");
  const [limit, setLimit] = useState("1000");
  const [reason, setReason] = useState("");
  const [submitted, setSubmitted] = useState<ApplicationRequest | null>(null);
  const send = async (body: ApplicationRequest) => {
    if (busy.current) return;
    busy.current = true;
    try { const made = await create.mutateAsync(body); onSelect(made); } catch { /* Mutation state retains the error and exact request. */ } finally { busy.current = false; }
  };
  return <Card title={t("biz.applicationAccess.applications")}>
    <p className="mb-3 text-sm text-ink-secondary">{t("biz.applicationAccess.applicationHelp")}</p>
    <form className="mb-4 flex flex-col gap-3" onSubmit={(event) => {
      event.preventDefault();
      if (busy.current || create.isSuccess) return;
      if (submitted) { void send(submitted); return; }
      if (!/^[1-9][0-9]{0,4}$/.test(limit) || Number(limit) > 10000) return;
      const body: ApplicationRequest = { idempotencyKey: crypto.randomUUID(), key, name: name.trim(), customerLimit: Number(limit), reason: reason.trim() };
      setSubmitted(body); void send(body);
    }}>
      <fieldset disabled={submitted !== null} className="grid gap-3 sm:grid-cols-2">
        <Field label={t("biz.applicationAccess.applicationKey")}><TextInput value={key} onChange={(event) => setKey(event.target.value)} required maxLength={80} pattern="[a-z0-9]+(-[a-z0-9]+)*" autoComplete="off" /></Field>
        <Field label={t("biz.integrations.colName")}><TextInput value={name} onChange={(event) => setName(event.target.value)} required maxLength={80} /></Field>
        <Field label={t("biz.applicationAccess.limit")}><TextInput value={limit} onChange={(event) => setLimit(event.target.value)} required inputMode="numeric" pattern="[1-9][0-9]{0,4}" /></Field>
        <Field label={t("biz.reason")}><Textarea value={reason} onChange={(event) => setReason(event.target.value)} required maxLength={500} /></Field>
      </fieldset>
      {create.error ? <Banner tone="error">{create.error.message}</Banner> : null}
      {create.isSuccess ? <p role="status">{t("biz.applicationAccess.created")}</p> : <Button type="submit" isLoading={create.isPending}>{t(submitted ? "biz.applicationAccess.retry" : "biz.applicationAccess.create")}</Button>}
      {submitted ? <><p className="text-sm text-ink-secondary">{t("biz.applicationAccess.frozen")}</p><Button type="button" tone="ghost" disabled={create.isPending} onClick={() => { setSubmitted(null); create.reset(); }}>{t("biz.applicationAccess.newIntent")}</Button></> : null}
    </form>
    {apps.isError ? <Banner tone="error">{t("biz.loadError")} <Button type="button" tone="ghost" onClick={() => void apps.refetch()}>{t("biz.provider.retry")}</Button></Banner> : <>
      <TableShell head={[t("biz.integrations.colName"), t("biz.applicationAccess.applicationKey"), t("biz.applicationAccess.limit"), ""]} loading={apps.isLoading ? 3 : 0}>
        {apps.data?.applications.map((app) => <tr key={app.applicationId}><td>{app.name}</td><Td mono>{app.key}</Td><Td mono>{app.customerLimit}</Td><td><Button type="button" tone={selected?.applicationId === app.applicationId ? "primary" : "ghost"} onClick={() => onSelect(app)}>{t("biz.applicationAccess.select")}</Button></td></tr>)}
        {!apps.isLoading && !apps.data?.applications.length ? <EmptyRows colSpan={4}>{t("biz.applicationAccess.empty")}</EmptyRows> : null}
      </TableShell><PageButtons next={apps.data?.nextCursor} onPage={setAfter} />
    </>}
  </Card>;
}

function CustomerChoice({ choices, onChange }: { choices: Map<string, string>; onChange: (choices: Map<string, string>) => void }) {
  const { t } = useTranslation();
  const [after, setAfter] = useState<string>();
  const customers = useBusinessCustomers(after);
  return <div className="flex flex-col gap-2">
    <p>{t("biz.applicationAccess.selectedCount", { count: choices.size })}</p>
    {Array.from(choices, ([id, name]) => <label key={id} className="flex items-center gap-2"><input type="checkbox" checked onChange={() => { const next = new Map(choices); next.delete(id); onChange(next); }} />{name}</label>)}
    {customers.isError ? <Banner tone="error">{t("biz.loadError")} <Button type="button" tone="ghost" onClick={() => void customers.refetch()}>{t("biz.provider.retry")}</Button></Banner> : customers.isLoading ? <p role="status">{t("common.loading")}</p> : <>
      {customers.data?.customers.filter((customer) => !choices.has(customer.organizationId)).map((customer) => <label key={customer.organizationId} className="flex items-center gap-2"><input type="checkbox" checked={false} disabled={choices.size >= 1000} onChange={() => onChange(new Map(choices).set(customer.organizationId, `${customer.name} (${customer.externalId})`))} />{customer.name} <span className="text-sm text-ink-secondary">{customer.externalId}</span></label>)}
      {!customers.data?.customers.length ? <p>{t("biz.applicationAccess.noCustomers")}</p> : null}
      <PageButtons next={customers.data?.nextCursor} onPage={setAfter} />
    </>}
  </div>;
}

function GrantForm({ application }: { application: Application | null }) {
  const { t } = useTranslation();
  const create = useCreateIntegrationCredential();
  const busy = useRef(false);
  const [mode, setMode] = useState<GrantMode>("application");
  const [name, setName] = useState("");
  const [scopes, setScopes] = useState<GrantDraft["scopes"]>(["customersRead"]);
  const [customers, setCustomers] = useState(new Map<string, string>());
  const [days, setDays] = useState("30");
  const [reason, setReason] = useState("");
  const [submitted, setSubmitted] = useState<CredentialRequest | null>(null);
  const [submittedLabel, setSubmittedLabel] = useState("");
  const [invalid, setInvalid] = useState(false);
  const [copyStatus, setCopyStatus] = useState("");
  return <Card title={t("biz.integrations.credentialsTitle")}>
    <form className="flex flex-col gap-3" onSubmit={async (event) => {
      event.preventDefault();
      if (busy.current || create.isSuccess) return;
      let body = submitted;
      if (!body) {
        try { body = credentialRequest({ mode, applicationId: application?.applicationId ?? "", customerIds: [...customers.keys()], scopes, name, days, reason }, crypto.randomUUID(), Date.now()); }
        catch { setInvalid(true); return; }
        setInvalid(false); setSubmitted(body);
        setSubmittedLabel(mode === "application" ? `${application?.name} (${application?.key})` : mode === "customers" ? [...customers.values()].join(", ") : t("biz.applicationAccess.provision"));
      }
      busy.current = true;
      try { await create.mutateAsync(body); } catch { /* Keep the body, absolute expiry and operation key for retry. */ } finally { busy.current = false; }
    }}>
      <fieldset disabled={submitted !== null} className="flex flex-col gap-3">
        <Field label={t("biz.applicationAccess.grantKind")}><Select value={mode} onChange={(event) => { setMode(event.target.value as GrantMode); setScopes(["customersRead"]); }}><option value="application">{t("biz.applicationAccess.application")}</option><option value="customers">{t("biz.applicationAccess.customers")}</option><option value="provision">{t("biz.applicationAccess.provision")}</option></Select></Field>
        {mode === "application" ? <p>{submitted ? submittedLabel : application ? t("biz.applicationAccess.selected", { name: application.name, key: application.key }) : t("biz.applicationAccess.selectFirst")}</p> : mode === "customers" ? <CustomerChoice choices={customers} onChange={setCustomers} /> : <p className="text-sm text-ink-secondary">{t("biz.applicationAccess.provisionHelp")}</p>}
        {mode !== "provision" ? <fieldset className="flex flex-col gap-2"><legend className="mb-2 text-sm font-medium">{t("biz.integrations.colScopes")}</legend>{grantScopes.filter((scope) => mode === "application" || scope !== "customersLifecycleWrite").map((scope) => <label key={scope} className="flex items-start gap-2"><input type="checkbox" className="mt-1" checked={scopes.includes(scope)} onChange={(event) => setScopes(event.target.checked ? [...scopes, scope] : scopes.filter((entry) => entry !== scope))} /><span>{t(`biz.applicationAccess.scopes.${scope}`)}</span></label>)}</fieldset> : null}
        <Field label={t("biz.integrations.colName")}><TextInput value={name} onChange={(event) => setName(event.target.value)} maxLength={80} /></Field>
        <Field label={t("biz.applicationAccess.days")}><TextInput required inputMode="numeric" pattern="[1-9][0-9]{0,2}" value={days} onChange={(event) => setDays(event.target.value)} /></Field>
        <Field label={t("biz.reason")}><Textarea required maxLength={500} value={reason} onChange={(event) => setReason(event.target.value)} /></Field>
      </fieldset>
      {invalid ? <Banner tone="error">{t("biz.applicationAccess.invalid")}</Banner> : null}
      {create.error ? <Banner tone="error">{create.error.message}</Banner> : null}
      {submitted ? <div className="break-words text-sm"><p>{submittedLabel}</p><p>{submitted.scopes.join(", ")} · {submitted.expiresAt}</p><p className="text-ink-secondary">{t("biz.applicationAccess.frozen")}</p></div> : null}
      {!create.isSuccess ? <Button type="submit" isLoading={create.isPending} disabled={mode === "application" && !application && !submitted}>{t(submitted ? "biz.applicationAccess.retry" : "biz.integrations.mint")}</Button> : <Banner tone="warn">{create.data.secret ? <><p>{t("biz.integrations.secretOnce")}</p><code className="block break-all">{create.data.secret}</code><Button type="button" tone="ghost" onClick={async () => { try { await navigator.clipboard.writeText(create.data.secret!); setCopyStatus(t("biz.applicationAccess.copied")); } catch { setCopyStatus(t("biz.applicationAccess.copyFailed")); } }}>{t("common.copy")}</Button><span role="status">{copyStatus}</span></> : <p>{t("biz.applicationAccess.secretUnavailable", { id: create.data.credentialId })}</p>}</Banner>}
      {submitted ? <Button type="button" tone="ghost" disabled={create.isPending} onClick={() => { setSubmitted(null); setSubmittedLabel(""); setCopyStatus(""); create.reset(); }}>{t("biz.applicationAccess.newIntent")}</Button> : null}
    </form>
  </Card>;
}

function Revoke({ credential }: { credential: IntegrationCredential }) {
  const { t } = useTranslation();
  const revoke = useRevokeIntegrationCredential();
  const [open, setOpen] = useState(false);
  const [reason, setReason] = useState("");
  const [submitted, setSubmitted] = useState<{ credentialId: string; idempotencyKey: string; reason: string } | null>(null);
  const busy = useRef(false);
  return <><Button type="button" tone="danger" onClick={() => setOpen(true)}>{t("biz.integrations.revoke")}</Button><AlertDialog open={open} onOpenChange={(next) => { if (!revoke.isPending) setOpen(next); }}><AlertDialogContent><AlertDialogHeader><AlertDialogTitle>{t("biz.integrations.revokeTitle")}</AlertDialogTitle><AlertDialogDescription>{credential.name ?? credential.prefix}: {t("biz.integrations.revokeExplain")}</AlertDialogDescription></AlertDialogHeader><Field label={t("biz.reason")}><Textarea required maxLength={500} disabled={submitted !== null} value={reason} onChange={(event) => setReason(event.target.value)} /></Field>{revoke.error ? <Banner tone="error">{revoke.error.message}</Banner> : null}<AlertDialogFooter><Button type="button" tone="ghost" disabled={revoke.isPending} onClick={() => setOpen(false)}>{t("common.cancel")}</Button><Button type="button" tone="danger" isLoading={revoke.isPending} disabled={!reason.trim()} onClick={async () => { if (busy.current) return; const body = submitted ?? { credentialId: credential.credentialId, idempotencyKey: crypto.randomUUID(), reason: reason.trim() }; setSubmitted(body); busy.current = true; try { await revoke.mutateAsync(body); setOpen(false); } catch { /* Preserve the exact revocation across dialog reopen/retry. */ } finally { busy.current = false; } }}>{t(submitted ? "biz.applicationAccess.retry" : "biz.integrations.revoke")}</Button></AlertDialogFooter></AlertDialogContent></AlertDialog></>;
}

function CredentialList() {
  const { t } = useTranslation();
  const [after, setAfter] = useState<string>();
  const credentials = useIntegrationCredentials(after);
  return <Card title={t("biz.applicationAccess.issued")}>
    {credentials.isError ? <Banner tone="error">{t("biz.loadError")} <Button type="button" tone="ghost" onClick={() => void credentials.refetch()}>{t("biz.provider.retry")}</Button></Banner> : <>
      <TableShell head={[t("biz.integrations.colName"), t("biz.applicationAccess.grantKind"), t("biz.integrations.colScopes"), t("biz.integrations.colExpires"), t("common.status"), ""]} loading={credentials.isLoading ? 3 : 0}>
        {credentials.data?.credentials.map((credential) => {
          const state = credential.revokedAt ? "revoked" : Date.parse(credential.expiresAt) <= Date.now() ? "expired" : "active";
          return <tr key={credential.credentialId}><td>{credential.name ?? "—"}<div className="font-mono text-xs">{credential.prefix}</div></td><Td mono>{credential.applicationId ?? t("biz.applicationAccess.selectedCount", { count: credential.customerIds.length })}</Td><td>{credential.scopes.map((scope) => <div key={scope}>{t(`biz.applicationAccess.scopes.${scope}`)}</div>)}</td><Td mono>{credential.expiresAt}</Td><td><Badge tone={state === "active" ? "success" : "secondary"}>{t(`biz.applicationAccess.${state}`)}</Badge></td><td>{state === "active" ? <Revoke credential={credential} /> : null}</td></tr>;
        })}
        {!credentials.isLoading && !credentials.data?.credentials.length ? <EmptyRows colSpan={6}>{t("biz.integrations.noCredentials")}</EmptyRows> : null}
      </TableShell><PageButtons next={credentials.data?.nextCursor} onPage={setAfter} />
    </>}
  </Card>;
}

export function ApplicationAccess() {
  const [application, setApplication] = useState<Application | null>(null);
  return <><Applications selected={application} onSelect={setApplication} /><GrantForm application={application} /><CredentialList /></>;
}
