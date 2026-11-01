import { useState } from "react";
import { Webhook } from "lucide-react";
import { Trans, useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { ApiError } from "../api/client";
import { CopyButton } from "../components/ui";
import {
  useCreateWebhook,
  useDeleteWebhook,
  useWebhookDeliveries,
  useWebhooks,
} from "../api/hooks";
import {
  AlertDialog,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
  Banner,
  Button,
  Card,
  EmptyRows,
  Field,
  NumberInput,
  PageState,
  TableShell,
  Td,
  TextInput,
} from "../components/ui";

const EVENT_GROUPS: { labelKey: string; events: string[] }[] = [
  { labelKey: "webhooks.groupRequests", events: ["request.settled"] },
  {
    labelKey: "webhooks.groupDeposits",
    events: ["deposit.credited", "deposit.failed", "deposit.reversed"],
  },
  {
    labelKey: "webhooks.groupStatements",
    events: ["statement.finalized", "statement.payment_overdue", "statement.payment_status_updated"],
  },
  {
    labelKey: "webhooks.groupOrg",
    events: ["org.suspended", "org.credit_limit_exceeded"],
  },
  { labelKey: "webhooks.groupBalance", events: ["balance.low"] },
  {
    labelKey: "webhooks.groupBudget",
    events: ["budget.exhaustion_forecast", "budget.threshold_crossed", "anomaly.detected"],
  },
];

export function Webhooks() {
  const { t } = useTranslation();
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
      <h1 className="text-2xl font-bold">{t("webhooks.title")}</h1>

      {secret ? (
        <Banner tone="success">
          <Trans
            i18nKey="webhooks.createdNote"
            components={{ code: <code className="mono" /> }}
            values={{ secret }}
          />
          <CopyButton value={secret} aria-label={t("webhooks.copySecret")} />
          <Button tone="ghost" className="ml-3" onClick={() => setSecret(null)}>
            {t("webhooks.done")}
          </Button>
        </Banner>
      ) : null}
      {create.error instanceof ApiError ? <Banner tone="error">{create.error.message}</Banner> : null}

      <Card title={t("webhooks.newEndpoint")}>
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
                  ? thresholds.split(",").map((v) => Number(v.trim()))
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
              placeholder={t("webhooks.endpointUrl")}
            />
          </Field>
          <Field label={t("webhooks.events")} hint={t("webhooks.selectedCount", { count: events.length })}>
            <div className="flex flex-col gap-2.5">
              {EVENT_GROUPS.map((group) => (
                <div key={group.labelKey}>
                  <p className="mb-1 text-xs font-semibold uppercase tracking-wider text-muted">
                    {t(group.labelKey)}
                  </p>
                  <div className="flex flex-wrap gap-1.5">
                    {group.events.map((event) => {
                      const on = events.includes(event);
                      return (
                        <button
                          key={event}
                          type="button"
                          aria-pressed={on}
                          onClick={() =>
                            setEvents((current) =>
                              on ? current.filter((x) => x !== event) : [...current, event],
                            )
                          }
                          className={`mono rounded-full border px-2.5 py-1 text-[11px] transition-colors ${
                            on
                              ? "border-primary bg-primary-soft text-primary"
                              : "border-line bg-surface text-muted hover:border-primary/40 hover:text-ink"
                          }`}
                        >
                          {event}
                        </button>
                      );
                    })}
                  </div>
                </div>
              ))}
            </div>
          </Field>
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
            <Field label={t("webhooks.balanceLow")}>
              <NumberInput value={balanceLow} min={0} onChange={(e) => setBalanceLow(e.target.value)} />
            </Field>
            <Field label={t("webhooks.budgetThresholds")} hint={t("webhooks.thresholdsHint")}>
              <TextInput value={thresholds} onChange={(e) => setThresholds(e.target.value)} />
            </Field>
          </div>
          <div>
            <Button type="submit" disabled={create.isPending || events.length === 0}>
              {create.isPending ? t("common.creating") : t("webhooks.createEndpoint")}
            </Button>
          </div>
        </form>
      </Card>

      {webhooks.isError ? (
        <PageState>{t("webhooks.readError")}</PageState>
      ) : (
        <TableShell
          head={[{ label: "URL", truncate: true }, t("webhooks.events"), t("webhooks.colSecret"), t("webhooks.colEnabled"), ""]}
          loading={webhooks.isLoading ? 4 : 0}
        >
          {!webhooks.isLoading &&
            (webhooks.data ?? []).map((endpoint) => (
            <tr key={endpoint.id}>
              <Td mono truncate>{endpoint.url}</Td>
              <td className="text-xs">{endpoint.events.join(", ")}</td>
              <td className="mono">…{endpoint.secretLast4 ?? "????"}</td>
              <td>{endpoint.enabled ? t("webhooks.enabledYes") : t("webhooks.enabledNo")}</td>
              <td className="text-right">
                <span className="inline-flex gap-2">
                  <Button
                    tone="ghost"
                    onClick={() =>
                      setDeliveriesFor(deliveriesFor === endpoint.id ? null : endpoint.id)
                    }
                  >
                    {t("webhooks.deliveries")}
                  </Button>
                  <AlertDialog>
                    <AlertDialogTrigger asChild>
                      <Button tone="danger">{t("common.delete")}</Button>
                    </AlertDialogTrigger>
                    <AlertDialogContent>
                      <AlertDialogHeader>
                        <AlertDialogTitle>{t("webhooks.deleteEndpoint")}</AlertDialogTitle>
                        <AlertDialogDescription>
                          {t("webhooks.deleteNote", { url: endpoint.url })}
                        </AlertDialogDescription>
                      </AlertDialogHeader>
                      <AlertDialogFooter>
                        <AlertDialogCancel>{t("common.cancel")}</AlertDialogCancel>
                        <Button
                          tone="danger"
                          isLoading={remove.isPending}
                          loadingText={t("common.deleting")}
                          onClick={() => remove.mutate(endpoint.id)}
                        >
                          {t("common.delete")}
                        </Button>
                      </AlertDialogFooter>
                    </AlertDialogContent>
                  </AlertDialog>
                </span>
              </td>
            </tr>
            ))}
          {!webhooks.isLoading && (webhooks.data?.length ?? 0) === 0 ? (
            <EmptyRows colSpan={5} icon={<Webhook />}>{t("webhooks.noEndpoints")}</EmptyRows>
          ) : null}
        </TableShell>
      )}

      {deliveriesFor ? (
        <Deliveries endpointId={deliveriesFor} t={t} />
      ) : (webhooks.data?.length ?? 0) > 0 ? (
        <Deliveries endpointId={webhooks.data![0].id} t={t} />
      ) : null}
    </div>
  );
}

