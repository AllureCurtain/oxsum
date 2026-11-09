import { useRef, useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { useCreateOrder, useStatementPaymentQuote, type CreateOrderBody } from "../api/hooks";
import { decimalMinor } from "../lib/money";
import { CheckoutChoice, useCheckoutChoice } from "./checkout-choice";
import {
  AlertDialog, AlertDialogContent, AlertDialogDescription, AlertDialogFooter,
  AlertDialogHeader, AlertDialogTitle, Banner, Button, Card, Cr, Field, TextInput,
} from "./ui";

/** Review exact debt and retain one unchanged payment request across uncertain retries. */
export function StatementPayment({ statementId }: { statementId: string }) {
  const { t } = useTranslation();
  const [raw, setRaw] = useState("");
  const [selected, setSelected] = useState<number | undefined>();
  const [review, setReview] = useState(false);
  const quote = useStatementPaymentQuote(statementId, selected);
  const checkout = useCheckoutChoice();
  const create = useCreateOrder();
  const navigate = useNavigate();
  const intent = useRef<CreateOrderBody | null>(null);
  const payment = quote.data;
  const terms = payment?.terms;
  const reviewed = intent.current ?? terms;
  const fiat = reviewed ? BigInt(reviewed.displayAmountMinor) : 0n;
  const price = `${fiat / 100n}.${(fiat % 100n).toString().padStart(2, "0")} ${reviewed?.displayCurrency ?? ""}`;
  const parsed = raw.trim() ? decimalMinor(raw, 6) : undefined;
  const partial = parsed === 0 ? null : parsed;
  const pay = () => {
    if (!intent.current && terms && payment?.fingerprint && checkout.choice) {
      intent.current = {
        idempotencyKey: crypto.randomUUID(), purpose: "statementRepayment", ...checkout.choice,
        purposeRef: statementId, expectedStatementQuote: payment.fingerprint,
        statementAmountMinor: terms.amountMinor, expectedCollectionRevision: terms.collectionRevision,
        displayAmountMinor: terms.displayAmountMinor, displayCurrency: terms.displayCurrency,
      };
    }
    if (intent.current) create.mutate(intent.current, {
      onSuccess: (answer) => navigate(`/orders/${answer.order.orderId}`),
      onError: () => { void quote.refetch(); },
    });
  };
  return <Card title={t("statementPayment.title")}>
    <div className="flex flex-col gap-3">
      <p className="text-sm text-ink-secondary">{t("statementPayment.help")}</p>
      {quote.isLoading ? <p>{t("common.loading")}</p> : null}
      {quote.error ? <Banner tone="error">{quote.error.message}</Banner> : null}
      {payment ? <div className="grid grid-cols-1 gap-2 sm:grid-cols-3">
        <p>{t("statementPayment.outstanding")}: <Cr minor={payment.outstandingMinor} /></p>
        <p>{t("statementPayment.reserved")}: <Cr minor={payment.reservedMinor} /></p>
        <p>{t("statementPayment.available")}: <Cr minor={payment.availableMinor} /></p>
      </div> : null}
      {payment?.pendingOrderIds.map((id) => <Link className="text-primary" key={id} to={`/orders/${id}`}>{t("statementPayment.resume")} · {id.slice(0, 8)}</Link>)}
      {payment?.reason ? <Banner tone="warn">{payment.reason === "noAvailableDebt" ? t("statementPayment.none") : payment.reason === "statementNotPayable" ? t("statementPayment.notPayable") : t("statementPayment.reviewRequired")}</Banner> : null}
      <Field label={t("statementPayment.partial")}><TextInput inputMode="decimal" value={raw} onChange={(e) => setRaw(e.target.value)} disabled={create.isPending || intent.current !== null} /></Field>
      {partial === null ? <Banner tone="error">{t("statementPayment.invalid")}</Banner> : null}
      <div className="flex flex-wrap gap-2">
        <Button tone="ghost" disabled={create.isPending || intent.current !== null || partial === null} onClick={() => { if (partial !== null) { setSelected(partial); void quote.refetch(); } }}>{t("statementPayment.quote")}</Button>
        <Button tone="ghost" disabled={create.isPending} onClick={() => void quote.refetch()}>{t("statementPayment.refresh")}</Button>
        <Button disabled={create.isPending || (!terms && !intent.current) || (!intent.current && quote.isError) || quote.isFetching || (!intent.current && partial !== selected)} onClick={() => setReview(true)}>{intent.current ? t("statementPayment.retry") : t("statementPayment.review")}</Button>
      </div>
      <AlertDialog open={review} onOpenChange={(open) => { if (!create.isPending) setReview(open); }}>
        <AlertDialogContent><AlertDialogHeader><AlertDialogTitle>{t("statementPayment.confirm", { price })}</AlertDialogTitle><AlertDialogDescription>{t("statementPayment.confirmHelp")}</AlertDialogDescription></AlertDialogHeader>
          {reviewed ? <p>{t("statementPayment.exact")}: <Cr minor={intent.current?.statementAmountMinor ?? terms?.amountMinor ?? 0} /></p> : null}
          {intent.current ? <Banner tone="info">{t("statementPayment.retained", { amount: intent.current.displayAmountMinor, currency: intent.current.displayCurrency })}</Banner> : null}
          <CheckoutChoice state={checkout} disabled={create.isPending || intent.current !== null} />
          {create.error ? <Banner tone="error">{create.error.message} {t("statementPayment.retryHelp")}</Banner> : null}
          <AlertDialogFooter><Button tone="ghost" disabled={create.isPending} onClick={() => setReview(false)}>{t("statementPayment.keepReviewing")}</Button><Button disabled={!intent.current && (!terms || !checkout.choice || checkout.stale)} isLoading={create.isPending} onClick={pay}>{t("statementPayment.continue")}</Button></AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  </Card>;
}
