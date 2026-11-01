import { useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { Link, useNavigate } from "react-router-dom";
import { useMutation } from "@tanstack/react-query";
import { api } from "../../api/client";
import { useAuthMethods } from "../../api/hooks";
import { Banner, Button, Field, TextInput } from "../../components/ui";
import { AuthCard, ErrorText } from "./shared";

export function Register() {
  const { t } = useTranslation();
  const methods = useAuthMethods();
  const navigate = useNavigate();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [orgName, setOrgName] = useState("");
  const [secret, setSecret] = useState<string | null>(null);
  const register = useMutation({
    mutationFn: (body: { email: string; password: string; organizationName?: string; turnstileToken?: string | null }) =>
      api<{ apiKey: { secret: string }; verificationSent: boolean }>("/auth/register", {
        method: "POST",
        body,
      }),
  });

  function submit(e: FormEvent) {
    e.preventDefault();
    register.mutate(
      {
        email,
        password,
        organizationName: orgName || undefined,
        // The Turnstile widget renders only when a site key is configured; this
        // scaffold leaves the token absent and lets the server decide.
        turnstileToken: null,
      },
      { onSuccess: (data) => setSecret(data.apiKey.secret) },
    );
  }

  if (secret) {
    return (
      <AuthCard title={t("auth.registeredTitle")}>
        <Banner tone="success">
          {t("auth.firstKey")}
          <code className="mono ml-2 break-all">{secret}</code>
        </Banner>
        <Button className="mt-4 w-full" onClick={() => navigate("/login")}>
          {t("auth.toSignIn")}
        </Button>
      </AuthCard>
    );
  }

  return (
    <AuthCard title={t("auth.register")}>
      <form onSubmit={submit} className="flex flex-col gap-3">
        <Field label={t("auth.email")}>
          <TextInput type="email" required value={email} onChange={(e) => setEmail(e.target.value)} />
        </Field>
        <Field label={t("auth.password")} hint={t("auth.passwordHint")}>
          <TextInput
            type="password"
            required
            minLength={12}
            autoComplete="new-password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
          />
        </Field>
        <Field label={t("auth.orgName")} hint={t("auth.orgNameHint")}>
          <TextInput value={orgName} maxLength={80} onChange={(e) => setOrgName(e.target.value)} />
        </Field>
        <ErrorText error={register.error} />
        <Button type="submit" disabled={register.isPending}>
          {register.isPending ? t("auth.registering") : t("auth.register")}
        </Button>
      </form>
      {methods.data?.oauthGithub ? (
        <a
          href="/api/v1/auth/oauth/github"
          className="mt-3 block rounded-lg border border-line px-3 py-2 text-center text-sm hover:bg-canvas"
        >
          {t("auth.registerGithub")}
        </a>
      ) : null}
      <p className="mt-4 text-sm">
        <Link to="/login" className="text-primary">
          {t("auth.haveAccount")}
        </Link>
      </p>
    </AuthCard>
  );
}
