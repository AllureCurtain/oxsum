import { useRef, useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { useProviderAliases, useProviderFallbacks, useProviderPrices, useProviderWrite, type Channel, type ProviderIntent } from "../../api/hooks";
import { AlertDialog, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle,
  Banner, Button, Card, EmptyRows, Field, NativeSelect, Skeleton, TableShell, Td, TextInput, Textarea } from "../../components/ui";
import { cacheValue, fallbackValue, modelName, providerDraft, providerValue, type ModelPrice, type ProviderDraft } from "../../lib/provider-draft";
import { ProviderFields } from "./provider-fields";

type Action =
  | { kind: "price"; price?: ModelPrice; draft: ProviderDraft }
  | { kind: "alias" }
  | { kind: "removeAlias"; alias: string }
  | { kind: "fallbacks"; model: string }
  | { kind: "cache" }
  | { kind: "history" };

function PriceHistory({ channel }: { channel: Channel }) {
  const { t } = useTranslation();
  const history = useProviderPrices(channel.name);
  return <>
    {history.isError ? <Banner tone="error">{t("biz.loadError")} <Button tone="ghost" onClick={() => void history.refetch()}>{t("biz.provider.reload")}</Button></Banner> : null}
    {history.isLoading ? <Skeleton className="h-32 w-full" /> : history.data?.map((price) => {
      let draft: ProviderDraft;
      try { draft = providerDraft(price); } catch { return <Banner tone="error" key={`${price.model}/${price.version}`}>{price.model} v{price.version}: {t("biz.providerModels.errors.storedRange")}</Banner>; }
      return <details key={`${price.model}/${price.version}`} className="rounded-lg border border-line p-4">
        <summary className="cursor-pointer break-all text-sm text-primary">{price.model} · v{price.version} · {price.createdAt}</summary>
        <fieldset disabled className="mt-4"><ProviderFields value={draft} onChange={() => {}} isNew={false} /></fieldset>
      </details>;
    })}
    {!history.isLoading && !history.isError && !history.data?.length ? <p>{t("biz.channels.noModels")}</p> : null}
  </>;
}

function ConfigurationForm({ channel, action, fallbacks, onClose, onSaved, onPending }: {
  channel: Channel; action: Exclude<Action, { kind: "history" }>; fallbacks?: string[]; onClose: () => void; onSaved: () => void; onPending: (pending: boolean) => void;
}) {
  const { t } = useTranslation();
  const [draft, setDraft] = useState<ProviderDraft | null>(action.kind === "price" ? action.draft : null);
  const [alias, setAlias] = useState("");
  const [upstreamModel, setUpstreamModel] = useState(channel.models.find((model) => model.mode !== "event")?.model ?? "");
  const [fallbackText, setFallbackText] = useState(fallbacks?.join("\n") ?? "");
  const cache = channel.settings.semanticCache as { enabled?: boolean; ttlSeconds?: number | null } | undefined;
  const [cacheState, setCacheState] = useState<"default" | "disabled" | "enabled">(!cache ? "default" : cache.enabled ? "enabled" : "disabled");
  const [expiry, setExpiry] = useState<"bounded" | "forever">(cache && cache.ttlSeconds == null ? "forever" : "bounded");
  const [ttl, setTtl] = useState(String(cache?.ttlSeconds ?? 300));
  const [reason, setReason] = useState("");
  const [intentKey, setIntentKey] = useState(() => crypto.randomUUID());
  const [failed, setFailed] = useState(false);
  const [validation, setValidation] = useState<string | null>(null);
  const submitting = useRef(false);
  const write = useProviderWrite();
  function edited() { setIntentKey(crypto.randomUUID()); setFailed(false); setValidation(null); write.reset(); }
  async function submit(event: FormEvent) {
    event.preventDefault();
    if (submitting.current) return;
    submitting.current = true;
    let intent: ProviderIntent;
    try {
      switch (action.kind) {
        case "price": intent = { operation: "price", channel: channel.name, config: providerValue(draft!) }; break;
        case "alias": intent = { operation: "alias", channel: channel.name, config: { alias: modelName(alias), upstreamModel: modelName(upstreamModel) } }; break;
        case "removeAlias": intent = { operation: "removeAlias", channel: channel.name, alias: action.alias, config: {} }; break;
        case "fallbacks": intent = { operation: "fallbacks", model: action.model, config: { fallbacks: fallbackValue(action.model, fallbackText) } }; break;
        case "cache": intent = { operation: "cache", channel: channel.name, config: cacheValue(cacheState, expiry, ttl) }; break;
      }
    } catch (error) {
      setValidation(error instanceof Error ? error.message : "rate"); submitting.current = false; return;
    }
    onPending(true);
    try {
      await write.mutateAsync({ intent, reason: reason.trim(), idempotencyKey: intentKey });
      onSaved(); onClose();
    } catch { setFailed(true); } finally { submitting.current = false; onPending(false); }
  }
  return <form onSubmit={(e) => void submit(e)} className="flex min-w-0 flex-col gap-4">
    <fieldset disabled={write.isPending} className="flex min-w-0 flex-col gap-4" onChange={edited}>
      {action.kind === "price" && draft ? <>
        <Banner>{t("biz.providerModels.priceHelp")}</Banner>
        {action.price ? <p className="text-sm">{t("biz.providerModels.appendHelp", { model: action.price.model, version: action.price.version })}</p> : null}
        <ProviderFields value={draft} isNew={!action.price} onChange={(next) => { edited(); setDraft(next); }} />
      </> : null}
      {action.kind === "alias" ? <>
        <Field label={t("biz.providerModels.aliasName")}><TextInput required value={alias} maxLength={200} onChange={(e) => setAlias(e.target.value)} /></Field>
        <Field label={t("biz.providerModels.aliasTarget")} hint={t("biz.providerModels.aliasHelp")}>
          <NativeSelect value={upstreamModel} onChange={(e) => setUpstreamModel(e.target.value)} required>
            {channel.models.filter((model) => model.mode !== "event").map((model) => <option key={model.model}>{model.model}</option>)}
          </NativeSelect>
        </Field>
      </> : null}
      {action.kind === "removeAlias" ? <Banner tone="warn">{t("biz.providerModels.removeHelp", { alias: action.alias, channel: channel.name })}</Banner> : null}
      {action.kind === "fallbacks" ? <Field label={t("biz.providerModels.fallbackOrder", { model: action.model })} hint={t("biz.providerModels.fallbackHelp")}>
        <Textarea rows={6} value={fallbackText} onChange={(e) => setFallbackText(e.target.value)} maxLength={3216} />
      </Field> : null}
      {action.kind === "cache" ? <>
        <Banner>{t("biz.providerModels.cacheHelp")}</Banner>
        <Field label={t("biz.providerModels.cacheState")}><NativeSelect value={cacheState} onChange={(e) => setCacheState(e.target.value as typeof cacheState)}>
          {(["default", "disabled", "enabled"] as const).map((state) => <option key={state} value={state}>{t(`biz.providerModels.cacheStates.${state}`)}</option>)}
        </NativeSelect></Field>
        {cacheState !== "default" ? <>
          <Field label={t("biz.providerModels.expiry")}><NativeSelect value={expiry} onChange={(e) => setExpiry(e.target.value as typeof expiry)}>
            <option value="bounded">{t("biz.providerModels.bounded")}</option><option value="forever">{t("biz.providerModels.forever")}</option>
          </NativeSelect></Field>
          {expiry === "bounded" ? <Field label={t("biz.providerModels.ttl")}><TextInput inputMode="numeric" value={ttl} maxLength={16} onChange={(e) => setTtl(e.target.value)} required /></Field>
            : <Banner tone="warn">{t("biz.providerModels.foreverHelp")}</Banner>}
        </> : null}
      </> : null}
      <Field label={t("biz.reason")}><Textarea required value={reason} rows={2} maxLength={500} onChange={(e) => setReason(e.target.value)} /></Field>
    </fieldset>
    {validation ? <Banner tone="error">{t(`biz.providerModels.errors.${validation}`)}</Banner> : null}
    {write.error ? <Banner tone="error">{write.error.message}</Banner> : null}
    {failed ? <p role="status" className="text-sm text-ink-secondary">{t("biz.provider.retryHelp")}</p> : null}
    <AlertDialogFooter>
      <Button type="button" tone="ghost" disabled={write.isPending} onClick={onClose}>{t("common.cancel")}</Button>
      <Button type="submit" tone={action.kind === "removeAlias" ? "danger" : "primary"} isLoading={write.isPending} disabled={!reason.trim()}>
        {failed ? t("biz.provider.retry") : t(action.kind === "removeAlias" ? "biz.providerModels.removeAlias" : "common.save")}
      </Button>
    </AlertDialogFooter>
  </form>;
}

function ConfigurationDialog({ channel, action, onClose, onSaved }: { channel: Channel; action: Action; onClose: () => void; onSaved: () => void }) {
  const { t } = useTranslation();
  const focus = useRef(document.activeElement as HTMLElement | null);
  const [pending, setPending] = useState(false);
  const fallbacks = useProviderFallbacks(action.kind === "fallbacks" ? action.model : null);
  const ready = action.kind !== "fallbacks" || fallbacks.data !== undefined;
  return <AlertDialog open onOpenChange={(open) => { if (!open && !pending) onClose(); }}>
    <AlertDialogContent className="max-h-[calc(100dvh-2rem)] overflow-y-auto sm:max-w-2xl" onCloseAutoFocus={(e) => { e.preventDefault(); focus.current?.focus(); }}>
      <AlertDialogHeader><AlertDialogTitle>{t(`biz.providerModels.titles.${action.kind}`)}</AlertDialogTitle>
        <AlertDialogDescription>{t("biz.providerModels.changeHelp", { channel: channel.name })}</AlertDialogDescription></AlertDialogHeader>
      {action.kind === "history" ? <><PriceHistory channel={channel} /><Button tone="ghost" onClick={onClose}>{t("biz.provider.close")}</Button></>
        : ready ? <ConfigurationForm channel={channel} action={action} fallbacks={fallbacks.data?.fallbacks} onClose={onClose} onSaved={onSaved} onPending={setPending} />
        : <>{fallbacks.isError ? <Banner tone="error">{t("biz.loadError")} <Button tone="ghost" onClick={() => void fallbacks.refetch()}>{t("biz.provider.reload")}</Button></Banner>
          : <Skeleton className="h-32 w-full" />}<Button tone="ghost" onClick={onClose}>{t("common.cancel")}</Button></>}
    </AlertDialogContent>
  </AlertDialog>;
}

export function ProviderModels({ channel, canCatalog, canRoute }: { channel: Channel; canCatalog: boolean; canRoute: boolean }) {
  const { t } = useTranslation();
  const aliases = useProviderAliases(channel.name);
  const [action, setAction] = useState<Action | null>(null);
  const [error, setError] = useState(false);
  const [saved, setSaved] = useState(false);
  function open(next: Action) { setSaved(false); setError(false); setAction(next); }
  function editPrice(price?: ModelPrice) {
    try { open({ kind: "price", price, draft: providerDraft(price) }); } catch { setError(true); }
  }
  return <>
    {saved ? <div role="status"><Banner tone="success">{t("biz.provider.saved")}</Banner></div> : null}
    {error ? <Banner tone="error">{t("biz.providerModels.errors.storedRange")}</Banner> : null}
    <Card title={t("biz.channels.models")} actions={<div className="flex flex-wrap gap-2"><Button tone="ghost" onClick={() => open({ kind: "history" })}>{t("biz.providerModels.history")}</Button>
      {canCatalog ? <Button onClick={() => editPrice()}>{t("biz.providerModels.addModel")}</Button> : null}</div>}>
      <p className="mb-4 text-sm text-ink-secondary">{t("biz.providerModels.priceHelp")}</p>
      <TableShell head={[t("common.model"), t("biz.channels.colVersion"), t("biz.channels.colMode"), t("biz.providerModels.weight"), t("biz.provider.actions")]}>
        {channel.models.map((model) => <tr key={model.model}><td className="font-medium">{model.model}</td><Td mono>v{model.version}</Td><Td>{model.mode ?? "chat"}</Td><Td mono>{model.weight}</Td>
          <td><div className="flex gap-2">{canCatalog ? <Button tone="ghost" onClick={() => editPrice(model)}>{t("biz.providerModels.append")}</Button> : null}
            {canRoute && model.mode !== "event" ? <Button tone="ghost" onClick={() => open({ kind: "fallbacks", model: model.model })}>{t("biz.providerModels.fallbacks")}</Button> : null}</div></td></tr>)}
        {!channel.models.length ? <EmptyRows colSpan={5}>{t("biz.channels.noModels")}</EmptyRows> : null}
      </TableShell>
    </Card>
    <Card title={t("biz.providerModels.aliases")} actions={canCatalog ? <Button disabled={!channel.models.some((model) => model.mode !== "event")} onClick={() => open({ kind: "alias" })}>{t("biz.providerModels.addAlias")}</Button> : null}>
      {aliases.isError ? <Banner tone="error">{t("biz.loadError")} <Button tone="ghost" onClick={() => void aliases.refetch()}>{t("biz.provider.reload")}</Button></Banner> : null}
      <TableShell head={[t("biz.providerModels.aliasName"), t("biz.providerModels.aliasTarget"), t("biz.provider.actions")]} loading={aliases.isLoading ? 2 : 0}>
        {!aliases.isLoading && aliases.data?.map((alias) => <tr key={alias.alias}><Td>{alias.alias}</Td><Td>{alias.upstreamModel}</Td><td><div className="flex gap-2">
          {canRoute ? <Button tone="ghost" disabled={aliases.isError} onClick={() => open({ kind: "fallbacks", model: alias.alias })}>{t("biz.providerModels.fallbacks")}</Button> : null}
          {canCatalog ? <Button tone="danger" disabled={aliases.isError} onClick={() => open({ kind: "removeAlias", alias: alias.alias })}>{t("biz.providerModels.removeAlias")}</Button> : null}
        </div></td></tr>)}
        {!aliases.isLoading && !aliases.isError && !aliases.data?.length ? <EmptyRows colSpan={3}>{t("biz.providerModels.noAliases")}</EmptyRows> : null}
      </TableShell>
    </Card>
    <Card title={t("biz.providerModels.cache")} actions={canRoute ? <Button tone="ghost" onClick={() => open({ kind: "cache" })}>{t("biz.providerModels.editCache")}</Button> : null}>
      <p className="text-sm text-ink-secondary">{t("biz.providerModels.cacheHelp")}</p>
      <p className="mt-4 text-sm">{t(`biz.providerModels.cacheStates.${!channel.settings.semanticCache ? "default" : (channel.settings.semanticCache as { enabled?: boolean }).enabled ? "enabled" : "disabled"}`)}</p>
    </Card>
    {action ? <ConfigurationDialog channel={channel} action={action} onClose={() => setAction(null)} onSaved={() => setSaved(true)} /> : null}
  </>;
}
