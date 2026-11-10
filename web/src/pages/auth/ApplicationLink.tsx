import { useEffect, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { useLogout } from "../../api/hooks/session";
import { Link } from "react-router-dom";
import { ApiError, api } from "../../api/client";
import type { components } from "../../api/schema";
import { Banner, Button, Checkbox } from "../../components/ui";
import { applicationLinkKey, applicationLinkToken } from "../../lib/application-link";
import { AuthCard, ErrorText } from "./shared";

type Invitation = components["schemas"]["ApplicationLinkInvitation"];
export function ApplicationLink() {
  const { t } = useTranslation();
  const client = useQueryClient();
  const logout = useLogout();
  const [token, setToken] = useState(() => {
    let retained: string | null = null;
    try { retained = sessionStorage.getItem(applicationLinkKey); } catch { /* A disabled store requires reopening the original link after login. */ }
    return applicationLinkToken(window.location.hash, retained, Date.now());
  });
  const [confirmed, setConfirmed] = useState(false);
  const busy = useRef(false);
  useEffect(() => {
    if (window.location.hash) {
      try {
        if (token) sessionStorage.setItem(applicationLinkKey, JSON.stringify({ token, until: Date.now() + 7 * 86_400_000 }));
        else sessionStorage.removeItem(applicationLinkKey);
      } catch { /* The in-memory token still works in the current page. */ }
      history.replaceState(history.state, "", window.location.pathname + window.location.search);
    }
  }, [token]);
  const identity = useQuery({ queryKey: ["session-context"], queryFn: () => api<components["schemas"]["SessionContextInfo"]>("/session/context"), retry: false, refetchOnWindowFocus: true });
  // Keep secrets out of query keys and persistent caches. Only POST bodies carry them.
  const preview = useMutation({ mutationFn: () => api<Invitation>("/application-links/preview", { method: "POST", body: { token } }) });
  const accept = useMutation({ mutationFn: () => api<Invitation>("/application-links/accept", { method: "POST", body: { token } }), onSuccess: () => { try { sessionStorage.removeItem(applicationLinkKey); } catch { /* Optional tab storage. */ } void client.invalidateQueries({ queryKey: ["session-context"] }); void client.invalidateQueries({ queryKey: ["orgs"] }); } });
  const userId = identity.data?.user.id;
  const verified = identity.data?.user.emailVerified;
  useEffect(() => {
    setConfirmed(false);
    preview.reset();
    accept.reset();
    if (token && userId && verified) preview.mutate();
    // Each identity or token change starts a fresh explicit consent preview.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [token, userId, verified]);
  const result = accept.data ?? preview.data;
  return <AuthCard title={t("auth.applicationLink.title")}><div className="flex flex-col gap-4">
    {!token ? <p>{t("auth.applicationLink.missing")}</p> : identity.isPending ? <p role="status">{t("common.loading")}</p> : !identity.data ? <><ErrorText error={identity.error} /><Link className="text-primary" to="/login?returnTo=%2Fapplication-link">{t("auth.applicationLink.signIn")}</Link></> : <>
      <p className="break-words">{t("auth.applicationLink.as", { email: identity.data.user.email })}</p>
      <Button tone="ghost" disabled={accept.isPending || logout.isPending} onClick={() => { preview.reset(); accept.reset(); setConfirmed(false); logout.mutate(); }}>{t("auth.applicationLink.signOut")}</Button>
      {!verified ? <><Banner tone="warn">{t("auth.applicationLink.verify")}</Banner><Link className="text-primary" to="/settings">{t("auth.applicationLink.settings")}</Link></> : null}
      {preview.isPending ? <p role="status">{t("common.loading")}</p> : null}
      <ErrorText error={preview.error} /><ErrorText error={accept.error} />
      {(preview.error && !(preview.error instanceof ApiError)) || (accept.error && !(accept.error instanceof ApiError)) ? <Banner tone="error">{t("auth.applicationLink.networkError")}</Banner> : null}
      {result ? <>
        <p className="break-words">{t("auth.applicationLink.offer", { application: result.applicationName, customer: result.customerName })}</p>
        <dl className="flex flex-col gap-2 text-sm"><div><dt>{t("auth.applicationLink.account")}</dt><dd className="break-all font-mono">{result.organizationId}</dd></div><div><dt>{t("auth.applicationLink.email")}</dt><dd className="break-words">{result.intendedEmail}</dd></div><div><dt>{t("auth.applicationLink.expires")}</dt><dd>{result.expiresAt}</dd></div></dl>
        {result.acceptedAt ? <><Banner tone="success">{t("auth.applicationLink.accepted")}</Banner><Link className="text-primary" to="/">{t("auth.applicationLink.continue")}</Link></> : <>
          <p className="text-sm">{t("auth.applicationLink.disclosure")}</p>
          <label className="flex items-start gap-2"><Checkbox checked={confirmed} disabled={accept.isPending} aria-label={t("auth.applicationLink.confirm")} onCheckedChange={(value) => setConfirmed(value === true)} /><span>{t("auth.applicationLink.confirm")}</span></label>
          <Button disabled={!confirmed || accept.isPending || preview.isPending} isLoading={accept.isPending} onClick={async () => { if (busy.current) return; busy.current = true; try { await accept.mutateAsync(); } catch { /* Keep the exact token for a safe acceptance retry. */ } finally { busy.current = false; } }}>{t("auth.applicationLink.accept")}</Button>
        </>}
      </> : null}
      {!result?.acceptedAt ? <Button tone="ghost" disabled={identity.isFetching || preview.isPending || accept.isPending} onClick={async () => { setConfirmed(false); preview.reset(); accept.reset(); const refreshed = await identity.refetch(); if (refreshed.data?.user.emailVerified) preview.mutate(); }}>{t("auth.applicationLink.refresh")}</Button> : null}
    </>}
    {token ? <Button tone="ghost" disabled={accept.isPending} onClick={() => { try { sessionStorage.removeItem(applicationLinkKey); } catch { /* Optional tab storage. */ } setToken(""); preview.reset(); accept.reset(); }}>{t("auth.applicationLink.forget")}</Button> : null}
  </div></AuthCard>;
}
