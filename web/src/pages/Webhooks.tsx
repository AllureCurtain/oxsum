import { useState } from "react";
import { ApiError } from "../api/client";
import { Switch } from "../components/ui";
import { CopyButton } from "../components/ui";
import {
  useCreateWebhook,
  useDeleteWebhook,
  useWebhookDeliveries,
  useWebhooks,
} from "../api/hooks";
import {
  Banner,
  Button,
  Card,
  EmptyRows,
  Field,
  NumberInput,
  PageState,
  TableShell,
  TextInput,
} from "../components/ui";

const EVENT_TYPES = [
  "request.settled",
  "org.suspended",
  "deposit.credited",
  "deposit.failed",
  "deposit.reversed",
  "balance.low",
  "statement.finalized",
  "statement.payment_overdue",
  "statement.payment_status_updated",
  "org.credit_limit_exceeded",
  "anomaly.detected",
  "budget.exhaustion_forecast",
  "budget.threshold_crossed",
] as const;

export function Webhooks() {
  const webhooks = useWebhooks();
  const create = useCreateWebhook();
  const remove = useDeleteWebhook();
  const [url, setUrl] = useState("");
  const [events, setEvents] = useState<string[]>([]);
  const [balanceLow, setBalanceLow] = useState("");
  const [thresholds, setThresholds] = useState("");
  const [secret, setSecret] = useState<string | null>(null);
  const [deliveriesFor, setDeliveriesFor] = useState<string | null>(null);

  return (
    <div className="flex flex-col gap-6">
      <h1 className="text-2xl font-bold">Webhooks</h1>

      {secret ? (
        <Banner tone="success">
          Endpoint created. Signing secret — shown once: <code className="mono">{secret}</code>
          <CopyButton value={secret} aria-label="Copy signing secret" />
          <Button tone="ghost" className="ml-3" onClick={() => setSecret(null)}>
            Done
          </Button>
        </Banner>
      ) : null}
      {create.error instanceof ApiError ? <Banner tone="error">{create.error.message}</Banner> : null}

      <Card title="New endpoint">
        <form
          className="grid grid-cols-1 gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            create.mutate(
              {
                url,
                events,
                balanceLowMinor: balanceLow === "" ? null : Math.round(Number(balanceLow) * 1_000_000),
                budgetThresholdsPercent: thresholds.trim()
                  ? thresholds.split(",").map((t) => Number(t.trim()))
                  : undefined,
              },
              {
                onSuccess: (created) => {
                  setSecret(created.secret);
                  setUrl("");
                  setEvents([]);
                  setBalanceLow("");
                  setThresholds("");
                },
              },
            );
          }}
        >
          <Field label="URL">
            <TextInput
              type="url"
              required
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              placeholder="https://example.com/hook"
            />
          </Field>
          <Field label="Events">
            <div className="grid grid-cols-2 gap-1 sm:grid-cols-3">
              {EVENT_TYPES.map((event) => (
                <label key={event} className="flex items-center gap-2 text-sm">
                  <Switch
                    size="sm"
                    checked={events.includes(event)}
                    onCheckedChange={(checked) =>
                      setEvents((current) =>
                        checked ? [...current, event] : current.filter((x) => x !== event),
                      )
                    }
                  />
                  {event}
                </label>
              ))}
            </div>
          </Field>
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
            <Field label="balance.low below (credits)">
              <NumberInput value={balanceLow} min={0} onChange={(e) => setBalanceLow(e.target.value)} />
            </Field>
            <Field label="Budget thresholds %" hint="comma-separated, e.g. 50,80,90">
              <TextInput value={thresholds} onChange={(e) => setThresholds(e.target.value)} />
            </Field>
          </div>
          <div>
            <Button type="submit" disabled={create.isPending || events.length === 0}>
              {create.isPending ? "Creating…" : "Create endpoint"}
            </Button>
          </div>
        </form>
      </Card>

      {webhooks.isLoading ? (
        <PageState>Loading…</PageState>
      ) : webhooks.isError ? (
        <PageState>The webhooks could not be read.</PageState>
      ) : (
        <TableShell head={["URL", "Events", "Secret", "Enabled", ""]}>
          {(webhooks.data ?? []).map((endpoint) => (
            <tr key={endpoint.id}>
              <td className="mono max-w-60 truncate">{endpoint.url}</td>
              <td className="text-xs">{endpoint.events.join(", ")}</td>
              <td className="mono">…{endpoint.secretLast4 ?? "????"}</td>
              <td>{endpoint.enabled ? "yes" : "no"}</td>
              <td className="text-right">
                <span className="inline-flex gap-2">
                  <Button
                    tone="ghost"
                    onClick={() =>
                      setDeliveriesFor(deliveriesFor === endpoint.id ? null : endpoint.id)
                    }
                  >
                    Deliveries
                  </Button>
                  <Button tone="danger" onClick={() => remove.mutate(endpoint.id)}>
                    Delete
                  </Button>
                </span>
              </td>
            </tr>
          ))}
          {(webhooks.data?.length ?? 0) === 0 ? (
            <EmptyRows colSpan={5}>No webhook endpoints yet.</EmptyRows>
          ) : null}
        </TableShell>
      )}

      {deliveriesFor ? <Deliveries endpointId={deliveriesFor} /> : null}
    </div>
  );
}

function Deliveries({ endpointId }: { endpointId: string }) {
  const deliveries = useWebhookDeliveries(endpointId);
  return (
    <Card title="Recent deliveries">
      {deliveries.isLoading ? (
        <PageState>Loading…</PageState>
      ) : deliveries.isError ? (
        <PageState>The deliveries could not be read.</PageState>
      ) : (
        <TableShell head={["When", "Event", "Status", "Attempts", "Error"]}>
          {(deliveries.data ?? []).map((d) => (
            <tr key={d.id}>
              <td>{String(d.createdAt).slice(0, 19).replace("T", " ")}</td>
              <td>{d.event}</td>
              <td>{d.status}</td>
              <td className="mono">{d.attempts}</td>
              <td className="text-xs text-danger">{d.lastError ?? ""}</td>
            </tr>
          ))}
          {(deliveries.data?.length ?? 0) === 0 ? (
            <EmptyRows colSpan={5}>No deliveries yet.</EmptyRows>
          ) : null}
        </TableShell>
      )}
    </Card>
  );
}
