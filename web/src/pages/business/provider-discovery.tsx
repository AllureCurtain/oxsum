import { useRef, useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { useChannelModelDiscovery, type Channel } from "../../api/hooks";
import { AlertDialogFooter, Banner, Button, Field, NativeSelect, Textarea } from "../../components/ui";

export function ProviderDiscovery({ channel, onChoose, onClose, onPending }: {
  channel: Channel; onChoose: (model: string) => void; onClose: () => void; onPending: (pending: boolean) => void;
}) {
  const { t } = useTranslation();
  const discovery = useChannelModelDiscovery();
  const [reason, setReason] = useState("");
  const [model, setModel] = useState("");
  const [sent, setSent] = useState(false);
  const [result, setResult] = useState<typeof discovery.data>();
  const [idempotencyKey] = useState(() => crypto.randomUUID());
  const submitting = useRef(false);
  const configured = new Set(channel.models.map((row) => row.model));
  const canChoose = result?.models.includes(model) && !configured.has(model);

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (result?.state === "listed") {
      if (canChoose) onChoose(model);
      return;
    }
    if (submitting.current || !reason.trim() || (result && result.state !== "pending")) return;
    submitting.current = true;
    onPending(true);
    setSent(true);
    try {
      setResult(await discovery.mutateAsync({ channelName: channel.name, reason: reason.trim(), idempotencyKey }));
    } catch {
      // Keep the original intent so a retry can only read its standing result.
    } finally {
      submitting.current = false;
      onPending(false);
    }
  }

  return <form onSubmit={(event) => void submit(event)} className="flex min-w-0 flex-col gap-4">
    {!result ? <Field label={t("biz.reason")}>
      <Textarea value={reason} onChange={(event) => setReason(event.target.value)} readOnly={sent} required maxLength={500} rows={2} />
    </Field> : null}
    {discovery.isError ? <Banner tone="error">{t("biz.providerModels.discovery.requestFailed")}</Banner> : null}
    {result?.state === "listed" ? <>
      {result.partial ? <Banner tone="warn">{t("biz.providerModels.discovery.partial")}</Banner> : null}
      {result.models.length === 0 ? <p role="status">{t("biz.providerModels.discovery.empty")}</p>
        : result.models.every((id) => configured.has(id)) ? <p role="status">{t("biz.providerModels.discovery.configured")}</p>
        : <Field label={t("biz.providerModels.discovery.model")}>
          <NativeSelect value={model} onChange={(event) => setModel(event.target.value)} required>
            <option value="" disabled>{t("biz.providerModels.discovery.choose")}</option>
            {result.models.map((id) => <option key={id} value={id} disabled={configured.has(id)}>
              {configured.has(id) ? t("biz.providerModels.discovery.configuredModel", { model: id }) : id}
            </option>)}
          </NativeSelect>
        </Field>}
    </> : result ? <Banner tone={result.state === "pending" ? "info" : "warn"}>
      {t(`biz.providerModels.discovery.states.${result.state}`)}{result.httpStatus !== null ? ` (HTTP ${result.httpStatus})` : ""}
    </Banner> : null}
    <AlertDialogFooter>
      <Button type="button" tone="ghost" disabled={discovery.isPending} onClick={onClose}>{t("biz.provider.close")}</Button>
      {result?.state === "listed" ? <Button type="submit" disabled={!canChoose}>{t("biz.providerModels.discovery.useModel")}</Button>
        : !result || result.state === "pending" ? <Button type="submit" disabled={!reason.trim()} isLoading={discovery.isPending}>
          {t(sent ? "biz.providerModels.discovery.checkResult" : "biz.providerModels.discovery.fetch")}
        </Button> : null}
    </AlertDialogFooter>
  </form>;
}
