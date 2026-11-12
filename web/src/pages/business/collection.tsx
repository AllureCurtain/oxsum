import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useBusiness, useCollectionTerms, useSetCollectionTerms, type CollectionTerms } from "../../api/hooks";
import { ApiError } from "../../api/client";
import { Banner, Button, Card, Field, PageState, TextInput } from "../../components/ui";

function Editor({ initial }: { initial: CollectionTerms }) {
  const { t } = useTranslation();
  const query = useCollectionTerms(true);
  const save = useSetCollectionTerms();
  const [revision, setRevision] = useState(initial.revision);
  const [code, setCode] = useState(initial.currency?.code ?? "USD");
  const [rate, setRate] = useState(initial.currency?.ratePerCredit ?? "1");
  const [reason, setReason] = useState("");
  const intent = useRef<{ payload: string; key: string } | null>(null);
  const load = (terms: CollectionTerms) => {
    setRevision(terms.revision);
    setCode(terms.currency?.code ?? "USD");
    setRate(terms.currency?.ratePerCredit ?? "1");
    setReason("");
    intent.current = null;
  };
  return (
    <form className="flex flex-col gap-4" onSubmit={(event) => {
      event.preventDefault();
      const body = { expectedRevision: revision, currency: { code, ratePerCredit: rate }, reason };
      const payload = JSON.stringify(body);
      if (intent.current?.payload !== payload) intent.current = { payload, key: crypto.randomUUID() };
      save.mutate({ ...body, idempotencyKey: intent.current.key }, { onSuccess: (result) => load(result.terms) });
    }}>
      <p>{t("biz.collection.intro")}</p>
      <Field label={t("biz.collection.currency")} hint={t("biz.collection.currencyHint")}>
        <TextInput value={code} onChange={(e) => setCode(e.target.value.toUpperCase())} maxLength={3} required />
      </Field>
      <Field label={t("biz.collection.rate")} hint={t("biz.collection.rateHint")}>
        <TextInput inputMode="decimal" value={rate} onChange={(e) => setRate(e.target.value)} maxLength={30} required />
      </Field>
      <Field label={t("biz.reason")}><TextInput value={reason} onChange={(e) => setReason(e.target.value)} maxLength={500} required /></Field>
      <p className="font-mono text-sm">{t("biz.collection.preview", { rate: rate || "—", code: code || "—", revision })}</p>
      <p className="text-sm">{t("biz.collection.note")}</p>
      {query.data && query.data.revision !== revision ? <Banner tone="warn">{t("biz.collection.staleWarn")}</Banner> : null}
      {save.error instanceof ApiError ? <Banner tone="error">{save.error.message}</Banner> : null}
      {save.isSuccess ? <Banner tone="success">{t("biz.collection.saved")}</Banner> : null}
      <div className="flex flex-wrap gap-3">
        <Button type="submit" isLoading={save.isPending} disabled={!reason.trim()}>{t("biz.collection.save")}</Button>
        <Button type="button" tone="ghost" disabled={save.isPending || query.isFetching} onClick={async () => {
          const result = await query.refetch();
          if (result.data) { load(result.data); save.reset(); }
        }}>{t("biz.collection.loadCurrent")}</Button>
      </div>
    </form>
  );
}

export function BusinessCollection() {
  const { t } = useTranslation();
  const query = useCollectionTerms(true);
  const principal = useBusiness();
  if (query.isLoading) return <PageState>{t("biz.collection.loading")}</PageState>;
  if (query.isError || !query.data) return <Banner tone="error">{t("biz.collection.loadError")}</Banner>;
  const terms = query.data;
  return <Card title={t("biz.collection.title")}>
    <div className="flex flex-col gap-4">
      <Banner tone={terms.ready ? "info" : "warn"}>
        {terms.ready ? t("biz.collection.ready", { rate: terms.currency?.ratePerCredit, code: terms.currency?.code, source: terms.source }) : t("biz.collection.unavailable")}
      </Banner>
      <p className="text-sm">{t("biz.collection.historical", { list: terms.historicalCurrencies.join(", ") || t("biz.collection.noCurrencies") })}</p>
      {principal.data?.capabilities.includes("collectionWrite") ? <Editor initial={terms} /> : <p>{t("biz.collection.ownerOnly")}</p>}
    </div>
  </Card>;
}
