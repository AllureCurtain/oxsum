import { Trans, useTranslation } from "react-i18next";
import { saleError, saleLabel } from "../../lib/sale-text";
import { SaleInputError } from "../../lib/sale-input-error";
import { useState } from "react";
import { useSalePreview, type SaleHistory } from "../../api/hooks/sales";
import { Banner, Button, Card, Cr, Field, NativeSelect, TableShell, Td, TextInput } from "../../components/ui";
import { whole, utc } from "../../lib/sale-draft";
import { CustomerPicker } from "./sale-schedules";

const COUNTS = [
  ["inputTokens", "Input tokens / event units"], ["outputTokens", "Output tokens"],
  ["cachedTokens", "Cached input tokens"], ["cacheWrite5mTokens", "Cache write 5m tokens"],
  ["cacheWrite1hTokens", "Cache write 1h tokens"], ["reasoningTokens", "Reasoning tokens"],
] as const;
export function SalePreview({ history }: { history: SaleHistory }) {
  const { t } = useTranslation();
  const [customer, setCustomer] = useState("");
  const [model, setModel] = useState("");
  const [source, setSource] = useState("effective");
  const [at, setAt] = useState("");
  const [tier, setTier] = useState("");
  const [counts, setCounts] = useState<Record<string, string>>({ inputTokens: "1000", outputTokens: "100" });
  const [error, setError] = useState<unknown>(null);
  const preview = useSalePreview();
  const result = preview.data;
  return <Card title={t("sale.previewTitle")}><div className="flex flex-col gap-4">
    <p>{t("sale.previewDescription")}</p>
    <form className="flex flex-col gap-4" onSubmit={async (e) => {
      e.preventDefault(); setError(null); preview.reset();
      try {
        const revision = history.drafts.find((d) => String(d.revision) === source)?.revision;
        if (source !== "effective" && revision == null) throw new SaleInputError("selectExistingDraft", "selectExistingDraft");
        await preview.mutateAsync({ organizationId: customer, model, at: utc(at),
          source: revision == null ? { kind: "effective" } : { kind: "draft", bookId: history.book.id, revision },
          usage: { ...Object.fromEntries(COUNTS.map(([key, label]) => [key, whole(counts[key] || "0", label)])), serviceTier: tier || null },
        });
      } catch (e) { setError(e); }
    }}>
      <CustomerPicker value={customer} onChange={setCustomer} />
      <div className="grid gap-4 sm:grid-cols-2">
        <Field label={t("sale.requestedModel")}><TextInput value={model} maxLength={200} onChange={(e) => setModel(e.target.value)} required /></Field>
        <Field label={t("sale.resolveFrom")}><NativeSelect value={source} onChange={(e) => setSource(e.target.value)}><option value="effective">{t("sale.effectiveTerms")}</option>{history.drafts.map((d) => <option key={d.revision} value={d.revision}>{t("sale.compareDraft", { revision: d.revision })}</option>)}</NativeSelect></Field>
        <Field label={t("sale.atUtc")} hint={t("sale.atHint")}><TextInput value={at} onChange={(e) => setAt(e.target.value)} placeholder="2026-12-01T00:00:00Z" /></Field>
        <Field label={t("sale.serviceTier")}><TextInput value={tier} maxLength={64} onChange={(e) => setTier(e.target.value)} /></Field>
        {COUNTS.map(([key, label]) => <Field key={key} label={saleLabel(label, t)}><TextInput inputMode="numeric" value={counts[key] ?? ""} maxLength={16} onChange={(e) => setCounts({ ...counts, [key]: e.target.value })} placeholder="0" /></Field>)}
      </div>
      {error != null ? <Banner tone="error">{saleError(error, t, t("sale.previewError"))}</Banner> : null}
      <Button type="submit" isLoading={preview.isPending} disabled={!customer || !model.trim()}>{t("sale.preview")}</Button>
    </form>
    {result ? <div className="flex flex-col gap-3" aria-live="polite">
      <p>{t("sale.previewCustomer", { customer: result.snapshot.organizationId, model: result.snapshot.model, at: result.snapshot.at })}</p>
      <p><Trans i18nKey="sale.resolution" values={{ source: saleLabel(result.snapshot.source, t), book: result.snapshot.bookId ?? "—", version: result.snapshot.version ?? "—", revision: result.snapshot.draftRevision ?? "—", schedule: result.snapshot.scheduleId ?? "—" }} components={{ strong: <strong /> }} /></p>
      <p>{t("sale.beforeDiscount")} <Cr minor={result.undiscountedMinor} /> · {t("sale.selectedDiscount", { discount: result.snapshot.terms.discountPercent ?? 0 })} · {t("sale.exactCharge")} <Cr minor={result.chargeMinor} /></p>
      <p>{t("sale.matchedCondition")} {result.matchedRule ? [result.matchedRule.serviceTier ? t("sale.tierCondition", { tier: result.matchedRule.serviceTier }) : null, result.matchedRule.minInputTokens != null ? t("sale.minCondition", { count: result.matchedRule.minInputTokens }) : null, result.matchedRule.maxInputTokens != null ? t("sale.maxCondition", { count: result.matchedRule.maxInputTokens }) : null].filter(Boolean).join(", ") : t("sale.baseRate")}</p>
      <TableShell head={[t("sale.dimension"), { label: t("sale.quantity"), align: "right" }, { label: t("sale.rateMinor"), align: "right" }]}>
        {result.lines.map((line, i) => <tr key={i}><Td>{saleLabel(String(line[0]), t)}</Td><Td mono align="right">{String(line[1])}</Td><Td mono align="right">{String(line[2])}</Td></tr>)}
      </TableShell>
    </div> : null}
  </div></Card>;
}
