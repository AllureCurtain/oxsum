import { useEffect, useRef, useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { Landmark } from "lucide-react";
import { useBusiness, useCapability, useMerchants, useMerchantEvents, type Merchant } from "../../api/hooks";
import { useMerchantVersions, useMerchantWrite, type MerchantIntent } from "../../api/hooks/merchants";
import { merchantConfig, merchantDraft, type MerchantDraft, type MerchantRail } from "../../lib/merchant-draft";
import {
  AlertDialog, AlertDialogContent, AlertDialogDescription, AlertDialogFooter,
  AlertDialogHeader, AlertDialogTitle, Badge, Banner, Button, Card, EmptyRows,
  Field, Select, Skeleton, TableShell, Td, TextInput, Textarea,
} from "../../components/ui";
import { stateBadge } from "./biz";
import { CheckoutRoutes } from "./checkout-routes";

type Action = { kind: "create" } | { kind: "version" | "status"; merchant: Merchant };

function MerchantDialog({ action, onClose, onSaved }: {
  action: Action; onClose: () => void; onSaved: () => void;
}) {
  const { t } = useTranslation();
  const merchant = action.kind === "create" ? undefined : action.merchant;
  const [name, setName] = useState(merchant?.name ?? "");
  const [rail, setRail] = useState<MerchantRail>(merchant?.rail ?? "hyperswitch");
  const [mode, setMode] = useState<"test" | "live">(merchant?.mode ?? "test");
  const [accountId, setAccountId] = useState(merchant?.accountId ?? "");
  const [draft, setDraft] = useState(() => merchantDraft(merchant?.currentVersion));
  const [reason, setReason] = useState("");
  const [key, setKey] = useState(() => crypto.randomUUID());
  const [failed, setFailed] = useState(false);
  const submitting = useRef(false);
  const returnFocus = useRef(document.activeElement as HTMLElement | null);
  const write = useMerchantWrite();
  const isStatus = action.kind === "status";
  const titleKey = isStatus ? (merchant?.enabled ? "disable" : "enable") : action.kind;
  function edited() {
    setKey(crypto.randomUUID()); setFailed(false); write.reset();
  }
  function field(name: keyof MerchantDraft, value: string) {
    setDraft((previous) => ({ ...previous, [name]: value }));
  }
  async function submit(event: FormEvent) {
    event.preventDefault();
    if (submitting.current) return;
    submitting.current = true;
    try {
      const common = { idempotencyKey: key, reason: reason.trim() };
      let intent: MerchantIntent;
      if (action.kind === "status") {
        intent = { operation: "status", merchantId: action.merchant.id,
          body: { ...common, enabled: !action.merchant.enabled } };
      } else {
        const config = merchantConfig(rail, draft);
        intent = action.kind === "create"
          ? { operation: "create", body: { ...common, name: name.trim(), rail, mode, accountId: accountId.trim(), config } }
          : { operation: "version", merchantId: action.merchant.id, body: { ...common, config } };
      }
      await write.mutateAsync(intent);
      setDraft(merchantDraft()); write.reset(); onSaved(); onClose();
    } catch { setFailed(true); } finally { submitting.current = false; }
  }
  return <AlertDialog open onOpenChange={(open) => { if (!open && !write.isPending) onClose(); }}>
    <AlertDialogContent className="max-h-[calc(100dvh-2rem)] overflow-y-auto" onCloseAutoFocus={(event) => { event.preventDefault(); returnFocus.current?.focus(); }}>
      <AlertDialogHeader>
        <AlertDialogTitle>{t(`biz.merchant.${titleKey}`)}</AlertDialogTitle>
        <AlertDialogDescription>{merchant ? `${merchant.name} · ${merchant.rail} · ${merchant.mode} · ${merchant.accountId}` : t("biz.merchant.createHelp")}</AlertDialogDescription>
      </AlertDialogHeader>
      <form onSubmit={(event) => void submit(event)} className="flex flex-col gap-4">
        <fieldset disabled={write.isPending} onChange={edited} className="flex min-w-0 flex-col gap-4">
          {action.kind === "create" ? <>
            <Field label={t("biz.integrations.colName")} hint={t("biz.merchant.nameHelp")}><TextInput required maxLength={80} pattern="[a-z0-9]+(-[a-z0-9]+)*" value={name} onChange={(e) => setName(e.target.value)} autoComplete="off" /></Field>
            <Field label={t("biz.integrations.colRail")}><Select value={rail} onChange={(e) => { setRail(e.target.value as MerchantRail); setDraft(merchantDraft()); }}>
              <option value="hyperswitch">Hyperswitch / Stripe</option><option value="stripe">Stripe</option><option value="airwallex">Airwallex</option>
            </Select></Field>
            <Field label={t("biz.integrations.colMode")}><Select value={mode} onChange={(e) => setMode(e.target.value as typeof mode)}><option value="test">{t("biz.merchant.test")}</option><option value="live">{t("biz.merchant.live")}</option></Select></Field>
            <Field label={t("biz.integrations.colAccount")} hint={t("biz.merchant.identityHelp")}><TextInput required maxLength={200} value={accountId} onChange={(e) => setAccountId(e.target.value)} autoComplete="off" /></Field>
          </> : null}
          {!isStatus ? <>
            <Banner tone="info">{t("biz.merchant.localOnly")}</Banner>
            <Field label={t("biz.merchant.apiUrl")}><TextInput type="url" required value={draft.apiUrl} onChange={(e) => field("apiUrl", e.target.value)} autoComplete="off" /></Field>
            <Field label={t("biz.provider.network")} hint={t("biz.provider.networkHelp")}><Select value={draft.network} onChange={(e) => field("network", e.target.value)}><option value="public">{t("biz.provider.public")}</option><option value="internal">{t("biz.provider.internal")}</option></Select></Field>
            {draft.network === "internal" ? <Field label={t("biz.provider.ips")} hint={t("biz.provider.ipsHelp")}><Textarea required rows={2} value={draft.allowedIps} onChange={(e) => field("allowedIps", e.target.value)} /></Field> : null}
            {(["apiKey", "webhookSecret"] as const).map((name) => <Field key={name} label={t(`biz.merchant.${name}`)} hint={t("biz.merchant.secretHelp")}><TextInput type="password" required minLength={8} maxLength={8192} value={draft[name]} onChange={(e) => field(name, e.target.value)} autoComplete="new-password" spellCheck={false} /></Field>)}
            {rail === "hyperswitch" ? <>
              <Field label={t("biz.merchant.publishableKey")}><TextInput required maxLength={200} value={draft.publishableKey} onChange={(e) => field("publishableKey", e.target.value)} autoComplete="off" /></Field>
              <Field label={t("biz.merchant.sdkUrl")} hint={t("biz.merchant.sdkHelp")}><TextInput type="url" required value={draft.sdkUrl} onChange={(e) => field("sdkUrl", e.target.value)} autoComplete="off" /></Field>
            </> : rail === "airwallex" ? <>
              <Field label={t("biz.merchant.clientId")} hint={t("biz.merchant.secretHelp")}><TextInput type="password" required maxLength={200} value={draft.clientId} onChange={(e) => field("clientId", e.target.value)} autoComplete="new-password" /></Field>
              <Field label={t("biz.merchant.checkoutUrl")}><TextInput type="url" required value={draft.checkoutUrl} onChange={(e) => field("checkoutUrl", e.target.value)} autoComplete="off" /></Field>
            </> : null}
            {action.kind === "version" ? <Banner tone="warn">{t("biz.merchant.rotationHelp", { revision: merchant?.currentVersion.revision })}</Banner> : null}
          </> : <Banner tone="warn">{t(merchant?.enabled ? "biz.merchant.disableHelp" : "biz.merchant.enableHelp")}</Banner>}
          {mode === "live" ? <Banner tone="warn">{t("biz.merchant.liveHelp")}</Banner> : null}
          <Field label={t("biz.reason")}><Textarea required maxLength={500} rows={2} value={reason} onChange={(e) => setReason(e.target.value)} /></Field>
        </fieldset>
        {write.error ? <Banner tone="error">{write.error instanceof Error ? write.error.message : t("biz.loadError")}</Banner> : null}
        {failed ? <p role="status" className="text-sm text-ink-secondary">{t("biz.provider.retryHelp")}</p> : null}
        <AlertDialogFooter><Button type="button" tone="ghost" disabled={write.isPending} onClick={onClose}>{t("common.cancel")}</Button><Button type="submit" tone={isStatus || mode === "live" ? "danger" : "primary"} disabled={!reason.trim()} isLoading={write.isPending}>{t(failed ? "biz.provider.retry" : "common.save")}</Button></AlertDialogFooter>
      </form>
    </AlertDialogContent>
  </AlertDialog>;
}

function Pager<T>({ cursors, setCursors, next, busy }: {
  cursors: (T | undefined)[]; setCursors: (value: (T | undefined)[]) => void; next: T | null | undefined; busy: boolean;
}) {
  const { t } = useTranslation();
  return <div className="mt-4 flex flex-wrap justify-between gap-2"><Button tone="ghost" disabled={busy || cursors.length === 1} onClick={() => setCursors(cursors.slice(0, -1))}>{t("biz.provider.previous")}</Button><Button tone="ghost" disabled={busy || next == null} onClick={() => { if (next != null) setCursors([...cursors, next]); }}>{t("biz.provider.next")}</Button></div>;
}

function MerchantDetail({ merchant, canWrite, onAction, onBack }: {
  merchant: Merchant; canWrite: boolean; onAction: (action: Action) => void; onBack: () => void;
}) {
  const { t } = useTranslation();
  const [versionCursors, setVersionCursors] = useState<(number | undefined)[]>([undefined]);
  const [eventCursors, setEventCursors] = useState<(string | undefined)[]>([undefined]);
  const versions = useMerchantVersions(merchant.id, versionCursors.at(-1));
  const events = useMerchantEvents(merchant.id, eventCursors.at(-1));
  const current = merchant.currentVersion;
  return <div className="flex flex-col gap-4">
    <div className="flex flex-wrap gap-2"><Button tone="ghost" onClick={onBack}>{t("biz.merchant.back")}</Button>{canWrite ? <><Button onClick={() => onAction({ kind: "version", merchant })}>{t("biz.merchant.version")}</Button><Button tone={merchant.enabled ? "danger" : "ghost"} onClick={() => onAction({ kind: "status", merchant })}>{t(merchant.enabled ? "biz.merchant.disable" : "biz.merchant.enable")}</Button></> : null}</div>
    <Card title={merchant.name}>
      <dl className="grid min-w-0 grid-cols-1 gap-4 sm:grid-cols-2">
        {[
          [t("biz.integrations.colRail"), `${merchant.rail} / ${merchant.connector}`],
          [t("biz.integrations.colMode"), t(`biz.merchant.${merchant.mode}`)],
          [t("biz.integrations.colAccount"), merchant.accountId],
          [t("common.status"), t(merchant.enabled ? "biz.integrations.enabled" : "biz.integrations.disabled")],
          [t("biz.merchant.support"), t(`biz.merchant.${merchant.supportStatus}`)],
          [t("biz.merchant.apiUrl"), current.apiUrl],
        ].map(([label, value]) => <div key={label}><dt className="text-xs text-muted">{label}</dt><dd className="break-all text-sm">{value}</dd></div>)}
      </dl>
      <p className="mt-4 text-sm text-ink-secondary">{t("biz.merchant.localOnly")}</p>
    </Card>
    <CheckoutRoutes merchant={merchant} canWrite={canWrite} />
    <Card title={t("biz.merchant.history")}>
      {versions.isError ? <Banner tone="error">{t("biz.loadError")} <Button tone="ghost" onClick={() => void versions.refetch()}>{t("biz.provider.retry")}</Button></Banner> : null}
      {versions.isLoading ? <Skeleton className="h-24 w-full" /> : !versions.isError ? <div className="flex flex-col gap-4">{versions.data?.versions.map((version) => <section key={version.id} className="min-w-0 rounded-lg border border-line p-4">
        <h3 className="font-medium">v{version.revision} · {version.createdAt} · {version.source}</h3>
        <dl className="mt-2 flex flex-col gap-2">{[
          [t("biz.merchant.apiUrl"), version.apiUrl],
          [t("biz.provider.network"), `${version.destinationPolicy.network}${version.destinationPolicy.allowedIps?.length ? `: ${version.destinationPolicy.allowedIps.join(", ")}` : ""}`],
          [t("biz.merchant.publishableKey"), version.publishableKey],
          [t("biz.merchant.sdkUrl"), version.sdkUrl],
          [t("biz.merchant.checkoutUrl"), version.checkoutUrl],
          [t("biz.merchant.callback"), `${window.location.origin}/api/v1/webhooks/merchants/${merchant.id}/versions/${version.id}`],
        ].filter(([, value]) => value != null).map(([label, value]) => <div key={label}><dt className="text-xs text-muted">{label}</dt><dd className="break-all font-mono text-xs">{value}</dd></div>)}</dl>
        <p className="mt-2 text-sm text-ink-secondary">{t("biz.merchant.callbackHelp")}</p>
      </section>)}</div> : null}
      {!versions.isLoading && !versions.isError && !versions.data?.versions.length ? <p className="text-sm text-ink-secondary">{t("biz.merchant.noVersions")}</p> : null}
      <Pager cursors={versionCursors} setCursors={setVersionCursors} next={versions.isError ? null : versions.data?.nextCursor} busy={versions.isFetching} />
    </Card>
    <Card title={t("biz.integrations.events")}>
      {events.isError ? <Banner tone="error">{t("biz.loadError")} <Button tone="ghost" onClick={() => void events.refetch()}>{t("biz.provider.retry")}</Button></Banner> : <TableShell density="compact" loading={events.isLoading ? 3 : 0} head={[t("biz.integrations.colEvent"), t("biz.integrations.colKind"), t("common.status"), t("biz.integrations.colReason")]}>
        {!events.isLoading && events.data?.events.map((event) => <tr key={event.id}><Td mono truncate>{event.eventId}</Td><td>{event.kind}</td><td>{stateBadge(event.state)}</td><Td truncate>{event.reason ?? "—"}</Td></tr>)}
        {!events.isLoading && !events.data?.events.length ? <EmptyRows colSpan={4}>{t("biz.integrations.noEvents")}</EmptyRows> : null}
      </TableShell>}
      <Pager cursors={eventCursors} setCursors={setEventCursors} next={events.isError ? null : events.data?.nextCursor} busy={events.isFetching} />
    </Card>
  </div>;
}

export function MerchantSettings() {
  const { t } = useTranslation();
  const business = useBusiness();
  const canRead = useCapability(business.data, "reportsRead");
  const canWrite = useCapability(business.data, "merchantSecretsWrite");
  const [cursors, setCursors] = useState<(string | undefined)[]>([undefined]);
  const merchants = useMerchants(cursors.at(-1), canRead);
  const [picked, setPicked] = useState<string | null>(null);
  const [action, setAction] = useState<Action | null>(null);
  const [saved, setSaved] = useState(false);
  useEffect(() => { if (!canRead || !canWrite) setAction(null); }, [canRead, canWrite]);
  const selected = merchants.data?.merchants.find((merchant) => merchant.id === picked);
  return <div className="flex flex-col gap-4">
    {business.isLoading ? <Skeleton className="h-24 w-full" /> : business.isError ? <Banner tone="error">{t("biz.loadError")} <Button tone="ghost" onClick={() => void business.refetch()}>{t("biz.provider.retry")}</Button></Banner> : !canRead ? <Banner tone="info">{t("biz.merchant.noAccess")}</Banner> : <>
      {saved ? <p role="status" className="text-sm text-success">{t("biz.merchant.saved")}</p> : null}
      {merchants.isError ? <Banner tone="error">{t("biz.loadError")} <Button tone="ghost" onClick={() => void merchants.refetch()}>{t("biz.provider.retry")}</Button></Banner> : null}
      {selected ? <MerchantDetail key={selected.id} merchant={selected} canWrite={canWrite} onAction={(next) => { setSaved(false); setAction(next); }} onBack={() => setPicked(null)} /> : <Card title={t("biz.integrations.merchantsTitle")} actions={canWrite ? <Button onClick={() => { setSaved(false); setAction({ kind: "create" }); }}>{t("biz.merchant.create")}</Button> : undefined}>
        <TableShell density="compact" loading={merchants.isLoading ? 3 : 0} head={[t("biz.integrations.colName"), t("biz.integrations.colRail"), t("biz.integrations.colMode"), t("biz.integrations.colAccount"), t("common.status")]}>
          {!merchants.isLoading && !merchants.isError && merchants.data?.merchants.map((merchant) => <tr key={merchant.id}><td><Button tone="ghost" onClick={() => setPicked(merchant.id)}>{merchant.name}</Button></td><td>{merchant.rail}</td><td>{t(`biz.merchant.${merchant.mode}`)}</td><Td mono truncate>{merchant.accountId}</Td><td><Badge tone={merchant.enabled ? "success" : "secondary"}>{t(merchant.enabled ? "biz.integrations.enabled" : "biz.integrations.disabled")}</Badge></td></tr>)}
          {!merchants.isLoading && !merchants.isError && !merchants.data?.merchants.length ? <EmptyRows colSpan={5} icon={<Landmark />}>{t("biz.integrations.noMerchants")}</EmptyRows> : null}
        </TableShell>
        <Pager cursors={cursors} setCursors={setCursors} next={merchants.isError ? null : merchants.data?.nextCursor} busy={merchants.isFetching} />
      </Card>}
    </>}
    {action && canRead && canWrite ? <MerchantDialog action={action} onClose={() => setAction(null)} onSaved={() => { setSaved(true); if (action.kind === "create") { setPicked(null); setCursors([undefined]); } }} /> : null}
  </div>;
}
