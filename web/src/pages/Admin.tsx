import { useEffect, useState } from "react";
import { settlementOutcome } from "../lib/kinds";
import {
  Banner,
  Button,
  Card,
  EmptyRows,
  Field,
  NumberInput,
  PageState,
  Select,
  TableShell,
  TextInput,
  credits,
} from "../components/ui";
import { ApiError } from "../api/client";
import { HaloSwitch } from "../components/cult/halo-switch";
import { HaloButton } from "../components/cult/halo-button";
import {
  HaloTabs,
  HaloTabsList,
  HaloTabsTrigger,
} from "../components/cult/halo-tabs";

// The admin console: the operator surface, token-gated like the server-rendered
// `/admin` — the token lives in this browser's localStorage and rides
// `Authorization: Bearer` on `/api/v1/admin/*`, never the session cookie.
const TOKEN_KEY = "oxsum.admin-token";

function useAdminToken(): [string | null, (token: string | null) => void] {
  const [token, setToken] = useState<string | null>(() => localStorage.getItem(TOKEN_KEY));
  return [
    token,
    (next) => {
      if (next === null) {
        localStorage.removeItem(TOKEN_KEY);
      } else {
        localStorage.setItem(TOKEN_KEY, next);
      }
      setToken(next);
    },
  ];
}

async function adminApi<T>(token: string, path: string, init?: { method?: string; body?: unknown }): Promise<T> {
  const res = await fetch(`/api/v1/admin${path}`, {
    method: init?.method ?? "GET",
    headers: {
      Authorization: `Bearer ${token}`,
      ...(init?.body !== undefined ? { "Content-Type": "application/json" } : {}),
    },
    body: init?.body !== undefined ? JSON.stringify(init.body) : undefined,
    credentials: "same-origin",
  });
  const text = await res.text();
  const body = text ? (JSON.parse(text) as { data?: T; error?: { code: string; message: string } }) : {};
  if (!res.ok) {
    throw new ApiError(body.error?.code ?? "UNKNOWN", body.error?.message ?? `HTTP ${res.status}`, res.status);
  }
  return body.data as T;
}

const SECTIONS = [
  "channels",
  "organizations",
  "in-flight",
  "anomalies",
  "margin",
  "reconciliation",
  "closings",
  "statements",
  "redemptions",
  "tiers",
  "discounts",
  "audit",
  "credentials",
  "users",
  "metering",
] as const;

export function Admin() {
  const [token, setToken] = useAdminToken();
  const [section, setSection] = useState<(typeof SECTIONS)[number]>("channels");
  const [draft, setDraft] = useState("");
  const [verified, setVerified] = useState<boolean | null>(null);

  useEffect(() => {
    if (!token) {
      setVerified(null);
      return;
    }
    adminApi(token, "/channels")
      .then(() => setVerified(true))
      .catch(() => setVerified(false));
  }, [token]);

  if (!token) {
    return (
      <div className="mx-auto mt-24 max-w-sm">
        <Card title="Admin console">
          <form
            className="flex flex-col gap-3"
            onSubmit={(e) => {
              e.preventDefault();
              setToken(draft);
            }}
          >
            <Field label="Operator token" hint="OXSUM_ADMIN_TOKEN — checked against the admin surface">
              <TextInput type="password" value={draft} onChange={(e) => setDraft(e.target.value)} required />
            </Field>
            <Button type="submit">Enter</Button>
          </form>
        </Card>
      </div>
    );
  }

  if (verified === false) {
    return (
      <div className="mx-auto mt-24 max-w-sm">
        <Card title="Admin console">
          <Banner tone="error">The token was refused.</Banner>
          <Button tone="ghost" className="mt-3" onClick={() => setToken(null)}>
            Re-enter
          </Button>
        </Card>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold">Admin</h1>
        <div className="flex items-center gap-3">
          <a href="/metrics" className="text-xs text-muted hover:text-ink" target="_blank" rel="noreferrer">
            /metrics
          </a>
          <Button tone="ghost" onClick={() => setToken(null)}>
            Leave console
          </Button>
        </div>
      </div>
      {verified === true && token ? (
        <HaloTabs
          value={section}
          onValueChange={(v) => setSection(v as (typeof SECTIONS)[number])}
        >
          <HaloTabsList variant="underline" className="flex-wrap">
            {SECTIONS.map((s) => (
              <HaloTabsTrigger key={s} value={s}>
                {s}
              </HaloTabsTrigger>
            ))}
          </HaloTabsList>
          <div className="mt-4">
            <Section token={token} name={section} />
          </div>
        </HaloTabs>
      ) : (
        <PageState>Checking token…</PageState>
      )}
    </div>
  );
}

function Section({ token, name }: { token: string; name: (typeof SECTIONS)[number] }) {
  switch (name) {
    case "channels":
      return <Channels token={token} />;
    case "organizations":
      return <Organizations token={token} />;
    case "in-flight":
      return <InFlight token={token} />;
    case "anomalies":
      return <Anomalies token={token} />;
    case "margin":
      return <Margin token={token} />;
    case "reconciliation":
      return <Reconciliation token={token} />;
    case "closings":
      return <Closings token={token} />;
    case "statements":
      return <AdminStatements token={token} />;
    case "redemptions":
      return <Redemptions token={token} />;
    case "tiers":
      return <Tiers token={token} />;
    case "discounts":
      return <Discounts token={token} />;
    case "audit":
      return <Audit token={token} />;
    case "credentials":
      return <Credentials token={token} />;
    case "users":
      return <UserReset token={token} />;
    case "metering":
      return <Metering />;
  }
}

// The metering surface takes a service credential, not the operator token —
// mint one under "credentials", paste it here, and the four routes become a
// manual test bench: hold, settle, release, or the one-shot bill.
const METERING_CRED_KEY = "oxsum.metering-credential";

async function meteringApi<T>(credential: string, path: string, body: unknown): Promise<T> {
  const res = await fetch(`/api/v1/metering${path}`, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${credential}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify(body),
    credentials: "same-origin",
  });
  const text = await res.text();
  const parsed = text ? (JSON.parse(text) as { data?: T; error?: { code: string; message: string } }) : {};
  if (!res.ok) {
    throw new ApiError(parsed.error?.code ?? "UNKNOWN", parsed.error?.message ?? `HTTP ${res.status}`, res.status);
  }
  return parsed.data as T;
}

