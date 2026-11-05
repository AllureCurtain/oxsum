import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useNavigate, useSearchParams } from "react-router-dom";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../../api/client";
import type { components } from "../../api/schema";
import { Banner, Button, Field, TextInput } from "../../components/ui";
import { AuthCard, ErrorText } from "./shared";

export function RedeemInvitation() {
  const { t } = useTranslation();
  const [params] = useSearchParams();
  const navigate = useNavigate();
  const client = useQueryClient();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const token = params.get("token") ?? "";
  const identity = useQuery({ queryKey: ["session-context"], queryFn: () => api<components["schemas"]["SessionContextInfo"]>("/session/context"), retry: false });
  const preview = useQuery({ queryKey: ["invitation", token], queryFn: () => api<components["schemas"]["InvitationPreview"]>("/invitations/preview", { method: "POST", body: { token } }), enabled: !!token, retry: false });
  const redeem = useMutation({
    mutationFn: (body: { token: string; email: string; password: string }) =>
      api<components["schemas"]["Registration"]>("/invitations/redeem", { method: "POST", body }),
  });
  const accept = useMutation({
    mutationFn: () => api<components["schemas"]["InvitationAcceptance"]>("/invitations/accept", { method: "POST", body: { token } }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["orgs"] }),
  });
  if (accept.isSuccess || redeem.isSuccess) return (
    <AuthCard title={t("auth.invitationAccepted")}>
      {redeem.data ? <Banner tone="success">{t("auth.firstKey")} <code className="mono break-all">{redeem.data.apiKey.secret}</code></Banner> : <p>{t("auth.invitationAcceptedHint")}</p>}
      <Button className="mt-4 w-full" onClick={() => navigate(redeem.data ? "/login" : "/")}>{t("auth.invitationContinue")}</Button>
    </AuthCard>
  );
  return (
    <AuthCard title={t("auth.joinOrgTitle")}>
      {preview.data ? <p className="mb-4 text-sm">{t("auth.invitationOffer", { name: preview.data.name, role: preview.data.role })}</p> : null}
      <ErrorText error={preview.error} />
      {!token ? <p>{t("auth.invitationMissing")}</p> : null}
      {identity.isPending ? <p>{t("auth.invitationLoading")}</p> : identity.data ? (
        <div className="flex flex-col gap-3">
          <p className="text-sm">{t("auth.invitationAs", { email: identity.data.user.email })}</p>
          <ErrorText error={accept.error} />
          <Button disabled={accept.isPending || !preview.data} onClick={() => accept.mutate()}>{t("auth.invitationAccept")}</Button>
        </div>
      ) : (
        <>
          <Link className="mb-4 block text-primary" to={`/login?returnTo=${encodeURIComponent(`/invite?token=${encodeURIComponent(token)}`)}`}>{t("auth.invitationSignIn")}</Link>
          <form className="flex flex-col gap-3" onSubmit={(e) => { e.preventDefault(); redeem.mutate({ token, email, password }); }}>
            <Field label={t("auth.email")}><TextInput type="email" required value={email} onChange={(e) => setEmail(e.target.value)} /></Field>
            <Field label={t("auth.password")} hint={t("auth.passwordHint")}>
              <TextInput type="password" required minLength={12} autoComplete="new-password" value={password} onChange={(e) => setPassword(e.target.value)} />
            </Field>
            <ErrorText error={redeem.error} />
            <Button type="submit" disabled={redeem.isPending || !preview.data}>{t("auth.registerIntoOrg")}</Button>
          </form>
        </>
      )}
    </AuthCard>
  );
}
