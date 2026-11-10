import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useBindCustomerPlan, useCustomerPlan, type BusinessCustomer, type SoldPlan } from "../../api/hooks";
import {
  AlertDialog, AlertDialogContent, AlertDialogDescription, AlertDialogFooter,
  AlertDialogHeader, AlertDialogTitle, Banner, Button, Cr, Field, NativeSelect,
} from "../../components/ui";

type Intent = { planVersionId: string; complimentaryGrant: string | null; name: string; priceMinor: number };

/** Keep one explicit plan authorization until its outcome is established. */
export function CustomerPlanBinding({ customer, plans }: { customer: BusinessCustomer; plans: SoldPlan[] }) {
  const { t } = useTranslation();
  const standing = useCustomerPlan(customer.organizationId);
  const mutation = useBindCustomerPlan(customer.organizationId);
  const busy = useRef(false);
  const attempted = useRef(false);
  const [target, setTarget] = useState("");
  const [intent, setIntent] = useState<Intent | null>(null);
  const [open, setOpen] = useState(false);
  const [error, setError] = useState("");
  const [pending, setPending] = useState(false);
  const published = plans.filter((plan) => plan.publishedAt != null && plan.retiredAt == null);
  const chosen = published.find((plan) => plan.planVersionId === target);
  const finish = () => { setIntent(null); setOpen(false); setError(""); attempted.current = false; };
  const recover = async (selected: Intent) => {
    const result = await standing.refetch();
    if (result.error) throw result.error;
    const view = result.data;
    if (!view?.plan) return false;
    if (view.plan.planVersionId !== selected.planVersionId) throw new Error(t("biz.customers.bindingConflict"));
    if (selected.complimentaryGrant && !view.periods.some((period) =>
      period.customerPlanId === view.plan?.customerPlanId && period.periodIndex === 0 &&
      period.authorizationKind === "complimentary" && period.authorizationRef === selected.complimentaryGrant)) {
      throw new Error(t("biz.customers.bindingConflict"));
    }
    return true;
  };
  const send = async () => {
    if (!intent || busy.current) return;
    busy.current = true; setPending(true); setError("");
    try {
      // A committed bind answers conflict on replay. Read its recorded authority first.
      if (attempted.current && await recover(intent)) { finish(); return; }
      attempted.current = true;
      try {
        await mutation.mutateAsync({ planVersionId: intent.planVersionId, complimentaryGrant: intent.complimentaryGrant });
      } catch (failure) {
        if (!await recover(intent)) throw failure;
      }
      finish();
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : t("biz.loadError"));
    } finally { busy.current = false; setPending(false); }
  };
  if (standing.data?.plan && !intent) return null;
  if (!intent && (standing.isLoading || standing.isError)) return null;
  return <div className="mt-4 border-t border-line pt-4">
    <form className="flex flex-col gap-3" onSubmit={(event) => {
      event.preventDefault();
      if (!intent && chosen) setIntent({ planVersionId: chosen.planVersionId,
        complimentaryGrant: chosen.priceMinor === 0 ? crypto.randomUUID() : null,
        name: chosen.name, priceMinor: chosen.priceMinor });
      setOpen(true);
    }}>
      <Field label={t("biz.customers.bindPlan")}>
        <NativeSelect value={target} disabled={intent !== null} onChange={(event) => setTarget(event.target.value)} required>
          <option value="">{t("biz.customers.pickPlan")}</option>
          {published.map((plan) => <option key={plan.planVersionId} value={plan.planVersionId}>
            {plan.name} · v{plan.version} · <Cr minor={plan.priceMinor} />
          </option>)}
        </NativeSelect>
      </Field>
      {intent && attempted.current ? <p className="text-sm text-muted">{t("biz.customers.bindingRetained")}</p> : null}
      <Button type="submit" disabled={!chosen && !intent}>{t(intent && attempted.current ? "biz.customers.bindingRecover" : "biz.customers.bindingReview")}</Button>
    </form>
    <AlertDialog open={open} onOpenChange={(next) => { if (!busy.current) { setOpen(next); if (!next && !attempted.current) setIntent(null); } }}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{t("biz.customers.bindingTitle", { customer: customer.name, plan: intent?.name })}</AlertDialogTitle>
          <AlertDialogDescription>{t(intent?.complimentaryGrant ? "biz.customers.bindingFree" : "biz.customers.bindingPaid")}</AlertDialogDescription>
        </AlertDialogHeader>
        <p>{t("common.amount")}: <Cr minor={intent?.priceMinor ?? 0} /></p>
        {error ? <Banner tone="error">{error}</Banner> : null}
        {attempted.current ? <p className="text-sm text-muted">{t("biz.customers.bindingRetained")}</p> : null}
        <AlertDialogFooter>
          <Button tone="ghost" disabled={pending} onClick={() => { setOpen(false); if (!attempted.current) setIntent(null); }}>{t("common.cancel")}</Button>
          <Button isLoading={pending} onClick={() => void send()}>{t(attempted.current ? "biz.customers.bindingRecover" : intent?.complimentaryGrant ? "biz.customers.bindingGrant" : "biz.customers.bindConfirm")}</Button>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  </div>;
}