function parseUsage(raw: string): Record<string, unknown> | string {
  try {
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    return parsed;
  } catch {
    return "usage is not JSON";
  }
}

function Metering() {
  const [cred, setCred] = useState(() => localStorage.getItem(METERING_CRED_KEY) ?? "");
  const [organizationId, setOrganizationId] = useState("");
  const [channel, setChannel] = useState("");
  const [billableCode, setBillableCode] = useState("");
  const [usageRaw, setUsageRaw] = useState('{\n  "eventType": "call",\n  "inputTokens": 0\n}');
  const [holdKey, setHoldKey] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [msg, setMsg] = useState<string | null>(null);

  async function call(
    path: string,
    body: Record<string, unknown>,
    label: string,
    withUsage = true,
  ) {
    const usage = parseUsage(usageRaw);
    if (withUsage && typeof usage === "string") {
      setMsg(usage);
      return;
    }
    setBusy(label);
    setMsg(null);
    try {
      const res = await meteringApi<Record<string, unknown>>(cred, path, {
        ...body,
        ...(withUsage ? { usage } : {}),
      });
      const key = typeof res.holdKey === "string" ? res.holdKey : null;
      if (key) setHoldKey(key);
      setMsg(`${label}: ${JSON.stringify(res)}`);
    } catch (err) {
      setMsg(err instanceof ApiError ? err.message : "failed");
    } finally {
      setBusy(null);
    }
  }

  const common = { organizationId, channel, billableCode };

  return (
    <div className="flex flex-col gap-4">
      <Card title="Service credential">
        <form
          className="flex items-end gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            localStorage.setItem(METERING_CRED_KEY, cred);
            setMsg("credential saved to this browser");
          }}
        >
          <Field label="Credential" hint="a service credential minted under 'credentials' — bearer on /api/v1/metering/*">
            <TextInput
              type="password"
              value={cred}
              onChange={(e) => setCred(e.target.value)}
              className="w-96 mono"
              required
            />
          </Field>
          <Button type="submit" tone="ghost">
            Save
          </Button>
        </form>
      </Card>

      <Card title="Event context">
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
          <Field label="Organization id">
            <TextInput value={organizationId} onChange={(e) => setOrganizationId(e.target.value)} className="mono" />
          </Field>
          <Field label="Channel">
            <TextInput value={channel} onChange={(e) => setChannel(e.target.value)} />
          </Field>
          <Field label="Billable code" hint="the price book's model name">
            <TextInput value={billableCode} onChange={(e) => setBillableCode(e.target.value)} />
          </Field>
        </div>
        <Field label="Usage (EventUsage JSON)" hint="eventType is required">
          <textarea
            className="mono w-full rounded-md border border-line bg-surface px-2.5 py-1.5 text-sm"
            rows={5}
            value={usageRaw}
            onChange={(e) => setUsageRaw(e.target.value)}
          />
        </Field>
      </Card>

      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
        <Card title="Hold then settle">
          <div className="flex flex-col gap-2">
            <HaloButton
              type="button"
              isLoading={busy === "hold"}
              loadingText="Holding…"
              onClick={() =>
                call("/holds", { ...common, idempotencyKey: `met-${Date.now()}` }, "hold")
              }
            >
              POST /metering/holds
            </HaloButton>
            <Field label="Hold key" hint="the key a holds answer returned">
              <TextInput value={holdKey} onChange={(e) => setHoldKey(e.target.value)} className="mono" />
            </Field>
            <HaloButton
              type="button"
              isLoading={busy === "settle"}
              loadingText="Settling…"
              onClick={() =>
                call("/settle", { organizationId, holdKey }, "settle")
              }
            >
              POST /metering/settle
            </HaloButton>
            <HaloButton
              type="button"
              isLoading={busy === "release"}
              loadingText="Releasing…"
              onClick={() =>
                call("/release", { organizationId, holdKey }, "release", false)
              }
            >
              POST /metering/release
            </HaloButton>
          </div>
        </Card>
        <Card title="One-shot bill">
          <div className="flex flex-col gap-2">
            <p className="text-sm text-muted">
              An event that already ran — declared usage bills at the code's latest
              <code className="mono"> mode: event</code> price, hold and settle in one answer.
            </p>
            <HaloButton
              type="button"
              isLoading={busy === "bill"}
              loadingText="Billing…"
              onClick={() =>
                call("/settlements", { ...common, idempotencyKey: `met-${Date.now()}` }, "settlement")
              }
            >
              POST /metering/settlements
            </HaloButton>
          </div>
        </Card>
      </div>
      {msg ? <Banner tone={msg.startsWith("usage") ? "error" : "info"}>{msg}</Banner> : null}
    </div>
  );
}

