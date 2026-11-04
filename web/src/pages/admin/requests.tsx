import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Search } from "lucide-react";
import {
  Banner,
  Button,
  Card,
  EmptyRows,
  Field,
  Select,
  StatusBadge,
  TableShell,
  Td,
  TextInput,
  credits,
} from "../../components/ui";
import { useAdmin } from "./api";

/** One gateway request's operational record, as `GET /api/v1/admin/requests`
 * answers it (issue #505). The admin read adds `organizationId`, the field the
 * member read elides. */
interface AdminRequestRow {
  requestId: string;
  entryId?: string;
  organizationId: string;
  keyId?: string;
  modelRequested: string;
  modelServed: string;
  channel: string;
  admissionOutcome: "admitted" | "refused";
  status: string;
  ttftMs?: number;
  totalMs?: number;
  inputTokens?: number;
  outputTokens?: number;
  chargeMinor?: number;
  upstreamAttempts?: number;
  errorKind?: string;
  createdAt: string;
}

/**
 * The operator's all-organization request log (issue #574): the same search
 * surface the member Requests page runs, widened to every organization. It is
 * read-only — the page answers "which requests were turned away" and links each
 * settled row to its proof, and never writes.
 */
export function AdminRequests({ token }: { token: string }) {
  const { t } = useTranslation();
  const [organizationId, setOrganizationId] = useState("");
  const [model, setModel] = useState("");
  const [channel, setChannel] = useState("");
  const [status, setStatus] = useState("");
  const [minCharge, setMinCharge] = useState("");
  const [from, setFrom] = useState("");
  const [to, setTo] = useState("");
  const [cursor, setCursor] = useState<string | undefined>(undefined);
  // Filters bind on submit — a query run is a deliberate act, not a live
  // filter over keystrokes, the same rule the member page keeps.
  const [active, setActive] = useState<Record<string, string | number | undefined>>({});

  const params = new URLSearchParams();
  for (const [key, value] of Object.entries({ ...active, limit: 50, cursor })) {
    if (value !== undefined && value !== "") {
      params.set(key, String(value));
    }
  }
  const log = useAdmin<{ rows: AdminRequestRow[]; nextCursor?: string }>(
    token,
    `/requests?${params.toString()}`,
    [active, cursor],
  );
  const rows = log.data?.rows ?? [];

  function submit(event: React.FormEvent) {
    event.preventDefault();
    setCursor(undefined);
    setActive({
      organizationId: organizationId.trim() || undefined,
      model: model.trim() || undefined,
      channel: channel.trim() || undefined,
      status: status || undefined,
      minCharge: minCharge ? Number(minCharge) : undefined,
      from: from ? new Date(from).toISOString() : undefined,
      to: to ? new Date(to).toISOString() : undefined,
    });
  }

  function narrowToOrganization(id: string) {
    setOrganizationId(id);
    setCursor(undefined);
    setActive((current) => ({ ...current, organizationId: id }));
  }

  return (
    <div className="flex flex-col gap-3">
      <Card title={t("admin.requests.filterTitle")}>
        <form onSubmit={submit} className="flex flex-col gap-3">
          <p className="text-sm text-muted">{t("admin.requests.intro")}</p>
          <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
            <Field label={t("admin.requests.filterOrganization")} hint={t("admin.requests.filterOrganizationHint")}>
              <TextInput
                value={organizationId}
                onChange={(e) => setOrganizationId(e.target.value)}
                placeholder="00000000-0000-0000-0000-000000000000"
              />
            </Field>
            <Field label={t("admin.requests.filterModel")}>
              <TextInput value={model} onChange={(e) => setModel(e.target.value)} placeholder="gpt-4o-mini" />
            </Field>
            <Field label={t("admin.requests.filterChannel")}>
              <TextInput value={channel} onChange={(e) => setChannel(e.target.value)} placeholder="openai-main" />
            </Field>
            <Field label={t("admin.requests.filterStatus")}>
              <Select value={status} onChange={(e) => setStatus(e.target.value)}>
                <option value="">{t("admin.requests.statusAny")}</option>
                <option value="settled">settled</option>
                <option value="errored">errored</option>
                <option value="refused">refused</option>
              </Select>
            </Field>
            <Field label={t("admin.requests.filterFrom")}>
              <TextInput type="datetime-local" value={from} onChange={(e) => setFrom(e.target.value)} />
            </Field>
            <Field label={t("admin.requests.filterTo")}>
              <TextInput type="datetime-local" value={to} onChange={(e) => setTo(e.target.value)} />
            </Field>
            <Field label={t("admin.requests.filterMinCharge")} hint={t("admin.requests.minChargeHint")}>
              <TextInput value={minCharge} onChange={(e) => setMinCharge(e.target.value)} placeholder="0" />
            </Field>
            <div className="flex items-end gap-2">
              <Button type="submit" isLoading={log.loading} loadingText={t("admin.requests.searching")}>
                <Search className="h-4 w-4" /> {t("admin.requests.search")}
              </Button>
              <Button
                tone="ghost"
                type="button"
                onClick={() => {
                  setOrganizationId("");
                  setModel("");
                  setChannel("");
                  setStatus("");
                  setMinCharge("");
                  setFrom("");
                  setTo("");
                  setCursor(undefined);
                  setActive({});
                }}
              >
                {t("admin.requests.clear")}
              </Button>
            </div>
          </div>
        </form>
      </Card>

      {log.error ? (
        <Banner tone="error">{log.error}</Banner>
      ) : (
        <>
          <TableShell
            head={[
              t("admin.requests.colTime"),
              t("admin.requests.colOrganization"),
              { label: t("admin.requests.colRequest"), truncate: true },
              t("admin.requests.colAsked"),
              t("admin.requests.colServed"),
              t("admin.requests.colChannel"),
              t("admin.requests.colStatus"),
              { label: t("admin.requests.colTokens"), align: "right" },
              { label: t("admin.requests.colCharged"), align: "right" },
              "",
            ]}
            density="compact"
            loading={log.loading ? 8 : 0}
          >
            {!log.loading &&
              rows.map((row) => (
                <tr key={row.requestId}>
                  <td>{String(row.createdAt).slice(0, 19).replace("T", " ")}</td>
                  <td>
                    <button
                      type="button"
                      className="mono text-primary hover:underline"
                      title={row.organizationId}
                      onClick={() => narrowToOrganization(row.organizationId)}
                    >
                      {row.organizationId.slice(0, 8)}
                    </button>
                  </td>
                  <Td mono truncate>{row.requestId}</Td>
                  <td>{row.modelRequested}</td>
                  <td>{row.modelServed || "—"}</td>
                  <td>{row.channel || "—"}</td>
                  <td>
                    <StatusBadge status={row.status} />
                    {row.errorKind ? <p className="text-xs text-danger">{row.errorKind}</p> : null}
                  </td>
                  <Td mono align="right">
                    {(row.inputTokens ?? 0)}+{row.outputTokens ?? 0}
                  </Td>
                  <Td mono align="right">{credits(row.chargeMinor ?? 0)} cr</Td>
                  <td>
                    {row.entryId ? (
                      <a href={`/verify?entry=${row.entryId}`} className="text-primary">
                        {t("common.verify")}
                      </a>
                    ) : null}
                  </td>
                </tr>
              ))}
            {!log.loading && rows.length === 0 ? (
              <EmptyRows colSpan={10} icon={<Search />}>
                {t("admin.requests.empty")}
              </EmptyRows>
            ) : null}
          </TableShell>
          {log.data?.nextCursor != null ? (
            <p>
              <Button tone="ghost" onClick={() => setCursor(log.data?.nextCursor)}>
                {t("common.older")}
              </Button>
            </p>
          ) : null}
        </>
      )}
    </div>
  );
}
