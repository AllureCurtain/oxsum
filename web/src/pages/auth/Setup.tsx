import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { Link, useNavigate } from "react-router-dom";
import { ApiError, api } from "../../api/client";
import type { components } from "../../api/schema";
import { Banner, Button, Checkbox, Field, TextInput } from "../../components/ui";
import { AuthCard } from "./shared";

type SetupResult = components["schemas"]["BusinessSetupResult"];
type SetupRequest = components["schemas"]["BusinessSetupRequest"];
type RegistrationRequest = components["schemas"]["BusinessSetupRegistrationRequest"];

/** The deployment invitation stays in component memory, never in a URL or storage. */
export function Setup() {
  const { t, i18n } = useTranslation();
  const navigate = useNavigate();
  const client = useQueryClient();
  const identity = useQuery({
    queryKey: ["session-context"],
    queryFn: () => api<components["schemas"]["SessionContextInfo"]>("/session/context"),
    retry: false,
    refetchOnWindowFocus: false,
  });
  const [token, setToken] = useState("");
  const [businessName, setBusinessName] = useState("");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [consent, setConsent] = useState(false);
  const [result, setResult] = useState<SetupResult | null>(null);
  const [contextKey] = useState(() => crypto.randomUUID());
  const setup = useMutation({
    mutationFn: (request: { path: string; body: SetupRequest | RegistrationRequest }) =>
      api<SetupResult>(request.path, { method: "POST", body: request.body }),
    onSuccess: async (data) => {
      setResult(data);
      setToken("");
      setPassword("");
      await client.invalidateQueries({ queryKey: ["session-context"] });
    },
  });
  const enterBusiness = useMutation({
    mutationFn: () => api<components["schemas"]["SessionContextInfo"]>("/session/context", {
      method: "POST",
      body: { idempotencyKey: contextKey, context: "business" },
    }),
    onSuccess: async (data) => {
      await client.cancelQueries();
      client.clear();
      client.setQueryData(["session-context"], data);
      navigate("/business", { replace: true });
    },
  });
  const signedOut = identity.error instanceof ApiError && identity.error.status === 401;
  const identityUnavailable = identity.isError && !signedOut;
  const staff = identity.data?.business;
  const ready = result !== null || staff != null;

  function submit(event: FormEvent) {
    event.preventDefault();
    if (!consent || setup.isPending || identityUnavailable || identity.isPending) return;
    const common = { token: token.trim(), businessName: businessName.trim() };
    setup.mutate(identity.data
      ? { path: "/business/setup", body: common }
      : { path: "/business/setup/register", body: { ...common, email: email.trim(), password } });
  }

  function errorMessage(error: unknown): string {
    if (error instanceof ApiError) {
      if (error.status === 404) return t("setup.invalidToken");
      if (error.status === 409) return t("setup.conflict");
      return error.message;
    }
    return t("setup.networkError");
  }

  return (
    <AuthCard title={t(ready ? "setup.completeTitle" : "setup.title")}>
      <div className="mb-4 flex justify-end">
        <Button tone="ghost" onClick={() => void i18n.changeLanguage(i18n.language.startsWith("zh") ? "en" : "zh-CN")}>
          {i18n.language.startsWith("zh") ? "English" : "中文"}
        </Button>
      </div>
      {identity.isPending ? <p role="status">{t("common.loading")}</p> : identityUnavailable ? (
        <div className="flex flex-col gap-3">
          <Banner tone="error">{t("setup.identityUnavailable")}</Banner>
          <Button onClick={() => void identity.refetch()}>{t("setup.retryIdentity")}</Button>
        </div>
      ) : ready ? (
        <div className="flex flex-col gap-4">
          <Banner tone="success">{result ? t("setup.created", { name: result.ownership.business.name }) : t("setup.alreadyStaff")}</Banner>
          <p className="text-sm text-muted">{t("setup.completeHint")}</p>
          {result?.registration ? (
            <details>
              <summary className="cursor-pointer text-sm text-primary">{t("setup.firstKey")}</summary>
              <p className="mt-2 text-sm text-muted">{t("setup.firstKeyHint")}</p>
              <code className="mono mt-2 block break-all text-sm">{result.registration.apiKey.secret}</code>
            </details>
          ) : null}
          {enterBusiness.error ? <div role="alert"><Banner tone="error">{errorMessage(enterBusiness.error)}</Banner></div> : null}
          {identity.data ? (
            <Button disabled={enterBusiness.isPending} onClick={() => enterBusiness.mutate()}>
              {t(enterBusiness.isPending ? "common.loading" : "setup.openBusiness")}
            </Button>
          ) : (
            <Link className="text-primary" to="/login?returnTo=%2Fsetup">{t("setup.signInContinue")}</Link>
          )}
        </div>
      ) : (
        <>
          <p className="mb-4 text-sm text-muted">{t("setup.intro")}</p>
          {identity.data ? (
            <p className="mb-4 break-all text-sm">{t("setup.identity", { email: identity.data.user.email })}</p>
          ) : (
            <Link className="mb-4 block text-primary" to="/login?returnTo=%2Fsetup">{t("setup.existingAccount")}</Link>
          )}
          <form onSubmit={submit} className="flex flex-col gap-3">
            <Field label={t("setup.token")} hint={t("setup.tokenHint")}>
              <TextInput id="setup-token" type="password" autoComplete="off" required disabled={setup.isPending} value={token} onChange={(event) => setToken(event.target.value)} />
            </Field>
            <Field label={t("setup.businessName")}>
              <TextInput id="setup-name" required maxLength={120} disabled={setup.isPending} value={businessName} onChange={(event) => setBusinessName(event.target.value)} />
            </Field>
            {!identity.data ? (
              <>
                <Field label={t("auth.email")} hint={t("setup.emailHint")}>
                  <TextInput id="setup-email" type="email" autoComplete="email" required disabled={setup.isPending} value={email} onChange={(event) => setEmail(event.target.value)} />
                </Field>
                <Field label={t("auth.password")} hint={t("auth.passwordHint")}>
                  <TextInput id="setup-password" type="password" autoComplete="new-password" required minLength={12} disabled={setup.isPending} value={password} onChange={(event) => setPassword(event.target.value)} />
                </Field>
              </>
            ) : null}
            <div className="flex items-start gap-3 py-2">
              <Checkbox id="setup-consent" className="mt-1 shrink-0" checked={consent} disabled={setup.isPending} onCheckedChange={(checked) => setConsent(checked === true)} />
              <label htmlFor="setup-consent" className="text-sm">{t("setup.consent")}</label>
            </div>
            {setup.error ? <div role="alert"><Banner tone="error">{errorMessage(setup.error)}</Banner></div> : null}
            <Button type="submit" disabled={!consent || setup.isPending}>
              {t(setup.isPending ? "setup.accepting" : "setup.accept")}
            </Button>
          </form>
          <p className="mt-4 text-sm text-muted">{t("setup.recovery")}</p>
        </>
      )}
    </AuthCard>
  );
}
