import { useRef, useState } from "react";
import { useBusiness, useCollectionTerms, useSetCollectionTerms, type CollectionTerms } from "../../api/hooks";
import { ApiError } from "../../api/client";
import { Banner, Button, Card, Field, PageState, TextInput } from "../../components/ui";

function Editor({ initial }: { initial: CollectionTerms }) {
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
      <p>These settings determine the credits purchased by a payment. Customer display preferences only change presentation.</p>
      <Field label="Collection currency" hint="Three uppercase letters. Existing payment history pins the currency.">
        <TextInput value={code} onChange={(e) => setCode(e.target.value.toUpperCase())} maxLength={3} required />
      </Field>
      <Field label="Currency amount per credit" hint="Exact decimal amount for 1 credit; provider amounts currently use two decimal minor digits.">
        <TextInput inputMode="decimal" value={rate} onChange={(e) => setRate(e.target.value)} maxLength={30} required />
      </Field>
      <Field label="Reason"><TextInput value={reason} onChange={(e) => setReason(e.target.value)} maxLength={500} required /></Field>
      <p className="font-mono text-sm">1 credit = {rate || "—"} {code || "—"} · revision {revision}</p>
      <p className="text-sm">A saved rate applies to new payment intents. Existing orders retain their agreed conversion. Saving does not contact a payment provider.</p>
      {query.data && query.data.revision !== revision ? <Banner tone="warn">Settings changed while this draft was open. Your draft is retained; load the current settings before revising it.</Banner> : null}
      {save.error instanceof ApiError ? <Banner tone="error">{save.error.message}</Banner> : null}
      {save.isSuccess ? <Banner tone="success">Collection settings saved.</Banner> : null}
      <div className="flex flex-wrap gap-3">
        <Button type="submit" isLoading={save.isPending} disabled={!reason.trim()}>Save collection settings</Button>
        <Button type="button" tone="ghost" disabled={save.isPending || query.isFetching} onClick={async () => {
          const result = await query.refetch();
          if (result.data) { load(result.data); save.reset(); }
        }}>Load current settings</Button>
      </div>
    </form>
  );
}

export function BusinessCollection() {
  const query = useCollectionTerms(true);
  const principal = useBusiness();
  if (query.isLoading) return <PageState>Loading collection settings…</PageState>;
  if (query.isError || !query.data) return <Banner tone="error">Collection settings could not be loaded.</Banner>;
  const terms = query.data;
  return <Card title="Collection settings">
    <div className="flex flex-col gap-4">
      <Banner tone={terms.ready ? "info" : "warn"}>
        {terms.ready ? `1 credit = ${terms.currency?.ratePerCredit} ${terms.currency?.code}. Source: ${terms.source}.` : "Collection is unavailable until the owner adopts terms compatible with payment history."}
      </Banner>
      <p className="text-sm">Retained payment currencies: {terms.historicalCurrencies.join(", ") || "none"}. Payment provider readiness is checked separately.</p>
      {principal.data?.capabilities.includes("collectionWrite") ? <Editor initial={terms} /> : <p>Only the business owner can change collection settings.</p>}
    </div>
  </Card>;
}
