import { useTranslation } from "react-i18next";
import { Button, Field, NativeSelect, TextInput } from "../../components/ui";
import { RATE_KEYS, rateDraft, type PriceDraft, type ProviderDraft, type Rates } from "../../lib/provider-draft";

function RatesFields({ value, onChange, upstream = false }: { value: Rates; onChange: (value: Rates) => void; upstream?: boolean }) {
  const { t } = useTranslation();
  return <div className="grid gap-4 sm:grid-cols-2">
    {RATE_KEYS.map((key) => <Field key={key} label={t(`biz.providerModels.rates.${key}`)}
      hint={t(upstream ? "biz.providerModels.upstreamOptional" : key === "inputPricePerMillion" || key === "outputPricePerMillion" ? "biz.providerModels.requiredRate" : "biz.providerModels.optionalRate")}>
      <TextInput inputMode="decimal" maxLength={30} value={value[key]} onChange={(e) => onChange({ ...value, [key]: e.target.value })}
        required={!upstream && (key === "inputPricePerMillion" || key === "outputPricePerMillion")} />
    </Field>)}
  </div>;
}

function PriceFields({ value, onChange }: { value: PriceDraft; onChange: (value: PriceDraft) => void }) {
  const { t } = useTranslation();
  return <div className="flex flex-col gap-4">
    <RatesFields value={value.rates} onChange={(rates) => onChange({ ...value, rates })} />
    <Field label={t("biz.providerModels.maximum")}><TextInput inputMode="numeric" maxLength={16} value={value.maxOutputTokens}
      onChange={(e) => onChange({ ...value, maxOutputTokens: e.target.value })} required /></Field>
    <fieldset className="rounded-lg border border-line p-4">
      <legend className="px-2 text-sm font-medium">{t("biz.providerModels.upstream")}</legend>
      <p className="mb-4 text-sm text-ink-secondary">{t("biz.providerModels.upstreamHelp")}</p>
      {value.upstream === null ? <Button type="button" tone="ghost" onClick={() => onChange({ ...value, upstream: rateDraft() })}>{t("biz.providerModels.addUpstream")}</Button> : <>
        <RatesFields value={value.upstream} upstream onChange={(upstream) => onChange({ ...value, upstream })} />
        <Button type="button" tone="ghost" className="mt-4" onClick={() => onChange({ ...value, upstream: null })}>{t("biz.providerModels.clearUpstream")}</Button>
      </>}
    </fieldset>
  </div>;
}

export function ProviderFields({ value, onChange, isNew }: { value: ProviderDraft; onChange: (value: ProviderDraft) => void; isNew: boolean }) {
  const { t } = useTranslation();
  return <div className="flex flex-col gap-4">
    <div className="grid gap-4 sm:grid-cols-2">
      <Field label={t("common.model")}><TextInput value={value.model} maxLength={200} disabled={!isNew}
        onChange={(e) => onChange({ ...value, model: e.target.value })} required /></Field>
      <Field label={t("biz.providerModels.mode")} hint={t("biz.providerModels.modeHelp")}>
        <NativeSelect value={value.mode} onChange={(e) => onChange({ ...value, mode: e.target.value as ProviderDraft["mode"] })}>
          {(["chat", "embeddings", "rerank", "event"] as const).map((mode) => <option key={mode}>{mode}</option>)}
        </NativeSelect>
      </Field>
      <Field label={t("biz.providerModels.weight")} hint={t("biz.providerModels.weightHelp")}>
        <TextInput inputMode="numeric" value={value.weight} maxLength={4} onChange={(e) => onChange({ ...value, weight: e.target.value })} required />
      </Field>
    </div>
    <PriceFields value={value.price} onChange={(price) => onChange({ ...value, price })} />
    <details>
      <summary className="cursor-pointer text-sm text-primary">{t("biz.providerModels.rules", { count: value.rules.length })}</summary>
      <p className="my-4 text-sm text-ink-secondary">{t("biz.providerModels.rulesHelp")}</p>
      {value.rules.map((rule, index) => <fieldset key={index} className="mb-4 rounded-lg border border-line p-4">
        <legend className="px-2">{t("biz.providerModels.rule", { index: index + 1 })}</legend>
        <div className="mb-4 grid gap-4 sm:grid-cols-3">
          {(["serviceTier", "minInputTokens", "maxInputTokens"] as const).map((key) => <Field key={key} label={t(`biz.providerModels.${key}`)}>
            <TextInput value={rule[key]} maxLength={key === "serviceTier" ? 64 : 16} inputMode={key === "serviceTier" ? "text" : "numeric"}
              onChange={(e) => onChange({ ...value, rules: value.rules.map((row, i) => i === index ? { ...row, [key]: e.target.value } : row) })} />
          </Field>)}
        </div>
        <PriceFields value={rule.price} onChange={(price) => onChange({ ...value, rules: value.rules.map((row, i) => i === index ? { ...row, price } : row) })} />
        <Button type="button" tone="ghost" className="mt-4" onClick={() => onChange({ ...value, rules: value.rules.filter((_, i) => i !== index) })}>{t("biz.providerModels.removeRule")}</Button>
      </fieldset>)}
      <Button type="button" tone="ghost" disabled={value.rules.length >= 100} onClick={() => onChange({ ...value, rules: [...value.rules,
        { serviceTier: "", minInputTokens: "", maxInputTokens: "", price: structuredClone(value.price) }] })}>{t("biz.providerModels.addRule")}</Button>
    </details>
  </div>;
}
