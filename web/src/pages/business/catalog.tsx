import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Tags } from "lucide-react";
import {
  useDisplay,
  useSaleBookHistory,
  useSaleBooks,
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

export { BusinessChannels } from "./providers";

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
