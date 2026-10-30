import { useState } from "react";
import {
  Banner,
  Button,
  Card,
  EmptyRows,
  Field,
  NumberInput,
  Select,
  TableShell,
  Td,
  TextInput,
} from "../../components/ui";
import { ApiError } from "../../api/client";
import { adminApi, useAdmin } from "./api";

interface Channel {
  name: string;
  baseUrl: string;
  protocol: string;
  apiKeyLast4: string;
  createdAt: string;
  models: { model: string; version: number; mode?: string }[];
}

export function Channels({ token }: { token: string }) {
  const channels = useAdmin<Channel[]>(token, "/channels");
  const [name, setName] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [protocol, setProtocol] = useState("openai");
  const [priceChannel, setPriceChannel] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);

  return (
    <div className="flex flex-col gap-6">
      <Card title="Upsert channel">
        <form
          className="grid grid-cols-1 gap-3 sm:grid-cols-2"
          onSubmit={async (e) => {
            e.preventDefault();
            setBusy(true);
            setFormError(null);
            try {
              await adminApi(token, "/channels", {
                method: "POST",
                body: { name, baseUrl, apiKey, protocol },
              });
              setName("");
              setBaseUrl("");
              setApiKey("");
              channels.refresh();
            } catch (err) {
              setFormError(err instanceof ApiError ? err.message : "failed");
            } finally {
              setBusy(false);
            }
          }}
        >
          <Field label="Name">
            <TextInput required value={name} onChange={(e) => setName(e.target.value)} />
          </Field>
          <Field label="Base URL">
            <TextInput required type="url" value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} />
          </Field>
          <Field label="Upstream API key">
            <TextInput required type="password" value={apiKey} onChange={(e) => setApiKey(e.target.value)} />
          </Field>
          <Field label="Protocol">
            <Select value={protocol} onChange={(e) => setProtocol(e.target.value)}>
              <option value="openai">openai</option>
              <option value="anthropic">anthropic</option>
            </Select>
          </Field>
          <div className="flex items-end gap-2">
            <Button type="submit" disabled={busy}>
              Save channel
            </Button>
            {formError ? <span className="text-xs text-danger">{formError}</span> : null}
          </div>
        </form>
      </Card>
      {channels.error ? (
        <Banner tone="error">{channels.error}</Banner>
      ) : (
        <TableShell
          head={["Name", { label: "Base URL", truncate: true }, "Protocol", "Key", { label: "Models", align: "right" }, ""]}
          density="compact"
          loading={channels.loading ? 5 : 0}
        >
          {!channels.loading &&
            (channels.data ?? []).map((c) => (
            <tr key={c.name}>
              <td className="mono">{c.name}</td>
              <Td mono truncate>{c.baseUrl}</Td>
              <td>{c.protocol ?? "openai"}</td>
              <td className="mono">…{c.apiKeyLast4}</td>
              <Td align="right">{c.models.length}</Td>
              <td>
                <Button tone="ghost" onClick={() => setPriceChannel(priceChannel === c.name ? null : c.name)}>
                  Prices
                </Button>
              </td>
            </tr>
          ))}
          {!channels.loading && (channels.data?.length ?? 0) === 0 ? (
            <EmptyRows colSpan={6}>No channels.</EmptyRows>
          ) : null}
        </TableShell>
      )}
      <p className="text-xs text-muted">
        Price drift is probed from each channel's <code className="mono">GET /api/v1/pricing</code>{" "}
        catalog — an oxsum-compatible endpoint. A channel pointing at a real provider serves no such
        catalog, so its cost signal is the upstream bill sync, not this probe.
      </p>
      {priceChannel ? <ChannelPrices token={token} channel={priceChannel} /> : null}
    </div>
  );
}

function ChannelPrices({ token, channel }: { token: string; channel: string }) {
  // The channel's models with their current price versions are already on the
  // channel row; the price form appends a new version to one model.
  const [model, setModel] = useState("");
  const [mode, setMode] = useState("chat");
  const [inputPrice, setInputPrice] = useState("");
  const [outputPrice, setOutputPrice] = useState("");
  const [costPerRequest, setCostPerRequest] = useState("");
  const [msg, setMsg] = useState<string | null>(null);

  return (
    <Card title={`Append a price version — ${channel}`}>
      <form
        className="grid grid-cols-1 gap-3 sm:grid-cols-3"
        onSubmit={async (e) => {
          e.preventDefault();
          try {
            await adminApi(token, `/channels/${channel}/prices`, {
              method: "POST",
              body: {
                model,
                mode,
                inputPricePerMillion: Math.round(Number(inputPrice) * 1_000_000),
                outputPricePerMillion:
                  outputPrice === "" ? null : Math.round(Number(outputPrice) * 1_000_000),
                costPerRequest:
                  costPerRequest === "" ? null : Math.round(Number(costPerRequest) * 1_000_000),
              },
            });
            setMsg("price version stored");
          } catch (err) {
            setMsg(err instanceof ApiError ? err.message : "failed");
          }
        }}
      >
        <Field label="Model">
          <TextInput required value={model} onChange={(e) => setModel(e.target.value)} />
        </Field>
        <Field label="Mode">
          <Select value={mode} onChange={(e) => setMode(e.target.value)}>
            <option value="chat">chat</option>
            <option value="embeddings">embeddings</option>
            <option value="rerank">rerank</option>
            <option value="event">event</option>
          </Select>
        </Field>
        <Field label="Input price / million (credits)">
          <NumberInput required min={0} value={inputPrice} onChange={(e) => setInputPrice(e.target.value)} />
        </Field>
        <Field label="Output price / million (credits)">
          <NumberInput min={0} value={outputPrice} onChange={(e) => setOutputPrice(e.target.value)} />
        </Field>
        <Field label="Cost per request (credits)">
          <NumberInput min={0} value={costPerRequest} onChange={(e) => setCostPerRequest(e.target.value)} />
        </Field>
        <div className="flex items-end gap-2">
          <Button type="submit">Set price</Button>
          {msg ? <span className="text-xs text-muted">{msg}</span> : null}
        </div>
      </form>
    </Card>
  );
}
