import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Network, Tags } from "lucide-react";
import { ApiError } from "../../api/client";
import {
  useBusinessChannels,
  useCapability,
  useBusiness,
  useChannelConnectionCheck,
  useDisplay,
  useSaleBookHistory,
  useSaleBooks,
  type Channel,
} from "../../api/hooks";
import {
  Badge,
  Banner,
  Button,
  Card,
  Cr,
  EmptyRows,
  PageState,
  Skeleton,
  TableShell,
  Td,
} from "../../components/ui";
import { DetailGrid, Field2, stateBadge } from "./biz";

// ---------------------------------------------------------------------------
// /app/business#business/channels + #business/prices — the provider channels
// and the wholesale price catalogue (sale_books, T08). Channel detail drills
// to its model routes and credential health; prices read the published book.
// ---------------------------------------------------------------------------

function ChannelDetail({ channel, onBack }: { channel: Channel; onBack: () => void }) {
  const { t } = useTranslation();
  const display = useDisplay();
  const check = useChannelConnectionCheck();
  const canCheck = useCapability(useBusiness().data, "routingWrite");

  return (
    <div className="flex flex-col gap-6">
      <div>
        <button type="button" className="text-sm text-primary" onClick={onBack}>
          {t("biz.channels.back")}
        </button>
        <h2 className="mt-1 text-xl font-bold">{channel.name}</h2>
      </div>
      <Card title={t("biz.channels.detail")}>
        <DetailGrid>
          <Field2 label={t("biz.channels.colProtocol")}>{channel.protocol}</Field2>
          <Field2 label={t("biz.channels.colUrl")}>
            <span className="mono break-all text-xs">{channel.baseUrl}</span>
          </Field2>
          <Field2 label={t("biz.channels.credentials")}>
            {channel.credentials.length === 0
              ? t("biz.channels.noCredentials")
              : t("biz.channels.credentialCount", { count: channel.credentials.length })}
          </Field2>
          <Field2 label={t("biz.channels.billKey")}>{channel.billKeyLast4 ?? "—"}</Field2>
        </DetailGrid>
        {channel.credentials.length > 0 ? (
          <TableShell
            className="mt-4"
            density="compact"
            head={[
              t("biz.channels.colKeyTail"),
              t("common.status"),
              t("biz.channels.colCooldown"),
              t("biz.channels.colLastUsed"),
            ]}
            sticky={false}
          >
            {channel.credentials.map((cr) => (
              <tr key={cr.credentialId}>
                <Td mono>•••{cr.apiKeyLast4}</Td>
                <td>
                  {cr.status}
                  {cr.cooldownUntil ? (
                    <Badge tone="danger" className="ml-2">{t("biz.channels.cooling")}</Badge>
                  ) : null}
                </td>
                <Td>{cr.cooldownUntil?.slice(0, 10) ?? "—"}</Td>
                <Td>{cr.lastUsedAt?.slice(0, 10) ?? "—"}</Td>
              </tr>
            ))}
          </TableShell>
        ) : null}
        {canCheck ? (
          <div className="mt-4 border-t border-line pt-4">
            <Button
              tone="ghost"
              isLoading={check.isPending}
              onClick={() =>
                check.mutate({
                  channelName: channel.name,
                  idempotencyKey: crypto.randomUUID(),
                  reason: "console connection check",
                })
              }
            >
              {t("biz.channels.check")}
            </Button>
            {check.data ? (
              <div className="mt-2">{stateBadge(check.data.state)}</div>
            ) : null}
            {check.error instanceof ApiError ? (
              <Banner tone="error" className="mt-2">{check.error.message}</Banner>
            ) : null}
          </div>
        ) : null}
      </Card>
      <Card title={t("biz.channels.models")}>
        <TableShell
          head={[
            t("common.model"),
            t("biz.channels.colVersion"),
            { label: t("biz.channels.colIn"), align: "right" },
            { label: t("biz.channels.colOut"), align: "right" },
            t("biz.channels.colMode"),
          ]}
        >
          {(channel.models ?? []).map((m) => (
            <tr key={m.model}>
              <td className="font-medium">{m.model}</td>
              <Td mono>v{m.version}</Td>
              <Td mono align="right"><Cr minor={m.inputPricePerMillion} currency={display} /></Td>
              <Td mono align="right">
                {m.outputPricePerMillion != null ? <Cr minor={m.outputPricePerMillion} currency={display} /> : "—"}
              </Td>
              <td>{m.mode ?? "chat"}</td>
            </tr>
          ))}
          {(channel.models?.length ?? 0) === 0 ? (
            <EmptyRows colSpan={5}>{t("biz.channels.noModels")}</EmptyRows>
          ) : null}
        </TableShell>
      </Card>
    </div>
  );
}

