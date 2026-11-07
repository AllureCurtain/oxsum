import { useRef, useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { Network } from "lucide-react";
import {
  useBusiness, useBusinessChannels, useCapability, useChannelConnectionCheck,
  useDisplay, useProviderWrite, type Channel, type ProviderIntent,
} from "../../api/hooks";
import {
  AlertDialog, AlertDialogContent, AlertDialogDescription, AlertDialogFooter,
  AlertDialogHeader, AlertDialogTitle, Badge, Banner, Button, Card, Cr,
  EmptyRows, Field, Select, Skeleton, TableShell, Td, TextInput, Textarea,
} from "../../components/ui";
import { DetailGrid, Field2 } from "./biz";

// A dialog is one in-memory intent. Failed unchanged submissions reuse its key;
// editing any field starts a new intent. Closing discards unsent secret inputs.
type Credential = Channel["credentials"][number];
type Action =
  | { kind: "connection"; channel?: Channel }
  | { kind: "credential" | "billing" | "clearBilling" | "check"; channel: Channel }
  | { kind: "status" | "remove"; channel: Channel; credential: Credential };

function ProviderDialog({ action, onClose, onSaved }: {
  action: Action; onClose: () => void; onSaved: () => void;
}) {
  const { t } = useTranslation();
  const channel = action.channel;
  const initialPolicy = channel?.settings.destinationPolicy;
  const [name, setName] = useState(channel?.name ?? "");
  const [baseUrl, setBaseUrl] = useState(channel?.baseUrl ?? "");
  const [protocol, setProtocol] = useState<"openai" | "anthropic" | "gemini" | "tasks">(
    (channel?.protocol as "openai" | "anthropic" | "gemini" | "tasks") ?? "openai",
  );
  const [network, setNetwork] = useState(initialPolicy?.network ?? "public");
  const [allowedIps, setAllowedIps] = useState(initialPolicy?.allowedIps?.join("\n") ?? "");
  const [secret, setSecret] = useState("");
  const [reason, setReason] = useState("");
  const [intentKey, setIntentKey] = useState(() => crypto.randomUUID());
  const [failed, setFailed] = useState(false);
  const submitting = useRef(false);
  const returnFocus = useRef(document.activeElement as HTMLElement | null);
  const write = useProviderWrite();
  const check = useChannelConnectionCheck();
  const busy = write.isPending || check.isPending;
  const error = write.error || check.error;
  const repoint = !!channel && (
    baseUrl.trim().replace(/\/+$/, "") !== channel.baseUrl.replace(/\/+$/, "") || protocol !== channel.protocol
  );
  const needsKey = action.kind === "credential" || action.kind === "billing"
    || (action.kind === "connection" && (!channel || repoint));
  const disabling = action.kind === "status" && action.credential.status === "active";
  const dangerous = (action.kind === "connection" && !channel) || repoint || disabling || action.kind === "remove" || action.kind === "clearBilling";
  const title = t(`biz.provider.${action.kind === "connection" ? (channel ? "edit" : "create") : action.kind}`);
  const target = action.kind === "status" || action.kind === "remove"
    ? `${channel?.name} / •••${action.credential.apiKeyLast4}` : channel?.name ?? name;

  function edited() {
    setIntentKey(crypto.randomUUID());
    setFailed(false);
    write.reset();
    check.reset();
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (submitting.current) return;
    submitting.current = true;
    try {
      if (action.kind === "check") {
        await check.mutateAsync({ channelName: action.channel.name, reason: reason.trim(), idempotencyKey: intentKey });
        setFailed(false);
        return;
      }
      let intent: ProviderIntent;
      switch (action.kind) {
        case "connection":
          intent = { operation: "connection", config: {
            name: name.trim(), baseUrl: baseUrl.trim(), protocol,
            ...(needsKey ? { apiKeys: [secret] } : {}),
            destinationPolicy: network === "public" ? { network: "public" } : {
              network: "internal", allowedIps: allowedIps.split(/[\s,]+/).filter(Boolean),
            },
          } };
          break;
        case "credential":
          intent = { operation: "credential", channel: action.channel.name, config: { apiKey: secret } };
          break;
        case "billing":
        case "clearBilling":
          intent = { operation: "billing", channel: action.channel.name, config: { apiKey: action.kind === "billing" ? secret : null } };
          break;
        case "status":
          intent = { operation: "status", channel: action.channel.name, credential: action.credential.credentialId,
            config: { status: disabling ? "disabled" : "active" } };
          break;
        case "remove":
          intent = { operation: "remove", channel: action.channel.name, credential: action.credential.credentialId, config: {} };
          break;
      }
      await write.mutateAsync({ intent, reason: reason.trim(), idempotencyKey: intentKey });
      setSecret("");
      write.reset();
      onSaved();
      onClose();
    } catch {
      setFailed(true);
    } finally {
      submitting.current = false;
    }
  }

  return (
    <AlertDialog open onOpenChange={(open) => { if (!open && !busy) onClose(); }}>
      <AlertDialogContent className="max-h-[calc(100dvh-2rem)] overflow-y-auto" onCloseAutoFocus={(event) => { event.preventDefault(); returnFocus.current?.focus(); }}>
        <AlertDialogHeader>
          <AlertDialogTitle>{title}</AlertDialogTitle>
          <AlertDialogDescription className="break-all">
            {action.kind === "check" ? t("biz.provider.checkHelp", { name: target, url: channel?.baseUrl })
              : t("biz.provider.changeHelp", { name: target })}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <form onSubmit={(event) => void submit(event)} className="flex flex-col gap-4">
          {!check.data ? (
            <fieldset disabled={busy} className="flex min-w-0 flex-col gap-4" onChange={edited}>
              {action.kind === "connection" ? <>
                <Field label={t("biz.channels.colName")} hint={t("biz.provider.nameHelp")}>
                  <TextInput value={name} onChange={(event) => setName(event.target.value)} required maxLength={128} disabled={!!channel} autoComplete="off" />
                </Field>
                <Field label={t("biz.channels.colUrl")}>
                  <TextInput type="url" value={baseUrl} onChange={(event) => setBaseUrl(event.target.value)} required autoComplete="off" placeholder="https://api.example.com/v1" />
                </Field>
                <Field label={t("biz.channels.colProtocol")}>
                  <Select value={protocol} onChange={(event) => setProtocol(event.target.value as typeof protocol)}>
                    <option value="openai">OpenAI</option><option value="anthropic">Anthropic</option>
                    <option value="gemini">Gemini</option><option value="tasks">Tasks</option>
                  </Select>
                </Field>
                <Field label={t("biz.provider.network")} hint={t("biz.provider.networkHelp")}>
                  <Select value={network} onChange={(event) => setNetwork(event.target.value as typeof network)}>
                    <option value="public">{t("biz.provider.public")}</option>
                    <option value="internal">{t("biz.provider.internal")}</option>
                  </Select>
                </Field>
                {network === "internal" ? <Field label={t("biz.provider.ips")} hint={t("biz.provider.ipsHelp")}>
                  <Textarea value={allowedIps} onChange={(event) => setAllowedIps(event.target.value)} required rows={2} autoComplete="off" />
                </Field> : null}
                {!channel ? <Banner tone="warn">{t("biz.provider.upsertHelp")}</Banner> : null}
                {repoint ? <Banner tone="warn">{t("biz.provider.repoint", { url: channel?.baseUrl })}</Banner> : null}
              </> : null}
              {needsKey ? <Field label={t(action.kind === "billing" ? "biz.provider.billingSecret" : "biz.provider.secret")} hint={t("biz.provider.secretHelp")}>
                <TextInput type="password" value={secret} onChange={(event) => setSecret(event.target.value)} required minLength={8} maxLength={8192} autoComplete="new-password" spellCheck={false} />
              </Field> : null}
              {action.kind === "credential" ? <p className="text-sm text-ink-secondary">{t("biz.provider.rotateHelp")}</p> : null}
              {disabling ? <Banner tone="warn">{t("biz.provider.disableHelp", { name: target })}</Banner> : null}
              {action.kind === "remove" ? <Banner tone="warn">{t("biz.provider.removeHelp", { name: target })}</Banner> : null}
              {action.kind === "clearBilling" ? <Banner tone="warn">{t("biz.provider.clearHelp", { name: target })}</Banner> : null}
              <Field label={t("biz.reason")}>
                <Textarea value={reason} onChange={(event) => setReason(event.target.value)} required maxLength={500} rows={2} />
              </Field>
            </fieldset>
          ) : (
            <div role="status" className="flex flex-col gap-2">
              <p className="font-medium">{t(`biz.provider.checkState.${check.data.state}`)}</p>
              <p className="text-sm text-ink-secondary">{t("biz.provider.checkLimit")}</p>
              <p className="mono break-all text-xs">{t("biz.provider.checkReceipt", { id: check.data.id, status: check.data.httpStatus ?? "—" })}</p>
              <p className="text-sm text-ink-secondary">{t("biz.provider.checkOnce")}</p>
            </div>
          )}
          {error ? <Banner tone="error">{error instanceof Error ? error.message : t("biz.loadError")}</Banner> : null}
          {failed ? <p role="status" className="text-sm text-ink-secondary">{t("biz.provider.retryHelp")}</p> : null}
          <AlertDialogFooter>
            <Button type="button" tone="ghost" disabled={busy} onClick={onClose}>{t(check.data ? "biz.provider.close" : "common.cancel")}</Button>
            {!check.data ? <Button type="submit" tone={dangerous ? "danger" : "primary"} isLoading={busy} disabled={!reason.trim()}>
              {failed ? t("biz.provider.retry") : action.kind === "check" ? t("biz.provider.sendHead") : t("common.save")}
            </Button> : null}
          </AlertDialogFooter>
        </form>
      </AlertDialogContent>
    </AlertDialog>
  );
}

function ChannelDetail({ channel, canWrite, onBack, onAction }: {
  channel: Channel; canWrite: boolean; onBack: () => void; onAction: (action: Action) => void;
}) {
  const { t } = useTranslation();
  const display = useDisplay();
  const policy = channel.settings.destinationPolicy;
  return (
    <div className="flex flex-col gap-6">
      <div>
        <Button tone="ghost" onClick={onBack}>{t("biz.channels.back")}</Button>
        <h2 className="mt-4 break-all text-xl font-bold">{channel.name}</h2>
      </div>
      <Card title={t("biz.channels.detail")} actions={canWrite ? <Button tone="ghost" onClick={() => onAction({ kind: "connection", channel })}>{t("biz.provider.edit")}</Button> : null}>
        <DetailGrid>
          <Field2 label={t("biz.channels.colProtocol")}>{channel.protocol}</Field2>
          <Field2 label={t("biz.channels.colUrl")}><span className="mono break-all text-xs">{channel.baseUrl}</span></Field2>
          <Field2 label={t("biz.provider.network")}>{t(policy?.network === "internal" ? "biz.provider.internal" : "biz.provider.public")}</Field2>
          <Field2 label={t("biz.provider.ips")}><span className="mono break-all text-xs">{policy?.allowedIps?.join(", ") || "—"}</span></Field2>
        </DetailGrid>
        {canWrite ? <div className="mt-4 flex flex-wrap items-center gap-4 border-t border-line pt-4">
          <Button tone="ghost" onClick={() => onAction({ kind: "check", channel })}>{t("biz.provider.check")}</Button>
          <p className="text-sm text-ink-secondary">{t("biz.provider.checkLimit")}</p>
        </div> : null}
      </Card>
      <Card title={t("biz.channels.credentials")} actions={canWrite ? <Button onClick={() => onAction({ kind: "credential", channel })}>{t("biz.provider.credential")}</Button> : null}>
        <p className="mb-4 text-sm text-ink-secondary">{t("biz.provider.rotateHelp")}</p>
        <TableShell density="compact" sticky={false} head={[t("biz.channels.colKeyTail"), t("common.status"), t("biz.channels.colCooldown"), t("biz.channels.colLastUsed"), ...(canWrite ? [t("biz.provider.actions")] : [])]}>
          {channel.credentials.map((credential) => <tr key={credential.credentialId}>
            <Td mono>•••{credential.apiKeyLast4}</Td>
            <td><Badge tone={credential.status === "active" ? "success" : "secondary"}>{t(`biz.provider.${credential.status}`)}</Badge></td>
            <Td>{credential.cooldownUntil ?? "—"}</Td>
            <Td>{credential.lastUsedAt ?? "—"}</Td>
            {canWrite ? <td><div className="flex gap-2">
              <Button tone="ghost" onClick={() => onAction({ kind: "status", channel, credential })}>{t(credential.status === "active" ? "biz.provider.disable" : "biz.provider.enable")}</Button>
              <Button tone="danger" onClick={() => onAction({ kind: "remove", channel, credential })}>{t("biz.provider.remove")}</Button>
            </div></td> : null}
          </tr>)}
          {channel.credentials.length === 0 ? <EmptyRows colSpan={canWrite ? 5 : 4}>{t("biz.channels.noCredentials")}</EmptyRows> : null}
        </TableShell>
      </Card>
      <Card title={t("biz.channels.billKey")}>
        <p className="mono text-sm">{channel.billKeyLast4 ? `•••${channel.billKeyLast4}` : t("biz.provider.notSet")}</p>
        {canWrite ? <div className="mt-4 flex flex-wrap gap-2">
          <Button tone="ghost" onClick={() => onAction({ kind: "billing", channel })}>{t("biz.provider.billing")}</Button>
          {channel.billKeyLast4 ? <Button tone="danger" onClick={() => onAction({ kind: "clearBilling", channel })}>{t("biz.provider.clearBilling")}</Button> : null}
        </div> : null}
      </Card>
      <Card title={t("biz.channels.models")}>
        <p className="mb-4 text-sm text-ink-secondary">{t("biz.provider.costHelp")}</p>
        <TableShell head={[t("common.model"), t("biz.channels.colVersion"), { label: t("biz.channels.colIn"), align: "right" }, { label: t("biz.channels.colOut"), align: "right" }, t("biz.channels.colMode")]}>
          {channel.models.map((model) => <tr key={model.model}>
            <td className="font-medium">{model.model}</td><Td mono>v{model.version}</Td>
            <Td mono align="right"><Cr minor={model.inputPricePerMillion} currency={display} /></Td>
            <Td mono align="right">{model.outputPricePerMillion != null ? <Cr minor={model.outputPricePerMillion} currency={display} /> : "—"}</Td>
            <td>{model.mode ?? "chat"}</td>
          </tr>)}
          {channel.models.length === 0 ? <EmptyRows colSpan={5}>{t("biz.channels.noModels")}</EmptyRows> : null}
        </TableShell>
      </Card>
    </div>
  );
}

export function BusinessChannels() {
  const { t } = useTranslation();
  const [picked, setPicked] = useState<string | null>(null);
  const [cursors, setCursors] = useState<(string | undefined)[]>([undefined]);
  const [action, setAction] = useState<Action | null>(null);
  const [saved, setSaved] = useState(false);
  const channels = useBusinessChannels(cursors[cursors.length - 1]);
  const business = useBusiness();
  const canWrite = useCapability(business.data, "providerSecretsWrite");
  const selected = channels.data?.channels.find((channel) => channel.id === picked);
  function open(next: Action) { setSaved(false); setAction(next); }
  return (
    <div className="flex flex-col gap-6">
      {saved ? <div role="status"><Banner tone="success">{t("biz.provider.saved")}</Banner></div> : null}
      {business.isError ? <Banner tone="error">{t("biz.loadError")}</Banner> : null}
      {channels.isError ? <Banner tone="error"><div className="flex flex-wrap items-center gap-4">
        {t("biz.loadError")}<Button tone="ghost" onClick={() => void channels.refetch()}>{t("biz.provider.reload")}</Button>
      </div></Banner> : null}
      {picked ? (
        selected ? <ChannelDetail channel={selected} canWrite={canWrite && !channels.isError} onBack={() => setPicked(null)} onAction={open} />
          : channels.isLoading ? <Skeleton className="h-32 w-full" />
          : <><Banner tone="warn">{t("biz.provider.unavailable")}</Banner><Button tone="ghost" onClick={() => setPicked(null)}>{t("biz.channels.back")}</Button></>
      ) : <>
        <h2 className="text-xl font-bold">{t("biz.channels.title")}</h2>
        <p className="text-sm text-ink-secondary">{t("biz.provider.intro")}</p>
        <Card title={t("biz.channels.listTitle")} actions={canWrite ? <Button onClick={() => open({ kind: "connection" })}>{t("biz.provider.create")}</Button> : null}>
          <TableShell head={[t("biz.channels.colName"), t("biz.channels.colProtocol"), t("biz.channels.colUrl"), t("biz.channels.colModels"), t("biz.channels.colKeys")]} loading={channels.isLoading ? 4 : 0}>
            {!channels.isLoading && channels.data?.channels.map((channel) => <tr key={channel.id}>
              <td><button type="button" className="font-medium text-primary" onClick={() => { setPicked(channel.id); setSaved(false); }}>{channel.name}</button></td>
              <td>{channel.protocol}</td><Td mono truncate>{channel.baseUrl}</Td><Td mono>{channel.models.length}</Td><Td mono>{channel.credentials.length}</Td>
            </tr>)}
            {!channels.isLoading && !channels.isError && !channels.data?.channels.length ? <EmptyRows colSpan={5} icon={<Network />}>{t("biz.channels.empty")}</EmptyRows> : null}
          </TableShell>
          <div className="mt-4 flex flex-wrap justify-between gap-2">
            <Button tone="ghost" disabled={channels.isFetching || cursors.length === 1} onClick={() => setCursors((values) => values.slice(0, -1))}>{t("biz.provider.previous")}</Button>
            <Button tone="ghost" disabled={channels.isFetching || channels.isError || !channels.data?.nextCursor} onClick={() => setCursors((values) => [...values, channels.data!.nextCursor!])}>{t("biz.provider.next")}</Button>
          </div>
        </Card>
      </>}
      {action && canWrite ? <ProviderDialog action={action} onClose={() => setAction(null)} onSaved={() => { setSaved(true); if (!action.channel) { setPicked(null); setCursors([undefined]); } }} /> : null}
    </div>
  );
}
