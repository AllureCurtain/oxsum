import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useCheckoutRoutes, useSelectCheckoutRoute, type CheckoutRoute } from "../../api/hooks/merchants";
import type { Merchant } from "../../api/hooks";
import { AlertDialog, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle, Banner, Button, Card, Field, Select, Skeleton, Textarea } from "../../components/ui";

function RouteEditor({ route, merchant }: { route: CheckoutRoute; merchant: Merchant }) {
  const { t } = useTranslation();
  const write = useSelectCheckoutRoute();
  const [reason, setReason] = useState("");
  const [target, setTarget] = useState<"select" | "disable">("select");
  const [key, setKey] = useState(() => crypto.randomUUID());
  const [receipt, setReceipt] = useState(false);
  const [revision, setRevision] = useState(route.revision);
  const [confirm, setConfirm] = useState(false);
  const submit = () => {
    if (write.isPending) return;
    write.mutate({ rail: route.rail, expectedRevision: revision, merchantId: target === "select" ? merchant.id : null, idempotencyKey: key, reason: reason.trim() }, { onSuccess: () => { setReceipt(true); setConfirm(false); } });
  };
  return <form className="flex flex-col gap-3" onSubmit={(event) => {
    event.preventDefault();
    if (!write.isPending) setConfirm(true);
  }}>
    <p className="text-sm text-ink-secondary">{t("biz.checkoutRoutes.current", { name: route.name ?? "—", revision: route.revision, source: route.source })}</p>
    {receipt ? <p role="status">{t("biz.checkoutRoutes.saved")}</p> : <>
      <fieldset className="flex flex-col gap-3" disabled={write.isPending} onChange={() => { setKey(crypto.randomUUID()); setRevision(route.revision); write.reset(); }}>
        <Field label={t("biz.checkoutRoutes.action")}><Select value={target} onChange={(event) => setTarget(event.target.value as typeof target)}><option value="select" disabled={!merchant.enabled}>{t("biz.checkoutRoutes.select", { name: merchant.name })}</option><option value="disable">{t("biz.checkoutRoutes.disable", { rail: route.rail })}</option></Select></Field>
        <Field label={t("biz.reason")}><Textarea required maxLength={500} value={reason} onChange={(event) => setReason(event.target.value)} rows={2} /></Field>
      </fieldset>
      <p className="text-sm text-ink-secondary">{t("biz.checkoutRoutes.help")}</p>
      {write.error ? <Banner tone="error">{write.error.message}</Banner> : null}
      <Button type="submit" disabled={!reason.trim() || (target === "select" && !merchant.enabled)} isLoading={write.isPending}>{t("biz.checkoutRoutes.confirm")}</Button>
    </>}
    <AlertDialog open={confirm} onOpenChange={(open) => { if (!write.isPending) setConfirm(open); }}>
      <AlertDialogContent><AlertDialogHeader><AlertDialogTitle>{t("biz.checkoutRoutes.confirm")}</AlertDialogTitle><AlertDialogDescription>{route.rail}: {route.name ?? "—"} → {target === "select" ? `${merchant.name} (${merchant.mode})` : t("biz.checkoutRoutes.disable", { rail: route.rail })}. {t("biz.checkoutRoutes.help")}</AlertDialogDescription></AlertDialogHeader>
        <p className="break-words text-sm">{reason}</p>
        {write.error ? <Banner tone="error">{write.error.message}</Banner> : null}
        <AlertDialogFooter><Button type="button" tone="ghost" disabled={write.isPending} onClick={() => setConfirm(false)}>{t("common.cancel")}</Button><Button type="button" tone="danger" isLoading={write.isPending} onClick={submit}>{t("biz.checkoutRoutes.confirm")}</Button></AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  </form>;
}

export function CheckoutRoutes({ merchant, canWrite }: { merchant: Merchant; canWrite: boolean }) {
  const { t } = useTranslation();
  const routes = useCheckoutRoutes();
  const selected = routes.data?.find((route) => route.rail === merchant.rail);
  return <Card title={t("biz.checkoutRoutes.title")}>
    {routes.isLoading ? <Skeleton className="h-12 w-full" /> : routes.isError ? <Banner tone="error">{t("biz.loadError")} <Button tone="ghost" onClick={() => void routes.refetch()}>{t("biz.provider.retry")}</Button></Banner> : selected ? canWrite ? <RouteEditor key={merchant.id} route={selected} merchant={merchant} /> : <p>{t("biz.checkoutRoutes.current", { name: selected.name ?? "—", revision: selected.revision, source: selected.source })}</p> : null}
  </Card>;
}
