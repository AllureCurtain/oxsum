import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { api } from "../api/client";
import type { components } from "../api/schema";
import { Banner, Button, Field, Select, Skeleton } from "./ui";

type Route = components["schemas"]["CheckoutRoute"];
/** Keep the customer's explicit selection across refreshes; never silently pick another rail. */
export function useCheckoutChoice(enabled = true) {
  const query = useQuery({ queryKey: ["checkout-options"], queryFn: () => api<Route[]>("/checkout-options"), enabled });
  const [selected, setSelected] = useState<Route | null>(null);
  const choice = selected?.available && selected.merchantId && selected.versionId ? {
    rail: selected.rail, expectedCheckout: { revision: selected.revision, merchantId: selected.merchantId, versionId: selected.versionId },
  } : null;
  const identity = (route: Route) => `${route.rail}:${route.revision}:${route.merchantId}:${route.versionId}`;
  const current = selected ? query.data?.find((route) => identity(route) === identity(selected)) : null;
  const stale = selected !== null && !query.isLoading && !current;
  return { query, selected, setSelected, choice, identity, stale };

}
export function CheckoutChoice({ state, disabled = false }: { state: ReturnType<typeof useCheckoutChoice>; disabled?: boolean }) {
  const { t } = useTranslation();
  const { query, selected, setSelected, identity, stale } = state;
  return <div className="flex flex-col gap-2">
    {query.isLoading ? <Skeleton className="h-12 w-full" /> : query.isError ? <Banner tone="error">{t("checkoutChoices.error")}</Banner> : !query.data?.length && !selected ? <Banner tone="warn">{t("checkoutChoices.empty")}</Banner> : <Field label={t("checkoutChoices.label")}>
      <Select required value={selected ? identity(selected) : ""} disabled={disabled} onChange={(event) => setSelected(query.data?.find((route) => identity(route) === event.target.value) ?? null)}>
        <option value="">{t("checkoutChoices.select")}</option>
        {stale && selected ? <option value={identity(selected)}>{selected.name} · {selected.rail} · {t(`biz.merchant.${selected.mode}`)} ({t("checkoutChoices.retained")})</option> : null}
        {query.data?.map((route) => <option key={identity(route)} value={identity(route)}>{route.name} · {route.rail} · {t(`biz.merchant.${route.mode}`)}</option>)}
      </Select>
    </Field>}
    {stale ? <Banner tone="warn">{t("checkoutChoices.stale")}</Banner> : null}
    <p className="text-xs text-ink-secondary">{t("checkoutChoices.help")}</p>
    <Button type="button" tone="ghost" disabled={disabled || query.isFetching} onClick={() => void query.refetch()}>{t("checkoutChoices.refresh")}</Button>
  </div>;
}