export function BusinessChannels() {
  const { t } = useTranslation();
  const [picked, setPicked] = useState<Channel | null>(null);
  const channels = useBusinessChannels();
  const page = channels.data;

  if (picked) {
    return <ChannelDetail channel={picked} onBack={() => setPicked(null)} />;
  }
  return (
    <div className="flex flex-col gap-6">
      <h2 className="text-xl font-bold">{t("biz.channels.title")}</h2>
      <Card title={t("biz.channels.listTitle")}>
        {channels.isError ? (
          <Banner tone="error">{t("biz.loadError")}</Banner>
        ) : (
          <TableShell
            head={[
              t("biz.channels.colName"),
              t("biz.channels.colProtocol"),
              t("biz.channels.colUrl"),
              t("biz.channels.colModels"),
              t("biz.channels.colKeys"),
            ]}
            loading={channels.isLoading ? 4 : 0}
          >
            {!channels.isLoading &&
              (page?.channels ?? []).map((c) => (
                <tr key={c.id}>
                  <td>
                    <button type="button" className="font-medium text-primary" onClick={() => setPicked(c)}>
                      {c.name}
                    </button>
                  </td>
                  <td>{c.protocol}</td>
                  <Td mono truncate>{c.baseUrl}</Td>
                  <Td mono>{c.models?.length ?? 0}</Td>
                  <Td mono>{c.credentials.length}</Td>
                </tr>
              ))}
            {!channels.isLoading && (page?.channels.length ?? 0) === 0 ? (
              <EmptyRows colSpan={5} icon={<Network />}>
                {t("biz.channels.empty")}
              </EmptyRows>
            ) : null}
          </TableShell>
        )}
      </Card>
    </div>
  );
}

// ---------------------------------------------------------------------------
// prices — the wholesale catalogue: each sale book's published versions with
// their model terms, and the assigned/default schedule badges (T08).
// ---------------------------------------------------------------------------

function BookCard({ bookId, name }: { bookId: string; name: string }) {
  const { t } = useTranslation();
  const display = useDisplay();
  const history = useSaleBookHistory(bookId);
  const [open, setOpen] = useState(false);
  const h = history.data;
  const latest = h?.versions[h.versions.length - 1];
  const isDefault = (h?.schedules ?? []).some(
    (s) => s.kind === "default" && s.effectiveUntil == null,
  );
  const assigned = (h?.schedules ?? []).filter(
    (s) => s.kind === "assigned" && s.effectiveUntil == null,
  ).length;

  return (
    <Card
      title={name}
      actions={
        <div className="flex items-center gap-2">
          {isDefault ? <Badge tone="default">{t("biz.prices.default")}</Badge> : null}
          {assigned > 0 ? <Badge tone="secondary">{t("biz.prices.assigned", { count: assigned })}</Badge> : null}
          <Button tone="ghost" onClick={() => setOpen((v) => !v)}>
            {open ? t("biz.prices.hide") : t("biz.prices.show")}
          </Button>
        </div>
      }
    >
      {open ? (
        history.isLoading ? (
          <Skeleton className="h-20 w-full" />
        ) : history.isError ? (
          <Banner tone="error">{t("biz.loadError")}</Banner>
        ) : latest ? (
          <TableShell
            density="compact"
            sticky={false}
            head={[
              t("common.model"),
              { label: t("biz.prices.colIn"), align: "right" },
              { label: t("biz.prices.colOut"), align: "right" },
              { label: t("biz.prices.colDiscount"), align: "right" },
            ]}
          >
            {Object.entries(latest.models).map(([model, terms]) => (
              <tr key={model}>
                <td className="font-medium">{model}</td>
                <Td mono align="right"><Cr minor={terms.price.inputPricePerMillion} currency={display} /></Td>
                <Td mono align="right">
                  {terms.price.outputPricePerMillion != null ? (
                    <Cr minor={terms.price.outputPricePerMillion} currency={display} />
                  ) : (
                    "—"
                  )}
                </Td>
                <Td mono align="right">{terms.discountPercent}%</Td>
              </tr>
            ))}
            {Object.keys(latest.models).length === 0 ? (
              <EmptyRows colSpan={4}>{t("biz.prices.noModels")}</EmptyRows>
            ) : null}
          </TableShell>
        ) : (
          <p className="text-sm text-muted">{t("biz.prices.noVersion")}</p>
        )
      ) : (
        <p className="text-sm text-muted">
          {t("biz.prices.summary", {
            versions: h?.versions.length ?? 0,
            models: latest ? Object.keys(latest.models).length : 0,
          })}
        </p>
      )}
    </Card>
  );
}

export function BusinessPrices() {
  const { t } = useTranslation();
  const books = useSaleBooks();
  return (
    <div className="flex flex-col gap-6">
      <h2 className="text-xl font-bold">{t("biz.prices.title")}</h2>
      {books.isError ? (
        <Banner tone="error">{t("biz.loadError")}</Banner>
      ) : books.isLoading ? (
        <Skeleton className="h-32 w-full" />
      ) : (books.data?.books.length ?? 0) === 0 ? (
        <PageState icon={<Tags />}>{t("biz.prices.empty")}</PageState>
      ) : (
        (books.data?.books ?? []).map((b) => <BookCard key={b.id} bookId={b.id} name={b.name} />)
      )}
    </div>
  );
}
