import { useRef, useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { useBusiness, useBusinessProfile, useCapability, useSaveBusinessProfile, type BusinessProfile } from "../../api/hooks";
import { Banner, Button, Card, Field, Skeleton, TextInput, Textarea } from "../../components/ui";
import { DetailGrid, Field2 } from "./biz";

function ProfileEditor({ profile, onClose, onSaved }: { profile: BusinessProfile; onClose: () => void; onSaved: () => void }) {
  const { t } = useTranslation();
  const [draft, setDraft] = useState(profile.branding);
  const [reason, setReason] = useState("");
  const [key, setKey] = useState(() => crypto.randomUUID());
  const save = useSaveBusinessProfile();
  const busy = useRef(false);
  async function submit(event: FormEvent) {
    event.preventDefault();
    if (busy.current) return;
    busy.current = true;
    try {
      await save.mutateAsync({ idempotencyKey: key, reason: reason.trim(), expectedRevision: profile.revision, branding: draft });
      onSaved();
      onClose();
    } catch {
      // Draft and operation identity survive both conflict and network failure.
    } finally { busy.current = false; }
  }
  return (
    <Card title={t("biz.profile.edit")}>
      <form className="flex flex-col gap-4" onSubmit={(event) => void submit(event)}>
        <p className="text-sm text-ink-secondary">{t("biz.profile.saveHelp", { revision: profile.revision })}</p>
        <fieldset disabled={save.isPending} className="grid min-w-0 gap-4 sm:grid-cols-2" onChange={() => { setKey(crypto.randomUUID()); save.reset(); }}>
          <Field label={t("biz.profile.name")}>
            <TextInput required maxLength={80} value={draft.name} onChange={(event) => setDraft({ ...draft, name: event.target.value })} />
          </Field>
          <Field label={t("biz.profile.contact")} hint={t("biz.profile.publicContact")}>
            <TextInput type="email" maxLength={320} value={draft.contactEmail ?? ""} onChange={(event) => setDraft({ ...draft, contactEmail: event.target.value || null })} />
          </Field>
          <Field label={t("biz.profile.help")} hint={t("biz.profile.urlHelp")}>
            <TextInput type="url" maxLength={2048} value={draft.helpUrl ?? ""} onChange={(event) => setDraft({ ...draft, helpUrl: event.target.value || null })} />
          </Field>
          <Field label={t("biz.profile.logo")} hint={t("biz.profile.urlHelp")}>
            <TextInput type="url" maxLength={2048} value={draft.logoUrl ?? ""} onChange={(event) => setDraft({ ...draft, logoUrl: event.target.value || null })} />
          </Field>
          <Field label={t("biz.profile.accent")} hint={t("biz.profile.accentHelp")}>
            <TextInput pattern="#[0-9A-Fa-f]{6}" maxLength={7} value={draft.accentColor ?? ""} placeholder="#2d5bd8" onChange={(event) => setDraft({ ...draft, accentColor: event.target.value || null })} />
          </Field>
          <Field label={t("biz.reason")}>
            <Textarea required maxLength={500} rows={2} value={reason} onChange={(event) => setReason(event.target.value)} />
          </Field>
        </fieldset>
        {save.error ? <Banner tone="error">{save.error instanceof Error ? save.error.message : t("biz.loadError")}</Banner> : null}
        {save.error ? <p className="text-sm text-ink-secondary">{t("biz.profile.retryHelp")}</p> : null}
        <div className="flex flex-wrap gap-2">
          <Button type="submit" isLoading={save.isPending} disabled={!reason.trim()}>{t("common.save")}</Button>
          <Button type="button" tone="ghost" disabled={save.isPending} onClick={onClose}>{t("biz.profile.discard")}</Button>
        </div>
      </form>
    </Card>
  );
}

export function BusinessProfileSection() {
  const { t } = useTranslation();
  const profile = useBusinessProfile();
  const canWrite = useCapability(useBusiness().data, "profileWrite");
  const [editing, setEditing] = useState<BusinessProfile | null>(null);
  const [saved, setSaved] = useState(false);
  const data = profile.data;
  return (
    <div className="flex flex-col gap-6">
      <h2 className="text-xl font-bold">{t("biz.profile.title")}</h2>
      <p className="text-sm text-ink-secondary">{t("biz.profile.intro")}</p>
      {saved ? <div role="status"><Banner tone="success">{t("biz.profile.saved")}</Banner></div> : null}
      {profile.isError ? <Banner tone="error"><div className="flex flex-wrap items-center gap-4">{t("biz.loadError")}<Button tone="ghost" onClick={() => void profile.refetch()}>{t("biz.profile.reload")}</Button></div></Banner> : null}
      {profile.isLoading ? <Skeleton className="h-32 w-full" /> : null}
      {editing && canWrite ? <ProfileEditor profile={editing} onClose={() => { setEditing(null); void profile.refetch(); }} onSaved={() => setSaved(true)} /> : data ? (
        <Card title={t("biz.profile.current")} actions={canWrite ? <Button disabled={profile.isError} onClick={() => { setEditing(data); setSaved(false); }}>{t("biz.profile.edit")}</Button> : null}>
          <p className="mb-4 text-sm text-ink-secondary">{t(data.source === "stored" ? "biz.profile.stored" : "biz.profile.defaults", { revision: data.revision })}</p>
          <DetailGrid>
            {([
              ["name", data.branding.name], ["contact", data.branding.contactEmail], ["help", data.branding.helpUrl],
              ["logo", data.branding.logoUrl], ["accent", data.branding.accentColor],
            ] as const).map(([label, value]) => <Field2 key={label} label={t(`biz.profile.${label}`)}><span className="break-all">{value || t("biz.profile.notSet")}</span></Field2>)}
          </DetailGrid>
        </Card>
      ) : null}
    </div>
  );
}