function Deliveries({ endpointId, t }: { endpointId: string; t: TFunction }) {
  const deliveries = useWebhookDeliveries(endpointId);
  const webhooks = useWebhooks();
  const endpoint = (webhooks.data ?? []).find((e) => e.id === endpointId);
  return (
    <Card
      title={t("webhooks.recentDeliveries")}
      actions={
        endpoint ? (
          <span className="mono max-w-96 truncate text-xs text-muted" title={endpoint.url}>
            {endpoint.url}
          </span>
        ) : undefined
      }
    >
      {deliveries.isError ? (
        <PageState>{t("webhooks.deliveriesError")}</PageState>
      ) : (
        <TableShell
          head={[t("webhooks.colWhen"), t("webhooks.colEvent"), t("common.status"), { label: t("webhooks.colAttempts"), align: "right" }, t("webhooks.colError")]}
          loading={deliveries.isLoading ? 4 : 0}
        >
          {!deliveries.isLoading &&
            (deliveries.data ?? []).map((d) => (
            <tr key={d.id}>
              <td>{String(d.createdAt).slice(0, 19).replace("T", " ")}</td>
              <td>{d.eventType}</td>
              <td>{d.status}</td>
              <Td mono align="right">{d.attempts}</Td>
              <td className="text-xs text-danger">{d.lastError ?? ""}</td>
            </tr>
          ))}
          {!deliveries.isLoading && (deliveries.data?.length ?? 0) === 0 ? (
            <EmptyRows colSpan={5} icon={<Webhook />}>{t("webhooks.noDeliveries")}</EmptyRows>
          ) : null}
        </TableShell>
      )}
    </Card>
  );
}
