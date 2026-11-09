import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { Application } from "../../api/hooks/applications";
import { useCreateNotification, useDisableNotification, useNotificationDeliveries, useNotificationEndpoints, useRetryNotification, type NotificationEndpoint, type NotificationEvent, type NotificationMutation, type NotificationRequest } from "../../api/hooks/application-notifications";
import { AlertDialog, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle, Badge, Banner, Button, Card, Checkbox, Field, Skeleton, Textarea, TextInput } from "../../components/ui";
const events: NotificationEvent[] = ["customer.changed", "key.changed", "entitlement.changed", "order.changed", "statement.changed", "evidence.published", "balance.changed"];
function Endpoint({ app, endpoint }: { app: string; endpoint: NotificationEndpoint }) {
  const { t } = useTranslation();
  const [after, setAfter] = useState<string>();
  const rows = useNotificationDeliveries(app, endpoint.endpointId, after);
  const disable = useDisableNotification(app, endpoint.endpointId);
  const retry = useRetryNotification(app, endpoint.endpointId);
  const [reason, setReason] = useState("");
  const [confirmDisable, setConfirmDisable] = useState(false);
  const [intent, setIntent] = useState<(NotificationMutation & { delivery?: string }) | null>(null);
  const busy = useRef(false);
  const mutate = async (delivery?: string) => {
    if (busy.current || !reason.trim()) return;
    const body = intent ?? { idempotencyKey: crypto.randomUUID(), reason: reason.trim(), delivery };
    if (body.delivery !== delivery) return;
    setIntent(body); busy.current = true;
    try { if (delivery) await retry.mutateAsync({ idempotencyKey: body.idempotencyKey, reason: body.reason, delivery }); else await disable.mutateAsync({ idempotencyKey: body.idempotencyKey, reason: body.reason }); setIntent(null); setReason(""); setConfirmDisable(false); } catch { /* Keep exact action for retries. */ } finally { busy.current = false; }
  };
  return <div className="border-t border-border py-4">
    <p className="break-all">{endpoint.url} <Badge tone={endpoint.enabled ? "success" : "secondary"}>{t(endpoint.enabled ? "biz.applicationAccess.active" : "biz.notifications.disabled")}</Badge></p>
    <p className="my-2 text-sm text-ink-secondary">{endpoint.events.map(event => t(`biz.notifications.eventsList.${event.replace(".", "_")}`)).join(", ")} · …{endpoint.secretLast4}</p>
    <Field label={t("biz.reason")}><Textarea value={reason} disabled={intent !== null} onChange={e => setReason(e.target.value)} maxLength={500} /></Field>
    <p className="my-2 text-sm text-ink-secondary">{t("biz.notifications.disableHelp")}</p>
    {endpoint.enabled ? <Button type="button" tone="danger" disabled={!reason.trim() || (intent?.delivery !== undefined)} isLoading={disable.isPending} onClick={() => setConfirmDisable(true)}>{t("biz.notifications.disable")}</Button> : null}
    <AlertDialog open={confirmDisable} onOpenChange={next => { if (!disable.isPending) setConfirmDisable(next); }}><AlertDialogContent><AlertDialogHeader><AlertDialogTitle>{t("biz.notifications.disable")}</AlertDialogTitle><AlertDialogDescription className="break-all">{endpoint.url}: {t("biz.notifications.disableHelp")}</AlertDialogDescription></AlertDialogHeader>{disable.error ? <Banner tone="error">{disable.error.message}</Banner> : null}<AlertDialogFooter><Button type="button" tone="ghost" disabled={disable.isPending} onClick={() => setConfirmDisable(false)}>{t("common.cancel")}</Button><Button type="button" tone="danger" isLoading={disable.isPending} onClick={() => void mutate()}>{t(intent ? "biz.applicationAccess.retry" : "biz.notifications.disable")}</Button></AlertDialogFooter></AlertDialogContent></AlertDialog>
    {disable.error || retry.error ? <Banner tone="error">{disable.error?.message ?? retry.error?.message}</Banner> : null}
    {intent ? <div className="mt-2 space-y-2"><p className="text-sm">{t("biz.applicationAccess.frozen")}</p>{intent.delivery ? <Button type="button" isLoading={retry.isPending} onClick={() => void mutate(intent.delivery)}>{t("biz.applicationAccess.retry")}</Button> : null}<Button type="button" tone="ghost" disabled={disable.isPending || retry.isPending} onClick={() => { setIntent(null); disable.reset(); retry.reset(); }}>{t("biz.applicationAccess.newIntent")}</Button></div> : null}
    {rows.isError ? <Banner tone="error">{t("biz.loadError")} <Button tone="ghost" onClick={() => void rows.refetch()}>{t("biz.provider.retry")}</Button></Banner> : rows.isLoading ? <Skeleton className="mt-3 h-24" role="status" aria-label={t("common.loading")} /> : <ul className="mt-3 space-y-2">
      {rows.data?.deliveries.map(row => <li key={row.deliveryId} className="break-words text-sm"><span>{row.eventType} · {t(`biz.notifications.status.${row.status}`)} · {t("biz.notifications.attempts", { count: row.attempts })}</span><p className="font-mono text-xs">{row.organizationId} · {row.deliveryId}</p>{row.lastError ? <p>{row.lastError}</p> : null}{row.status === "failed" && endpoint.enabled ? <Button tone="ghost" disabled={!reason.trim() || (intent !== null && intent.delivery !== row.deliveryId)} isLoading={retry.isPending && intent?.delivery === row.deliveryId} onClick={() => void mutate(row.deliveryId)}>{t("biz.notifications.retry")}</Button> : null}</li>)}
      {!rows.data?.deliveries.length ? <li>{t("biz.notifications.emptyDeliveries")}</li> : null}
    </ul>}
    <div className="mt-3 flex gap-2"><Button tone="ghost" onClick={() => setAfter(undefined)}>{t("biz.applicationAccess.firstPage")}</Button><Button tone="ghost" disabled={!rows.data?.nextCursor} onClick={() => setAfter(rows.data?.nextCursor ?? undefined)}>{t("biz.applicationAccess.nextPage")}</Button></div>
  </div>;
}
export function ApplicationNotifications({ application }: { application: Application }) {
  const { t } = useTranslation();
  const list = useNotificationEndpoints(application.applicationId);
  const create = useCreateNotification(application.applicationId);
  const [url, setUrl] = useState(""); const [reason, setReason] = useState("");
  const [internal, setInternal] = useState(false); const [ips, setIps] = useState("");
  const [selected, setSelected] = useState<NotificationEvent[]>([...events]);
  const [intent, setIntent] = useState<NotificationRequest | null>(null);
  const [copied, setCopied] = useState(""); const busy = useRef(false);
  return <Card title={t("biz.notifications.title", { name: application.name })}>
    <p className="mb-3 text-sm text-ink-secondary">{t("biz.notifications.help")}</p>
    <form className="flex flex-col gap-3" onSubmit={async e => { e.preventDefault(); if (busy.current || create.isSuccess) return; const body = intent ?? { idempotencyKey: crypto.randomUUID(), reason: reason.trim(), url, events: selected, destinationPolicy: { network: internal ? "internal" as const : "public" as const, allowedIps: internal ? ips.split(",").map(v => v.trim()).filter(Boolean) : [] } }; setIntent(body); busy.current = true; try { await create.mutateAsync(body); } catch { /* Keep original destination and operation. */ } finally { busy.current = false; } }}>
      <fieldset disabled={intent !== null} className="flex flex-col gap-3">
        <Field label={t("biz.notifications.url")}><TextInput type="url" required maxLength={2000} value={url} onChange={e => setUrl(e.target.value)} /></Field>
        <label className="flex items-center gap-2"><Checkbox checked={internal} onCheckedChange={v => setInternal(v === true)} />{t("biz.notifications.internal")}</label>
        {internal ? <Field label={t("biz.notifications.ips")}><TextInput required value={ips} onChange={e => setIps(e.target.value)} /></Field> : null}
        <fieldset className="flex flex-col gap-2"><legend>{t("biz.notifications.events")}</legend>{events.map(event => <label key={event} className="flex items-center gap-2"><Checkbox checked={selected.includes(event)} onCheckedChange={v => setSelected(v === true ? [...selected, event] : selected.filter(x => x !== event))} />{t(`biz.notifications.eventsList.${event.replace(".", "_")}`)}</label>)}</fieldset>
        <Field label={t("biz.reason")}><Textarea required maxLength={500} value={reason} onChange={e => setReason(e.target.value)} /></Field>
      </fieldset>
      {create.error ? <Banner tone="error">{create.error.message}</Banner> : null}
      {!create.isSuccess ? <Button type="submit" isLoading={create.isPending} disabled={!selected.length}>{t(intent ? "biz.applicationAccess.retry" : "biz.notifications.create")}</Button> : <Banner tone="warn">{create.data.secret ? <><p>{t("biz.integrations.secretOnce")}</p><code className="break-all">{create.data.secret}</code><Button type="button" tone="ghost" onClick={async () => { try { await navigator.clipboard.writeText(create.data.secret!); setCopied(t("biz.applicationAccess.copied")); } catch { setCopied(t("biz.applicationAccess.copyFailed")); } }}>{t("common.copy")}</Button><p role="status">{copied}</p></> : <p>{t("biz.notifications.secretUnavailable")}</p>}</Banner>}
      {intent ? <><p className="text-sm">{t("biz.applicationAccess.frozen")}</p><Button type="button" tone="ghost" disabled={create.isPending} onClick={() => { setIntent(null); create.reset(); setCopied(""); }}>{t("biz.applicationAccess.newIntent")}</Button></> : null}
    </form>
    {list.isError ? <Banner tone="error">{t("biz.loadError")} <Button tone="ghost" onClick={() => void list.refetch()}>{t("biz.provider.retry")}</Button></Banner> : list.isLoading ? <Skeleton className="mt-3 h-24" role="status" aria-label={t("common.loading")} /> : list.data?.endpoints.length ? list.data.endpoints.map(endpoint => <Endpoint key={endpoint.endpointId} app={application.applicationId} endpoint={endpoint} />) : <p className="mt-3 text-sm text-ink-secondary">{t("biz.notifications.emptyEndpoints")}</p>}
  </Card>;
}
