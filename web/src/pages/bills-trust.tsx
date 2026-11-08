import { useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { CheckCircle2, ShieldAlert } from "lucide-react";
import { api } from "../api/client";
import { useSession } from "../api/hooks";
import type { SessionInfo } from "../api/hooks/types";
import { Banner, Button, Card, CodeBlock, PageState } from "../components/ui";
import { archiveKey, checkHead, loadHeadVerifier, readArchive, retainHead, type Consistency, type HeadCheck, type LedgerIdentity, type RetainedHead, type SignedHead } from "../lib/retained-head";

export function BillsTrust() {
  const session = useSession();
  const { t } = useTranslation();
  if (!session.data) return <PageState>{session.isError ? t("bills.trustLoadError") : t("common.loading")}</PageState>;
  const organization = session.data.organization;
  const identity = { site: window.location.origin, organizationId: organization.id, tenantId: organization.tenantId };
  return <RetainedTrust key={archiveKey(identity)} identity={identity} />;
}

function RetainedTrust({ identity }: { identity: LedgerIdentity }) {
  const { t } = useTranslation();
  const client = useQueryClient();
  const key = archiveKey(identity);
  const [initial] = useState(() => {
    try { return { held: readArchive(localStorage.getItem(key), identity), error: null }; }
    catch (error) { return { held: null, error: error instanceof Error ? error.message : String(error) }; }
  });
  const [held, setHeld] = useState<RetainedHead | null>(initial.held);
  const [error, setError] = useState<string | null>(initial.error);
  const [checked, setChecked] = useState<HeadCheck | null>(null);
  const [busy, setBusy] = useState(false);
  const active = useRef(true);
  const working = useRef(false);
  useEffect(() => { active.current = true; return () => { active.current = false; }; }, []);
  function current() {
    const session = client.getQueryData<SessionInfo>(["session"]);
    return active.current && session?.organization.id === identity.organizationId && session.organization.tenantId === identity.tenantId;
  }
  async function verify() {
    if (working.current) return;
    working.current = true;
    setBusy(true); setError(null); setChecked(null);
    try {
      const raw = localStorage.getItem(key);
      const archived = readArchive(raw, identity);
      if (held && JSON.stringify(archived) !== JSON.stringify(held)) throw new Error(t("bills.archiveChanged"));
      const served = await api<SignedHead>("/log/head");
      const verifier = await loadHeadVerifier();
      if (!current()) return;
      if (archived) {
        const result = await checkHead(archived, served, verifier, (size) => api<Consistency>(`/log/consistency?from=${size}`));
        if (!current()) return;
        if (localStorage.getItem(key) !== raw) throw new Error(t("bills.archiveChanged"));
        setHeld(archived); setChecked(result);
      } else {
        const record = await retainHead(identity, served, verifier);
        // Web Locks prevent concurrent tabs from replacing each other's first observation.
        if (!navigator.locks) throw new Error(t("bills.storageUnavailable"));
        await navigator.locks.request(key, () => {
          if (!current()) return;
          if (localStorage.getItem(key) !== null) throw new Error(t("bills.archiveChanged"));
          localStorage.setItem(key, JSON.stringify(record));
          setHeld(record);
        });
      }
    } catch (failure) {
      if (current()) setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      working.current = false;
      if (active.current) setBusy(false);
    }
  }
  function download() {
    if (!held) return;
    try {
      const blob = new Blob([JSON.stringify({ retained: held, checked }, null, 2) + "\n"], { type: "application/json" });
      const url = URL.createObjectURL(blob);
      const anchor = document.createElement("a");
      anchor.href = url; anchor.download = `oxsum-trust-${identity.organizationId}.json`;
      anchor.click(); setTimeout(() => URL.revokeObjectURL(url), 1000);
    } catch (failure) { setError(failure instanceof Error ? failure.message : String(failure)); }
  }
  return <Card title={t("bills.retainedTrust")}>
    <p className="text-sm">{t("bills.trustIntro")}</p>
    <p className="mt-2 text-sm text-muted">{t("bills.trustScope")}</p>
    {held ? <div className="mt-4 flex flex-col gap-2">
      <p className="text-sm">{t("bills.archived", { size: held.head.size })} · {held.retainedAt}</p>
      <CodeBlock language="json" code={JSON.stringify({ organizationId: held.identity.organizationId, origin: held.origin, key: held.key, head: held.head }, null, 2)} />
    </div> : <p className="mt-4 text-sm">{t("bills.noRetainedTrust")}</p>}
    <div className="mt-4 flex flex-wrap gap-3">
      <Button tone="ghost" onClick={verify} isLoading={busy} disabled={busy}>{held ? t("bills.checkConsistency") : t("bills.retainVerified")}</Button>
      <Button tone="ghost" onClick={download} disabled={!held || busy}>{t("bills.downloadTrust")}</Button>
      <a className="self-center text-sm text-primary" href="https://github.com/AllureCurtain/oxsum/blob/main/docs/verify-your-bill.md">{t("bills.offlineGuide")}</a>
    </div>
    <div className="mt-3" aria-live="polite">
      {error ? <Banner tone="error"><ShieldAlert className="inline size-4" /> {t("bills.verificationFailed")} {error}</Banner> : null}
      {checked ? <Banner tone={checked.kind === "emptyAnchor" ? "warn" : "success"}>
        {checked.kind === "emptyAnchor" ? <ShieldAlert className="inline size-4" /> : <CheckCircle2 className="inline size-4" />} {t(`bills.verified_${checked.kind}`, { size: checked.head.size, oldSize: held?.head.size })}
      </Banner> : null}
    </div>
  </Card>;
}
