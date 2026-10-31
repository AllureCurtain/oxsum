import { useMemo, useState, type FormEvent } from "react";
import { Tags } from "lucide-react";
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

function formatRate(minor: number | undefined): string {
  if (minor === undefined) return "—";
  return `${credits(minor)} cr / Mtok`;
}

function Catalog() {
  const pricing = usePricing();
  if (pricing.error) {
    return <Banner tone="error">{(pricing.error as ApiError).message}</Banner>;
  }
  const models = pricing.data?.models ?? [];
  return (
    <TableShell
      head={[
        { label: "Model", truncate: true },
        "Channel",
        "Protocol",
        "v",
        { label: "Weight", align: "right" },
        { label: "Input", align: "right" },
        { label: "Output", align: "right" },
        { label: "Max out", align: "right" },
        { label: "Cached", align: "right" },
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
          <Td mono align="right">{formatRate(m.inputPricePerMillion)}</Td>
          <Td mono align="right">{formatRate(m.outputPricePerMillion)}</Td>
          <Td mono align="right">{m.maxOutputTokens?.toLocaleString() ?? "—"}</Td>
          <Td mono align="right">{formatRate(m.cacheReadPricePerMillion)}</Td>
        </tr>
      ))}
      {!pricing.isLoading && models.length === 0 ? (
        <EmptyRows colSpan={9} icon={<Tags />}>No routes priced yet.</EmptyRows>
      ) : null}
    </TableShell>
  );
}

function Estimator({ models }: { models: string[] }) {
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
      <p className="text-sm text-muted">
        The upper bound the gateway would freeze for a request of this shape. Nothing is held or
        charged — a real settle never exceeds the answer.
      </p>
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
        <Field label="Model">
          <SelectRoot
            value={model}
            onValueChange={(v) => setModel(String(v))}
          >
            <SelectTrigger>
              <SelectValue placeholder="Pick a model" />
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
        <Field label="Billing mode">
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
        <Field label="Input tokens" hint="empty estimates a prompt-free shape">
          <NumberInput value={inputTokens} onChange={(e) => setInputTokens(e.target.value)} min="0" />
        </Field>
        <Field label="Output tokens" hint="empty clamps to the model's ceiling">
          <NumberInput
            value={outputTokens}
            onChange={(e) => setOutputTokens(e.target.value)}
            min="0"
            disabled={mode !== "chat"}
          />
        </Field>
        <Field label="Service tier" hint="optional pricing dimension">
          <TextInput value={serviceTier} onChange={(e) => setServiceTier(e.target.value)} />
        </Field>
      </div>
      <div className="flex items-center gap-3">
        <Button
          type="submit"
          isLoading={estimate.isPending}
          loadingText="Estimating…"
          disabled={model === ""}
        >
          Estimate
        </Button>
        {estimate.data ? (
          <p className="text-lg font-semibold tabular-nums">
            ≤ {credits(estimate.data.estimateMinor)} cr
            <span className="ml-2 text-xs font-normal text-muted">
              price v{estimate.data.version}
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
      <p className="text-sm text-muted">
        A channel-model price row, signed under the same key that signs tree heads. A bill names the
        version it priced under — pass that to re-sign an older price.
      </p>
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
        <Field label="Channel">
          <SelectRoot value={channel} onValueChange={(v) => setChannel(String(v))}>
            <SelectTrigger>
              <SelectValue placeholder="Pick a channel" />
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
        <Field label="Model">
          <TextInput value={model} onChange={(e) => setModel(e.target.value)} placeholder="model id" />
        </Field>
        <Field label="Version" hint="empty signs the latest">
          <NumberInput value={version} onChange={(e) => setVersion(e.target.value)} min="1" />
        </Field>
      </div>
      {head.isLoading ? <PageState>Signing…</PageState> : null}
      {head.error instanceof ApiError ? (
        <Banner tone={head.error.status === 503 ? "warn" : "error"}>
          {head.error.status === 503
            ? "This deployment has no head-signing key configured."
            : head.error.status === 404
              ? "No such (channel, model, version) row."
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
            Verify against <code className="mono">{logKey.data.keyName}</code>:
          </span>
          <code className="mono">{logKey.data.publicKey}</code>
          <CopyButton value={logKey.data.publicKey} aria-label="Copy verifying key" />
        </div>
      ) : null}
    </div>
  );
}

export function Pricing() {
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
      <h1 className="text-2xl font-bold">Pricing</h1>
      <Card title="Catalog" actions={<span className="text-xs text-muted">what the gateway charges</span>}>
        <Catalog />
      </Card>
      <Card title="Estimate a request">
        <Estimator models={models} />
      </Card>
      <Card title="Signed price checkpoint">
        <PriceHeadViewer channels={channels} />
      </Card>
    </div>
  );
}
