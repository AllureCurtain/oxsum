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
  const [customer, setCustomer] = useState("");
  const [model, setModel] = useState("");
  const [source, setSource] = useState("effective");
  const [at, setAt] = useState("");
  const [tier, setTier] = useState("");
  const [counts, setCounts] = useState<Record<string, string>>({ inputTokens: "1000", outputTokens: "100" });
  const [error, setError] = useState("");
  const preview = useSalePreview();
  const result = preview.data;
  return <Card title="Customer price preview"><div className="flex flex-col gap-4">
    <p>Preview rates declared usage without contacting a model or reserving funds. Effective terms are resolved for the selected customer and instant. A draft comparison does not activate that draft.</p>
    <form className="flex flex-col gap-4" onSubmit={async (e) => {
      e.preventDefault(); setError(""); preview.reset();
      try {
        const revision = history.drafts.find((d) => String(d.revision) === source)?.revision;
        if (source !== "effective" && revision == null) throw new Error("Select an existing draft revision");
        await preview.mutateAsync({ organizationId: customer, model, at: utc(at),
          source: revision == null ? { kind: "effective" } : { kind: "draft", bookId: history.book.id, revision },
          usage: { ...Object.fromEntries(COUNTS.map(([key, label]) => [key, whole(counts[key] || "0", label)])), serviceTier: tier || null },
        });
      } catch (e) { setError(e instanceof Error ? e.message : "Preview failed"); }
    }}>
      <CustomerPicker value={customer} onChange={setCustomer} />
      <div className="grid gap-4 sm:grid-cols-2">
        <Field label="Requested model"><TextInput value={model} maxLength={200} onChange={(e) => setModel(e.target.value)} required /></Field>
        <Field label="Resolve from"><NativeSelect value={source} onChange={(e) => setSource(e.target.value)}><option value="effective">Effective customer terms</option>{history.drafts.map((d) => <option key={d.revision} value={d.revision}>Compare draft revision {d.revision}</option>)}</NativeSelect></Field>
        <Field label="At (UTC)" hint="Blank uses the server's current instant."><TextInput value={at} onChange={(e) => setAt(e.target.value)} placeholder="2026-12-01T00:00:00Z" /></Field>
        <Field label="Service tier"><TextInput value={tier} maxLength={64} onChange={(e) => setTier(e.target.value)} /></Field>
        {COUNTS.map(([key, label]) => <Field key={key} label={label}><TextInput inputMode="numeric" value={counts[key] ?? ""} maxLength={16} onChange={(e) => setCounts({ ...counts, [key]: e.target.value })} placeholder="0" /></Field>)}
      </div>
      {error ? <Banner tone="error">{error}</Banner> : null}
      <Button type="submit" isLoading={preview.isPending} disabled={!customer || !model.trim()}>Preview exact charge</Button>
    </form>
    {result ? <div className="flex flex-col gap-3" aria-live="polite">
      <p>Customer {result.snapshot.organizationId} · {result.snapshot.model} · {result.snapshot.at}</p>
      <p>Resolution: <strong>{result.snapshot.source}</strong> · book {result.snapshot.bookId ?? "—"} · version {result.snapshot.version ?? "—"} · draft {result.snapshot.draftRevision ?? "—"} · schedule {result.snapshot.scheduleId ?? "—"}</p>
      <p>Before discount: <Cr minor={result.undiscountedMinor} /> · selected discount {result.snapshot.terms.discountPercent ?? 0}% · exact charge <Cr minor={result.chargeMinor} /></p>
      <p>Matched condition: {result.matchedRule ? [result.matchedRule.serviceTier ? `tier ${result.matchedRule.serviceTier}` : null, result.matchedRule.minInputTokens != null ? `input ≥ ${result.matchedRule.minInputTokens}` : null, result.matchedRule.maxInputTokens != null ? `input ≤ ${result.matchedRule.maxInputTokens}` : null].filter(Boolean).join(", ") : "base rate"}</p>
      <TableShell head={["Dimension", { label: "Quantity", align: "right" }, { label: "Rate (credit minor)", align: "right" }]}>
        {result.lines.map((line, i) => <tr key={i}><Td>{String(line[0])}</Td><Td mono align="right">{String(line[1])}</Td><Td mono align="right">{String(line[2])}</Td></tr>)}
      </TableShell>
    </div> : null}
  </div></Card>;
}
