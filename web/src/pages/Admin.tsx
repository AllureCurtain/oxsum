import { Fragment, useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";
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
  Switch,
  TableShell,
  Tabs,
  TabsList,
  TabsTrigger,
  Td,
  TextInput,
  credits,
} from "../components/ui";
import { ApiError } from "../api/client";

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
  "deposits",
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

// Sections address through the URL hash (#admin/<section>) so the
// breadcrumb can show real depth — `Admin / margin` instead of a bare
// "Admin".
const HASH_RE = /^#?admin\/([a-z-]+)$/;

function sectionFromHash(hash: string): (typeof SECTIONS)[number] | null {
  const match = HASH_RE.exec(hash);
  const s = match?.[1];
  return SECTIONS.includes(s as (typeof SECTIONS)[number])
    ? (s as (typeof SECTIONS)[number])
    : null;
}

export function Admin() {
  const [token, setToken] = useAdminToken();
  const [section, setSectionState] = useState<(typeof SECTIONS)[number]>(
    () => sectionFromHash(location.hash) ?? "channels",
  );
  const navigate = useNavigate();

  const setSection = (s: (typeof SECTIONS)[number]) => {
    setSectionState(s);
    navigate({ hash: `admin/${s}` }, { replace: true });
  };

  // The hash can change under us (the breadcrumb links, the back button);
  // keep the tab in step.
  useEffect(() => {
    const onHash = () => {
      const s = sectionFromHash(location.hash);
      if (s) setSectionState(s);
    };
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);
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
        <Tabs
          value={section}
          onValueChange={(v) => setSection(v as (typeof SECTIONS)[number])}
        >
          <TabsList variant="underline" className="flex-wrap">
            {SECTIONS.map((s) => (
              <TabsTrigger key={s} value={s}>
                {s}
              </TabsTrigger>
            ))}
          </TabsList>
          <div className="mt-4">
            <Section token={token} name={section} />
          </div>
        </Tabs>
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
    case "deposits":
      return <Deposits token={token} />;
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
            <Button
              type="button"
              isLoading={busy === "hold"}
              loadingText="Holding…"
              onClick={() =>
                call("/holds", { ...common, idempotencyKey: `met-${Date.now()}` }, "hold")
              }
            >
              POST /metering/holds
            </Button>
            <Field label="Hold key" hint="the key a holds answer returned">
              <TextInput value={holdKey} onChange={(e) => setHoldKey(e.target.value)} className="mono" />
            </Field>
            <Button
              type="button"
              isLoading={busy === "settle"}
              loadingText="Settling…"
              onClick={() =>
                call("/settle", { organizationId, holdKey }, "settle")
              }
            >
              POST /metering/settle
            </Button>
            <Button
              type="button"
              isLoading={busy === "release"}
              loadingText="Releasing…"
              onClick={() =>
                call("/release", { organizationId, holdKey }, "release", false)
              }
            >
              POST /metering/release
            </Button>
          </div>
        </Card>
        <Card title="One-shot bill">
          <div className="flex flex-col gap-2">
            <p className="text-sm text-muted">
              An event that already ran — declared usage bills at the code's latest
              <code className="mono"> mode: event</code> price, hold and settle in one answer.
            </p>
            <Button
              type="button"
              isLoading={busy === "bill"}
              loadingText="Billing…"
              onClick={() =>
                call("/settlements", { ...common, idempotencyKey: `met-${Date.now()}` }, "settlement")
              }
            >
              POST /metering/settlements
            </Button>
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
          <Button type="submit" isLoading={busy} loadingText="Resetting…">
            Reset password
          </Button>
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
              <Td mono align="right">{c.models.length}</Td>
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
      {orgs.error ? (
        <Banner tone="error">{orgs.error}</Banner>
      ) : (
        <>
          <TableShell
            head={[
              "Name",
              "Kind",
              { label: "Members", align: "right" },
              { label: "Available", align: "right" },
              { label: "Reserved", align: "right" },
              { label: "Credit", align: "right" },
              "State",
              "",
            ]}
            density="compact"
            loading={orgs.loading ? 6 : 0}
          >
            {!orgs.loading &&
              rows.map((o) => (
              <tr key={o.id}>
                <td>{o.name}</td>
                <td>{o.kind}</td>
                <Td mono align="right">{o.members}</Td>
                <Td mono align="right">{credits(o.availableMinor)} cr</Td>
                <Td mono align="right">{credits(o.reservedMinor)} cr</Td>
                <Td mono align="right">
                  {credits(o.creditUsedMinor)} / {credits(o.creditLimitMinor)} cr
                </Td>
                <td>{o.suspended ? <span className="text-danger">suspended</span> : "active"}</td>
                <td>
                  <Button tone="ghost" onClick={() => setAdjustId(adjustId === o.id ? null : o.id)}>
                    Adjust
                  </Button>
                </td>
              </tr>
            ))}
            {!orgs.loading && rows.length === 0 ? (
              <EmptyRows colSpan={8}>No organizations.</EmptyRows>
            ) : null}
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
            <Switch
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
      <CreditNotes token={token} id={id} />
    </Card>
  );
}

/// One credit note the admin surface lists (issue #352).
type CreditNote = {
  creditNoteId: string;
  entryId: string;
  organizationId: string;
  amountMinor: number;
  reason: string;
  createdAt: string;
};

/// The credit-note half of the per-organization card: issue a reasoned credit
/// (a new ledger entry, Revenue → Wallet, no money moved) and read back what
/// was issued. Deliberately beside the adjustment form, not merged into it.
function CreditNotes({ token, id }: { token: string; id: string }) {
  const [amount, setAmount] = useState("");
  const [reason, setReason] = useState("");
  const [notes, setNotes] = useState<CreditNote[]>([]);
  const [msg, setMsg] = useState<string | null>(null);
  const [version, setVersion] = useState(0);

  useEffect(() => {
    adminApi<{ rows: CreditNote[] }>(token, `/organizations/${id}/credit-notes`)
      .then((data) => setNotes(data.rows))
      .catch(() => setNotes([]));
  }, [token, id, version]);

  return (
    <div className="mt-4 border-t border-line pt-3">
      <h3 className="text-sm font-semibold">Credit notes</h3>
      <p className="text-xs text-muted">
        A reasoned credit for this organization: it posts a new ledger entry (Revenue debited,
        Wallet credited) and moves no money. It is not a refund.
      </p>
      <form
        className="mt-2 flex flex-wrap items-end gap-2"
        onSubmit={async (e) => {
          e.preventDefault();
          try {
            await adminApi(token, `/organizations/${id}/credit-notes`, {
              method: "POST",
              body: {
                amountMinor: Math.round(Number(amount) * 1_000_000),
                reason,
                idempotencyKey: `cn-${crypto.randomUUID()}`,
              },
            });
            setMsg("issued");
            setAmount("");
            setReason("");
            setVersion((v) => v + 1);
          } catch (err) {
            setMsg(err instanceof ApiError ? err.message : "failed");
          }
        }}
      >
        <Field label="Amount (credits)">
          <NumberInput
            required
            min={0}
            value={amount}
            onChange={(e) => setAmount(e.target.value)}
          />
        </Field>
        <Field label="Reason" hint="required — it becomes the entry's description">
          <TextInput value={reason} maxLength={200} onChange={(e) => setReason(e.target.value)} />
        </Field>
        <Button type="submit">Issue note</Button>
        {msg ? <span className="text-xs text-muted">{msg}</span> : null}
      </form>
      {notes.length > 0 ? (
        <div className="mt-2">
          <TableShell
            head={["Issued", { label: "Amount", align: "right" }, "Reason", { label: "Entry", truncate: true }]}
            density="compact"
            sticky={false}
          >
            {notes.map((note) => (
              <tr key={note.creditNoteId}>
                <td className="mono text-xs">{note.createdAt.slice(0, 19).replace("T", " ")}</td>
                <Td mono align="right">{credits(note.amountMinor)} cr</Td>
                <td className="text-xs">{note.reason}</td>
                <Td mono truncate className="text-xs text-muted">
                  {note.entryId}
                </Td>
              </tr>
            ))}
          </TableShell>
        </div>
      ) : (
        <p className="mt-2 text-xs text-muted">No credit notes issued to this organization yet.</p>
      )}
    </div>
  );
}

/// One money-in row the deposits view lists (issue #354); the reversal fields
/// land once `reversed` (issue #356).
type AdminDeposit = {
  depositId: string;
  organizationId: string;
  organization: string;
  rail: string;
  paymentRef: string;
  amountMinor: number;
  receivedMinor: number | null;
  status: string;
  entryId: string | null;
  reversalEntryId: string | null;
  reversedAt: string | null;
  refundStatus: "pending" | "succeeded" | "failed" | null;
  disputedAt: string | null;
  createdAt: string;
};

/// The deposits view: the money-in record, newest first, filterable by one
/// organization. A `credited` row carries a **Reverse** action — a refund
/// (issues #356, #360): it posts one entry taking the purchased credit back,
/// flips the row to `reversed`, and for a rail that moves money back it also
/// issues the rail's refund, whose standing the row carries as `refundStatus`.
function Deposits({ token }: { token: string }) {
  const [organization, setOrganization] = useState("");
  const [reversing, setReversing] = useState<string | null>(null);
  const orgs = useAdmin<
    { organizations?: AdminOrg[]; orgs?: AdminOrg[]; nextCursor?: string } | AdminOrg[]
  >(token, "/organizations", []);
  const orgRows = Array.isArray(orgs.data)
    ? orgs.data
    : (orgs.data?.organizations ?? orgs.data?.orgs ?? []);
  const deposits = useAdmin<AdminDeposit[]>(
    token,
    `/deposits?limit=100${organization ? `&organizationId=${organization}` : ""}`,
    [organization],
  );
  const rows = deposits.data ?? [];
  return (
    <div className="flex flex-col gap-3">
      <div className="flex items-center gap-2">
        <label className="text-xs text-muted" htmlFor="deposit-org">
          Organization
        </label>
        <select
          id="deposit-org"
          className="rounded-md border border-line bg-surface px-2 py-1 text-sm"
          value={organization}
          onChange={(e) => setOrganization(e.target.value)}
        >
          <option value="">All organizations</option>
          {orgRows.map((o) => (
            <option key={o.id} value={o.id}>
              {o.name}
            </option>
          ))}
        </select>
      </div>
      {deposits.error ? (
        <Banner tone="error">{deposits.error}</Banner>
      ) : (
        <TableShell
          head={[
            "When",
            "Organization",
            "Rail",
            "Status",
            { label: "Amount", align: "right" },
            { label: "Received", align: "right" },
            { label: "Reference", truncate: true },
            "Entry",
            "",
          ]}
          density="compact"
          loading={deposits.loading ? 6 : 0}
        >
          {!deposits.loading &&
            rows.map((d) => (
            <Fragment key={d.depositId}>
              <tr>
                <td className="mono text-xs">{d.createdAt.slice(0, 19).replace("T", " ")}</td>
                <td>{d.organization}</td>
                <td>{d.rail}</td>
                <td className="text-xs">
                  <span
                    className={
                      d.status === "credited"
                        ? "text-success"
                        : d.status === "reversed"
                          ? "text-danger"
                          : "text-muted"
                    }
                  >
                    {d.status}
                  </span>
                  {d.refundStatus ? (
                    <span
                      className={`block ${
                        d.refundStatus === "succeeded"
                          ? "text-success"
                          : d.refundStatus === "failed"
                            ? "text-danger"
                            : "text-muted"
                      }`}
                    >
                      refund {d.refundStatus}
                    </span>
                  ) : null}
                  {d.disputedAt ? (
                    <span className="block text-danger">disputed</span>
                  ) : null}
                </td>
                <Td mono align="right">{credits(d.amountMinor)} cr</Td>
                <Td mono align="right">
                  {d.receivedMinor === null ? "—" : `${credits(d.receivedMinor)} cr`}
                </Td>
                <Td mono truncate className="text-xs text-muted">{d.paymentRef}</Td>
                <td className="mono text-xs text-muted">
                  {d.status === "reversed" && d.reversalEntryId
                    ? `↩ ${d.reversalEntryId.slice(0, 8)}…`
                    : d.entryId
                      ? `${d.entryId.slice(0, 8)}…`
                      : "—"}
                </td>
                <td>
                  {d.status === "credited" ? (
                    <Button
                      tone="ghost"
                      onClick={() =>
                        setReversing(reversing === d.depositId ? null : d.depositId)
                      }
                    >
                      Reverse
                    </Button>
                  ) : null}
                </td>
              </tr>
              {reversing === d.depositId ? (
                <tr>
                  <td colSpan={9}>
                    <ReverseDeposit
                      token={token}
                      deposit={d}
                      onDone={() => {
                        setReversing(null);
                        deposits.refresh();
                      }}
                    />
                  </td>
                </tr>
              ) : null}
            </Fragment>
            ))}
          {!deposits.loading && rows.length === 0 ? (
            <EmptyRows colSpan={9}>
              No deposits yet — a top-up, a code redemption or a rail payment appears here.
            </EmptyRows>
          ) : null}
        </TableShell>
      )}
      <p className="text-xs text-muted">
        Statuses are the rails' facts: pending (opened, not paid), confirmed (the rail
        says paid, the ledger has not credited yet), credited (the entry named on the
        row moved the wallet), reversed (a refund or chargeback — the Entry cell then
        names the reversal's own entry) or expired. A received amount appears only
        when the rail reported a different figure than the deposit was opened for.
        Reversing posts one ledger entry taking the purchased credit back — and
        for a rail that moves money back (the Hyperswitch switch) it issues the
        rail's refund too: the row then shows `refund pending`/`succeeded`/`failed`
        under the status, `failed` meaning the rail refused the call so a retry
        re-issues it. `disputed` marks a charge the rail reported contested.
      </p>
    </div>
  );
}

/// The inline reverse form under a credited deposit's row (issue #356): a
/// required reason — it becomes the reversal entry's description — then one
/// call that debits the wallet and credits cash. A refusal surfaces inline:
/// `402` when the customer already spent the credit, `409` when the row moved
/// on between listing and the click.
function ReverseDeposit({
  token,
  deposit,
  onDone,
}: {
  token: string;
  deposit: AdminDeposit;
  onDone: () => void;
}) {
  const [reason, setReason] = useState("");
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);
  return (
    <form
      className="my-1 flex flex-wrap items-end gap-2"
      onSubmit={async (e) => {
        e.preventDefault();
        setBusy(true);
        setMsg(null);
        try {
          await adminApi(token, `/deposits/${deposit.depositId}/reverse`, {
            method: "POST",
            body: { reason },
          });
          onDone();
        } catch (err) {
          setMsg(err instanceof ApiError ? err.message : "failed");
          setBusy(false);
        }
      }}
    >
      <Field
        label={`Reverse ${credits(deposit.amountMinor)} cr — reason`}
        hint="required — it becomes the reversal entry's description"
      >
        <TextInput
          value={reason}
          maxLength={200}
          onChange={(e) => setReason(e.target.value)}
        />
      </Field>
      <Button type="submit" disabled={busy || reason.trim() === ""}>
        Reverse deposit
      </Button>
      {msg ? <span className="text-xs text-danger">{msg}</span> : null}
    </form>
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
      {holds.error ? (
        <Banner tone="error">{holds.error}</Banner>
      ) : (
        <TableShell
          head={[
            "Organization",
            { label: "Request", truncate: true },
            "Model",
            "Channel",
            { label: "Frozen", align: "right" },
            "Opened",
            "Sweeps",
          ]}
          density="compact"
          loading={holds.loading ? 6 : 0}
        >
          {!holds.loading &&
            (holds.data ?? []).map((h) => (
            <tr key={h.requestId}>
              <td>{h.organization}</td>
              <Td mono truncate>{h.requestId}</Td>
              <td>{h.model}</td>
              <td>{h.channel}</td>
              <Td mono align="right">{credits(h.freezeMinor)} cr</Td>
              <td>{String(h.openedAt).slice(0, 19).replace("T", " ")}</td>
              <td className="text-xs">
                {h.sweepAttempts ?? 0}
                {h.lastError ? <span className="block text-danger">{h.lastError}</span> : null}
              </td>
            </tr>
          ))}
          {!holds.loading && (holds.data?.length ?? 0) === 0 ? (
            <EmptyRows colSpan={7}>Nothing in flight.</EmptyRows>
          ) : null}
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
      {anomalies.error ? (
        <Banner tone="error">{anomalies.error}</Banner>
      ) : (
        <TableShell
          head={[
            "Organization",
            { label: "Request", truncate: true },
            "Model",
            "Channel",
            "Kind",
            { label: "Frozen", align: "right" },
            { label: "Charged", align: "right" },
            { label: "Attempts", align: "right" },
            "Booked",
          ]}
          density="compact"
          loading={anomalies.loading ? 6 : 0}
        >
          {!anomalies.loading &&
            (anomalies.data ?? []).map((a) => (
            <tr key={a.requestId}>
              <td>{a.organization}</td>
              <Td mono truncate>{a.requestId}</Td>
              <td>{a.model}</td>
              <td>{a.channel}</td>
              <td title={settlementOutcome(a.kind).detail}>
                {settlementOutcome(a.kind).label}
              </td>
              <Td mono align="right">{credits(a.freezeMinor)} cr</Td>
              <Td mono align="right">{credits(a.chargedMinor)} cr</Td>
              <Td mono align="right">{a.upstreamAttempts}</Td>
              <td>{a.bookedOn}</td>
            </tr>
          ))}
          {!anomalies.loading && (anomalies.data?.length ?? 0) === 0 ? (
            <EmptyRows colSpan={9}>No anomalies.</EmptyRows>
          ) : null}
        </TableShell>
      )}
    </div>
  );
}

function Margin({ token }: { token: string }) {
  const margin = useAdmin<
    {
      channel: string;
      model: string;
      turns: number;
      chargedMinor: number;
      upstreamCostMinor: number;
      providerBilledMinor: number | null;
      marginMinor: number;
      untrackedTurns: number;
    }[]
  >(token, "/margin");
  const rows = margin.data ?? [];
  const totals = rows.reduce(
    (sum, row) => ({
      turns: sum.turns + row.turns,
      charged: sum.charged + row.chargedMinor,
      upstream: sum.upstream + row.upstreamCostMinor,
      margin: sum.margin + row.marginMinor,
      untracked: sum.untracked + row.untrackedTurns,
    }),
    { turns: 0, charged: 0, upstream: 0, margin: 0, untracked: 0 },
  );
  // A channel's provider bill repeats on every one of its model rows (the
  // figure is channel-scoped), so the total counts each channel once.
  const billedByChannel = new Map<string, number>();
  for (const row of rows) {
    if (row.providerBilledMinor !== null) {
      billedByChannel.set(row.channel, row.providerBilledMinor);
    }
  }
  const providerTotal = [...billedByChannel.values()].reduce((a, b) => a + b, 0);
  return (
    <div className="flex flex-col gap-3">
      {margin.error ? (
        <Banner tone="error">{margin.error}</Banner>
      ) : (
        <>
          <TableShell
            head={[
              "Channel",
              "Model",
              { label: "Turns", align: "right" },
              { label: "Charged", align: "right" },
              { label: "Upstream cost", align: "right" },
              { label: "Provider bill", align: "right" },
              { label: "Margin", align: "right" },
              { label: "Untracked", align: "right" },
            ]}
            density="compact"
            loading={margin.loading ? 6 : 0}
          >
            {!margin.loading &&
              rows.map((m) => (
              <tr key={`${m.channel}-${m.model}`}>
                <td>{m.channel}</td>
                <td>{m.model}</td>
                <Td mono align="right">{m.turns}</Td>
                <Td mono align="right">{credits(m.chargedMinor)} cr</Td>
                <Td mono align="right">{credits(m.upstreamCostMinor)} cr</Td>
                <Td mono align="right" className="text-xs">
                  {m.providerBilledMinor === null ? (
                    <span className="text-muted">not reconciled</span>
                  ) : (
                    `${credits(m.providerBilledMinor)} cr`
                  )}
                </Td>
                <Td mono align="right" className={m.marginMinor < 0 ? "text-danger" : "text-success"}>
                  {credits(m.marginMinor)} cr
                </Td>
                <Td mono align="right">{m.untrackedTurns}</Td>
              </tr>
            ))}
            {!margin.loading && rows.length > 0 ? (
              <tr className="font-semibold">
                <td>Total</td>
                <td>
                  {rows.length} row{rows.length === 1 ? "" : "s"}
                </td>
                <Td mono align="right">{totals.turns}</Td>
                <Td mono align="right">{credits(totals.charged)} cr</Td>
                <Td mono align="right">{credits(totals.upstream)} cr</Td>
                <Td mono align="right" className="text-xs">
                  {billedByChannel.size === 0 ? (
                    <span className="text-muted">no channel reconciled</span>
                  ) : (
                    <>
                      {credits(providerTotal)} cr{" "}
                      <span className="text-muted">
                        ({billedByChannel.size} channel{billedByChannel.size === 1 ? "" : "s"})
                      </span>
                    </>
                  )}
                </Td>
                <Td mono align="right" className={totals.margin < 0 ? "text-danger" : "text-success"}>
                  {credits(totals.margin)} cr
                </Td>
                <Td mono align="right">{totals.untracked}</Td>
              </tr>
            ) : !margin.loading ? (
              <EmptyRows colSpan={8}>
                No settled turns yet — a row appears once a gateway turn settles and its upstream
                cost is recorded.
              </EmptyRows>
            ) : null}
          </TableShell>
          <p className="text-xs text-muted">
            The provider bill is what the upstream billed the channel, summed per day by the
            upstream-bill-sync pass; “not reconciled” means that pass has not billed the channel
            yet, not that it billed nothing. The total counts each channel once — the API repeats
            the channel-scoped figure on every model row. Untracked turns are settled turns with no
            upstream cost recorded (an untracked price, a swept hold, or history), so a growing gap
            there is where missing upstream pricing shows up first.
          </p>
        </>
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
  const [openClass, setOpenClass] = useState<string | null>(null);
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
        <TableShell
          head={["Class", { label: "Count", align: "right" }, "Sample"]}
          density="compact"
          sticky={false}
        >
          {report.data.classes.map((c) => {
            const open = openClass === c.class;
            const first = c.samples[0];
            return (
              <Fragment key={c.class}>
                <tr>
                  <td>
                    <button
                      type="button"
                      className="text-left text-primary"
                      onClick={() => setOpenClass(open ? null : c.class)}
                      aria-expanded={open}
                    >
                      {c.class}
                    </button>
                  </td>
                  <Td mono align="right" className={c.count > 0 ? "text-danger" : ""}>
                    {c.count}
                  </Td>
                  <td className="text-xs text-muted">
                    {c.count === 0
                      ? "none detected"
                      : first
                        ? `${first.organization ?? "platform"} ${first.detail ?? ""}`.trim()
                        : "—"}
                  </td>
                </tr>
                {open ? (
                  <tr>
                    <td colSpan={3} className="bg-canvas">
                      {c.samples.length === 0 ? (
                        <p className="text-xs text-muted">
                          Nothing in this class on the last scan.
                        </p>
                      ) : (
                        <ul className="flex flex-col gap-1">
                          {c.samples.map((sample, index) => (
                            <li key={index} className="mono text-xs">
                              <span className="text-muted">
                                {sample.organization ?? "platform"}
                              </span>{" "}
                              {sample.detail ?? "—"}
                            </li>
                          ))}
                        </ul>
                      )}
                      <p className="mt-1 text-xs text-muted">
                        {c.count > c.samples.length
                          ? `Showing ${c.samples.length} of ${c.count} offenders.`
                          : `${c.count} in this scan.`}
                      </p>
                    </td>
                  </tr>
                ) : null}
              </Fragment>
            );
          })}
        </TableShell>
      ) : null}
      <p className="text-xs text-muted">
        <code className="mono">upstream_price_drift</code> only appears for channels whose base URL
        serves the oxsum-compatible <code className="mono">GET /api/v1/pricing</code>. A real
        provider's cost is compared by the upstream bill sync instead.
      </p>
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
      {closings.error ? (
        <Banner tone="error">{closings.error}</Banner>
      ) : (
        <TableShell
          head={[
            "Month",
            "Organization",
            { label: "Entries", align: "right" },
            { label: "Tree size", align: "right" },
            { label: "Seal", truncate: true },
          ]}
          density="compact"
          loading={closings.loading ? 5 : 0}
        >
          {!closings.loading &&
            (closings.data ?? []).map((r) => (
            <tr key={`${r.organization}-${r.period}`}>
              <td className="mono">{r.period}</td>
              <td>{r.organization}</td>
              <Td mono align="right">{r.entryCount}</Td>
              <Td mono align="right">{r.treeSize}</Td>
              <Td mono truncate>{r.sealHash}</Td>
            </tr>
          ))}
          {!closings.loading && (closings.data?.length ?? 0) === 0 ? (
            <EmptyRows colSpan={5}>No sealed months.</EmptyRows>
          ) : null}
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
      {statements.error ? (
        <Banner tone="error">{statements.error}</Banner>
      ) : (
        <TableShell
          head={[
            "Period",
            "Organization",
            "Status",
            "Payment",
            { label: "Total", align: "right" },
            { label: "Outstanding", align: "right" },
            "",
          ]}
          density="compact"
          loading={statements.loading ? 5 : 0}
        >
          {!statements.loading &&
            (statements.data ?? []).map((s) => (
            <StatementRow key={s.id} token={token} s={s} onDone={statements.refresh} />
          ))}
          {!statements.loading && (statements.data?.length ?? 0) === 0 ? (
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
        <TableShell
          head={["Tier", { label: "RPM", align: "right" }, "Models", { label: "Organizations", align: "right" }, ""]}
          density="compact"
          sticky={false}
        >
          {rows.map((t) => (
            <tr key={t.name}>
              <td className="mono">{t.name}</td>
              <Td mono align="right">{t.requestsPerMinute ?? "∞"}</Td>
              <td>{t.modelAllowlist?.join(", ") ?? "all"}</td>
              <Td mono align="right">{t.organizations}</Td>
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
        <TableShell
          head={[{ label: "Percent", align: "right" }, "Organization", "Model", "Label", "Until", ""]}
          density="compact"
          sticky={false}
        >
          {rows.map((d) => (
            <tr key={d.discountId}>
              <Td mono align="right">{d.percent}%</Td>
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
      {audit.error ? (
        <Banner tone="error">{audit.error}</Banner>
      ) : (
        <TableShell
          head={["When", "Actor", "Action", "Target", { label: "Detail", truncate: true }]}
          density="compact"
          loading={audit.loading ? 8 : 0}
        >
          {!audit.loading &&
            rows.map((e) => (
            <tr key={e.auditId}>
              <td>{String(e.recordedAt).slice(0, 19).replace("T", " ")}</td>
              <td>{e.actor}</td>
              <td>{e.action}</td>
              <td className="mono">{e.target ?? "—"}</td>
              <Td truncate className="text-xs">
                {e.detail ? JSON.stringify(e.detail) : ""}
              </Td>
            </tr>
          ))}
          {!audit.loading && rows.length === 0 ? (
            <EmptyRows colSpan={5}>No audit entries.</EmptyRows>
          ) : null}
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
        <TableShell head={["Name", "Prefix", "Created", ""]} density="compact" sticky={false}>
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
