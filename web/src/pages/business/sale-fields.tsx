import { Button, Field, NativeSelect, TextInput } from "../../components/ui";
import { RATE_FIELDS, type ModelDraft, type PriceDraft } from "../../lib/sale-draft";

function PriceFields({ value, onChange }: { value: PriceDraft; onChange: (value: PriceDraft) => void }) {
  return <div className="grid gap-4 sm:grid-cols-2">
    {RATE_FIELDS.map(([key, label]) => <Field key={key} label={label} hint={key === "inputPricePerMillion" || key === "outputPricePerMillion" ? "Six fractional digits; zero is an explicit free rate." : "Blank uses the documented base-rate fallback; per-request blank means no flat fee."}>
      <TextInput inputMode="decimal" value={value[key]} maxLength={30} onChange={(e) => onChange({ ...value, [key]: e.target.value })} />
    </Field>)}
    <Field label="Maximum output tokens"><TextInput inputMode="numeric" value={value.maxOutputTokens} maxLength={16} onChange={(e) => onChange({ ...value, maxOutputTokens: e.target.value })} /></Field>
  </div>;
}
export function SaleFields({ value, onChange, modelEditable = true }: { value: ModelDraft; onChange: (value: ModelDraft) => void; modelEditable?: boolean }) {
  return <div className="flex flex-col gap-4">
    <div className="grid gap-4 sm:grid-cols-3">
      <Field label="Requested model"><TextInput value={value.model} maxLength={200} disabled={!modelEditable} onChange={(e) => onChange({ ...value, model: e.target.value })} required /></Field>
      <Field label="Billing mode"><NativeSelect value={value.mode} onChange={(e) => {
        const mode = e.target.value as ModelDraft["mode"];
        onChange({ ...value, mode, price: { ...value.price,
          maxOutputTokens: mode === "chat" ? (value.price.maxOutputTokens === "0" ? "1024" : value.price.maxOutputTokens) : "0",
          outputPricePerMillion: mode === "embeddings" || mode === "rerank" ? "0" : value.price.outputPricePerMillion },
          rules: value.rules.map((r) => ({ ...r, price: { ...r.price,
            maxOutputTokens: mode === "chat" ? (r.price.maxOutputTokens === "0" ? "1024" : r.price.maxOutputTokens) : "0",
            outputPricePerMillion: mode === "embeddings" || mode === "rerank" ? "0" : r.price.outputPricePerMillion } })) });
      }}>
        {(["chat", "embeddings", "rerank", "event"] as const).map((mode) => <option key={mode}>{mode}</option>)}
      </NativeSelect></Field>
      <Field label="Discount percent"><TextInput inputMode="numeric" value={value.discount} maxLength={3} onChange={(e) => onChange({ ...value, discount: e.target.value })} required /></Field>
    </div>
    <PriceFields value={value.price} onChange={(price) => onChange({ ...value, price })} />
    <details><summary className="cursor-pointer text-sm text-primary">Conditional prices ({value.rules.length})</summary>
      <p className="my-4 text-sm">Each match replaces the complete price set. Token bounds are inclusive; ambiguous overlaps are refused on save.</p>
      {value.rules.map((rule, index) => <fieldset key={index} className="mb-4 rounded-lg border border-line p-4">
        <legend>Conditional rate {index + 1}</legend>
        <div className="mb-4 grid gap-4 sm:grid-cols-3">
          {(["serviceTier", "minInputTokens", "maxInputTokens"] as const).map((key) => <Field key={key} label={{ serviceTier: "Service tier", minInputTokens: "Minimum input tokens", maxInputTokens: "Maximum input tokens" }[key]}>
            <TextInput value={rule[key]} maxLength={key === "serviceTier" ? 64 : 16} onChange={(e) => onChange({ ...value, rules: value.rules.map((r, i) => i === index ? { ...r, [key]: e.target.value } : r) })} />
          </Field>)}
        </div>
        <PriceFields value={rule.price} onChange={(price) => onChange({ ...value, rules: value.rules.map((r, i) => i === index ? { ...r, price } : r) })} />
        <Button type="button" tone="ghost" className="mt-4" onClick={() => onChange({ ...value, rules: value.rules.filter((_, i) => i !== index) })}>Remove unsaved conditional rate</Button>
      </fieldset>)}
      <Button type="button" tone="ghost" disabled={value.rules.length >= 100} onClick={() => onChange({ ...value, rules: [...value.rules, { serviceTier: "", minInputTokens: "", maxInputTokens: "", price: { ...value.price } }] })}>Add conditional rate</Button>
    </details>
  </div>;
}