function UserReset({ token }: { token: string }) {
  const [userId, setUserId] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);
  return (
    <Card title="Reset a user's password">
      <form
        className="flex flex-col gap-3"
        onSubmit={async (e) => {
          e.preventDefault();
          setBusy(true);
          setMsg(null);
          try {
            const res = await adminApi<{ userId: string; sessionsRevoked: number }>(
              token,
              `/users/${userId}/password-reset`,
              { method: "POST", body: { newPassword: password } },
            );
            setMsg(`password set — ${res.sessionsRevoked} session(s) revoked`);
          } catch (err) {
            setMsg(err instanceof ApiError ? err.message : "failed");
          } finally {
            setBusy(false);
          }
        }}
      >
        <p className="text-sm text-muted">
          Sets the password and revokes every session the user holds — the only recovery path, since
          no mail goes out.
        </p>
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <Field label="User id">
            <TextInput
              value={userId}
              onChange={(e) => setUserId(e.target.value)}
              placeholder="uuid"
              className="mono"
              required
            />
          </Field>
          <Field label="New password" hint="12+ characters">
            <TextInput
              type="password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              minLength={12}
              required
            />
          </Field>
        </div>
        <div className="flex items-center gap-3">
          <HaloButton type="submit" isLoading={busy} loadingText="Resetting…">
            Reset password
          </HaloButton>
          {msg ? <span className="text-sm text-muted">{msg}</span> : null}
        </div>
      </form>
    </Card>
  );
}

