import { useState } from "react";
import { Trans, useTranslation } from "react-i18next";
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

function parseUsage(raw: string, notJson: string): Record<string, unknown> | string {
  try {
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    return parsed;
  } catch {
    return notJson;
  }
}

export function Metering() {
  const { t } = useTranslation();
  const [cred, setCred] = useState(() => localStorage.getItem(METERING_CRED_KEY) ?? "");
  const [organizationId, setOrganizationId] = useState("");
  const [channel, setChannel] = useState("");
  const [billableCode, setBillableCode] = useState("");
  const [usageRaw, setUsageRaw] = useState('{\n  "eventType": "call",\n  "inputTokens": 0\n}');
  const [holdKey, setHoldKey] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [msg, setMsg] = useState<string | null>(null);
  const [msgTone, setMsgTone] = useState<"error" | "info">("info");

  async function call(
    path: string,
    body: Record<string, unknown>,
    label: string,
    withUsage = true,
  ) {
    const usage = parseUsage(usageRaw, t("admin.metering.usageNotJson"));
    if (withUsage && typeof usage === "string") {
      setMsg(usage);
      setMsgTone("error");
      return;
    }
    setBusy(label);
    setMsg(null);
    setMsgTone("info");
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
      <Card title={t("admin.metering.credentialTitle")}>
        <form
          className="flex items-end gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            localStorage.setItem(METERING_CRED_KEY, cred);
            setMsg(t("admin.metering.credentialSaved"));
            setMsgTone("info");
          }}
        >
          <Field label={t("admin.metering.credential")} hint={t("admin.metering.credentialHint")}>
            <TextInput
              type="password"
              value={cred}
              onChange={(e) => setCred(e.target.value)}
              className="w-96 mono"
              required
            />
          </Field>
          <Button type="submit" tone="ghost">
            {t("common.save")}
          </Button>
        </form>
      </Card>

      <Card title={t("admin.metering.eventContext")}>
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
          <Field label={t("admin.metering.organizationId")}>
            <TextInput value={organizationId} onChange={(e) => setOrganizationId(e.target.value)} className="mono" />
          </Field>
          <Field label={t("admin.metering.channel")}>
            <TextInput value={channel} onChange={(e) => setChannel(e.target.value)} />
          </Field>
          <Field label={t("admin.metering.billableCode")} hint={t("admin.metering.billableCodeHint")}>
            <TextInput value={billableCode} onChange={(e) => setBillableCode(e.target.value)} />
          </Field>
        </div>
        <Field label={t("admin.metering.usageJson")} hint={t("admin.metering.usageJsonHint")}>
          <textarea
            className="mono w-full rounded-md border border-line bg-surface px-2.5 py-1.5 text-sm"
            rows={5}
            value={usageRaw}
            onChange={(e) => setUsageRaw(e.target.value)}
          />
        </Field>
      </Card>

      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
        <Card title={t("admin.metering.holdThenSettle")}>
          <div className="flex flex-col gap-2">
            <Button
              type="button"
              isLoading={busy === t("admin.metering.hold")}
              loadingText={t("admin.metering.holding")}
              onClick={() =>
                call("/holds", { ...common, idempotencyKey: `met-${Date.now()}` }, t("admin.metering.hold"))
              }
            >
              POST /metering/holds
            </Button>
            <Field label={t("admin.metering.holdKey")} hint={t("admin.metering.holdKeyHint")}>
              <TextInput value={holdKey} onChange={(e) => setHoldKey(e.target.value)} className="mono" />
            </Field>
            <Button
              type="button"
              isLoading={busy === t("admin.metering.settle")}
              loadingText={t("admin.metering.settling")}
              onClick={() =>
                call("/settle", { organizationId, holdKey }, t("admin.metering.settle"))
              }
            >
              POST /metering/settle
            </Button>
            <Button
              type="button"
              isLoading={busy === t("admin.metering.release")}
              loadingText={t("admin.metering.releasing")}
              onClick={() =>
                call("/release", { organizationId, holdKey }, t("admin.metering.release"), false)
              }
            >
              POST /metering/release
            </Button>
          </div>
        </Card>
        <Card title={t("admin.metering.oneShotTitle")}>
          <div className="flex flex-col gap-2">
            <p className="text-sm text-muted">
              <Trans
                i18nKey="admin.metering.oneShotNote"
                components={{ code: <code className="mono" /> }}
              />
            </p>
            <Button
              type="button"
              isLoading={busy === t("admin.metering.settlement")}
              loadingText={t("admin.metering.billing")}
              onClick={() =>
                call("/settlements", { ...common, idempotencyKey: `met-${Date.now()}` }, t("admin.metering.settlement"))
              }
            >
              POST /metering/settlements
            </Button>
          </div>
        </Card>
      </div>
      {msg ? <Banner tone={msgTone}>{msg}</Banner> : null}
    </div>
  );
}
