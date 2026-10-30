import { useState } from "react";
import { Banner, Button, Card, Field, TextInput } from "../../components/ui";
import { ApiError } from "../../api/client";

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

export function Metering() {
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
