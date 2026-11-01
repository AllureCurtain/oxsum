import { useMemo, useState, type FormEvent } from "react";
import { Tags } from "lucide-react";
import { useTranslation } from "react-i18next";
import { ApiError, useEstimatePrice, useLogKey, usePriceHead, usePricing } from "../api/hooks";
import { CopyButton } from "../components/ui";
import { Button } from "../components/ui";
import { Badge } from "../components/ui";
import {
  SelectRoot,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "../components/ui";
import { CodeBlock } from "../components/ui";
import {
  Banner,
  Card,
  EmptyRows,
  Field,
  NumberInput,
  PageState,
  TableShell,
  Td,
  TextInput,
  credits,
} from "../components/ui";

const MODES = ["chat", "embeddings", "rerank", "event"] as const;

function formatRate(minor: number | undefined, perMtok: string): string {
  if (minor === undefined) return "—";
  return `${credits(minor)} ${perMtok}`;
}

function Catalog() {
  const { t } = useTranslation();
  const pricing = usePricing();
  if (pricing.error) {
    return <Banner tone="error">{(pricing.error as ApiError).message}</Banner>;
  }
  const models = pricing.data?.models ?? [];
  const perMtok = t("pricing.perMtok");
  return (
    <TableShell
      head={[
        { label: t("common.model"), truncate: true },
        t("common.channel"),
        t("pricing.colProtocol"),
        t("pricing.colV"),
        { label: t("pricing.colWeight"), align: "right" },
        { label: t("pricing.colInput"), align: "right" },
        { label: t("pricing.colOutput"), align: "right" },
        { label: t("pricing.colMaxOut"), align: "right" },
        { label: t("pricing.colCached"), align: "right" },
      ]}
      loading={pricing.isLoading ? 8 : 0}
    >
      {!pricing.isLoading &&
        models.map((m) => (
        <tr key={`${m.channel}/${m.model}`}>
          <Td mono truncate>{m.model}</Td>
          <td>{m.channel}</td>
          <td>
            <Badge variant="secondary">{m.protocol}</Badge>
          </td>
          <td className="mono">{m.version}</td>
          <Td mono align="right">{m.weight}</Td>
          <Td mono align="right">{formatRate(m.inputPricePerMillion, perMtok)}</Td>
          <Td mono align="right">{formatRate(m.outputPricePerMillion, perMtok)}</Td>
          <Td mono align="right">{m.maxOutputTokens?.toLocaleString() ?? "—"}</Td>
          <Td mono align="right">{formatRate(m.cacheReadPricePerMillion, perMtok)}</Td>
        </tr>
      ))}
      {!pricing.isLoading && models.length === 0 ? (
        <EmptyRows colSpan={9} icon={<Tags />}>{t("pricing.noRoutes")}</EmptyRows>
      ) : null}
    </TableShell>
  );
}

function Estimator({ models }: { models: string[] }) {
  const { t } = useTranslation();
  const estimate = useEstimatePrice();
  const [model, setModel] = useState("");
  const [mode, setMode] = useState<string>("chat");
  const [inputTokens, setInputTokens] = useState("");
  const [outputTokens, setOutputTokens] = useState("");
  const [serviceTier, setServiceTier] = useState("");

  const submit = (e: FormEvent) => {
    e.preventDefault();
    estimate.mutate({
      model,
      mode,
      inputTokens: inputTokens === "" ? undefined : Number(inputTokens),
      outputTokens: outputTokens === "" ? undefined : Number(outputTokens),
      serviceTier: serviceTier === "" ? undefined : serviceTier,
    });
  };

  return (
    <form className="flex flex-col gap-3" onSubmit={submit}>
      <p className="text-sm text-muted">{t("pricing.estimateNote")}</p>
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
        <Field label={t("common.model")}>
          <SelectRoot
            value={model}
            onValueChange={(v) => setModel(String(v))}
          >
            <SelectTrigger>
              <SelectValue placeholder={t("pricing.pickModel")} />
            </SelectTrigger>
            <SelectContent>
              {models.map((m) => (
                <SelectItem key={m} value={m}>
                  {m}
                </SelectItem>
              ))}
            </SelectContent>
          </SelectRoot>
        </Field>
        <Field label={t("pricing.billingMode")}>
          <SelectRoot value={mode} onValueChange={(v) => setMode(String(v))}>
            <SelectTrigger>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {MODES.map((m) => (
                <SelectItem key={m} value={m}>
                  {m}
                </SelectItem>
              ))}
            </SelectContent>
          </SelectRoot>
        </Field>
        <Field label={t("pricing.inputTokens")} hint={t("pricing.inputHint")}>
          <NumberInput value={inputTokens} onChange={(e) => setInputTokens(e.target.value)} min="0" />
        </Field>
        <Field label={t("pricing.outputTokens")} hint={t("pricing.outputHint")}>
          <NumberInput
            value={outputTokens}
            onChange={(e) => setOutputTokens(e.target.value)}
            min="0"
            disabled={mode !== "chat"}
          />
        </Field>
        <Field label={t("pricing.serviceTier")} hint={t("pricing.serviceTierHint")}>
          <TextInput value={serviceTier} onChange={(e) => setServiceTier(e.target.value)} />
        </Field>
      </div>
      <div className="flex items-center gap-3">
        <Button
          type="submit"
          isLoading={estimate.isPending}
          loadingText={t("pricing.estimating")}
          disabled={model === ""}
        >
          {t("pricing.estimate")}
        </Button>
        {estimate.data ? (
          <p className="text-lg font-semibold tabular-nums">
            ≤ {credits(estimate.data.estimateMinor)} cr
            <span className="ml-2 text-xs font-normal text-muted">
              {t("pricing.priceVersion", { version: estimate.data.version })}
            </span>
          </p>
        ) : null}
      </div>
      {estimate.error instanceof ApiError ? (
        <Banner tone="error">{estimate.error.message}</Banner>
      ) : null}
    </form>
  );
}

function PriceHeadViewer({ channels }: { channels: string[] }) {
  const { t } = useTranslation();
  const [channel, setChannel] = useState("");
  const [model, setModel] = useState("");
  const [version, setVersion] = useState("");
  const head = usePriceHead(
    channel,
    model,
    version === "" ? undefined : Number(version),
  );
  const logKey = useLogKey();

  return (
    <div className="flex flex-col gap-3">
      <p className="text-sm text-muted">{t("pricing.checkpointNote")}</p>
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
        <Field label={t("common.channel")}>
          <SelectRoot value={channel} onValueChange={(v) => setChannel(String(v))}>
            <SelectTrigger>
              <SelectValue placeholder={t("pricing.pickChannel")} />
            </SelectTrigger>
            <SelectContent>
              {channels.map((c) => (
                <SelectItem key={c} value={c}>
                  {c}
                </SelectItem>
              ))}
            </SelectContent>
          </SelectRoot>
        </Field>
        <Field label={t("common.model")}>
          <TextInput value={model} onChange={(e) => setModel(e.target.value)} placeholder={t("pricing.modelId")} />
        </Field>
        <Field label={t("pricing.version")} hint={t("pricing.versionHint")}>
          <NumberInput value={version} onChange={(e) => setVersion(e.target.value)} min="1" />
        </Field>
      </div>
      {head.isLoading ? <PageState>{t("pricing.signing")}</PageState> : null}
      {head.error instanceof ApiError ? (
        <Banner tone={head.error.status === 503 ? "warn" : "error"}>
          {head.error.status === 503
            ? t("pricing.noSigningKey")
            : head.error.status === 404
              ? t("pricing.noSuchRow")
              : head.error.message}
        </Banner>
      ) : null}
      {head.data ? (
        <CodeBlock
          language="json"
          code={JSON.stringify(
            {
              note: head.data.note,
              origin: head.data.origin,
              keyName: head.data.keyName,
              publicKey: head.data.publicKey,
              keyHash: head.data.keyHash,
              size: head.data.size,
              root: head.data.root,
            },
            null,
            2,
          )}
        />
      ) : null}
      {logKey.data ? (
        <div className="flex items-center gap-2 text-xs text-muted">
          <span>
            {t("pricing.verifyAgainst")} <code className="mono">{logKey.data.keyName}</code>:
          </span>
          <code className="mono">{logKey.data.publicKey}</code>
          <CopyButton value={logKey.data.publicKey} aria-label={t("pricing.copyKey")} />
        </div>
      ) : null}
    </div>
  );
}

export function Pricing() {
  const { t } = useTranslation();
  const pricing = usePricing();
  const models = useMemo(
    () => [...new Set((pricing.data?.models ?? []).map((m) => m.model))].sort(),
    [pricing.data],
  );
  const channels = useMemo(
    () => [...new Set((pricing.data?.models ?? []).map((m) => m.channel))].sort(),
    [pricing.data],
  );

  return (
    <div className="flex flex-col gap-6">
      <h1 className="text-2xl font-bold">{t("pricing.title")}</h1>
      <Card title={t("pricing.catalog")} actions={<span className="text-xs text-muted">{t("pricing.catalogNote")}</span>}>
        <Catalog />
      </Card>
      <Card title={t("pricing.estimateRequest")}>
        <Estimator models={models} />
      </Card>
      <Card title={t("pricing.signedCheckpoint")}>
        <PriceHeadViewer channels={channels} />
      </Card>
    </div>
  );
}