// A tiny query wrapper — the admin surface answers `data` arrays or objects.
function useAdmin<T>(token: string, path: string, deps: unknown[] = []) {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [tick, setTick] = useState(0);
  useEffect(() => {
    setLoading(true);
    adminApi<T>(token, path)
      .then((d) => {
        setData(d);
        setError(null);
      })
      .catch((e) => setError(e instanceof ApiError ? e.message : "failed"))
      .finally(() => setLoading(false));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [token, path, tick, ...deps]);
  return { data, error, loading, refresh: () => setTick((t) => t + 1) };
}

interface Channel {
  name: string;
  baseUrl: string;
  protocol: string;
  apiKeyLast4: string;
  createdAt: string;
  models: { model: string; version: number; mode?: string }[];
}

function Channels({ token }: { token: string }) {
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
      {channels.loading ? (
        <PageState>Loading…</PageState>
      ) : channels.error ? (
        <Banner tone="error">{channels.error}</Banner>
      ) : (
        <TableShell head={["Name", "Base URL", "Protocol", "Key", "Models", ""]}>
          {(channels.data ?? []).map((c) => (
            <tr key={c.name}>
              <td className="mono">{c.name}</td>
              <td className="mono">{c.baseUrl}</td>
              <td>{c.protocol ?? "openai"}</td>
              <td className="mono">…{c.apiKeyLast4}</td>
              <td className="mono">{c.models.length}</td>
              <td>
                <Button tone="ghost" onClick={() => setPriceChannel(priceChannel === c.name ? null : c.name)}>
                  Prices
                </Button>
              </td>
            </tr>
          ))}
          {(channels.data?.length ?? 0) === 0 ? <EmptyRows colSpan={5}>No channels.</EmptyRows> : null}
        </TableShell>
      )}
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

interface AdminOrg {
  id: string;
  name: string;
  kind: string;
  members: number;
  availableMinor: number;
  reservedMinor: number;
  creditLimitMinor: number;
  creditUsedMinor: number;
  suspended?: boolean;
}

function Organizations({ token }: { token: string }) {
  const [cursor, setCursor] = useState<string | undefined>(undefined);
  const orgs = useAdmin<{ organizations?: AdminOrg[]; orgs?: AdminOrg[]; nextCursor?: string } | AdminOrg[]>(
    token,
    `/organizations${cursor ? `?cursor=${cursor}` : ""}`,
    [cursor],
  );
  const [adjustId, setAdjustId] = useState<string | null>(null);
  const rows = Array.isArray(orgs.data) ? orgs.data : (orgs.data?.organizations ?? orgs.data?.orgs ?? []);
  const nextCursor = Array.isArray(orgs.data) ? undefined : orgs.data?.nextCursor;

  return (
    <div className="flex flex-col gap-4">
      {orgs.loading ? (
        <PageState>Loading…</PageState>
      ) : orgs.error ? (
        <Banner tone="error">{orgs.error}</Banner>
      ) : (
        <>
          <TableShell head={["Name", "Kind", "Members", "Available", "Reserved", "Credit", "State", ""]}>
            {rows.map((o) => (
              <tr key={o.id}>
                <td>{o.name}</td>
                <td>{o.kind}</td>
                <td className="mono">{o.members}</td>
                <td className="mono">{credits(o.availableMinor)} cr</td>
                <td className="mono">{credits(o.reservedMinor)} cr</td>
                <td className="mono">
                  {credits(o.creditUsedMinor)} / {credits(o.creditLimitMinor)} cr
                </td>
                <td>{o.suspended ? <span className="text-danger">suspended</span> : "active"}</td>
                <td>
                  <Button tone="ghost" onClick={() => setAdjustId(adjustId === o.id ? null : o.id)}>
                    Adjust
                  </Button>
                </td>
              </tr>
            ))}
            {rows.length === 0 ? <EmptyRows colSpan={8}>No organizations.</EmptyRows> : null}
          </TableShell>
          {nextCursor ? (
            <Button tone="ghost" onClick={() => setCursor(nextCursor)}>
              Older
            </Button>
          ) : null}
        </>
      )}
      {adjustId ? <AdjustOrg token={token} id={adjustId} onDone={() => { setAdjustId(null); orgs.refresh(); }} /> : null}
    </div>
  );
}

function AdjustOrg({ token, id, onDone }: { token: string; id: string; onDone: () => void }) {
  const [amount, setAmount] = useState("");
  const [reason, setReason] = useState("");
  const [credit, setCredit] = useState("");
  const [terms, setTerms] = useState("");
  const [tier, setTier] = useState("");
  const [displayCode, setDisplayCode] = useState("");
  const [displayRate, setDisplayRate] = useState("");
  const [suspend, setSuspend] = useState<boolean | null>(null);
  const [msg, setMsg] = useState<string | null>(null);

  return (
    <Card title={`Manage ${id.slice(0, 8)}…`}>
      <form
        className="grid grid-cols-1 gap-3 sm:grid-cols-3"
        onSubmit={async (e) => {
          e.preventDefault();
          try {
            if (amount !== "") {
              await adminApi(token, `/organizations/${id}/adjustments`, {
                method: "POST",
                body: {
                  amountMinor: Math.round(Number(amount) * 1_000_000),
                  reason,
                  idempotencyKey: `adm-${Date.now()}`,
                },
              });
            }
            if (
              credit !== "" ||
              terms !== "" ||
              tier !== "" ||
              suspend !== null ||
              displayCode !== "" ||
              displayRate !== ""
            ) {
              await adminApi(token, `/organizations/${id}`, {
                method: "PATCH",
                body: {
                  idempotencyKey: `adm-${Date.now()}`,
                  ...(credit !== "" ? { creditLimitMinor: Math.round(Number(credit) * 1_000_000) } : {}),
                  ...(terms !== "" ? { paymentTermsDays: Number(terms) } : {}),
                  ...(tier !== "" ? { tier } : {}),
                  ...(suspend !== null ? { suspended: suspend } : {}),
                  ...(displayCode !== ""
                    ? {
                        displayCurrency: {
                          code: displayCode.trim().toUpperCase(),
                          ratePerCredit: displayRate.trim(),
                          decimals: 2,
                        },
                      }
                    : {}),
                },
              });
            }
            setMsg("saved");
            onDone();
          } catch (err) {
            setMsg(err instanceof ApiError ? err.message : "failed");
          }
        }}
      >
        <Field label="Adjust amount (credits, signed)">
          <NumberInput value={amount} onChange={(e) => setAmount(e.target.value)} />
        </Field>
        <Field label="Reason" hint="required for an adjustment">
          <TextInput value={reason} maxLength={200} onChange={(e) => setReason(e.target.value)} />
        </Field>
        <Field label="Credit limit (credits)">
          <NumberInput min={0} value={credit} onChange={(e) => setCredit(e.target.value)} />
        </Field>
        <Field label="Payment terms (days)">
          <NumberInput min={0} value={terms} onChange={(e) => setTerms(e.target.value)} />
        </Field>
        <Field label="Tier">
          <TextInput value={tier} onChange={(e) => setTier(e.target.value)} />
        </Field>
        <Field label="Display currency" hint="ISO code + rate per credit, e.g. USD / 0.0014">
          <div className="flex gap-2">
            <TextInput
              value={displayCode}
              onChange={(e) => setDisplayCode(e.target.value)}
              placeholder="USD"
              maxLength={3}
              className="w-20 uppercase"
            />
            <TextInput
              value={displayRate}
              onChange={(e) => setDisplayRate(e.target.value)}
              placeholder="0.0014"
            />
          </div>
        </Field>
        <Field label="Suspend spend" hint="new holds refuse; in-flight still settles">
          <div className="flex items-center gap-2 pt-2">
            <HaloSwitch
              checked={suspend ?? false}
              onCheckedChange={(v) => setSuspend(v)}
            />
            <span className="text-xs text-muted">
              {suspend === null ? "leave as is" : suspend ? "suspend" : "reinstate"}
            </span>
          </div>
        </Field>
        <div className="flex items-end gap-2">
          <Button type="submit">Apply</Button>
          {msg ? <span className="text-xs text-muted">{msg}</span> : null}
        </div>
      </form>
    </Card>
  );
}

function InFlight({ token }: { token: string }) {
  const holds = useAdmin<
    { organization: string; requestId: string; model: string; channel: string; freezeMinor: number; openedAt: string; sweepAttempts?: number; lastError?: string | null }[]
  >(token, "/holds");
  return (
    <div className="flex flex-col gap-3">
      <Button tone="ghost" onClick={holds.refresh} className="self-start">
        Refresh
      </Button>
      {holds.loading ? (
        <PageState>Loading…</PageState>
      ) : (
        <TableShell head={["Organization", "Request", "Model", "Channel", "Frozen", "Opened", "Sweeps"]}>
          {(holds.data ?? []).map((h) => (
            <tr key={h.requestId}>
              <td>{h.organization}</td>
              <td className="mono">{h.requestId}</td>
              <td>{h.model}</td>
              <td>{h.channel}</td>
              <td className="mono">{credits(h.freezeMinor)} cr</td>
              <td>{String(h.openedAt).slice(0, 19).replace("T", " ")}</td>
              <td className="text-xs">
                {h.sweepAttempts ?? 0}
                {h.lastError ? <span className="block text-danger">{h.lastError}</span> : null}
              </td>
            </tr>
          ))}
          {(holds.data?.length ?? 0) === 0 ? <EmptyRows colSpan={7}>Nothing in flight.</EmptyRows> : null}
        </TableShell>
      )}
    </div>
  );
}

function Anomalies({ token }: { token: string }) {
  const anomalies = useAdmin<
    { organization: string; requestId: string; model: string; channel: string; kind: string; chargedMinor: number; freezeMinor: number; bookedOn: string; upstreamAttempts: number }[]
  >(token, "/anomalies");
  return (
    <div>
      {anomalies.loading ? (
        <PageState>Loading…</PageState>
      ) : (
        <TableShell head={["Organization", "Request", "Model", "Channel", "Kind", "Frozen", "Charged", "Attempts", "Booked"]}>
          {(anomalies.data ?? []).map((a) => (
            <tr key={a.requestId}>
              <td>{a.organization}</td>
              <td className="mono">{a.requestId}</td>
              <td>{a.model}</td>
              <td>{a.channel}</td>
              <td title={settlementOutcome(a.kind).detail}>
                {settlementOutcome(a.kind).label}
              </td>
              <td className="mono">{credits(a.freezeMinor)} cr</td>
              <td className="mono">{credits(a.chargedMinor)} cr</td>
              <td className="mono">{a.upstreamAttempts}</td>
              <td>{a.bookedOn}</td>
            </tr>
          ))}
          {(anomalies.data?.length ?? 0) === 0 ? <EmptyRows colSpan={9}>No anomalies.</EmptyRows> : null}
        </TableShell>
      )}
    </div>
  );
}

function Margin({ token }: { token: string }) {
  const margin = useAdmin<
    { channel: string; model: string; turns: number; chargedMinor: number; upstreamCostMinor: number; marginMinor: number; untrackedTurns: number }[]
  >(token, "/margin");
  return (
    <div>
      {margin.loading ? (
        <PageState>Loading…</PageState>
      ) : (
        <TableShell head={["Channel", "Model", "Turns", "Charged", "Upstream cost", "Margin", "Untracked"]}>
          {(margin.data ?? []).map((m) => (
            <tr key={`${m.channel}-${m.model}`}>
              <td>{m.channel}</td>
              <td>{m.model}</td>
              <td className="mono">{m.turns}</td>
              <td className="mono">{credits(m.chargedMinor)} cr</td>
              <td className="mono">{credits(m.upstreamCostMinor)} cr</td>
              <td className={`mono ${m.marginMinor < 0 ? "text-danger" : "text-success"}`}>
                {credits(m.marginMinor)} cr
              </td>
              <td className="mono">{m.untrackedTurns}</td>
            </tr>
          ))}
          {(margin.data?.length ?? 0) === 0 ? <EmptyRows colSpan={7}>No settled turns yet.</EmptyRows> : null}
        </TableShell>
      )}
    </div>
  );
}

function Reconciliation({ token }: { token: string }) {
  const report = useAdmin<{
    checkedAt: string;
    organizations: number;
    clean: boolean;
    classes: { class: string; count: number; samples: { organization?: string; detail?: string }[] }[];
  }>(token, "/reconciliation");
  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center gap-3">
        <Button tone="ghost" onClick={report.refresh}>
          Re-run scan
        </Button>
        {report.data ? (
          <span className="text-sm text-muted">
            {report.data.organizations} organizations · checked{" "}
            {String(report.data.checkedAt).slice(0, 19).replace("T", " ")}
          </span>
        ) : null}
      </div>
      {report.data ? (
        report.data.clean ? (
          <Banner tone="success">Every ledger reconciles.</Banner>
        ) : null
      ) : report.loading ? (
        <PageState>Scanning…</PageState>
      ) : null}
      {report.data ? (
        <TableShell head={["Class", "Count", "Sample"]}>
          {report.data.classes.map((c) => (
            <tr key={c.class}>
              <td>{c.class}</td>
              <td className={`mono ${c.count > 0 ? "text-danger" : ""}`}>{c.count}</td>
              <td className="text-xs text-muted">
                {c.samples?.[0] ? `${c.samples[0].organization ?? ""} ${c.samples[0].detail ?? ""}` : "—"}
              </td>
            </tr>
          ))}
        </TableShell>
      ) : null}
    </div>
  );
}

function Closings({ token }: { token: string }) {
  const closings = useAdmin<
    { organization: string; period: string; entryCount: number; treeSize: number; treeRoot: string; sealHash: string }[]
  >(token, "/closings");
  const [month, setMonth] = useState("");
  const [msg, setMsg] = useState<string | null>(null);

  return (
    <div className="flex flex-col gap-4">
      <Card title="Seal a month">
        <form
          className="flex items-end gap-2"
          onSubmit={async (e) => {
            e.preventDefault();
            try {
              await adminApi(token, "/closings", { method: "POST", body: { month } });
              setMsg("sealed");
              closings.refresh();
            } catch (err) {
              setMsg(err instanceof ApiError ? err.message : "failed");
            }
          }}
        >
          <Field label="Month" hint="YYYY-MM — must have fully ended">
            <TextInput required pattern="\d{4}-\d{2}" value={month} onChange={(e) => setMonth(e.target.value)} placeholder="2026-04" />
          </Field>
          <Button type="submit">Seal</Button>
        </form>
        {msg ? <p className="mt-2 text-xs text-muted">{msg}</p> : null}
      </Card>
      {closings.loading ? (
        <PageState>Loading…</PageState>
      ) : (
        <TableShell head={["Month", "Organization", "Entries", "Tree size", "Seal"]}>
          {(closings.data ?? []).map((r) => (
            <tr key={`${r.organization}-${r.period}`}>
              <td className="mono">{r.period}</td>
              <td>{r.organization}</td>
              <td className="mono">{r.entryCount}</td>
              <td className="mono">{r.treeSize}</td>
              <td className="mono">{r.sealHash.slice(0, 16)}…</td>
            </tr>
          ))}
          {(closings.data?.length ?? 0) === 0 ? <EmptyRows colSpan={5}>No sealed months.</EmptyRows> : null}
        </TableShell>
      )}
    </div>
  );
}

function AdminStatements({ token }: { token: string }) {
  const statements = useAdmin<
    {
      id: string;
      organizationId: string;
      period: string;
      status: string;
      paymentStatus: string;
      totalMinor: number;
      creditDrawnMinor: number;
      outstandingMinor: number;
    }[]
  >(token, "/statements");
  return (
    <div>
      {statements.loading ? (
        <PageState>Loading…</PageState>
      ) : (
        <TableShell head={["Period", "Organization", "Status", "Payment", "Total", "Outstanding", ""]}>
          {(statements.data ?? []).map((s) => (
            <StatementRow key={s.id} token={token} s={s} onDone={statements.refresh} />
          ))}
          {(statements.data?.length ?? 0) === 0 ? (
            <EmptyRows colSpan={7}>No statements.</EmptyRows>
          ) : null}
        </TableShell>
      )}
    </div>
  );
}

type AdminStatement = {
  id: string;
  organizationId: string;
  period: string;
  status: string;
  paymentStatus: string;
  totalMinor: number;
  creditDrawnMinor: number;
  outstandingMinor: number;
};

function StatementRow({
  token,
  s,
  onDone,
}: {
  token: string;
  s: AdminStatement;
  onDone: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [payment, setPayment] = useState("");
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);

  const call = async (path: string, body?: unknown) => {
    setBusy(true);
    setMsg(null);
    try {
      await adminApi(token, path, { method: "POST", body });
      onDone();
    } catch (err) {
      setMsg(err instanceof ApiError ? err.message : "failed");
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <tr>
        <td>{s.period}</td>
        <td className="mono">{s.organizationId.slice(0, 8)}…</td>
        <td>{s.status}</td>
        <td>{s.paymentStatus}</td>
        <td className="mono">{credits(s.totalMinor)} cr</td>
        <td className="mono">{credits(s.outstandingMinor)} cr</td>
        <td className="flex items-center gap-2">
          {s.status === "finalized" ? (
            <a href={`/api/v1/admin/statements/${s.id}/export.pdf`} className="text-primary">
              PDF
            </a>
          ) : null}
          <Button tone="ghost" onClick={() => setOpen((v) => !v)}>
            {open ? "Close" : "Ops"}
          </Button>
        </td>
      </tr>
      {open ? (
        <tr>
          <td colSpan={7} className="bg-canvas">
            <div className="flex flex-wrap items-end gap-2 py-1">
              {s.status === "draft" ? (
                <Button
                  tone="ghost"
                  disabled={busy}
                  onClick={() =>
                    call(`/statements/${s.id}/finalize`, {
                      idempotencyKey: `fin-${s.id}-${Date.now()}`,
                    })
                  }
                >
                  Finalize
                </Button>
              ) : null}
              {s.status === "finalized" && (s.paymentStatus === "pending" || s.paymentStatus === "overdue") ? (
                <>
                  <Field label="Payment (credits)">
                    <NumberInput
                      value={payment}
                      onChange={(e) => setPayment(e.target.value)}
                      min="0.000001"
                    />
                  </Field>
                  <Button
                    tone="ghost"
                    disabled={busy || payment === ""}
                    onClick={() =>
                      call(`/statements/${s.id}/payments`, {
                        amountMinor: Math.round(Number(payment) * 1_000_000),
                        idempotencyKey: `pay-${s.id}-${Date.now()}`,
                      })
                    }
                  >
                    Record payment
                  </Button>
                  <Button
                    tone="danger"
                    disabled={busy}
                    onClick={() =>
                      call(`/statements/${s.id}/suspend`, {
                        idempotencyKey: `sus-${s.id}-${Date.now()}`,
                      })
                    }
                  >
                    Suspend
                  </Button>
                </>
              ) : null}
              {s.status === "finalized" && s.paymentStatus === "suspended" ? (
                <span className="text-xs text-muted">payment suspended</span>
              ) : null}
              {msg ? <span className="text-xs text-danger">{msg}</span> : null}
            </div>
          </td>
        </tr>
      ) : null}
    </>
  );
}

function Redemptions({ token }: { token: string }) {
  const [amount, setAmount] = useState("");
  const [count, setCount] = useState("1");
  const [batch, setBatch] = useState<{ batchId: string; codes: string[] } | null>(null);
  const [msg, setMsg] = useState<string | null>(null);

  return (
    <div className="flex flex-col gap-4">
      <Card title="Mint redemption codes">
        <form
          className="flex items-end gap-2"
          onSubmit={async (e) => {
            e.preventDefault();
            try {
              const res = await adminApi<{ batchId: string; codes: string[] }>(
                token,
                "/redemption-codes",
                {
                  method: "POST",
                  body: { amountMinor: Math.round(Number(amount) * 1_000_000), count: Number(count) },
                },
              );
              setBatch(res);
              setMsg(`${res.codes.length} minted`);
            } catch (err) {
              setMsg(err instanceof ApiError ? err.message : "failed");
            }
          }}
        >
          <Field label="Amount each (credits)">
            <NumberInput required min={0} value={amount} onChange={(e) => setAmount(e.target.value)} />
          </Field>
          <Field label="Count">
            <NumberInput required min={1} max={50} value={count} onChange={(e) => setCount(e.target.value)} />
          </Field>
          <Button type="submit">Mint</Button>
        </form>
        {batch ? (
          <div className="mt-3">
            <p className="mb-1 text-xs text-muted">
              Batch {batch.batchId.slice(0, 8)}… — the secrets, shown once:
            </p>
            <ul className="max-h-60 overflow-y-auto space-y-1">
              {batch.codes.map((c) => (
                <li key={c} className="mono text-xs">
                  {c}
                </li>
              ))}
            </ul>
          </div>
        ) : null}
        {msg ? <p className="mt-2 text-xs text-muted">{msg}</p> : null}
      </Card>
      <p className="text-xs text-muted">
        The codes' secrets are answered here and nowhere else — the database keeps only their
        hashes. Redeemed codes land on the wallet through `POST /api/v1/redemptions`.
      </p>
    </div>
  );
}

function Tiers({ token }: { token: string }) {
  const tiers = useAdmin<
    { name: string; requestsPerMinute?: number | null; modelAllowlist?: string[] | null; organizations: number }[]
  >(token, "/tiers");
  const [name, setName] = useState("");
  const [rpm, setRpm] = useState("");
  const [models, setModels] = useState("");
  const [msg, setMsg] = useState<string | null>(null);
  const rows = tiers.data ?? [];

  return (
    <div className="flex flex-col gap-4">
      <Card title="Set tier">
        <form
          className="grid grid-cols-1 gap-3 sm:grid-cols-3"
          onSubmit={async (e) => {
            e.preventDefault();
            try {
              await adminApi(token, `/tiers/${name}`, {
                method: "PUT",
                body: {
                  requestsPerMinute: rpm === "" ? null : Number(rpm),
                  modelAllowlist: models.trim() ? models.split(",").map((m) => m.trim()) : null,
                },
              });
              setMsg("saved");
              tiers.refresh();
            } catch (err) {
              setMsg(err instanceof ApiError ? err.message : "failed");
            }
          }}
        >
          <Field label="Name">
            <TextInput required value={name} onChange={(e) => setName(e.target.value)} />
          </Field>
          <Field label="Requests per minute">
            <NumberInput min={1} value={rpm} onChange={(e) => setRpm(e.target.value)} />
          </Field>
          <Field label="Model allowlist">
            <TextInput value={models} onChange={(e) => setModels(e.target.value)} />
          </Field>
          <div className="flex items-end">
            <Button type="submit">Save</Button>
          </div>
        </form>
        {msg ? <p className="mt-2 text-xs text-muted">{msg}</p> : null}
      </Card>
      {rows.length ? (
        <TableShell head={["Tier", "RPM", "Models", "Organizations", ""]}>
          {rows.map((t) => (
            <tr key={t.name}>
              <td className="mono">{t.name}</td>
              <td className="mono">{t.requestsPerMinute ?? "∞"}</td>
              <td>{t.modelAllowlist?.join(", ") ?? "all"}</td>
              <td className="mono">{t.organizations}</td>
              <td>
                <Button
                  tone="danger"
                  onClick={async () => {
                    try {
                      await adminApi(token, `/tiers/${t.name}`, { method: "DELETE" });
                      tiers.refresh();
                    } catch (err) {
                      setMsg(err instanceof ApiError ? err.message : "failed");
                    }
                  }}
                >
                  Delete
                </Button>
              </td>
            </tr>
          ))}
        </TableShell>
      ) : null}
    </div>
  );
}

function Discounts({ token }: { token: string }) {
  const discounts = useAdmin<
    {
      discountId: string;
      percent: number;
      organization?: string | null;
      model?: string | null;
      label?: string | null;
      validUntil?: string | null;
    }[]
  >(token, "/discounts");
  const [percent, setPercent] = useState("");
  const [orgId, setOrgId] = useState("");
  const [model, setModel] = useState("");
  const [label, setLabel] = useState("");
  const [msg, setMsg] = useState<string | null>(null);
  const rows = discounts.data ?? [];

  return (
    <div className="flex flex-col gap-4">
      <Card title="Create discount">
        <form
          className="grid grid-cols-1 gap-3 sm:grid-cols-4"
          onSubmit={async (e) => {
            e.preventDefault();
            try {
              await adminApi(token, "/discounts", {
                method: "POST",
                body: {
                  percent: Number(percent),
                  organizationId: orgId || null,
                  model: model || null,
                  label: label || null,
                  idempotencyKey: `discount-${Date.now()}`,
                },
              });
              setMsg("created");
              discounts.refresh();
            } catch (err) {
              setMsg(err instanceof ApiError ? err.message : "failed");
            }
          }}
        >
          <Field label="Percent off (1-100)">
            <NumberInput required min={1} max={100} value={percent} onChange={(e) => setPercent(e.target.value)} />
          </Field>
          <Field label="Organization id">
            <TextInput value={orgId} onChange={(e) => setOrgId(e.target.value)} placeholder="all" />
          </Field>
          <Field label="Model">
            <TextInput value={model} onChange={(e) => setModel(e.target.value)} placeholder="all" />
          </Field>
          <Field label="Label">
            <TextInput value={label} onChange={(e) => setLabel(e.target.value)} />
          </Field>
          <div className="flex items-end">
            <Button type="submit">Create</Button>
          </div>
        </form>
        {msg ? <p className="mt-2 text-xs text-muted">{msg}</p> : null}
      </Card>
      {rows.length ? (
        <TableShell head={["Percent", "Organization", "Model", "Label", "Until", ""]}>
          {rows.map((d) => (
            <tr key={d.discountId}>
              <td className="mono">{d.percent}%</td>
              <td>{d.organization ?? "all"}</td>
              <td>{d.model ?? "all"}</td>
              <td>{d.label ?? "—"}</td>
              <td>{d.validUntil ? String(d.validUntil).slice(0, 10) : "—"}</td>
              <td>
                <Button
                  tone="danger"
                  onClick={async () => {
                    try {
                      await adminApi(token, `/discounts/${d.discountId}`, { method: "DELETE" });
                      discounts.refresh();
                    } catch (err) {
                      setMsg(err instanceof ApiError ? err.message : "failed");
                    }
                  }}
                >
                  End
                </Button>
              </td>
            </tr>
          ))}
        </TableShell>
      ) : null}
    </div>
  );
}

function Audit({ token }: { token: string }) {
  const audit = useAdmin<{
    entries: { auditId: string; recordedAt: string; actor: string; action: string; target?: string | null; detail?: unknown }[];
    nextCursor?: string;
  }>(token, "/audit");
  const rows = audit.data?.entries ?? [];
  return (
    <div>
      {audit.loading ? (
        <PageState>Loading…</PageState>
      ) : (
        <TableShell head={["When", "Actor", "Action", "Target", "Detail"]}>
          {rows.map((e) => (
            <tr key={e.auditId}>
              <td>{String(e.recordedAt).slice(0, 19).replace("T", " ")}</td>
              <td>{e.actor}</td>
              <td>{e.action}</td>
              <td className="mono">{e.target ?? "—"}</td>
              <td className="text-xs">{e.detail ? JSON.stringify(e.detail) : ""}</td>
            </tr>
          ))}
          {rows.length === 0 ? <EmptyRows colSpan={5}>No audit entries.</EmptyRows> : null}
        </TableShell>
      )}
    </div>
  );
}

function Credentials({ token }: { token: string }) {
  const credentials = useAdmin<
    { credentialId: string; name?: string | null; prefix: string; createdAt: string; revokedAt?: string | null }[]
  >(token, "/service-credentials");
  const [name, setName] = useState("");
  const [secret, setSecret] = useState<string | null>(null);
  const [msg, setMsg] = useState<string | null>(null);
  const rows = credentials.data ?? [];

  return (
    <div className="flex flex-col gap-4">
      {secret ? (
        <Banner tone="success">
          Credential minted — shown once: <code className="mono break-all">{secret}</code>
        </Banner>
      ) : null}
      <Card title="Mint a metering credential">
        <form
          className="flex items-end gap-2"
          onSubmit={async (e) => {
            e.preventDefault();
            try {
              const res = await adminApi<{ secret: string }>(token, "/service-credentials", {
                method: "POST",
                body: { name },
              });
              setSecret(res.secret);
              credentials.refresh();
            } catch (err) {
              setMsg(err instanceof ApiError ? err.message : "failed");
            }
          }}
        >
          <Field label="Name">
            <TextInput required value={name} onChange={(e) => setName(e.target.value)} />
          </Field>
          <Button type="submit">Mint</Button>
        </form>
        {msg ? <p className="mt-2 text-xs text-muted">{msg}</p> : null}
      </Card>
      {rows.length ? (
        <TableShell head={["Name", "Prefix", "Created", ""]}>
          {rows.map((c) => (
            <tr key={c.credentialId}>
              <td>{c.name ?? "—"}</td>
              <td className="mono">{c.prefix}</td>
              <td>{c.createdAt.slice(0, 10)}</td>
              <td>
                {c.revokedAt ? (
                  <span className="text-xs text-danger">revoked</span>
                ) : (
                  <Button
                    tone="danger"
                    onClick={async () => {
                      try {
                        await adminApi(token, `/service-credentials/${c.credentialId}`, {
                          method: "DELETE",
                        });
                      } catch (err) {
                        setMsg(err instanceof ApiError ? err.message : "failed");
                      }
                      credentials.refresh();
                    }}
                  >
                    Revoke
                  </Button>
                )}
              </td>
            </tr>
          ))}
        </TableShell>
      ) : null}
    </div>
  );
}
